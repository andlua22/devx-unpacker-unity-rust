//! Scene, prefab, and rigged hierarchy batch/single exporters (`.glb`, `.gltf`, `.fbx`, `.dae`, `.obj`+`.mtl`).

use crate::{ExportError, ExportResult};
use devx_codecs::collada::export_scene_to_dae;
use devx_codecs::fbx::export_scene_to_fbx;
use devx_codecs::gltf_scene::{
    GltfSceneOptions, GltfSceneSummary, export_hierarchy_to_glb, export_hierarchy_to_gltf_files,
};
use devx_codecs::mesh::{format_mtl_material, sanitize_obj_name};
use devx_codecs::scene_extract::{
    SceneDocument, deform_node_mesh_world, evaluate_scene_pose, extract_unity_scene,
};
use devx_plugin_api::AssetRef;
use devx_unity_io::{UnityWorkspaceData, extract_i64_field, read_object};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Supported multi-node 3D hierarchy export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HierarchyExportFormat {
    Glb,
    Gltf,
    Fbx,
    Dae,
    ObjMtl,
}

impl HierarchyExportFormat {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "glb" => Some(Self::Glb),
            "gltf" => Some(Self::Gltf),
            "fbx" => Some(Self::Fbx),
            "dae" | "collada" => Some(Self::Dae),
            "obj" | "obj+mtl" | "objmtl" => Some(Self::ObjMtl),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Glb => "glb",
            Self::Gltf => "gltf",
            Self::Fbx => "fbx",
            Self::Dae => "dae",
            Self::ObjMtl => "obj",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Glb => "GLB (.glb)",
            Self::Gltf => "glTF 2.0 Folder (.gltf + .bin + textures)",
            Self::Fbx => "Autodesk FBX Binary (.fbx)",
            Self::Dae => "COLLADA (.dae + textures)",
            Self::ObjMtl => "Wavefront OBJ (.obj + .mtl + textures)",
        }
    }
}

/// Summary returned after exporting a hierarchy in any supported 3D format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HierarchyFormatReport {
    pub format: HierarchyExportFormat,
    pub name: String,
    pub source_bundle: String,
    pub output_path: PathBuf,
    pub summary: GltfSceneSummary,
}

/// Summary entry written alongside exported hierarchy `.glb` files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HierarchyGlbReport {
    pub name: String,
    pub source_bundle: String,
    pub output_path: PathBuf,
    pub summary: GltfSceneSummary,
}

/// Discovered exportable hierarchy group (one per scene / prefab bundle or standalone file).
#[derive(Debug, Clone)]
pub struct DiscoveredHierarchyGroup {
    pub stem: String,
    pub source_bundle: String,
    pub roots: Vec<AssetRef>,
}

/// Exports specific hierarchy roots (plus optional external `AnimationClip`s) to `out_path`.
pub fn export_hierarchy_glb(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    out_path: &Path,
    opts: &GltfSceneOptions,
) -> ExportResult<HierarchyGlbReport> {
    let (glb_bytes, summary) = export_hierarchy_to_glb(workspace, roots, extra_clips, opts)?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, &glb_bytes)?;

    let source_bundle = roots
        .first()
        .and_then(|r| workspace.files.get(r.file_index))
        .map(|f| f.virtual_path.clone())
        .unwrap_or_else(|| "workspace".to_owned());
    let name = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hierarchy".to_owned());

    Ok(HierarchyGlbReport {
        name,
        source_bundle,
        output_path: out_path.to_path_buf(),
        summary,
    })
}

fn build_format_report(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    out_path: &Path,
    format: HierarchyExportFormat,
    summary: GltfSceneSummary,
) -> HierarchyFormatReport {
    let source_bundle = roots
        .first()
        .and_then(|r| workspace.files.get(r.file_index))
        .map(|f| f.virtual_path.clone())
        .unwrap_or_else(|| "workspace".to_owned());
    let name = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hierarchy".to_owned());

    HierarchyFormatReport {
        format,
        name,
        source_bundle,
        output_path: out_path.to_path_buf(),
        summary,
    }
}

fn summary_from_scene_doc(scene: &SceneDocument) -> GltfSceneSummary {
    let triangle_count: usize = scene
        .meshes
        .iter()
        .map(|m| m.indices.len() / 3)
        .sum();
    let mut bone_names: Vec<String> = scene
        .nodes
        .iter()
        .filter(|n| n.is_bone)
        .map(|n| n.name.clone())
        .collect();
    bone_names.sort();
    bone_names.dedup();
    let animation_names: Vec<String> = scene.animations.iter().map(|a| a.name.clone()).collect();

    GltfSceneSummary {
        node_count: scene.nodes.len(),
        mesh_count: scene.meshes.len(),
        triangle_count,
        material_count: scene.materials.len(),
        texture_count: scene.textures.len(),
        skin_count: scene.skins.len(),
        animation_count: scene.animations.len(),
        bone_names,
        animation_names,
    }
}

fn write_relative_files(base_dir: &Path, files: &[(String, Vec<u8>)]) -> ExportResult<()> {
    for (rel, bytes) in files {
        let dest = base_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, bytes)?;
    }
    Ok(())
}

/// Exports specific hierarchy roots (with skinning & animation tracks) as Binary FBX 7.4 (`.fbx`).
pub fn export_hierarchy_fbx(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    out_path: &Path,
    opts: &GltfSceneOptions,
) -> ExportResult<HierarchyFormatReport> {
    let scene = extract_unity_scene(workspace, roots, extra_clips, opts)?;
    let fbx_bytes = export_scene_to_fbx(&scene)?;
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, &fbx_bytes)?;
    Ok(build_format_report(
        workspace,
        roots,
        out_path,
        HierarchyExportFormat::Fbx,
        summary_from_scene_doc(&scene),
    ))
}

/// Exports specific hierarchy roots as unpacked glTF 2.0 (`.gltf` + `.bin` + `textures/*.png`).
pub fn export_hierarchy_gltf_dir(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    out_gltf_path: &Path,
    opts: &GltfSceneOptions,
) -> ExportResult<HierarchyFormatReport> {
    let out_dir = out_gltf_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(out_dir)?;
    let stem = out_gltf_path
        .file_stem()
        .map(|s| sanitize_stem(&s.to_string_lossy()))
        .unwrap_or_else(|| "scene".to_owned());

    let bundle = export_hierarchy_to_gltf_files(workspace, roots, extra_clips, &stem, opts)?;
    std::fs::write(out_gltf_path, bundle.gltf_json.as_bytes())?;
    std::fs::write(out_dir.join(format!("{stem}.bin")), &bundle.bin_bytes)?;
    write_relative_files(out_dir, &bundle.textures)?;

    Ok(build_format_report(
        workspace,
        roots,
        out_gltf_path,
        HierarchyExportFormat::Gltf,
        bundle.summary,
    ))
}

/// Exports specific hierarchy roots as COLLADA 1.4.1 XML (`.dae` + `textures/*.png`).
pub fn export_hierarchy_dae_dir(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    out_dae_path: &Path,
    opts: &GltfSceneOptions,
) -> ExportResult<HierarchyFormatReport> {
    let scene = extract_unity_scene(workspace, roots, extra_clips, opts)?;
    let (dae_xml, tex_files) = export_scene_to_dae(&scene, "textures")?;
    let out_dir = out_dae_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dae_path, dae_xml.as_bytes())?;
    write_relative_files(out_dir, &tex_files)?;

    Ok(build_format_report(
        workspace,
        roots,
        out_dae_path,
        HierarchyExportFormat::Dae,
        summary_from_scene_doc(&scene),
    ))
}

/// Exports specific hierarchy roots in right-handed posed Wavefront `.obj` + `.mtl` + `textures/*.png`.
pub fn export_hierarchy_obj_dir(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    out_obj_path: &Path,
    opts: &GltfSceneOptions,
) -> ExportResult<HierarchyFormatReport> {
    let scene = extract_unity_scene(workspace, roots, extra_clips, opts)?;
    let pose = evaluate_scene_pose(&scene, None, 0.0);

    let out_dir = out_obj_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(out_dir)?;
    let stem = out_obj_path
        .file_stem()
        .map(|s| sanitize_stem(&s.to_string_lossy()))
        .unwrap_or_else(|| "scene".to_owned());
    let mtl_filename = format!("{stem}.mtl");

    // Emit textures under `textures/*.png`.
    let tex_files: Vec<(String, Vec<u8>)> = scene
        .textures
        .iter()
        .map(|t| (format!("textures/{}.png", t.file_stem), t.png_bytes.to_vec()))
        .collect();
    write_relative_files(out_dir, &tex_files)?;

    // Build `.mtl` library with unique material names.
    let mut mtl_text = String::from("# Exported by devx-exporter (DevX Unity Rust)\n\n");
    let mut mat_names: Vec<String> = Vec::with_capacity(scene.materials.len());
    let mut used_mat_names = HashSet::new();
    for (idx, mat) in scene.materials.iter().enumerate() {
        let base = sanitize_obj_name(&mat.name);
        let mut unique = base.clone();
        if !used_mat_names.insert(unique.clone()) {
            unique = format!("{base}_{idx}");
            used_mat_names.insert(unique.clone());
        }
        let tex_rel = mat
            .albedo_tex
            .and_then(|ti| scene.textures.get(ti))
            .map(|t| format!("textures/{}.png", t.file_stem));
        mtl_text.push_str(&format_mtl_material(
            &unique,
            [mat.base_color[0], mat.base_color[1], mat.base_color[2]],
            mat.base_color[3],
            (1.0 - mat.roughness).clamp(0.0, 1.0),
            tex_rel.as_deref(),
        ));
        mtl_text.push('\n');
        mat_names.push(unique);
    }
    if mat_names.is_empty() {
        mtl_text.push_str(&format_mtl_material("default_mat", [0.8, 0.8, 0.8], 1.0, 0.2, None));
    }
    std::fs::write(out_dir.join(&mtl_filename), mtl_text.as_bytes())?;

    // Combine all mesh nodes transformed into bind pose world space into `<stem>.obj`.
    let mut obj_text = format!("# Exported by devx-exporter\nmtllib {mtl_filename}\n");
    let mut v_offset = 1usize;
    let mut vt_offset = 1usize;
    let mut vn_offset = 1usize;

    for (node_idx, node) in scene.nodes.iter().enumerate() {
        let Some(mesh_idx) = node.mesh_idx else {
            continue;
        };
        let Some(mesh) = scene.meshes.get(mesh_idx) else {
            continue;
        };
        let Some(deformed) = deform_node_mesh_world(&scene, node_idx, &pose) else {
            continue;
        };
        let _ = writeln!(obj_text, "\no {}_{}", sanitize_obj_name(&node.name), node_idx);
        for p in &deformed.positions {
            let _ = writeln!(obj_text, "v {:.6} {:.6} {:.6}", p[0], p[1], p[2]);
        }
        let has_uv = mesh.uvs.len() == deformed.positions.len();
        if has_uv {
            for uv in &mesh.uvs {
                let _ = writeln!(obj_text, "vt {:.6} {:.6}", uv[0], uv[1]);
            }
        }
        let has_vn = deformed.normals.len() == deformed.positions.len();
        if has_vn {
            for n in &deformed.normals {
                let _ = writeln!(obj_text, "vn {:.6} {:.6} {:.6}", n[0], n[1], n[2]);
            }
        }

        let subs = if mesh.submeshes.is_empty() {
            vec![devx_codecs::scene_extract::SceneSubMesh {
                index_start: 0,
                index_count: mesh.indices.len(),
                material_idx: None,
            }]
        } else {
            mesh.submeshes.clone()
        };

        for sub in subs {
            let mname = sub
                .material_idx
                .and_then(|mi| mat_names.get(mi))
                .map(String::as_str)
                .unwrap_or("default_mat");
            let _ = writeln!(obj_text, "usemtl {mname}");
            let end = (sub.index_start + sub.index_count).min(mesh.indices.len());
            let (tris, _) = mesh.indices[sub.index_start.min(end)..end].as_chunks::<3>();
            for &[ta, tb, tc] in tris {
                let (a, b, c) = (ta as usize, tb as usize, tc as usize);
                match (has_uv, has_vn) {
                    (true, true) => {
                        let _ = writeln!(
                            obj_text,
                            "f {}/{}/{} {}/{}/{} {}/{}/{}",
                            v_offset + a,
                            vt_offset + a,
                            vn_offset + a,
                            v_offset + b,
                            vt_offset + b,
                            vn_offset + b,
                            v_offset + c,
                            vt_offset + c,
                            vn_offset + c
                        );
                    }
                    (true, false) => {
                        let _ = writeln!(
                            obj_text,
                            "f {}/{} {}/{} {}/{}",
                            v_offset + a,
                            vt_offset + a,
                            v_offset + b,
                            vt_offset + b,
                            v_offset + c,
                            vt_offset + c
                        );
                    }
                    (false, true) => {
                        let _ = writeln!(
                            obj_text,
                            "f {}//{} {}//{} {}//{}",
                            v_offset + a,
                            vn_offset + a,
                            v_offset + b,
                            vn_offset + b,
                            v_offset + c,
                            vn_offset + c
                        );
                    }
                    (false, false) => {
                        let _ = writeln!(
                            obj_text,
                            "f {} {} {}",
                            v_offset + a,
                            v_offset + b,
                            v_offset + c
                        );
                    }
                }
            }
        }
        v_offset += deformed.positions.len();
        if has_uv {
            vt_offset += mesh.uvs.len();
        }
        if has_vn {
            vn_offset += deformed.normals.len();
        }
    }

    std::fs::write(out_obj_path, obj_text.as_bytes())?;
    Ok(build_format_report(
        workspace,
        roots,
        out_obj_path,
        HierarchyExportFormat::ObjMtl,
        summary_from_scene_doc(&scene),
    ))
}

/// Dispatches hierarchy export for any supported [`HierarchyExportFormat`].
pub fn export_hierarchy_with_format(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    out_path: &Path,
    format: HierarchyExportFormat,
    opts: &GltfSceneOptions,
) -> ExportResult<HierarchyFormatReport> {
    match format {
        HierarchyExportFormat::Glb => {
            let rep = export_hierarchy_glb(workspace, roots, extra_clips, out_path, opts)?;
            Ok(HierarchyFormatReport {
                format,
                name: rep.name,
                source_bundle: rep.source_bundle,
                output_path: rep.output_path,
                summary: rep.summary,
            })
        }
        HierarchyExportFormat::Gltf => {
            export_hierarchy_gltf_dir(workspace, roots, extra_clips, out_path, opts)
        }
        HierarchyExportFormat::Fbx => {
            export_hierarchy_fbx(workspace, roots, extra_clips, out_path, opts)
        }
        HierarchyExportFormat::Dae => {
            export_hierarchy_dae_dir(workspace, roots, extra_clips, out_path, opts)
        }
        HierarchyExportFormat::ObjMtl => {
            export_hierarchy_obj_dir(workspace, roots, extra_clips, out_path, opts)
        }
    }
}

/// Scans `workspace.files` for bundles/files containing 3D geometry (`Mesh`, `MeshFilter`, or
/// `SkinnedMeshRenderer`) and groups their root `Transform`s (or standalone `Mesh`es) for export.
pub fn discover_hierarchy_groups(
    workspace: &UnityWorkspaceData,
    filter: Option<&str>,
) -> Vec<DiscoveredHierarchyGroup> {
    let needle = filter.map(|s| s.to_ascii_lowercase());
    let mut grouped: BTreeMap<String, DiscoveredHierarchyGroup> = BTreeMap::new();

    for (file_index, loaded) in workspace.files.iter().enumerate() {
        if let Some(ref n) = needle
            && !loaded.virtual_path.to_ascii_lowercase().contains(n)
            && !loaded.bundle_container_paths.values().any(|p| p.to_ascii_lowercase().contains(n))
        {
            continue;
        }

        let mut has_renderable = false;
        let mut root_transforms = Vec::new();
        let mut standalone_meshes = Vec::new();

        for info in &loaded.file.objects {
            match info.type_id {
                23 | 33 | 137 => has_renderable = true,
                43 => {
                    has_renderable = true;
                    standalone_meshes.push(AssetRef {
                        file_index,
                        path_id: info.path_id,
                    });
                }
                4 | 224 => {
                    if let Ok(obj) = read_object(loaded, info) {
                        let father_pid = obj
                            .class
                            .properties()
                            .get("m_Father")
                            .and_then(|v| v.as_object())
                            .and_then(|m| m.get("m_PathID"))
                            .and_then(extract_i64_field)
                            .unwrap_or(0);
                        if father_pid == 0 {
                            root_transforms.push(AssetRef {
                                file_index,
                                path_id: info.path_id,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        if !has_renderable {
            continue;
        }
        let roots = if !root_transforms.is_empty() {
            root_transforms
        } else {
            standalone_meshes
        };
        if roots.is_empty() {
            continue;
        }

        let stem = derive_bundle_stem(&loaded.virtual_path, file_index);
        // Group strictly per enclosing `.ab` archive (before `/CAB-*`) so sibling CAB chunks
        // inside one bundle merge cleanly without contaminating a Prefab (`sceneprefab/*.ab`)
        // with raw untextured FBX dependency meshes (`scenetexture/*.ab`) of the same stem.
        // Streamed scenes use `BuildPlayer-*.sharedAssets` + `BuildPlayer-*` instead of CAB
        // names. Both files still belong to one enclosing bundle. Group by that explicit
        // container identity so raw Mesh roots do not consume a separate export slot and
        // hide the sibling scene hierarchy with its renderer/material references.
        let bundle_path = loaded.container_name.as_deref().unwrap_or_else(|| {
            loaded.virtual_path.rsplit_once("/CAB-")
                .map_or(loaded.virtual_path.as_str(), |(prefix, _)| prefix)
        });
        let bundle_key = format!("{bundle_path}::{stem}");
        grouped
            .entry(bundle_key)
            .and_modify(|g| g.roots.extend(roots.iter().copied()))
            .or_insert(DiscoveredHierarchyGroup {
                stem,
                source_bundle: loaded.virtual_path.clone(),
                roots,
            });
    }

    grouped.into_values().collect()
}

/// Collects every `AnimationClip` (`class_id == 74`) across all currently loaded workspace files.
pub fn collect_loaded_animation_clips(workspace: &UnityWorkspaceData) -> Vec<AssetRef> {
    let mut clips = Vec::new();
    for (file_index, loaded) in workspace.files.iter().enumerate() {
        for info in &loaded.file.objects {
            if info.type_id == 74 {
                clips.push(AssetRef {
                    file_index,
                    path_id: info.path_id,
                });
            }
        }
    }
    clips
}

/// Exports every matching hierarchy group in `workspace` as a `.glb` inside `out_dir`,
/// writing a `hierarchies.json` report alongside the files.
pub fn export_all_hierarchies(
    workspace: &UnityWorkspaceData,
    out_dir: &Path,
    filter: Option<&str>,
    limit: Option<usize>,
    include_loaded_clips: bool,
    opts: &GltfSceneOptions,
) -> ExportResult<Vec<HierarchyGlbReport>> {
    std::fs::create_dir_all(out_dir)?;
    let groups = discover_hierarchy_groups(workspace, filter);
    let extra_clips = if include_loaded_clips {
        collect_loaded_animation_clips(workspace)
    } else {
        Vec::new()
    };

    let mut used_stems = HashSet::new();
    let mut reports = Vec::new();

    for group in groups.into_iter().take(limit.unwrap_or(usize::MAX)) {
        let mut stem = sanitize_stem(&group.stem);
        if !used_stems.insert(stem.clone()) {
            let mut n = 2usize;
            while !used_stems.insert(format!("{stem}_{n}")) {
                n += 1;
            }
            stem = format!("{stem}_{n}");
        }
        let out_path = out_dir.join(format!("{stem}.glb"));
        match export_hierarchy_glb(workspace, &group.roots, &extra_clips, &out_path, opts) {
            Ok(rep) => {
                if rep.summary.mesh_count > 0 {
                    reports.push(rep);
                } else {
                    let _ = std::fs::remove_file(&out_path);
                }
            }
            Err(_) => continue,
        }
    }

    let manifest_json = serde_json::to_string_pretty(&reports)
        .map_err(|e| ExportError::Yaml(format!("hierarchy manifest serialize: {e}")))?;
    std::fs::write(out_dir.join("hierarchies.json"), manifest_json)?;
    Ok(reports)
}

/// Exports every matching hierarchy group in `workspace` using any [`HierarchyExportFormat`].
/// Folder-oriented formats (`Gltf`, `Dae`, `ObjMtl`) get a clean `<out_dir>/<stem>/` subdirectory
/// so sidecar textures/bins never collide across groups.
pub fn export_all_hierarchies_with_format(
    workspace: &UnityWorkspaceData,
    out_dir: &Path,
    format: HierarchyExportFormat,
    filter: Option<&str>,
    limit: Option<usize>,
    include_loaded_clips: bool,
    opts: &GltfSceneOptions,
) -> ExportResult<Vec<HierarchyFormatReport>> {
    if format == HierarchyExportFormat::Glb {
        let reps = export_all_hierarchies(
            workspace,
            out_dir,
            filter,
            limit,
            include_loaded_clips,
            opts,
        )?;
        return Ok(reps
            .into_iter()
            .map(|r| HierarchyFormatReport {
                format: HierarchyExportFormat::Glb,
                name: r.name,
                source_bundle: r.source_bundle,
                output_path: r.output_path,
                summary: r.summary,
            })
            .collect());
    }

    std::fs::create_dir_all(out_dir)?;
    let groups = discover_hierarchy_groups(workspace, filter);
    let extra_clips = if include_loaded_clips {
        collect_loaded_animation_clips(workspace)
    } else {
        Vec::new()
    };

    let mut used_stems = HashSet::new();
    let mut reports = Vec::new();
    let uses_subdir = matches!(
        format,
        HierarchyExportFormat::Gltf | HierarchyExportFormat::Dae | HierarchyExportFormat::ObjMtl
    );

    for group in groups.into_iter().take(limit.unwrap_or(usize::MAX)) {
        let mut stem = sanitize_stem(&group.stem);
        if !used_stems.insert(stem.clone()) {
            let mut n = 2usize;
            while !used_stems.insert(format!("{stem}_{n}")) {
                n += 1;
            }
            stem = format!("{stem}_{n}");
        }
        let target_dir = if uses_subdir {
            out_dir.join(&stem)
        } else {
            out_dir.to_path_buf()
        };
        let out_path = target_dir.join(format!("{stem}.{}", format.extension()));
        match export_hierarchy_with_format(workspace, &group.roots, &extra_clips, &out_path, format, opts) {
            Ok(rep) => {
                if rep.summary.mesh_count > 0 {
                    reports.push(rep);
                } else {
                    let _ = std::fs::remove_file(&out_path);
                }
            }
            Err(_) => continue,
        }
    }

    let manifest_json = serde_json::to_string_pretty(&reports)
        .map_err(|e| ExportError::Yaml(format!("hierarchy manifest serialize: {e}")))?;
    std::fs::write(out_dir.join("hierarchies.json"), manifest_json)?;
    Ok(reports)
}

pub fn derive_bundle_stem(display_path: &str, file_index: usize) -> String {
    let base = display_path.split("::").next().unwrap_or(display_path);
    for seg in base.split(['/', '\\']).rev() {
        for ext in [".ab", ".bundle", ".unity3d", ".assets"] {
            if let Some(stem) = seg.strip_suffix(ext)
                && !stem.is_empty()
            {
                return sanitize_stem(stem);
            }
        }
    }
    let file_name = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let trimmed = file_name
        .strip_suffix(".ab")
        .or_else(|| file_name.strip_suffix(".bundle"))
        .or_else(|| file_name.strip_suffix(".unity3d"))
        .unwrap_or(file_name);
    if trimmed.is_empty() {
        format!("asset_{file_index}")
    } else {
        sanitize_stem(trimmed)
    }
}

pub(crate) fn sanitize_stem(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out = "asset".to_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_scene_siblings_share_one_group_without_merging_dependency_bundles() {
        use devx_unity_io::{ContainerKind, LoadedAssetFile};
        use unity_asset_binary::asset::{ObjectInfo, SerializedFileParser};
        // Minimal parsed file backing; geometry metadata is enough for hierarchy discovery.
        let mut metadata = b"2021.3.23f1c1\0".to_vec();
        metadata.extend_from_slice(&0i32.to_le_bytes());
        metadata.push(0);
        for _ in 0..4 { metadata.extend_from_slice(&0i32.to_le_bytes()); }
        metadata.push(0);
        let data_offset = 20 + metadata.len();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&((data_offset + 1) as u32).to_be_bytes());
        bytes.extend_from_slice(&17u32.to_be_bytes());
        bytes.extend_from_slice(&(data_offset as u32).to_be_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&metadata);
        bytes.push(0);
        let mut workspace = UnityWorkspaceData::new(PathBuf::from("game.apk"));
        for (index, container, member) in [
            (0, "game.apk/assets/scene/a-57.ab", "BuildPlayer-A-57.sharedAssets"),
            (1, "game.apk/assets/scene/a-57.ab", "BuildPlayer-A-57"),
            (2, "game.apk/assets/scenetexture/a-57.ab", "CAB-dependency"),
        ] {
            let mut file = SerializedFileParser::from_bytes(bytes.clone()).expect("file");
            file.objects.push(ObjectInfo::new(1, 0, 0, 43, 0));
            workspace.files.push(LoadedAssetFile {
                file_index: index, container_name: Some(container.to_owned()),
                virtual_path: format!("{container}/{member}"), kind: ContainerKind::AssetBundle,
                file, raw_bytes: None, bundle_container_paths: Default::default(),
            });
        }
        let groups = discover_hierarchy_groups(&workspace, None);
        assert_eq!(groups.len(), 2);
        let scene = groups.iter().find(|g| g.source_bundle.contains("/scene/")).expect("scene");
        assert_eq!(scene.roots.len(), 2);
        assert!(scene.roots.iter().all(|r| r.file_index < 2));
    }

    #[test]
    fn derives_clean_bundle_stem() {
        assert_eq!(
            derive_bundle_stem("assets/role/monster_xlt_01.ab::CAB-1234", 0),
            "monster_xlt_01"
        );
        assert_eq!(
            derive_bundle_stem("lite4.0.apk/assets/role/monster_xlt_01.ab/CAB-1234", 0),
            "monster_xlt_01"
        );
        assert_eq!(sanitize_stem("a b/c:d"), "a_b_c_d");
    }
}
