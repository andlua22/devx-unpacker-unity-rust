//! Complete Unity scene / prefab hierarchy -> glTF 2.0 binary (`.glb`) exporter.
//!
//! Traverses `GameObject` / `Transform` / `RectTransform` trees across resolved
//! `.ab` bundle dependencies, assembling rigged skeletons (`SkinnedMeshRenderer`),
//! PBR `Material` + `Texture2D` images, and `AnimationClip` keyframes in right-handed
//! glTF 2.0 coordinates ready for Bevy 0.20 (`WorldAssetRoot` / `SceneRoot`).

use crate::animation::{DecodedAnimationClip, TrackProperty, decode_animation_clip, unity_path_crc32};
use crate::material::{DecodedMaterial, MaterialAlphaMode, decode_material};
use crate::mesh::{DecodedMeshFull, decode_mesh_full, pack_glb};
use crate::scene_extract::{
    collect_fallback_clip_refs, resolve_scene_root_transforms, resolve_standalone_mesh_roots,
};
use crate::texture::{TextureDecodeOptions, decode_texture2d, encode_png};
use crate::{CodecError, CodecResult};
use devx_plugin_api::{AssetRef, DecodedImage};
use devx_unity_io::{
    UnityValue, UnityWorkspaceData, extract_i32_field, extract_i64_field, read_object,
};
use gltf_json as json;
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};
use json::validation::Checked;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use unity_asset_binary::object::UnityObject;

type UnityRef = (usize, i64);

/// Options controlling what data is embedded in an exported hierarchy `.glb`.
#[derive(Debug, Clone)]
pub struct GltfSceneOptions {
    pub include_textures: bool,
    pub include_materials: bool,
    pub include_skinning: bool,
    pub include_animations: bool,
    pub fallback_unity_version: String,
    /// Optional max edge length (width/height) for embedded PNG textures (`None` = native size).
    pub max_texture_dim: Option<u32>,
    /// Clears root-node translation & rotation for standalone Character/Prop prefabs so they spawn
    /// cleanly centered at origin (`false` preserves full scene world placement).
    pub zero_root_transform: bool,
    /// Excludes `LOD1+` fallback renderers inside `LODGroup` (`class_id == 205`) when `LOD0` exists,
    /// avoiding coplanar Z-fighting in Bevy scenes (`false` preserves all LOD levels).
    pub skip_secondary_lods: bool,
}

impl Default for GltfSceneOptions {
    fn default() -> Self {
        Self {
            include_textures: true,
            include_materials: true,
            include_skinning: true,
            include_animations: true,
            fallback_unity_version: "2021.3.0f1".to_owned(),
            max_texture_dim: None,
            zero_root_transform: false,
            skip_secondary_lods: false,
        }
    }
}

/// Statistics returned after building a scene/prefab `.glb`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GltfSceneSummary {
    pub node_count: usize,
    pub mesh_count: usize,
    pub material_count: usize,
    pub texture_count: usize,
    pub skin_count: usize,
    pub animation_count: usize,
    pub triangle_count: usize,
    pub bone_names: Vec<String>,
    pub animation_names: Vec<String>,
}

/// External `.gltf` bundle output containing JSON text, binary `.bin` buffer, and PNG textures.
#[derive(Debug, Clone)]
pub struct GltfExternalBundle {
    pub gltf_json: String,
    pub bin_bytes: Vec<u8>,
    pub textures: Vec<(String, Vec<u8>)>,
    pub summary: GltfSceneSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GltfEmitMode {
    EmbeddedGlb,
    ExternalFiles,
}

/// Exports one or more Unity hierarchy roots (Prefabs, Transforms, GameObjects,
/// Renderers, AssetBundles, or standalone Meshes) plus optional extra animation clips
/// into a single right-handed `.glb` byte stream.
pub fn export_hierarchy_to_glb(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    opts: &GltfSceneOptions,
) -> CodecResult<(Vec<u8>, GltfSceneSummary)> {
    let ctx = build_gltf_scene_core(workspace, roots, extra_clips, opts, GltfEmitMode::EmbeddedGlb, "UnitySceneBuffer")?;
    let summary = build_gltf_summary(&ctx);
    let glb = pack_glb(&ctx.root, ctx.bin)?;
    Ok((glb, summary))
}

/// Exports one or more Unity hierarchy roots into separated `.gltf` JSON + `<bin_stem>.bin`
/// + `textures/*.png` bytes cleanly without duplicating scene assembly logic.
pub fn export_hierarchy_to_gltf_files(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    bin_stem: &str,
    opts: &GltfSceneOptions,
) -> CodecResult<GltfExternalBundle> {
    let clean_stem: String = bin_stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') { c } else { '_' })
        .collect();
    let stem = if clean_stem.is_empty() {
        "scene".to_owned()
    } else {
        clean_stem
    };
    let mut ctx = build_gltf_scene_core(
        workspace,
        roots,
        extra_clips,
        opts,
        GltfEmitMode::ExternalFiles,
        &format!("{stem}.bin"),
    )?;
    if let Some(buf) = ctx.root.buffers.first_mut() {
        buf.uri = Some(format!("{stem}.bin"));
    }
    let summary = build_gltf_summary(&ctx);
    let gltf_json = json::serialize::to_string_pretty(&ctx.root)
        .map_err(|e| CodecError::MalformedData(format!("gltf serialize error: {e}")))?;
    Ok(GltfExternalBundle {
        gltf_json,
        bin_bytes: ctx.bin,
        textures: ctx.external_textures,
        summary,
    })
}

fn build_gltf_summary(ctx: &GltfBuildCtx<'_>) -> GltfSceneSummary {
    let bone_names: Vec<String> = ctx
        .bone_node_indices
        .iter()
        .filter_map(|idx| ctx.node_names.get(idx).cloned())
        .collect();

    let animation_names: Vec<String> = ctx
        .root
        .animations
        .iter()
        .filter_map(|a| a.name.clone())
        .collect();

    GltfSceneSummary {
        node_count: ctx.root.nodes.len(),
        mesh_count: ctx.root.meshes.len(),
        material_count: ctx.root.materials.len(),
        texture_count: ctx.root.textures.len(),
        skin_count: ctx.root.skins.len(),
        animation_count: ctx.root.animations.len(),
        triangle_count: ctx.triangle_count,
        bone_names,
        animation_names,
    }
}

fn build_gltf_scene_core<'a>(
    workspace: &'a UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    opts: &'a GltfSceneOptions,
    emit_mode: GltfEmitMode,
    buffer_name: &str,
) -> CodecResult<GltfBuildCtx<'a>> {
    let mut ctx = GltfBuildCtx::new(workspace, opts, emit_mode, buffer_name);

    let root_transforms = resolve_scene_root_transforms(workspace, roots);
    if opts.skip_secondary_lods {
        ctx.collect_secondary_lod_renderers(&root_transforms);
    }
    let mut root_node_indices = Vec::new();
    for tr_ref in root_transforms {
        if let Some(node_idx) = ctx.build_transform_subtree(tr_ref)? {
            root_node_indices.push(node_idx);
        }
    }

    // Also allow direct export of standalone Mesh roots if no Transform was resolved.
    if root_node_indices.is_empty() {
        for r in resolve_standalone_mesh_roots(workspace, roots) {
            let uref = (r.file_index, r.path_id);
            if let Some(obj) = ctx.try_read_object(uref)
                && obj.class_id() == 43
            {
                let gltf_mesh = ctx.build_gltf_mesh(uref, &[], false)?;
                let mesh_name = obj.name().unwrap_or_else(|| format!("Mesh_{}", r.path_id));
                let node_idx = ctx.push_node(json::Node {
                    name: Some(mesh_name.clone()),
                    mesh: Some(gltf_mesh.mesh_idx),
                    ..Default::default()
                }, uref, &mesh_name);
                root_node_indices.push(node_idx);
            }
        }
    }

    if root_node_indices.is_empty() {
        return Err(CodecError::MalformedData(
            "no exportable GameObject, Transform, or Mesh found for selected roots".to_owned(),
        ));
    }

    // Bind deferred skins after the entire bone node forest exists.
    ctx.finalize_pending_skins()?;

    // Ensure every scene root lists top-level bones / orphan ancestors safely.
    let mut top_nodes: Vec<json::Index<json::Node>> = Vec::new();
    let mut seen_top = HashSet::new();
    for idx in root_node_indices {
        if seen_top.insert(idx) {
            top_nodes.push(json::Index::new(idx as u32));
        }
    }
    for idx in ctx.extra_scene_roots.clone() {
        if seen_top.insert(idx) {
            top_nodes.push(json::Index::new(idx as u32));
        }
    }

    if opts.zero_root_transform {
        for &top_idx in &top_nodes {
            if let Some(node) = ctx.root.nodes.get_mut(top_idx.value()) {
                node.translation = None;
                // Only strip pure Y-axis turnaround rotations; preserve ±90° X/Z pitch conversions
                // used to map Z-up static vehicle/prop meshes into Y-up glTF space.
                if let Some(q) = node.rotation
                    && q.0[0].abs() < 0.05
                    && q.0[2].abs() < 0.05
                {
                    node.rotation = None;
                }
            }
        }
    }

    // Write computed children arrays onto parent gltf nodes.
    for (parent_idx, kids) in &ctx.node_children {
        if let Some(node) = ctx.root.nodes.get_mut(*parent_idx)
            && !kids.is_empty()
        {
            node.children = Some(kids.iter().map(|&c| json::Index::new(c as u32)).collect());
        }
    }

    // Collect and bake animations into glTF Animation channels.
    if opts.include_animations {
        ctx.build_animations(roots, extra_clips, &top_nodes)?;
    }

    let scene_idx = json::Index::push(
        &mut ctx.root.scenes,
        json::Scene {
            name: Some("UnityHierarchy".to_owned()),
            extensions: None,
            extras: Default::default(),
            nodes: top_nodes,
        },
    );
    ctx.root.scene = Some(scene_idx);

    while !ctx.bin.len().is_multiple_of(4) {
        ctx.bin.push(0);
    }
    if let Some(buf) = ctx.root.buffers.first_mut() {
        buf.byte_length = json::validation::USize64::from(ctx.bin.len());
    } else {
        json::Index::push(
            &mut ctx.root.buffers,
            json::Buffer {
                byte_length: json::validation::USize64::from(ctx.bin.len()),
                name: Some(buffer_name.to_owned()),
                uri: None,
                extensions: None,
                extras: Default::default(),
            },
        );
    }

    Ok(ctx)
}

#[derive(Clone)]
struct CachedMeshGeometry {
    vertex_count: usize,
    pos_acc: json::Index<json::Accessor>,
    norm_acc: json::Index<json::Accessor>,
    uv_acc: json::Index<json::Accessor>,
    joints_acc: Option<json::Index<json::Accessor>>,
    weights_acc: Option<json::Index<json::Accessor>>,
    bind_poses_row_major: Vec<[f32; 16]>,
    submesh_indices: Vec<(json::Index<json::Accessor>, usize)>,
    morph_targets: Vec<json::mesh::MorphTarget>,
    morph_weights: Vec<f32>,
}

struct BuiltGltfMesh {
    mesh_idx: json::Index<json::Mesh>,
    has_skin_attributes: bool,
    bind_poses_row_major: Vec<[f32; 16]>,
}

struct PendingSkin {
    node_idx: usize,
    mesh_ref: UnityRef,
    bind_poses_row_major: Vec<[f32; 16]>,
    bone_refs: Vec<Option<UnityRef>>,
    root_bone_ref: Option<UnityRef>,
}

struct GltfBuildCtx<'a> {
    workspace: &'a UnityWorkspaceData,
    opts: &'a GltfSceneOptions,
    emit_mode: GltfEmitMode,
    external_textures: Vec<(String, Vec<u8>)>,
    used_tex_paths: HashSet<String>,
    root: json::Root,
    bin: Vec<u8>,
    buffer_idx: json::Index<json::Buffer>,

    transform_to_node: HashMap<UnityRef, usize>,
    node_names: HashMap<usize, String>,
    node_children: BTreeMap<usize, Vec<usize>>,
    extra_scene_roots: Vec<usize>,
    visiting_transforms: HashSet<UnityRef>,

    texture_cache: HashMap<(UnityRef, bool), json::Index<json::Texture>>,
    material_cache: HashMap<UnityRef, json::Index<json::Material>>,
    default_sampler: Option<json::Index<json::texture::Sampler>>,
    mesh_geo_cache: HashMap<UnityRef, CachedMeshGeometry>,
    mesh_mat_cache: HashMap<(UnityRef, Vec<Option<usize>>), usize>,

    pending_skins: Vec<PendingSkin>,
    bone_node_indices: BTreeSet<usize>,
    collected_anim_clips: Vec<UnityRef>,
    avatar_tos: HashMap<u32, String>,
    secondary_lod_renderers: HashSet<UnityRef>,
    triangle_count: usize,
}

impl<'a> GltfBuildCtx<'a> {
    fn new(
        workspace: &'a UnityWorkspaceData,
        opts: &'a GltfSceneOptions,
        emit_mode: GltfEmitMode,
        buffer_name: &str,
    ) -> Self {
        let mut root = json::Root {
            asset: json::Asset {
                generator: Some("devx-codecs (DevX Unity Rust Scene Exporter)".to_owned()),
                version: "2.0".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };
        let buffer_idx = json::Index::push(
            &mut root.buffers,
            json::Buffer {
                byte_length: json::validation::USize64::from(0usize),
                name: Some(buffer_name.to_owned()),
                uri: None,
                extensions: None,
                extras: Default::default(),
            },
        );
        Self {
            workspace,
            opts,
            emit_mode,
            external_textures: Vec::new(),
            used_tex_paths: HashSet::new(),
            root,
            bin: Vec::new(),
            buffer_idx,
            transform_to_node: HashMap::new(),
            node_names: HashMap::new(),
            node_children: BTreeMap::new(),
            extra_scene_roots: Vec::new(),
            visiting_transforms: HashSet::new(),
            texture_cache: HashMap::new(),
            material_cache: HashMap::new(),
            default_sampler: None,
            mesh_geo_cache: HashMap::new(),
            mesh_mat_cache: HashMap::new(),
            pending_skins: Vec::new(),
            bone_node_indices: BTreeSet::new(),
            collected_anim_clips: Vec::new(),
            avatar_tos: HashMap::new(),
            secondary_lod_renderers: HashSet::new(),
            triangle_count: 0,
        }
    }

    /// Scans loaded files touched by `root_transforms` for enabled `LODGroup` (`class_id == 205`)
    /// components and records renderers exclusive to `m_LODs[1..]` so only `LOD0` renders.
    fn collect_secondary_lod_renderers(&mut self, root_transforms: &[UnityRef]) {
        let file_indices: BTreeSet<usize> = root_transforms.iter().map(|&(fi, _)| fi).collect();
        let mut primary = HashSet::new();
        let mut secondary = HashSet::new();

        for fi in file_indices {
            let Some(loaded) = self.workspace.files.get(fi) else {
                continue;
            };
            let lod_ids: Vec<i64> = loaded
                .file
                .objects
                .iter()
                .filter(|info| info.type_id == 205)
                .map(|info| info.path_id)
                .collect();
            for pid in lod_ids {
                let Some(obj) = self.try_read_object((fi, pid)) else {
                    continue;
                };
                let props = obj.class.properties();
                if props.get("m_Enabled").and_then(extract_i32_field).unwrap_or(1) == 0 {
                    continue;
                }
                let Some(lods) = props.get("m_LODs").and_then(|v| v.as_array()) else {
                    continue;
                };
                if lods.len() < 2 {
                    continue;
                }
                let lod0 = self.extract_lod_renderer_refs(fi, &lods[0]);
                if lod0.is_empty() {
                    continue;
                }
                primary.extend(lod0);
                for lod_level in &lods[1..] {
                    secondary.extend(self.extract_lod_renderer_refs(fi, lod_level));
                }
            }
        }
        self.secondary_lod_renderers = secondary.difference(&primary).copied().collect();
    }

    fn extract_lod_renderer_refs(&self, file_idx: usize, lod_val: &UnityValue) -> Vec<UnityRef> {
        let Some(renderers) = lod_val
            .as_object()
            .and_then(|m| m.get("renderers"))
            .and_then(|v| v.as_array())
        else {
            return Vec::new();
        };
        renderers
            .iter()
            .filter_map(|entry| {
                entry
                    .as_object()
                    .and_then(|m| m.get("renderer"))
                    .or(Some(entry))
            })
            .filter_map(|pptr| self.resolve_pptr(file_idx, pptr))
            .collect()
    }

    fn unity_version_for(&self, file_index: usize) -> &str {
        self.workspace
            .files
            .get(file_index)
            .map(|f| f.file.unity_version.as_str())
            .filter(|v| !v.is_empty() && !v.starts_with("0."))
            .unwrap_or(&self.opts.fallback_unity_version)
    }

    fn try_read_object(&self, uref: UnityRef) -> Option<UnityObject> {
        let (loaded, info) = self.workspace.object_info(uref.0, uref.1)?;
        read_object(loaded, info).ok()
    }

    fn resolve_pptr(&self, source_file: usize, val: &UnityValue) -> Option<UnityRef> {
        let obj = val.as_object()?;
        let pid = obj.get("m_PathID").and_then(extract_i64_field)?;
        if pid == 0 {
            return None;
        }
        let fid = obj.get("m_FileID").and_then(extract_i32_field).unwrap_or(0);
        let target_file = self.workspace.resolve_external_file(source_file, fid)?;
        Some((target_file, pid))
    }

    fn build_transform_subtree(&mut self, tr_ref: UnityRef) -> CodecResult<Option<usize>> {
        if let Some(&existing) = self.transform_to_node.get(&tr_ref) {
            return Ok(Some(existing));
        }
        if !self.visiting_transforms.insert(tr_ref) {
            // Cycle guard on malformed hierarchies.
            return Ok(None);
        }

        let Some(tr_obj) = self.try_read_object(tr_ref) else {
            self.visiting_transforms.remove(&tr_ref);
            return Ok(None);
        };
        let props = tr_obj.class.properties();

        let (t, r, s) = extract_local_trs_rh(props);

        let mut node_name = format!("Transform_{}", tr_ref.1);
        let mut go_components = Vec::new();
        if let Some(go_ref) = props
            .get("m_GameObject")
            .and_then(|v| self.resolve_pptr(tr_ref.0, v))
            && let Some(go_obj) = self.try_read_object(go_ref)
        {
            if let Some(n) = go_obj
                .class
                .properties()
                .get("m_Name")
                .and_then(|v| v.as_str())
                && !n.is_empty()
            {
                node_name = n.to_owned();
            }
            if let Some(arr) = go_obj
                .class
                .properties()
                .get("m_Component")
                .and_then(|v| v.as_array())
            {
                for item in arr {
                    let comp_val = item
                        .as_object()
                        .and_then(|m| m.get("component").or_else(|| m.get("second")))
                        .unwrap_or(item);
                    if let Some(comp_ref) = self.resolve_pptr(go_ref.0, comp_val) {
                        go_components.push(comp_ref);
                    }
                }
            }
        }

        let node_idx = self.push_node(
            json::Node {
                name: Some(node_name.clone()),
                translation: t,
                rotation: r.map(json::scene::UnitQuaternion),
                scale: s,
                ..Default::default()
            },
            tr_ref,
            &node_name,
        );

        // Attach MeshFilter+MeshRenderer, SkinnedMeshRenderer, Animator, Animation.
        self.process_gameobject_components(node_idx, &go_components)?;

        // Traverse child transforms cleanly.
        let mut child_refs = Vec::new();
        if let Some(children) = props.get("m_Children").and_then(|v| v.as_array()) {
            for item in children {
                if let Some(c_ref) = self.resolve_pptr(tr_ref.0, item) {
                    child_refs.push(c_ref);
                }
            }
        }
        let mut child_indices = Vec::new();
        for c_ref in child_refs {
            if let Some(c_idx) = self.build_transform_subtree(c_ref)? {
                child_indices.push(c_idx);
            }
        }
        if !child_indices.is_empty() {
            self.node_children.entry(node_idx).or_default().extend(child_indices);
        }

        self.visiting_transforms.remove(&tr_ref);
        Ok(Some(node_idx))
    }

    fn ensure_transform_chain_in_scene(&mut self, tr_ref: UnityRef) -> CodecResult<Option<usize>> {
        if let Some(&idx) = self.transform_to_node.get(&tr_ref) {
            return Ok(Some(idx));
        }
        let Some(tr_obj) = self.try_read_object(tr_ref) else {
            return Ok(None);
        };
        let props = tr_obj.class.properties();
        let father_ref = props
            .get("m_Father")
            .and_then(|v| self.resolve_pptr(tr_ref.0, v));

        let parent_idx = if let Some(f_ref) = father_ref {
            if self.visiting_transforms.contains(&f_ref) {
                None
            } else {
                self.visiting_transforms.insert(tr_ref);
                let p = self.ensure_transform_chain_in_scene(f_ref)?;
                self.visiting_transforms.remove(&tr_ref);
                p
            }
        } else {
            None
        };

        let (t, r, s) = extract_local_trs_rh(props);
        let mut node_name = format!("Bone_{}", tr_ref.1);
        if let Some(go_ref) = props
            .get("m_GameObject")
            .and_then(|v| self.resolve_pptr(tr_ref.0, v))
            && let Some(go_obj) = self.try_read_object(go_ref)
            && let Some(n) = go_obj
                .class
                .properties()
                .get("m_Name")
                .and_then(|v| v.as_str())
            && !n.is_empty()
        {
            node_name = n.to_owned();
        }

        let node_idx = self.push_node(
            json::Node {
                name: Some(node_name.clone()),
                translation: t,
                rotation: r.map(json::scene::UnitQuaternion),
                scale: s,
                ..Default::default()
            },
            tr_ref,
            &node_name,
        );

        if let Some(p_idx) = parent_idx {
            self.node_children.entry(p_idx).or_default().push(node_idx);
        } else {
            self.extra_scene_roots.push(node_idx);
        }
        Ok(Some(node_idx))
    }

    fn push_node(&mut self, node: json::Node, tr_ref: UnityRef, name: &str) -> usize {
        let idx = self.root.nodes.len();
        self.root.nodes.push(node);
        self.transform_to_node.insert(tr_ref, idx);
        self.node_names.insert(idx, name.to_owned());
        idx
    }

    fn process_gameobject_components(
        &mut self,
        node_idx: usize,
        comp_refs: &[UnityRef],
    ) -> CodecResult<()> {
        let mut mf_mesh: Option<UnityRef> = None;
        let mut mr_mats: Option<Vec<Option<UnityRef>>> = None;
        let mut mr_enabled = true;

        for &comp_ref in comp_refs {
            let Some(comp_obj) = self.try_read_object(comp_ref) else {
                continue;
            };
            let props = comp_obj.class.properties();
            match comp_obj.class_id() {
                33 => {
                    // MeshFilter
                    if let Some(m_ref) = props
                        .get("m_Mesh")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        mf_mesh = Some(m_ref);
                    }
                }
                23 => {
                    // MeshRenderer
                    mr_enabled = !self.secondary_lod_renderers.contains(&comp_ref)
                        && props.get("m_Enabled").and_then(extract_i32_field).unwrap_or(1) != 0;
                    mr_mats = Some(self.read_material_list(comp_ref.0, props));
                }
                137 => {
                    // SkinnedMeshRenderer
                    let enabled = !self.secondary_lod_renderers.contains(&comp_ref)
                        && props.get("m_Enabled").and_then(extract_i32_field).unwrap_or(1) != 0;
                    if !enabled {
                        continue;
                    }
                    if let Some(m_ref) = props
                        .get("m_Mesh")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        let mats = self.read_material_list(comp_ref.0, props);
                        let built = self.build_gltf_mesh(m_ref, &mats, self.opts.include_skinning)?;
                        let bind_poses_row_major = built.bind_poses_row_major.clone();
                        self.attach_mesh_to_node(node_idx, built.mesh_idx);

                        if self.opts.include_skinning && built.has_skin_attributes {
                            let bone_refs = props
                                .get("m_Bones")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .map(|item| self.resolve_pptr(comp_ref.0, item))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            let root_bone_ref = props
                                .get("m_RootBone")
                                .and_then(|v| self.resolve_pptr(comp_ref.0, v));
                            if !bone_refs.is_empty() {
                                self.pending_skins.push(PendingSkin {
                                    node_idx,
                                    mesh_ref: m_ref,
                                    bind_poses_row_major,
                                    bone_refs,
                                    root_bone_ref,
                                });
                            }
                        }
                    }
                }
                95 => {
                    // Animator -> m_Avatar + m_Controller
                    if let Some(avatar_ref) = props
                        .get("m_Avatar")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        self.collect_avatar_tos(avatar_ref);
                    }
                    if let Some(ctrl_ref) = props
                        .get("m_Controller")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        self.collect_clips_from_controller(ctrl_ref);
                    }
                }
                111 => {
                    // Legacy Animation component
                    if let Some(c_ref) = props
                        .get("m_Animation")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        self.collected_anim_clips.push(c_ref);
                    }
                    if let Some(arr) = props.get("m_Animations").and_then(|v| v.as_array()) {
                        for item in arr {
                            if let Some(c_ref) = self.resolve_pptr(comp_ref.0, item) {
                                self.collected_anim_clips.push(c_ref);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Only export MeshFilter when an active MeshRenderer is present (skip invisible
        // MeshFilter + MeshCollider / unrendered helper GameObjects).
        if let Some(m_ref) = mf_mesh
            && mr_enabled
            && let Some(mats) = mr_mats
            && let Ok(built) = self.build_gltf_mesh(m_ref, &mats, false)
        {
            self.attach_mesh_to_node(node_idx, built.mesh_idx);
        }

        Ok(())
    }

    fn attach_mesh_to_node(&mut self, node_idx: usize, mesh_idx: json::Index<json::Mesh>) {
        if let Some(node) = self.root.nodes.get_mut(node_idx)
            && node.mesh.is_none()
        {
            node.mesh = Some(mesh_idx);
            return;
        }
        // If a single GameObject somehow had both MeshRenderer + SkinnedMeshRenderer, spawn a child node.
        let child_idx = self.root.nodes.len();
        self.root.nodes.push(json::Node {
            name: Some(format!("Renderer_{child_idx}")),
            mesh: Some(mesh_idx),
            ..Default::default()
        });
        self.node_children.entry(node_idx).or_default().push(child_idx);
    }

    fn read_material_list(
        &self,
        source_file: usize,
        props: &indexmap::IndexMap<String, UnityValue>,
    ) -> Vec<Option<UnityRef>> {
        props
            .get("m_Materials")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|item| self.resolve_pptr(source_file, item))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn build_gltf_mesh(
        &mut self,
        mesh_ref: UnityRef,
        mat_refs: &[Option<UnityRef>],
        want_skin: bool,
    ) -> CodecResult<BuiltGltfMesh> {
        let mut gltf_mats: Vec<Option<usize>> = Vec::with_capacity(mat_refs.len());
        if self.opts.include_materials {
            for mr in mat_refs {
                let idx = if let Some(r) = mr {
                    self.get_or_create_material(*r).map(|i| i.value())
                } else {
                    None
                };
                gltf_mats.push(idx);
            }
        } else {
            gltf_mats.resize(mat_refs.len(), None);
        }

        let geo = if let Some(cached) = self.mesh_geo_cache.get(&mesh_ref) {
            cached.clone()
        } else {
            let mesh_obj = self.try_read_object(mesh_ref).ok_or_else(|| {
                CodecError::MalformedData(format!(
                    "Mesh {} missing from file #{}",
                    mesh_ref.1, mesh_ref.0
                ))
            })?;
            let ver = self.unity_version_for(mesh_ref.0).to_owned();
            let decoded = decode_mesh_full(&mesh_obj, &ver, &self.workspace.streams)?;
            let cached = self.upload_mesh_geometry(&decoded)?;
            self.mesh_geo_cache.insert(mesh_ref, cached.clone());
            cached
        };

        let use_skin = want_skin
            && geo.joints_acc.is_some()
            && geo.weights_acc.is_some()
            && !geo.bind_poses_row_major.is_empty();

        let cache_key = (mesh_ref, gltf_mats.clone());
        if let Some(&existing_mesh_idx) = self.mesh_mat_cache.get(&cache_key) {
            return Ok(BuiltGltfMesh {
                mesh_idx: json::Index::new(existing_mesh_idx as u32),
                has_skin_attributes: use_skin,
                bind_poses_row_major: geo.bind_poses_row_major.clone(),
            });
        }

        let mut base_attrs = BTreeMap::new();
        base_attrs.insert(Checked::Valid(json::mesh::Semantic::Positions), geo.pos_acc);
        base_attrs.insert(Checked::Valid(json::mesh::Semantic::Normals), geo.norm_acc);
        base_attrs.insert(Checked::Valid(json::mesh::Semantic::TexCoords(0)), geo.uv_acc);
        if use_skin
            && let (Some(j_acc), Some(w_acc)) = (geo.joints_acc, geo.weights_acc)
        {
            base_attrs.insert(Checked::Valid(json::mesh::Semantic::Joints(0)), j_acc);
            base_attrs.insert(Checked::Valid(json::mesh::Semantic::Weights(0)), w_acc);
        }

        let targets_opt = (!geo.morph_targets.is_empty()).then(|| geo.morph_targets.clone());
        let weights_opt = (!geo.morph_weights.is_empty()).then(|| geo.morph_weights.clone());

        let mut primitives = Vec::with_capacity(geo.submesh_indices.len());
        for (sub_i, (idx_acc, idx_count)) in geo.submesh_indices.iter().enumerate() {
            let mat_idx = gltf_mats
                .get(sub_i)
                .copied()
                .flatten()
                .or_else(|| gltf_mats.last().copied().flatten())
                .map(|i| json::Index::new(i as u32));
            primitives.push(json::mesh::Primitive {
                attributes: base_attrs.clone(),
                extensions: None,
                extras: Default::default(),
                indices: Some(*idx_acc),
                material: mat_idx,
                mode: Checked::Valid(json::mesh::Mode::Triangles),
                targets: targets_opt.clone(),
            });
            self.triangle_count += idx_count / 3;
        }

        let mesh_name = self
            .try_read_object(mesh_ref)
            .and_then(|o| o.name())
            .unwrap_or_else(|| format!("Mesh_{}", mesh_ref.1));
        let mesh_idx = json::Index::push(
            &mut self.root.meshes,
            json::Mesh {
                name: Some(mesh_name),
                extensions: None,
                extras: Default::default(),
                primitives,
                weights: weights_opt,
            },
        );
        self.mesh_mat_cache.insert(cache_key, mesh_idx.value());

        let _ = geo.vertex_count;
        Ok(BuiltGltfMesh {
            mesh_idx,
            has_skin_attributes: use_skin,
            bind_poses_row_major: geo.bind_poses_row_major,
        })
    }

    fn upload_mesh_geometry(&mut self, decoded: &DecodedMeshFull) -> CodecResult<CachedMeshGeometry> {
        let base = &decoded.base;
        if base.positions.is_empty() || base.indices.is_empty() {
            return Err(CodecError::MalformedData(format!(
                "Mesh '{}' has empty geometry",
                base.name
            )));
        }

        // Convert Unity left-handed vertex coordinates & UVs to glTF right-handed space:
        // Position/Normal: [-x, y, z]
        // UV0: [u, 1.0 - v] (Unity V=0 is bottom; glTF & PNG V=0 is top)
        let mut min_pos = [f32::INFINITY; 3];
        let mut max_pos = [f32::NEG_INFINITY; 3];
        let pos_off = self.align_bin_4();
        for p in &base.positions {
            let rh = [-p[0], p[1], p[2]];
            for axis in 0..3 {
                min_pos[axis] = min_pos[axis].min(rh[axis]);
                max_pos[axis] = max_pos[axis].max(rh[axis]);
                self.bin.extend_from_slice(&rh[axis].to_le_bytes());
            }
        }
        let pos_len = self.bin.len() - pos_off;
        let pos_view = self.push_buffer_view(pos_off, pos_len, Some(json::buffer::Target::ArrayBuffer));
        let pos_acc = self.push_accessor(
            pos_view,
            base.positions.len(),
            json::accessor::ComponentType::F32,
            json::accessor::Type::Vec3,
            Some(json!(min_pos)),
            Some(json!(max_pos)),
        );

        let norm_off = self.align_bin_4();
        for n in &base.normals {
            let mut rh = [-n[0], n[1], n[2]];
            let len = (rh[0] * rh[0] + rh[1] * rh[1] + rh[2] * rh[2]).sqrt();
            if len > 1e-6 {
                rh[0] /= len;
                rh[1] /= len;
                rh[2] /= len;
            } else {
                rh = [0.0, 1.0, 0.0];
            }
            for f in rh {
                self.bin.extend_from_slice(&f.to_le_bytes());
            }
        }
        let norm_len = self.bin.len() - norm_off;
        let norm_view =
            self.push_buffer_view(norm_off, norm_len, Some(json::buffer::Target::ArrayBuffer));
        let norm_acc = self.push_accessor(
            norm_view,
            base.normals.len(),
            json::accessor::ComponentType::F32,
            json::accessor::Type::Vec3,
            None,
            None,
        );

        let uv_off = self.align_bin_4();
        for uv in &base.uvs {
            let gltf_uv = [uv[0], 1.0 - uv[1]];
            for f in gltf_uv {
                self.bin.extend_from_slice(&f.to_le_bytes());
            }
        }
        let uv_len = self.bin.len() - uv_off;
        let uv_view = self.push_buffer_view(uv_off, uv_len, Some(json::buffer::Target::ArrayBuffer));
        let uv_acc = self.push_accessor(
            uv_view,
            base.uvs.len(),
            json::accessor::ComponentType::F32,
            json::accessor::Type::Vec2,
            None,
            None,
        );

        let (joints_acc, weights_acc, bind_poses_row_major) = if let Some(skin) = &decoded.skin {
            let bp = skin.bind_poses_row_major.clone();
            if skin.joints.len() == base.positions.len()
                && skin.weights.len() == base.positions.len()
            {
                let max_joint = bp.len().saturating_sub(1).min(u16::MAX as usize) as u16;
                let j_off = self.align_bin_4();
                for j in &skin.joints {
                    for &idx in j {
                        let clamped = if bp.is_empty() { idx } else { idx.min(max_joint) };
                        self.bin.extend_from_slice(&clamped.to_le_bytes());
                    }
                }
                let j_len = self.bin.len() - j_off;
                let j_view = self.push_buffer_view(
                    j_off,
                    j_len,
                    Some(json::buffer::Target::ArrayBuffer),
                );
                let j_acc = self.push_accessor(
                    j_view,
                    skin.joints.len(),
                    json::accessor::ComponentType::U16,
                    json::accessor::Type::Vec4,
                    None,
                    None,
                );

                let w_off = self.align_bin_4();
                for w in &skin.weights {
                    for f in w {
                        self.bin.extend_from_slice(&f.to_le_bytes());
                    }
                }
                let w_len = self.bin.len() - w_off;
                let w_view = self.push_buffer_view(
                    w_off,
                    w_len,
                    Some(json::buffer::Target::ArrayBuffer),
                );
                let w_acc = self.push_accessor(
                    w_view,
                    skin.weights.len(),
                    json::accessor::ComponentType::F32,
                    json::accessor::Type::Vec4,
                    None,
                    None,
                );
                (Some(j_acc), Some(w_acc), bp)
            } else {
                (None, None, bp)
            }
        } else {
            (None, None, Vec::new())
        };

        // Upload triangle indices per submesh with flipped winding `(i0, i2, i1)` because X was mirrored.
        let mut submesh_indices = Vec::new();
        let slices = if base.submeshes.is_empty() {
            vec![devx_plugin_api::SubMeshSlice {
                first_index: 0,
                index_count: base.indices.len() as u32,
                topology: 0,
            }]
        } else {
            base.submeshes.clone()
        };

        for slice in slices {
            let start = slice.first_index as usize;
            let end = start.saturating_add(slice.index_count as usize).min(base.indices.len());
            if start + 3 > end {
                continue;
            }
            let raw = &base.indices[start..end];
            let tri_len = (raw.len() / 3) * 3;
            if tri_len == 0 {
                continue;
            }
            let idx_off = self.align_bin_4();
            for tri in raw[..tri_len].as_chunks::<3>().0 {
                self.bin.extend_from_slice(&tri[0].to_le_bytes());
                self.bin.extend_from_slice(&tri[2].to_le_bytes());
                self.bin.extend_from_slice(&tri[1].to_le_bytes());
            }
            let byte_len = self.bin.len() - idx_off;
            let idx_view = self.push_buffer_view(
                idx_off,
                byte_len,
                Some(json::buffer::Target::ElementArrayBuffer),
            );
            let idx_acc = self.push_accessor(
                idx_view,
                tri_len,
                json::accessor::ComponentType::U32,
                json::accessor::Type::Scalar,
                None,
                None,
            );
            submesh_indices.push((idx_acc, tri_len));
        }

        let mut morph_targets = Vec::new();
        let mut morph_weights = Vec::new();
        let vcount = base.positions.len();
        for bs in &decoded.blend_shapes {
            if bs.vertex_indices.is_empty() {
                continue;
            }
            let mut dense_dp = vec![[0.0f32; 3]; vcount];
            let mut dense_dn = vec![[0.0f32; 3]; vcount];
            for k in 0..bs.vertex_indices.len().min(bs.position_deltas.len()) {
                let vi = bs.vertex_indices[k] as usize;
                if vi >= vcount {
                    continue;
                }
                let dp = bs.position_deltas[k];
                dense_dp[vi] = [-dp[0], dp[1], dp[2]];
                if bs.has_normals && let Some(dn) = bs.normal_deltas.get(k) {
                    dense_dn[vi] = [-dn[0], dn[1], dn[2]];
                }
            }
            let mut min_d = [f32::INFINITY; 3];
            let mut max_d = [f32::NEG_INFINITY; 3];
            let dp_off = self.align_bin_4();
            for d in &dense_dp {
                for axis in 0..3 {
                    min_d[axis] = min_d[axis].min(d[axis]);
                    max_d[axis] = max_d[axis].max(d[axis]);
                    self.bin.extend_from_slice(&d[axis].to_le_bytes());
                }
            }
            let dp_len = self.bin.len() - dp_off;
            let dp_view = self.push_buffer_view(dp_off, dp_len, Some(json::buffer::Target::ArrayBuffer));
            let dp_acc = self.push_accessor(
                dp_view,
                vcount,
                json::accessor::ComponentType::F32,
                json::accessor::Type::Vec3,
                Some(json!(min_d)),
                Some(json!(max_d)),
            );

            let dn_acc = if bs.has_normals {
                let dn_off = self.align_bin_4();
                for n in &dense_dn {
                    for f in n {
                        self.bin.extend_from_slice(&f.to_le_bytes());
                    }
                }
                let dn_len = self.bin.len() - dn_off;
                let dn_view =
                    self.push_buffer_view(dn_off, dn_len, Some(json::buffer::Target::ArrayBuffer));
                Some(self.push_accessor(
                    dn_view,
                    vcount,
                    json::accessor::ComponentType::F32,
                    json::accessor::Type::Vec3,
                    None,
                    None,
                ))
            } else {
                None
            };

            morph_targets.push(json::mesh::MorphTarget {
                positions: Some(dp_acc),
                normals: dn_acc,
                tangents: None,
            });
            morph_weights.push(bs.default_weight);
        }

        Ok(CachedMeshGeometry {
            vertex_count: base.positions.len(),
            pos_acc,
            norm_acc,
            uv_acc,
            joints_acc,
            weights_acc,
            bind_poses_row_major,
            submesh_indices,
            morph_targets,
            morph_weights,
        })
    }

    fn finalize_pending_skins(&mut self) -> CodecResult<()> {
        let skins = std::mem::take(&mut self.pending_skins);
        for ps in skins {
            let mut joint_indices = Vec::with_capacity(ps.bone_refs.len());
            let mut unresolved = 0usize;
            for b_opt in &ps.bone_refs {
                let resolved = if let Some(b_ref) = *b_opt {
                    self.ensure_transform_chain_in_scene(b_ref)?
                } else {
                    None
                };
                if let Some(j_idx) = resolved {
                    self.bone_node_indices.insert(j_idx);
                    joint_indices.push(json::Index::new(j_idx as u32));
                } else {
                    unresolved += 1;
                    // Fall back cleanly to the skinned mesh node itself so glTF indices remain valid.
                    joint_indices.push(json::Index::new(ps.node_idx as u32));
                }
            }
            if joint_indices.is_empty() || unresolved == joint_indices.len() {
                continue;
            }

            let skeleton = ps
                .root_bone_ref
                .and_then(|r| self.ensure_transform_chain_in_scene(r).ok().flatten())
                .map(|i| json::Index::new(i as u32));

            // Upload inverseBindMatrices (column-major f32x16 converted via S * M * S).
            let ibm_off = self.align_bin_4();
            for j_slot in 0..joint_indices.len() {
                let row_major = ps
                    .bind_poses_row_major
                    .get(j_slot)
                    .copied()
                    .unwrap_or([
                        1.0, 0.0, 0.0, 0.0,
                        0.0, 1.0, 0.0, 0.0,
                        0.0, 0.0, 1.0, 0.0,
                        0.0, 0.0, 0.0, 1.0,
                    ]);
                let col_major_rh = unity_bindpose_row_major_to_gltf_col_major(&row_major);
                for f in col_major_rh {
                    self.bin.extend_from_slice(&f.to_le_bytes());
                }
            }
            let ibm_len = self.bin.len() - ibm_off;
            let ibm_view = self.push_buffer_view(ibm_off, ibm_len, None);
            let ibm_acc = self.push_accessor(
                ibm_view,
                joint_indices.len(),
                json::accessor::ComponentType::F32,
                json::accessor::Type::Mat4,
                None,
                None,
            );

            let skin_idx = json::Index::push(
                &mut self.root.skins,
                json::Skin {
                    name: Some(format!("Skin_{}", ps.mesh_ref.1)),
                    inverse_bind_matrices: Some(ibm_acc),
                    joints: joint_indices,
                    skeleton,
                    extensions: None,
                    extras: Default::default(),
                },
            );
            if let Some(node) = self.root.nodes.get_mut(ps.node_idx) {
                node.skin = Some(skin_idx);
            }
        }
        Ok(())
    }

    fn get_or_create_material(&mut self, mat_ref: UnityRef) -> Option<json::Index<json::Material>> {
        if let Some(&idx) = self.material_cache.get(&mat_ref) {
            return Some(idx);
        }
        let mat_obj = self.try_read_object(mat_ref)?;
        if mat_obj.class_id() != 21 {
            return None;
        }
        let decoded: DecodedMaterial = decode_material(&mat_obj).ok()?;
        let terrain_extras = self.export_terrain_layer_metadata(mat_ref, &decoded);

        let base_color = decoded.base_color();
        let metallic = decoded.metallic_factor();
        let roughness = decoded.roughness_factor();

        let base_color_texture = if self.opts.include_textures {
            decoded.albedo_slot().and_then(|slot| {
                self.resolve_material_tex_pptr(mat_ref.0, slot.texture_file_id, slot.texture_path_id)
                    .and_then(|tref| self.get_or_create_texture(tref, false))
                    .map(|idx| json::texture::Info {
                        index: idx,
                        tex_coord: 0,
                        extensions: None,
                        extras: Default::default(),
                    })
            })
        } else {
            None
        };

        let metallic_roughness_texture = if self.opts.include_textures {
            decoded.metallic_gloss_slot().and_then(|slot| {
                self.resolve_material_tex_pptr(mat_ref.0, slot.texture_file_id, slot.texture_path_id)
                    .and_then(|tref| self.get_or_create_texture(tref, true))
                    .map(|idx| json::texture::Info {
                        index: idx,
                        tex_coord: 0,
                        extensions: None,
                        extras: Default::default(),
                    })
            })
        } else {
            None
        };

        let normal_texture = if self.opts.include_textures {
            decoded.normal_slot().and_then(|slot| {
                self.resolve_material_tex_pptr(mat_ref.0, slot.texture_file_id, slot.texture_path_id)
                    .and_then(|tref| self.get_or_create_texture(tref, false))
                    .map(|idx| json::material::NormalTexture {
                        index: idx,
                        scale: decoded.find_float(&["_BumpScale"]).unwrap_or(1.0),
                        tex_coord: 0,
                        extensions: None,
                        extras: Default::default(),
                    })
            })
        } else {
            None
        };

        let emission_active = decoded.has_active_emission();
        let emissive_texture = if self.opts.include_textures && emission_active {
            decoded.emission_slot().and_then(|slot| {
                self.resolve_material_tex_pptr(mat_ref.0, slot.texture_file_id, slot.texture_path_id)
                    .and_then(|tref| self.get_or_create_texture(tref, false))
                    .map(|idx| json::texture::Info {
                        index: idx,
                        tex_coord: 0,
                        extensions: None,
                        extras: Default::default(),
                    })
            })
        } else {
            None
        };

        let (alpha_mode, alpha_cutoff) = match decoded.alpha_mode() {
            MaterialAlphaMode::Opaque => (Checked::Valid(json::material::AlphaMode::Opaque), None),
            MaterialAlphaMode::Mask { cutoff } => (
                Checked::Valid(json::material::AlphaMode::Mask),
                Some(json::material::AlphaCutoff(cutoff)),
            ),
            MaterialAlphaMode::Blend => (Checked::Valid(json::material::AlphaMode::Blend), None),
        };

        let emissive_factor = json::material::EmissiveFactor(if emission_active {
            decoded.emissive_color().unwrap_or([1.0, 1.0, 1.0])
        } else {
            [0.0, 0.0, 0.0]
        });

        let double_sided = decoded.double_sided();
        let mat_idx = json::Index::push(
            &mut self.root.materials,
            json::Material {
                name: Some(decoded.name),
                alpha_cutoff,
                alpha_mode,
                double_sided,
                pbr_metallic_roughness: json::material::PbrMetallicRoughness {
                    base_color_factor: json::material::PbrBaseColorFactor(base_color),
                    base_color_texture,
                    metallic_factor: json::material::StrengthFactor(metallic),
                    roughness_factor: json::material::StrengthFactor(roughness),
                    metallic_roughness_texture,
                    extensions: None,
                    extras: Default::default(),
                },
                normal_texture,
                occlusion_texture: None,
                emissive_texture,
                emissive_factor,
                extensions: None,
                extras: terrain_extras,
            },
        );
        self.material_cache.insert(mat_ref, mat_idx);
        Some(mat_idx)
    }

    /// Retains native TerrainMeshBase layer textures and shader scalars for a per-fragment
    /// renderer. A control map is blend data, never an albedo image. Keeping originals avoids
    /// flattening 40-90 repeated tiles into a low-resolution whole-chunk texture.
    fn export_terrain_layer_metadata(&mut self, mat_ref: UnityRef, material: &DecodedMaterial) -> json::Extras {
        if !self.opts.include_textures { return None; }
        let shader_ref = self.resolve_material_tex_pptr(mat_ref.0, material.shader_file_id, material.shader_path_id)?;
        let shader = self.try_read_object(shader_ref)?;
        let shader_name = shader.class.properties().get("m_ParsedForm")?.as_object()?.get("m_Name")?.as_str()?;
        let layer_count = match shader_name {
            "_URP/TerrainMeshBase/4 Textures" => 4,
            "_URP/TerrainMeshBase/3 Textures" => 3,
            _ => return None,
        };
        let control_slot = material.find_texture_slot(&["_Control"])?;
        let control_ref = self.resolve_material_tex_pptr(mat_ref.0, control_slot.texture_file_id, control_slot.texture_path_id)?;
        let control = self.get_or_create_texture(control_ref, false)?;
        let mut layers = Vec::new();
        for index in 0..layer_count {
            let name = format!("_Splat{index}");
            let slot = material.find_texture_slot(&[&name])?;
            let texture_ref = self.resolve_material_tex_pptr(mat_ref.0, slot.texture_file_id, slot.texture_path_id)?;
            let texture = self.get_or_create_texture(texture_ref, false)?;
            let normal_name = format!("_Splat{index}_Normal");
            let normal = material.find_texture_slot(&[&normal_name]).and_then(|s| {
                self.resolve_material_tex_pptr(mat_ref.0, s.texture_file_id, s.texture_path_id)
                    .and_then(|r| self.get_or_create_texture(r, false))
            });
            layers.push(json!({
                "texture": texture.value(), "normal_texture": normal.map(|n| n.value()),
                "scale": material.find_float(&[&format!("_Splat{index}_S")]).unwrap_or(1.0),
                "smoothness": material.find_float(&[&format!("_Splat{index}_Smoothness")]).unwrap_or(0.5),
                "source_property": name,
            }));
        }
        json::extras::RawValue::from_string(json!({"devx_terrain": {
            "version": 1, "shader": shader_name, "control_texture": control.value(),
            "control_st": [control_slot.scale[0], control_slot.scale[1], control_slot.offset[0], control_slot.offset[1]],
            "normal_enabled": material.valid_keywords.iter().any(|k| k == "_NORMALMAP"),
            "layers": layers,
        }}).to_string()).ok()
    }

    fn resolve_material_tex_pptr(
        &self,
        source_file: usize,
        file_id: i32,
        path_id: i64,
    ) -> Option<UnityRef> {
        if path_id == 0 {
            return None;
        }
        let f = self.workspace.resolve_external_file(source_file, file_id)?;
        Some((f, path_id))
    }

    fn get_or_create_texture(
        &mut self,
        tex_ref: UnityRef,
        convert_unity_metallic_gloss: bool,
    ) -> Option<json::Index<json::Texture>> {
        let key = (tex_ref, convert_unity_metallic_gloss);
        if let Some(&idx) = self.texture_cache.get(&key) {
            return Some(idx);
        }

        let tex_obj = self.try_read_object(tex_ref)?;
        if tex_obj.class_id() != 28 {
            return None;
        }
        let ver = self.unity_version_for(tex_ref.0).to_owned();
        let mut decoded: DecodedImage = decode_texture2d(
            &tex_obj,
            &ver,
            &self.workspace.streams,
            TextureDecodeOptions {
                flip_vertical: true,
                ..Default::default()
            },
        )
        .ok()?;

        if convert_unity_metallic_gloss {
            // Convert Unity `_MetallicGlossMap` (R=Metallic, A=Smoothness) -> glTF (G=Roughness, B=Metallic).
            for px in decoded.rgba8.as_chunks_mut::<4>().0 {
                let metallic = px[0];
                let smoothness = px[3];
                let roughness = 255u8.saturating_sub(smoothness);
                px[0] = 0;
                px[1] = roughness;
                px[2] = metallic;
                px[3] = 255;
            }
        }

        if let Some(max_dim) = self.opts.max_texture_dim
            && max_dim > 0
            && (decoded.width > max_dim || decoded.height > max_dim)
            && let Some(buf) = RgbaImage::from_raw(decoded.width, decoded.height, decoded.rgba8.clone())
        {
            let dyn_img = DynamicImage::ImageRgba8(buf);
            let resized = dyn_img.resize(max_dim, max_dim, FilterType::Triangle).to_rgba8();
            decoded.width = resized.width();
            decoded.height = resized.height();
            decoded.rgba8 = resized.into_raw();
        }

        let png_bytes = encode_png(&decoded).ok()?;
        let tex_name = tex_obj
            .name()
            .unwrap_or_else(|| format!("Texture_{}", tex_ref.1));
        let image_idx = match self.emit_mode {
            GltfEmitMode::EmbeddedGlb => {
                let off = self.align_bin_4();
                self.bin.extend_from_slice(&png_bytes);
                let view = self.push_buffer_view(off, png_bytes.len(), None);
                json::Index::push(
                    &mut self.root.images,
                    json::Image {
                        name: Some(tex_name.clone()),
                        buffer_view: Some(view),
                        mime_type: Some(json::image::MimeType("image/png".to_owned())),
                        uri: None,
                        extensions: None,
                        extras: Default::default(),
                    },
                )
            }
            GltfEmitMode::ExternalFiles => {
                let clean: String = tex_name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                let stem = if clean.is_empty() {
                    format!("tex_{}", tex_ref.1)
                } else {
                    clean
                };
                let mut rel = format!("textures/{stem}.png");
                let mut n = 2usize;
                while !self.used_tex_paths.insert(rel.clone()) {
                    rel = format!("textures/{stem}_{n}.png");
                    n += 1;
                }
                self.external_textures.push((rel.clone(), png_bytes));
                json::Index::push(
                    &mut self.root.images,
                    json::Image {
                        name: Some(tex_name.clone()),
                        buffer_view: None,
                        mime_type: None,
                        uri: Some(rel),
                        extensions: None,
                        extras: Default::default(),
                    },
                )
            }
        };
        let sampler_idx = self.texture_sampler(&tex_obj);
        let tex_idx = json::Index::push(
            &mut self.root.textures,
            json::Texture {
                name: Some(tex_name),
                sampler: Some(sampler_idx),
                source: image_idx,
                extensions: None,
                extras: Default::default(),
            },
        );
        self.texture_cache.insert(key, tex_idx);
        Some(tex_idx)
    }

    fn texture_sampler(&mut self, texture: &UnityObject) -> json::Index<json::texture::Sampler> {
        let Some(settings) = texture.class.properties().get("m_TextureSettings").and_then(UnityValue::as_object) else {
            return self.ensure_default_sampler();
        };
        let wrap = |field: &str| match settings.get(field).and_then(extract_i32_field).unwrap_or(0) {
            1 => json::texture::WrappingMode::ClampToEdge,
            2 => json::texture::WrappingMode::MirroredRepeat,
            _ => json::texture::WrappingMode::Repeat,
        };
        let (mag, min) = match settings.get("m_FilterMode").and_then(extract_i32_field).unwrap_or(1) {
            0 => (json::texture::MagFilter::Nearest, json::texture::MinFilter::NearestMipmapNearest),
            2 => (json::texture::MagFilter::Linear, json::texture::MinFilter::LinearMipmapLinear),
            _ => (json::texture::MagFilter::Linear, json::texture::MinFilter::LinearMipmapNearest),
        };
        json::Index::push(&mut self.root.samplers, json::texture::Sampler {
            mag_filter: Some(Checked::Valid(mag)), min_filter: Some(Checked::Valid(min)),
            name: texture.name().map(|name| format!("{name}_Sampler")),
            wrap_s: Checked::Valid(wrap("m_WrapU")), wrap_t: Checked::Valid(wrap("m_WrapV")),
            extensions: None, extras: Default::default(),
        })
    }

    fn ensure_default_sampler(&mut self) -> json::Index<json::texture::Sampler> {
        if let Some(idx) = self.default_sampler {
            return idx;
        }
        let idx = json::Index::push(
            &mut self.root.samplers,
            json::texture::Sampler {
                mag_filter: Some(Checked::Valid(json::texture::MagFilter::Linear)),
                min_filter: Some(Checked::Valid(json::texture::MinFilter::LinearMipmapLinear)),
                name: Some("DefaultSampler".to_owned()),
                wrap_s: Checked::Valid(json::texture::WrappingMode::Repeat),
                wrap_t: Checked::Valid(json::texture::WrappingMode::Repeat),
                extensions: None,
                extras: Default::default(),
            },
        );
        self.default_sampler = Some(idx);
        idx
    }

    fn collect_avatar_tos(&mut self, avatar_ref: UnityRef) {
        let Some(obj) = self.try_read_object(avatar_ref) else {
            return;
        };
        if let Some(arr) = obj
            .class
            .properties()
            .get("m_TOS")
            .and_then(|v| v.as_array())
        {
            for item in arr {
                if let Some(pair) = item.as_object()
                    && let Some(hash) = pair.get("first").and_then(extract_i64_field)
                    && let Some(path) = pair.get("second").and_then(|v| v.as_str())
                {
                    self.avatar_tos.insert(hash as u32, path.to_owned());
                }
            }
        }
    }

    fn collect_clips_from_controller(&mut self, ctrl_ref: UnityRef) {
        let Some(obj) = self.try_read_object(ctrl_ref) else {
            return;
        };
        let props = obj.class.properties();
        if let Some(arr) = props.get("m_AnimationClips").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(c_ref) = self.resolve_pptr(ctrl_ref.0, item) {
                    self.collected_anim_clips.push(c_ref);
                }
            }
        }
        if let Some(base_ctrl) = props
            .get("m_Controller")
            .and_then(|v| self.resolve_pptr(ctrl_ref.0, v))
            && base_ctrl != ctrl_ref
        {
            self.collect_clips_from_controller(base_ctrl);
        }
        if let Some(overrides) = props.get("m_Clips").and_then(|v| v.as_array()) {
            for ov in overrides {
                if let Some(m) = ov.as_object()
                    && let Some(clip_val) = m.get("m_OverrideClip")
                    && let Some(c_ref) = self.resolve_pptr(ctrl_ref.0, clip_val)
                {
                    self.collected_anim_clips.push(c_ref);
                }
            }
        }
    }

    fn build_animations(
        &mut self,
        roots: &[AssetRef],
        extra_clips: &[AssetRef],
        top_nodes: &[json::Index<json::Node>],
    ) -> CodecResult<()> {
        let clip_refs = collect_fallback_clip_refs(
            self.workspace,
            roots,
            extra_clips,
            &self.collected_anim_clips,
        );

        if clip_refs.is_empty() {
            return Ok(());
        }

        let (hash_to_node, path_to_node) = self.build_node_path_maps(top_nodes);

        for c_ref in clip_refs {
            let Some(obj) = self.try_read_object(c_ref) else {
                continue;
            };
            if obj.class_id() != 74 {
                continue;
            }
            let Ok(clip) = decode_animation_clip(&obj) else {
                continue;
            };
            self.push_decoded_animation_clip(&clip, &hash_to_node, &path_to_node);
        }

        Ok(())
    }

    fn build_node_path_maps(
        &self,
        top_nodes: &[json::Index<json::Node>],
    ) -> (HashMap<u32, usize>, HashMap<String, usize>) {
        let mut hash_to_node: HashMap<u32, usize> = HashMap::new();
        let mut path_to_node: HashMap<String, usize> = HashMap::new();
        let mut name_only_to_node: HashMap<String, usize> = HashMap::new();

        if let Some(first_root) = top_nodes.first() {
            hash_to_node.insert(0, first_root.value());
            path_to_node.insert(String::new(), first_root.value());
        }

        let mut stack: Vec<(usize, Vec<String>)> = top_nodes
            .iter()
            .map(|i| {
                let idx = i.value();
                let n = self.node_names.get(&idx).cloned().unwrap_or_default();
                (idx, vec![n])
            })
            .collect();

        while let Some((node_idx, segments)) = stack.pop() {
            let short_name = segments.last().cloned().unwrap_or_default();
            if !short_name.is_empty() {
                name_only_to_node.entry(short_name).or_insert(node_idx);
            }

            // Register full path and every suffix down to `node` (Unity AnimationClips are relative
            // to the Animator Transform, which could be scene root or one level inside).
            for start in 0..segments.len() {
                let rel = segments[start..].join("/");
                if !rel.is_empty() {
                    let h = unity_path_crc32(&rel);
                    hash_to_node.entry(h).or_insert(node_idx);
                    path_to_node.entry(rel).or_insert(node_idx);
                }
            }

            if let Some(children) = self.node_children.get(&node_idx) {
                for &child_idx in children {
                    let cname = self.node_names.get(&child_idx).cloned().unwrap_or_default();
                    let mut child_segs = segments.clone();
                    child_segs.push(cname);
                    stack.push((child_idx, child_segs));
                }
            }
        }

        // Cross-register Avatar `m_TOS` hashes against resolved node paths.
        for (&tos_hash, tos_path) in &self.avatar_tos {
            if let Some(&n) = path_to_node.get(tos_path) {
                hash_to_node.entry(tos_hash).or_insert(n);
            } else if let Some((_, tail)) = tos_path.rsplit_once('/')
                && let Some(&n) = name_only_to_node.get(tail)
            {
                hash_to_node.entry(tos_hash).or_insert(n);
            } else if let Some(&n) = name_only_to_node.get(tos_path) {
                hash_to_node.entry(tos_hash).or_insert(n);
            }
        }

        (hash_to_node, path_to_node)
    }

    fn push_decoded_animation_clip(
        &mut self,
        clip: &DecodedAnimationClip,
        hash_to_node: &HashMap<u32, usize>,
        path_to_node: &HashMap<String, usize>,
    ) {
        let mut channels = Vec::new();
        let mut samplers = Vec::new();
        let mut targeted_pairs: HashSet<(usize, u8)> = HashSet::new();

        for track in &clip.tracks {
            if track.times.is_empty() {
                continue;
            }
            let target_node = hash_to_node.get(&track.path_hash).copied().or_else(|| {
                track
                    .path_name
                    .as_ref()
                    .and_then(|p| path_to_node.get(p).copied())
            });
            let Some(node_idx) = target_node else {
                continue;
            };

            let prop_tag = match track.property {
                TrackProperty::Translation => 0u8,
                TrackProperty::Rotation => 1u8,
                TrackProperty::Scale => 2u8,
            };
            if !targeted_pairs.insert((node_idx, prop_tag)) {
                // glTF spec forbids two channels targeting the same (node, path) in one Animation.
                continue;
            }

            let stride = match track.property {
                TrackProperty::Rotation => 4usize,
                TrackProperty::Translation | TrackProperty::Scale => 3usize,
            };
            let key_count = track.times.len().min(track.values.len() / stride);
            if key_count == 0 {
                continue;
            }

            // Build monotonically increasing non-negative timestamps & shortest-arc RH values.
            let mut clean_times: Vec<f32> = Vec::with_capacity(key_count);
            let mut clean_vals: Vec<f32> = Vec::with_capacity(key_count * stride);
            let mut prev_quat: Option<[f32; 4]> = None;

            for k in 0..key_count {
                let t = track.times[k].max(0.0);
                if let Some(&last_t) = clean_times.last()
                    && (t - last_t).abs() < 1e-6
                {
                    continue;
                }
                clean_times.push(t);
                let src = &track.values[k * stride..(k + 1) * stride];
                match track.property {
                    TrackProperty::Translation => {
                        clean_vals.push(-src[0]);
                        clean_vals.push(src[1]);
                        clean_vals.push(src[2]);
                    }
                    TrackProperty::Scale => {
                        clean_vals.push(src[0]);
                        clean_vals.push(src[1]);
                        clean_vals.push(src[2]);
                    }
                    TrackProperty::Rotation => {
                        let mut q = [src[0], -src[1], -src[2], src[3]];
                        let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
                        if n > 1e-6 {
                            q[0] /= n;
                            q[1] /= n;
                            q[2] /= n;
                            q[3] /= n;
                        } else {
                            q = [0.0, 0.0, 0.0, 1.0];
                        }
                        if let Some(pq) = prev_quat {
                            let dot = pq[0] * q[0] + pq[1] * q[1] + pq[2] * q[2] + pq[3] * q[3];
                            if dot < 0.0 {
                                q = [-q[0], -q[1], -q[2], -q[3]];
                            }
                        }
                        prev_quat = Some(q);
                        clean_vals.extend_from_slice(&q);
                    }
                }
            }

            if clean_times.is_empty() {
                continue;
            }
            let min_t = *clean_times.first().unwrap_or(&0.0);
            let max_t = *clean_times.last().unwrap_or(&0.0);

            let t_off = self.align_bin_4();
            for t in &clean_times {
                self.bin.extend_from_slice(&t.to_le_bytes());
            }
            let t_len = self.bin.len() - t_off;
            let t_view = self.push_buffer_view(t_off, t_len, None);
            let t_acc = self.push_accessor(
                t_view,
                clean_times.len(),
                json::accessor::ComponentType::F32,
                json::accessor::Type::Scalar,
                Some(json!([min_t])),
                Some(json!([max_t])),
            );

            let v_off = self.align_bin_4();
            for v in &clean_vals {
                self.bin.extend_from_slice(&v.to_le_bytes());
            }
            let v_len = self.bin.len() - v_off;
            let v_view = self.push_buffer_view(v_off, v_len, None);
            let v_type = if stride == 4 {
                json::accessor::Type::Vec4
            } else {
                json::accessor::Type::Vec3
            };
            let v_acc = self.push_accessor(
                v_view,
                clean_times.len(),
                json::accessor::ComponentType::F32,
                v_type,
                None,
                None,
            );

            let sampler_idx = json::Index::push(
                &mut samplers,
                json::animation::Sampler {
                    extensions: None,
                    extras: Default::default(),
                    input: t_acc,
                    interpolation: Checked::Valid(json::animation::Interpolation::Linear),
                    output: v_acc,
                },
            );

            let gltf_prop = match track.property {
                TrackProperty::Translation => json::animation::Property::Translation,
                TrackProperty::Rotation => json::animation::Property::Rotation,
                TrackProperty::Scale => json::animation::Property::Scale,
            };

            channels.push(json::animation::Channel {
                sampler: sampler_idx,
                target: json::animation::Target {
                    extensions: None,
                    extras: Default::default(),
                    node: json::Index::new(node_idx as u32),
                    path: Checked::Valid(gltf_prop),
                },
                extensions: None,
                extras: Default::default(),
            });
        }

        if channels.is_empty() {
            return;
        }

        self.root.animations.push(json::Animation {
            name: Some(clip.name.clone()),
            channels,
            samplers,
            extensions: None,
            extras: Default::default(),
        });
    }

    fn align_bin_4(&mut self) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        self.bin.len()
    }

    fn push_buffer_view(
        &mut self,
        offset: usize,
        len: usize,
        target: Option<json::buffer::Target>,
    ) -> json::Index<json::buffer::View> {
        json::Index::push(
            &mut self.root.buffer_views,
            json::buffer::View {
                buffer: self.buffer_idx,
                byte_length: json::validation::USize64::from(len),
                byte_offset: Some(json::validation::USize64::from(offset)),
                byte_stride: None,
                name: None,
                target: target.map(Checked::Valid),
                extensions: None,
                extras: Default::default(),
            },
        )
    }

    fn push_accessor(
        &mut self,
        buffer_view: json::Index<json::buffer::View>,
        count: usize,
        component_type: json::accessor::ComponentType,
        type_: json::accessor::Type,
        min: Option<serde_json::Value>,
        max: Option<serde_json::Value>,
    ) -> json::Index<json::Accessor> {
        json::Index::push(
            &mut self.root.accessors,
            json::Accessor {
                buffer_view: Some(buffer_view),
                byte_offset: None,
                count: json::validation::USize64::from(count),
                component_type: Checked::Valid(json::accessor::GenericComponentType(component_type)),
                extensions: None,
                extras: Default::default(),
                type_: Checked::Valid(type_),
                min,
                max,
                normalized: false,
                name: None,
                sparse: None,
            },
        )
    }
}

/// Converts Unity row-major `m_BindPose` matrix to right-handed glTF column-major matrix
/// via mirror conjugation `M' = S * M * S` where `S = diag(-1, 1, 1, 1)`.
pub fn unity_bindpose_row_major_to_gltf_col_major(m: &[f32; 16]) -> [f32; 16] {
    // `m` is row-major: r*4 + c. Conjugating by S flips sign when (r == 0) ^ (c == 0).
    let mut s = [0.0f32; 16];
    for r in 0..4 {
        for c in 0..4 {
            let flip = (r == 0) ^ (c == 0);
            let v = m[r * 4 + c];
            s[r * 4 + c] = if flip { -v } else { v };
        }
    }
    // Transpose row-major -> column-major for glTF.
    [
        s[0], s[4], s[8], s[12],
        s[1], s[5], s[9], s[13],
        s[2], s[6], s[10], s[14],
        s[3], s[7], s[11], s[15],
    ]
}

pub type LocalTrsRh = (Option<[f32; 3]>, Option<[f32; 4]>, Option<[f32; 3]>);

/// Extracts local translation, rotation quaternion, and scale from a Unity `Transform`
/// converted into right-handed Y-up space (`[-x, y, z]`, conjugated quaternion).
pub fn extract_local_trs_rh(props: &indexmap::IndexMap<String, UnityValue>) -> LocalTrsRh {
    let t = props
        .get("m_LocalPosition")
        .and_then(|v| v.as_object())
        .and_then(|o| {
            let x = o.get("x").and_then(UnityValue::as_f64)? as f32;
            let y = o.get("y").and_then(UnityValue::as_f64)? as f32;
            let z = o.get("z").and_then(UnityValue::as_f64)? as f32;
            Some([-x, y, z])
        });

    let r = props
        .get("m_LocalRotation")
        .and_then(|v| v.as_object())
        .and_then(|o| {
            let x = o.get("x").and_then(UnityValue::as_f64)? as f32;
            let y = o.get("y").and_then(UnityValue::as_f64)? as f32;
            let z = o.get("z").and_then(UnityValue::as_f64)? as f32;
            let w = o.get("w").and_then(UnityValue::as_f64)? as f32;
            let mut q = [x, -y, -z, w];
            let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
            if len > 1e-6 {
                q[0] /= len;
                q[1] /= len;
                q[2] /= len;
                q[3] /= len;
                Some(q)
            } else {
                Some([0.0, 0.0, 0.0, 1.0])
            }
        });

    let s = props
        .get("m_LocalScale")
        .and_then(|v| v.as_object())
        .and_then(|o| {
            let x = o.get("x").and_then(UnityValue::as_f64)? as f32;
            let y = o.get("y").and_then(UnityValue::as_f64)? as f32;
            let z = o.get("z").and_then(UnityValue::as_f64)? as f32;
            Some([x, y, z])
        });

    (t, r, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindpose_mirror_transpose_preserves_translation_x_flip() {
        let mut unity_m = [
            1.0, 0.0, 0.0, 2.0,
            0.0, 1.0, 0.0, 3.0,
            0.0, 0.0, 1.0, -4.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        // Add an XY rotation term to verify off-diagonal conjugation sign flip.
        unity_m[1] = 0.25;
        unity_m[4] = -0.25;
        let col = unity_bindpose_row_major_to_gltf_col_major(&unity_m);
        // Column 3 (indices 12,13,14,15) holds glTF translation [-2.0, 3.0, -4.0, 1.0].
        assert_eq!(col[12], -2.0);
        assert_eq!(col[13], 3.0);
        assert_eq!(col[14], -4.0);
        assert_eq!(col[15], 1.0);
        // Row 0 Col 1 in col-major is index 4 -> negated `-0.25`.
        assert_eq!(col[4], -0.25);
        // Row 1 Col 0 in col-major is index 1 -> negated `+0.25`.
        assert_eq!(col[1], 0.25);
    }

    #[test]
    fn miroring_quaternion_produces_valid_unit_rotation() {
        let mut map = indexmap::IndexMap::new();
        let mut pos = indexmap::IndexMap::new();
        pos.insert("x".into(), UnityValue::Float(1.5));
        pos.insert("y".into(), UnityValue::Float(2.0));
        pos.insert("z".into(), UnityValue::Float(-3.0));
        map.insert("m_LocalPosition".into(), UnityValue::Object(pos));

        let mut rot = indexmap::IndexMap::new();
        rot.insert("x".into(), UnityValue::Float(0.0));
        rot.insert("y".into(), UnityValue::Float(std::f64::consts::FRAC_1_SQRT_2));
        rot.insert("z".into(), UnityValue::Float(0.0));
        rot.insert("w".into(), UnityValue::Float(std::f64::consts::FRAC_1_SQRT_2));
        map.insert("m_LocalRotation".into(), UnityValue::Object(rot));

        let (t, r, _) = extract_local_trs_rh(&map);
        assert_eq!(t, Some([-1.5, 2.0, -3.0]));
        let r = r.unwrap();
        assert!((r[1] + std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        assert!((r[3] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    }
}
