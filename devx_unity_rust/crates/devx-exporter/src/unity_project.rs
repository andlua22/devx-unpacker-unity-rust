//! Complete Unity Editor Project exporter.
//!
//! Writes a ready-to-open Unity project layout (`Assets/`, `ProjectSettings/`, `Packages/manifest.json`)
//! with deterministic `.meta` GUIDs so `.unity` scenes, `.prefab` assets, `.mat` materials, `.controller`
//! AnimatorControllers, `.anim` clips, `.shader` files, `.fbx` models, `.png` textures, `.wav` audio,
//! fonts, and recovered `[SerializeField]` C# `.cs` scripts (`MonoScript`) cross-link cleanly.

use crate::ExportResult;
use crate::hierarchy::{discover_hierarchy_groups, sanitize_stem};
use crate::prefab::{
    ExternalYamlRef, PrefabExportOptions, animator_controller_meta_text,
    build_animator_controller_yaml, build_prefab_bundle_with_external_refs, object_display_stem,
    rewrite_pptrs_in_value, serialize_unity_classes_yaml,
};
use crate::unitypackage::deterministic_guid;
use devx_codecs::animator::decode_animator_controller;
use devx_codecs::audio::{decode_audio_clip, encode_wav};
use devx_codecs::fbx::export_mesh_to_fbx;
use devx_codecs::mesh::decode_mesh;
use devx_codecs::shader::decode_shader;
use devx_codecs::text_font::decode_font;
use devx_codecs::texture::{TextureDecodeOptions, decode_texture2d, encode_png};
use devx_plugin_api::AssetRef;
use devx_scripts::{MonoScriptInfo, ScriptsIndex};
use devx_unity_io::{LoadedAssetFile, UnityWorkspaceData, extract_i64_field, read_object};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use unity_asset_core::{UnityClass, UnityValue};

/// Options controlling full Unity Editor project generation.
#[derive(Debug, Clone)]
pub struct UnityProjectExportOptions {
    /// Optional substring filter used to load matching deferred `.ab` bundles (+ dependencies).
    pub bundle_filter: Option<String>,
    /// If true, loads all deferred `.ab` bundles before exporting (may be large on mobile APKs).
    pub load_all_bundles: bool,
    /// Maximum `.unity` scenes to export (`0` = unlimited).
    pub max_scenes: usize,
    /// Maximum `.prefab` hierarchies to export (`0` = unlimited).
    pub max_prefabs: usize,
    /// Emit recovered `[SerializeField]` `.cs` stubs under `Assets/Scripts/` and link `MonoBehaviour.m_Script`.
    pub include_scripts: bool,
}

impl Default for UnityProjectExportOptions {
    fn default() -> Self {
        Self {
            bundle_filter: None,
            load_all_bundles: false,
            max_scenes: 64,
            max_prefabs: 256,
            include_scripts: true,
        }
    }
}

/// Summary report returned after exporting a complete Unity Editor project.
#[derive(Debug, Clone, Default)]
pub struct UnityProjectExportReport {
    pub output_dir: PathBuf,
    pub unity_version: String,
    pub scenes: usize,
    pub prefabs: usize,
    pub models: usize,
    pub materials: usize,
    pub textures: usize,
    pub animations: usize,
    pub controllers: usize,
    pub shaders: usize,
    pub audio: usize,
    pub fonts: usize,
    pub scripts: usize,
    pub files_written: usize,
    pub warnings: Vec<String>,
}

impl UnityProjectExportReport {
    /// Human-readable one-line summary for CLI and UI status banners.
    pub fn summary_line(&self) -> String {
        format!(
            "Unity {} project ({} files): {} scenes, {} prefabs, {} models, {} mats, {} tex, {} anims, {} ctrls, {} shaders, {} audio, {} fonts, {} scripts",
            self.unity_version,
            self.files_written,
            self.scenes,
            self.prefabs,
            self.models,
            self.materials,
            self.textures,
            self.animations,
            self.controllers,
            self.shaders,
            self.audio,
            self.fonts,
            self.scripts,
        )
    }
}

struct ExportCtx<'a> {
    out_dir: &'a Path,
    used_paths: HashSet<String>,
    external_refs: HashMap<(usize, i64), ExternalYamlRef>,
    files_written: usize,
    warnings: Vec<String>,
}

impl<'a> ExportCtx<'a> {
    fn alloc_rel_path(&mut self, folder: &str, stem: &str, ext: &str) -> String {
        let safe_stem = if stem.trim().is_empty() { "Asset" } else { stem };
        let base = format!("{folder}/{safe_stem}.{ext}");
        if self.used_paths.insert(base.clone()) {
            return base;
        }
        let mut idx = 2usize;
        loop {
            let cand = format!("{folder}/{safe_stem}_{idx}.{ext}");
            if self.used_paths.insert(cand.clone()) {
                return cand;
            }
            idx += 1;
        }
    }

    fn write_raw_file(&mut self, rel_from_root: &str, bytes: &[u8]) -> ExportResult<()> {
        let dest = self.out_dir.join(rel_from_root);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, bytes)?;
        self.files_written += 1;
        Ok(())
    }

    fn write_asset_with_meta(
        &mut self,
        rel_asset_under_assets: &str,
        asset_bytes: &[u8],
        meta_body: &str,
    ) -> ExportResult<String> {
        let asset_key = format!("Assets/{rel_asset_under_assets}");
        let guid = deterministic_guid(&asset_key);
        let meta = format!("fileFormatVersion: 2\nguid: {guid}\n{meta_body}");
        self.write_raw_file(&asset_key, asset_bytes)?;
        self.write_raw_file(&format!("{asset_key}.meta"), meta.as_bytes())?;
        Ok(guid)
    }

    fn create_folder_tree_metas(&mut self, top_folders: &[&str]) -> ExportResult<()> {
        let assets_dir = self.out_dir.join("Assets");
        std::fs::create_dir_all(&assets_dir)?;
        for folder in top_folders {
            let disk = assets_dir.join(folder);
            std::fs::create_dir_all(&disk)?;
            let key = format!("Assets/{folder}");
            let guid = deterministic_guid(&key);
            let meta = format!(
                "fileFormatVersion: 2\nguid: {guid}\nfolderAsset: yes\nDefaultImporter:\n  externalObjects: {{}}\n"
            );
            self.write_raw_file(&format!("{key}.meta"), meta.as_bytes())?;
        }
        Ok(())
    }
}

/// Exports the loaded Unity workspace into a structured Unity Editor project at `out_dir`.
pub fn export_unity_project(
    workspace: &mut UnityWorkspaceData,
    out_dir: &Path,
    opts: &UnityProjectExportOptions,
) -> ExportResult<UnityProjectExportReport> {
    if opts.load_all_bundles {
        let _ = workspace.load_deferred_bundles_filtered(None);
    } else if let Some(ref filter) = opts.bundle_filter
        && !filter.trim().is_empty()
    {
        let _ = workspace.load_deferred_bundles_with_deps(Some(filter.trim()));
    }

    let unity_version = detect_unity_version(workspace);
    let mut ctx = ExportCtx {
        out_dir,
        used_paths: HashSet::new(),
        external_refs: HashMap::new(),
        files_written: 0,
        warnings: Vec::new(),
    };

    ctx.create_folder_tree_metas(&[
        "Scenes",
        "Prefabs",
        "Models",
        "Materials",
        "Textures",
        "Animations",
        "Controllers",
        "Shaders",
        "Audio",
        "Fonts",
        "Scripts",
    ])?;

    write_packages_manifest(&mut ctx)?;

    let mut report = UnityProjectExportReport {
        output_dir: out_dir.to_path_buf(),
        unity_version: unity_version.clone(),
        ..Default::default()
    };

    // 1. Export MonoScript C# stubs first so prefabs/scenes wire `MonoBehaviour.m_Script`.
    if opts.include_scripts {
        report.scripts = export_scripts_stage(workspace, &mut ctx);
    }

    // 2. Export leaf assets (Textures, Shaders, Meshes, Audio, Fonts, AnimationClips).
    export_leaf_assets_stage(workspace, &mut ctx, &mut report);

    // 3. Export Materials (linking Texture2D & Shader GUIDs) and AnimatorControllers (linking Clips).
    export_materials_and_controllers_stage(workspace, &mut ctx, &mut report);

    // 4. Export Scenes (`Assets/Scenes/*.unity`) and Prefabs (`Assets/Prefabs/**/*.prefab`).
    let scene_indices = discover_scene_file_indices(workspace);
    let exported_scenes = export_scenes_stage(workspace, &scene_indices, opts.max_scenes, &mut ctx);
    report.scenes = exported_scenes.len();
    report.prefabs = export_prefabs_stage(workspace, &scene_indices, opts.max_prefabs, &mut ctx);

    // 5. Emit `ProjectSettings/` with detected Unity version, TagManager, QualitySettings & BuildSettings.
    write_project_settings(workspace, &unity_version, &exported_scenes, &mut ctx)?;

    report.files_written = ctx.files_written;
    report.warnings = ctx.warnings;
    Ok(report)
}

fn detect_unity_version(workspace: &UnityWorkspaceData) -> String {
    workspace
        .files
        .iter()
        .map(|f| f.file.unity_version.as_str())
        .find(|v| !v.trim().is_empty() && *v != "0.0.0")
        .unwrap_or("2021.3.45f1")
        .to_owned()
}

fn write_packages_manifest(ctx: &mut ExportCtx<'_>) -> ExportResult<()> {
    let manifest = serde_json::json!({
        "dependencies": {
            "com.unity.textmeshpro": "3.0.6",
            "com.unity.timeline": "1.7.6",
            "com.unity.ugui": "1.0.0",
            "com.unity.modules.animation": "1.0.0",
            "com.unity.modules.audio": "1.0.0",
            "com.unity.modules.particlesystem": "1.0.0",
            "com.unity.modules.physics": "1.0.0",
            "com.unity.modules.terrain": "1.0.0",
            "com.unity.modules.ui": "1.0.0"
        }
    });
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| crate::ExportError::Yaml(format!("manifest json: {e}")))?;
    ctx.write_raw_file("Packages/manifest.json", text.as_bytes())
}

fn write_project_settings(
    workspace: &UnityWorkspaceData,
    unity_version: &str,
    exported_scenes: &[(String, String)],
    ctx: &mut ExportCtx<'_>,
) -> ExportResult<()> {
    let version_txt = format!(
        "m_EditorVersion: {unity_version}\nm_EditorVersionWithRevision: {unity_version} (000000000000)\n"
    );
    ctx.write_raw_file("ProjectSettings/ProjectVersion.txt", version_txt.as_bytes())?;

    let tag_yaml = find_singleton_setting_yaml(workspace, 78, "TagManager").unwrap_or_else(|| {
        "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!78 &1\nTagManager:\n  serializedVersion: 2\n  tags: []\n  layers:\n    - Default\n    - TransparentFX\n    - Ignore Raycast\n    - \"\"\n    - Water\n    - UI\n  m_SortingLayers: []\n"
            .to_owned()
    });
    ctx.write_raw_file("ProjectSettings/TagManager.asset", tag_yaml.as_bytes())?;

    let quality_yaml =
        find_singleton_setting_yaml(workspace, 47, "QualitySettings").unwrap_or_else(|| {
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!47 &1\nQualitySettings:\n  serializedVersion: 5\n  m_CurrentQuality: 0\n  m_QualitySettings: []\n"
                .to_owned()
        });
    ctx.write_raw_file("ProjectSettings/QualitySettings.asset", quality_yaml.as_bytes())?;

    let mut build_cls = UnityClass::new(1045, "EditorBuildSettings".to_owned(), "1".to_owned());
    build_cls.set("m_ObjectHideFlags".to_owned(), UnityValue::Integer(0));
    build_cls.set("serializedVersion".to_owned(), UnityValue::Integer(2));
    let scene_entries: Vec<UnityValue> = exported_scenes
        .iter()
        .map(|(path, guid)| {
            let mut m = indexmap::IndexMap::new();
            m.insert("enabled".to_owned(), UnityValue::Integer(1));
            m.insert("path".to_owned(), UnityValue::String(path.clone()));
            m.insert("guid".to_owned(), UnityValue::String(guid.clone()));
            UnityValue::Object(m)
        })
        .collect();
    build_cls.set("m_Scenes".to_owned(), UnityValue::Array(scene_entries));
    if let Ok(yaml) = serialize_unity_classes_yaml(&[build_cls]) {
        ctx.write_raw_file("ProjectSettings/EditorBuildSettings.asset", yaml.as_bytes())?;
    }
    Ok(())
}

fn find_singleton_setting_yaml(
    workspace: &UnityWorkspaceData,
    target_class: i32,
    fallback_name: &str,
) -> Option<String> {
    for loaded in &workspace.files {
        for info in &loaded.file.objects {
            if info.type_id != target_class {
                continue;
            }
            if let Ok(mut obj) = read_object(loaded, info) {
                obj.class.anchor = "1".to_owned();
                if obj.class.class_name.is_empty() {
                    obj.class.class_name = fallback_name.to_owned();
                }
                if let Ok(yaml) = serialize_unity_classes_yaml(&[obj.class]) {
                    return Some(yaml);
                }
            }
        }
    }
    None
}

fn export_scripts_stage(workspace: &UnityWorkspaceData, ctx: &mut ExportCtx<'_>) -> usize {
    let idx = ScriptsIndex::from_workspace(workspace);
    let mut class_to_guid: HashMap<(String, String), String> = HashMap::new();
    let mut written_scripts = 0usize;

    for ms in &idx.mono_scripts {
        let cls_clean = ms.class_name.trim();
        if cls_clean.is_empty() || cls_clean.starts_with('<') {
            continue;
        }
        let key = (ms.namespace.trim().to_owned(), cls_clean.to_owned());
        let guid = if let Some(existing_guid) = class_to_guid.get(&key) {
            existing_guid.clone()
        } else {
            let folder = mono_script_folder(ms);
            let stem = sanitize_stem(cls_clean);
            let rel = ctx.alloc_rel_path(&folder, &stem, "cs");
            let code = idx.generate_csharp_stub_for_script(ms);
            let meta_body = "MonoImporter:\n  externalObjects: {}\n  serializedVersion: 2\n  defaultReferences: []\n  executionOrder: 0\n  icon: {instanceID: 0}\n  userData: \n  assetBundleName: \n  assetBundleVariant: \n";
            match ctx.write_asset_with_meta(&rel, code.as_bytes(), meta_body) {
                Ok(g) => {
                    written_scripts += 1;
                    class_to_guid.insert(key, g.clone());
                    g
                }
                Err(err) => {
                    ctx.warnings.push(format!("script `{}`: {err}", ms.full_name()));
                    continue;
                }
            }
        };

        ctx.external_refs.insert(
            (ms.file_index, ms.path_id),
            ExternalYamlRef {
                file_id: 11_500_000,
                guid,
                asset_type: 3,
            },
        );
    }

    written_scripts
}

fn mono_script_folder(ms: &MonoScriptInfo) -> String {
    let segs: Vec<String> = ms
        .namespace
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('<'))
        .map(sanitize_stem)
        .collect();
    if segs.is_empty() {
        "Scripts".to_owned()
    } else {
        format!("Scripts/{}", segs.join("/"))
    }
}

fn export_leaf_assets_stage(
    workspace: &UnityWorkspaceData,
    ctx: &mut ExportCtx<'_>,
    report: &mut UnityProjectExportReport,
) {
    for (file_index, loaded) in workspace.files.iter().enumerate() {
        let unity_ver = &loaded.file.unity_version;
        for info in &loaded.file.objects {
            if !matches!(info.type_id, 28 | 43 | 48 | 74 | 83 | 128) {
                continue;
            }
            let Ok(mut obj) = read_object(loaded, info) else {
                continue;
            };
            match info.type_id {
                28 => {
                    let Ok(img) = decode_texture2d(
                        &obj,
                        unity_ver,
                        &workspace.streams,
                        TextureDecodeOptions::default(),
                    ) else {
                        continue;
                    };
                    let Ok(png) = encode_png(&img) else {
                        continue;
                    };
                    let stem = object_display_stem(&obj, "Texture", info.path_id);
                    let rel = ctx.alloc_rel_path("Textures", &stem, "png");
                    if let Ok(guid) = ctx.write_asset_with_meta(
                        &rel,
                        &png,
                        "TextureImporter:\n  internalIDToNameTable: []\n  externalObjects: {}\n",
                    ) {
                        ctx.external_refs.insert(
                            (file_index, info.path_id),
                            ExternalYamlRef {
                                file_id: 2_800_000,
                                guid,
                                asset_type: 3,
                            },
                        );
                        report.textures += 1;
                    }
                }
                43 => {
                    let Ok(mesh) = decode_mesh(&obj, unity_ver, &workspace.streams) else {
                        continue;
                    };
                    let Ok(fbx) = export_mesh_to_fbx(&mesh) else {
                        continue;
                    };
                    let stem = object_display_stem(&obj, "Mesh", info.path_id);
                    let rel = ctx.alloc_rel_path("Models", &stem, "fbx");
                    let meta = format!(
                        "ModelImporter:\n  fileIDToRecycleName:\n    4300000: {}\n  externalObjects: {{}}\n",
                        mesh.name
                    );
                    if let Ok(guid) = ctx.write_asset_with_meta(&rel, &fbx, &meta) {
                        ctx.external_refs.insert(
                            (file_index, info.path_id),
                            ExternalYamlRef {
                                file_id: 4_300_000,
                                guid,
                                asset_type: 3,
                            },
                        );
                        report.models += 1;
                    }
                }
                48 => {
                    let Ok(shader) = decode_shader(&obj) else {
                        continue;
                    };
                    let text = if !shader.shaderlab_text.is_empty() {
                        shader.shaderlab_text
                    } else {
                        shader.decompiled_cg_text
                    };
                    if text.is_empty() {
                        continue;
                    }
                    let stem = object_display_stem(&obj, "Shader", info.path_id);
                    let rel = ctx.alloc_rel_path("Shaders", &stem, "shader");
                    if let Ok(guid) = ctx.write_asset_with_meta(
                        &rel,
                        text.as_bytes(),
                        "ShaderImporter:\n  externalObjects: {}\n",
                    ) {
                        ctx.external_refs.insert(
                            (file_index, info.path_id),
                            ExternalYamlRef {
                                file_id: 4_800_000,
                                guid,
                                asset_type: 3,
                            },
                        );
                        report.shaders += 1;
                    }
                }
                74 => {
                    obj.class.anchor = "7400000".to_owned();
                    let Ok(yaml) = serialize_unity_classes_yaml(&[obj.class.clone()]) else {
                        continue;
                    };
                    let stem = object_display_stem(&obj, "Clip", info.path_id);
                    let rel = ctx.alloc_rel_path("Animations", &stem, "anim");
                    if let Ok(guid) = ctx.write_asset_with_meta(
                        &rel,
                        yaml.as_bytes(),
                        "NativeFormatImporter:\n  externalObjects: {}\n  mainObjectFileID: 7400000\n",
                    ) {
                        ctx.external_refs.insert(
                            (file_index, info.path_id),
                            ExternalYamlRef {
                                file_id: 7_400_000,
                                guid,
                                asset_type: 2,
                            },
                        );
                        report.animations += 1;
                    }
                }
                83 => {
                    let Ok(clip) = decode_audio_clip(&obj, unity_ver, &workspace.streams) else {
                        continue;
                    };
                    let Some((ext, bytes)) = audio_export_payload(&clip) else {
                        continue;
                    };
                    let stem = sanitize_stem(&clip.name);
                    let rel = ctx.alloc_rel_path("Audio", &stem, ext);
                    if let Ok(guid) = ctx.write_asset_with_meta(
                        &rel,
                        &bytes,
                        "AudioImporter:\n  externalObjects: {}\n",
                    ) {
                        ctx.external_refs.insert(
                            (file_index, info.path_id),
                            ExternalYamlRef {
                                file_id: 8_300_000,
                                guid,
                                asset_type: 3,
                            },
                        );
                        report.audio += 1;
                    }
                }
                128 => {
                    let Ok(font) = decode_font(&obj) else {
                        continue;
                    };
                    if font.data.is_empty() {
                        continue;
                    }
                    let stem = sanitize_stem(&font.name);
                    let rel = ctx.alloc_rel_path("Fonts", &stem, font.format.extension());
                    if let Ok(guid) = ctx.write_asset_with_meta(
                        &rel,
                        &font.data,
                        "TrueTypeFontImporter:\n  externalObjects: {}\n",
                    ) {
                        ctx.external_refs.insert(
                            (file_index, info.path_id),
                            ExternalYamlRef {
                                file_id: 12_800_000,
                                guid,
                                asset_type: 3,
                            },
                        );
                        report.fonts += 1;
                    }
                }
                _ => {}
            }
        }
    }
}

fn audio_export_payload(clip: &devx_codecs::audio::DecodedAudioClip) -> Option<(&'static str, Vec<u8>)> {
    if !clip.primary.samples_i16.is_empty()
        && let Ok(wav) = encode_wav(&clip.primary)
    {
        return Some(("wav", wav));
    }
    let (ext_hint, raw) = clip.primary.raw_container.as_ref()?;
    let ext_low = ext_hint.to_ascii_lowercase();
    if raw.starts_with(b"OggS") || ext_low == "ogg" {
        Some(("ogg", raw.clone()))
    } else if raw.starts_with(b"RIFF") || ext_low == "wav" {
        Some(("wav", raw.clone()))
    } else if ext_low == "mp3"
        || raw.starts_with(b"ID3")
        || (raw.len() >= 2 && raw[0] == 0xFF && (raw[1] & 0xE0) == 0xE0)
    {
        Some(("mp3", raw.clone()))
    } else {
        None
    }
}

fn export_materials_and_controllers_stage(
    workspace: &UnityWorkspaceData,
    ctx: &mut ExportCtx<'_>,
    report: &mut UnityProjectExportReport,
) {
    let empty_internal = HashMap::new();
    for (file_index, loaded) in workspace.files.iter().enumerate() {
        for info in &loaded.file.objects {
            if info.type_id != 21 && info.type_id != 91 {
                continue;
            }
            let Ok(mut obj) = read_object(loaded, info) else {
                continue;
            };
            if info.type_id == 21 {
                let stem = object_display_stem(&obj, "Material", info.path_id);
                let mut val = UnityValue::Object(obj.class.properties().clone());
                rewrite_pptrs_in_value(workspace, file_index, &mut val, &empty_internal, &ctx.external_refs);
                if let UnityValue::Object(map) = val {
                    *obj.class.properties_mut() = map;
                }
                obj.class.anchor = "2100000".to_owned();
                let Ok(yaml) = serialize_unity_classes_yaml(&[obj.class]) else {
                    continue;
                };
                let rel = ctx.alloc_rel_path("Materials", &stem, "mat");
                if let Ok(guid) = ctx.write_asset_with_meta(
                    &rel,
                    yaml.as_bytes(),
                    "NativeFormatImporter:\n  externalObjects: {}\n  mainObjectFileID: 2100000\n",
                ) {
                    ctx.external_refs.insert(
                        (file_index, info.path_id),
                        ExternalYamlRef {
                            file_id: 2_100_000,
                            guid,
                            asset_type: 2,
                        },
                    );
                    report.materials += 1;
                }
            } else {
                let Ok(decoded) = decode_animator_controller(&obj, file_index, workspace) else {
                    continue;
                };
                let Ok(yaml) = build_animator_controller_yaml(&decoded, &ctx.external_refs) else {
                    continue;
                };
                let stem = object_display_stem(&obj, "AnimatorController", info.path_id);
                let rel = ctx.alloc_rel_path("Controllers", &stem, "controller");
                let key = format!("Assets/{rel}");
                let guid = deterministic_guid(&key);
                let meta = animator_controller_meta_text(&guid);
                if ctx.write_raw_file(&key, yaml.as_bytes()).is_ok()
                    && ctx.write_raw_file(&format!("{key}.meta"), meta.as_bytes()).is_ok()
                {
                    ctx.external_refs.insert(
                        (file_index, info.path_id),
                        ExternalYamlRef {
                            file_id: 9_100_000,
                            guid,
                            asset_type: 2,
                        },
                    );
                    report.controllers += 1;
                }
            }
        }
    }
}

fn discover_build_scene_names(workspace: &UnityWorkspaceData) -> Vec<String> {
    for loaded in &workspace.files {
        for info in &loaded.file.objects {
            if info.type_id != 141 {
                continue;
            }
            if let Ok(obj) = read_object(loaded, info)
                && let Some(arr) = obj.class.properties().get("scenes").and_then(UnityValue::as_array)
            {
                return arr
                    .iter()
                    .filter_map(UnityValue::as_str)
                    .map(|p| {
                        Path::new(p)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(sanitize_stem)
                            .unwrap_or_else(|| "Scene".to_owned())
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

fn is_level_scene_file(loaded: &LoadedAssetFile) -> Option<usize> {
    let base = Path::new(&loaded.virtual_path)
        .file_name()
        .and_then(|s| s.to_str())?;
    let suffix = base.strip_prefix("level")?;
    if suffix.chars().all(|c| c.is_ascii_digit()) && !suffix.is_empty() {
        suffix.parse::<usize>().ok()
    } else {
        None
    }
}

fn discover_scene_file_indices(workspace: &UnityWorkspaceData) -> HashSet<usize> {
    let mut set = HashSet::new();
    for (idx, loaded) in workspace.files.iter().enumerate() {
        if is_level_scene_file(loaded).is_some()
            || loaded
                .bundle_container_paths
                .values()
                .any(|p| p.to_ascii_lowercase().ends_with(".unity"))
        {
            set.insert(idx);
        }
    }
    set
}

fn collect_root_transforms_in_file(
    workspace: &UnityWorkspaceData,
    file_index: usize,
) -> Vec<AssetRef> {
    let loaded = &workspace.files[file_index];
    let mut roots = Vec::new();
    for info in &loaded.file.objects {
        if info.type_id != 4 && info.type_id != 224 {
            continue;
        }
        let Ok(obj) = read_object(loaded, info) else {
            continue;
        };
        let has_parent = obj
            .class
            .properties()
            .get("m_Father")
            .and_then(|v| v.as_object())
            .and_then(|m| m.get("m_PathID"))
            .and_then(extract_i64_field)
            .is_some_and(|pid| pid != 0);
        if !has_parent {
            roots.push(AssetRef {
                file_index,
                path_id: info.path_id,
            });
        }
    }
    roots
}

fn export_scenes_stage(
    workspace: &UnityWorkspaceData,
    scene_indices: &HashSet<usize>,
    max_scenes: usize,
    ctx: &mut ExportCtx<'_>,
) -> Vec<(String, String)> {
    let build_names = discover_build_scene_names(workspace);
    let mut ordered: Vec<usize> = scene_indices.iter().copied().collect();
    ordered.sort_unstable();

    let mut exported = Vec::new();
    for file_index in ordered {
        if max_scenes > 0 && exported.len() >= max_scenes {
            break;
        }
        let loaded = &workspace.files[file_index];
        let roots = collect_root_transforms_in_file(workspace, file_index);
        if roots.is_empty() {
            continue;
        }
        let stem = is_level_scene_file(loaded)
            .and_then(|lvl| build_names.get(lvl).cloned())
            .or_else(|| {
                loaded
                    .bundle_container_paths
                    .values()
                    .find(|p| p.to_ascii_lowercase().ends_with(".unity"))
                    .and_then(|p| Path::new(p).file_stem().and_then(|s| s.to_str()))
                    .map(sanitize_stem)
            })
            .unwrap_or_else(|| {
                let raw = Path::new(&loaded.virtual_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Scene");
                sanitize_stem(raw)
            });

        let rel = ctx.alloc_rel_path("Scenes", &stem, "unity");
        let opts = PrefabExportOptions {
            is_scene: true,
            include_companion_assets: false,
            package_prefix: "Assets/Scenes".to_owned(),
        };
        match build_prefab_bundle_with_external_refs(
            workspace,
            &roots,
            &stem,
            &opts,
            Some(&ctx.external_refs),
        ) {
            Ok(bundle) => {
                if let Ok(guid) = ctx.write_asset_with_meta(
                    &rel,
                    bundle.yaml_text.as_bytes(),
                    "DefaultAsset:\n  externalObjects: {}\n",
                ) {
                    exported.push((format!("Assets/{rel}"), guid));
                }
            }
            Err(err) => ctx.warnings.push(format!("scene `{stem}`: {err}")),
        }
    }
    exported
}

fn export_prefabs_stage(
    workspace: &UnityWorkspaceData,
    scene_indices: &HashSet<usize>,
    max_prefabs: usize,
    ctx: &mut ExportCtx<'_>,
) -> usize {
    let groups = discover_hierarchy_groups(workspace, None);
    let mut exported = 0usize;
    for group in groups {
        if max_prefabs > 0 && exported >= max_prefabs {
            break;
        }
        if group.roots.iter().all(|r| scene_indices.contains(&r.file_index)) {
            continue;
        }
        let stem = sanitize_stem(&group.stem);
        let rel = ctx.alloc_rel_path("Prefabs", &stem, "prefab");
        let opts = PrefabExportOptions {
            is_scene: false,
            include_companion_assets: false,
            package_prefix: "Assets/Prefabs".to_owned(),
        };
        match build_prefab_bundle_with_external_refs(
            workspace,
            &group.roots,
            &stem,
            &opts,
            Some(&ctx.external_refs),
        ) {
            Ok(bundle) => {
                if ctx
                    .write_asset_with_meta(
                        &rel,
                        bundle.yaml_text.as_bytes(),
                        "PrefabImporter:\n  externalObjects: {}\n",
                    )
                    .is_ok()
                {
                    exported += 1;
                }
            }
            Err(err) => ctx.warnings.push(format!("prefab `{stem}`: {err}")),
        }
    }
    exported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_complete_unity_project_structure_and_script_paths() {
        let ms = MonoScriptInfo {
            file_index: 0,
            path_id: 42,
            class_name: "HeroMover".to_owned(),
            namespace: "Game.Player".to_owned(),
            assembly_name: "Assembly-CSharp.dll".to_owned(),
            execution_order: 0,
        };
        assert_eq!(mono_script_folder(&ms), "Scripts/Game/Player");

        let mut ws = UnityWorkspaceData::new(PathBuf::from("test_proj"));
        let tmp = std::env::temp_dir().join(format!("devx_unity_proj_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let rep = export_unity_project(
            &mut ws,
            &tmp,
            &UnityProjectExportOptions {
                include_scripts: true,
                ..Default::default()
            },
        )
        .expect("unity project export ok");

        assert_eq!(rep.unity_version, "2021.3.45f1");
        assert!(tmp.join("Packages/manifest.json").exists());
        assert!(tmp.join("ProjectSettings/ProjectVersion.txt").exists());
        assert!(tmp.join("ProjectSettings/TagManager.asset").exists());
        assert!(tmp.join("ProjectSettings/QualitySettings.asset").exists());
        assert!(tmp.join("ProjectSettings/EditorBuildSettings.asset").exists());
        assert!(tmp.join("Assets/Scripts.meta").exists());
        assert!(tmp.join("Assets/Scenes.meta").exists());
        assert!(tmp.join("Assets/Prefabs.meta").exists());

        let manifest_text =
            std::fs::read_to_string(tmp.join("Packages/manifest.json")).expect("read manifest");
        assert!(manifest_text.contains("com.unity.ugui"));

        // Verify MonoImporter `.cs` + `.meta` deterministic GUID helper.
        let mut ctx = ExportCtx {
            out_dir: &tmp,
            used_paths: HashSet::new(),
            external_refs: HashMap::new(),
            files_written: 0,
            warnings: Vec::new(),
        };
        let cs_rel = ctx.alloc_rel_path(&mono_script_folder(&ms), "HeroMover", "cs");
        assert_eq!(cs_rel, "Scripts/Game/Player/HeroMover.cs");
        let guid = ctx
            .write_asset_with_meta(&cs_rel, b"public class HeroMover {}", "MonoImporter:\n")
            .expect("write cs");
        assert_eq!(guid, deterministic_guid("Assets/Scripts/Game/Player/HeroMover.cs"));
        assert!(tmp.join("Assets/Scripts/Game/Player/HeroMover.cs").exists());
        assert!(tmp.join("Assets/Scripts/Game/Player/HeroMover.cs.meta").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
