//! Asset, batch directory, Unity YAML, `.unitypackage`, and UnityFS repack exporters.

pub mod bevy_export;
pub mod hierarchy;
pub mod prefab;
pub mod repack;
pub mod ui_canvas;
pub mod unity_project;
pub mod unitypackage;

use devx_codecs::audio::decode_audio_clip;
use devx_codecs::mesh::decode_mesh;
use devx_codecs::shader::decode_shader;
use devx_codecs::text_font::{decode_font, decode_text_asset};
use devx_codecs::texture::{TextureDecodeOptions, decode_sprite_cached, decode_texture2d};
use devx_core::AssetCatalog;
use devx_plugin_api::{
    AssetRef, AssetSummary, DecodedImage, ExportArtifact, ExportPayload, PluginHostContext,
    PluginRegistry,
};
use devx_unity_io::{
    ExtractBundlesReport, extract_raw_bundles, read_object, shared_tpk_registry, unity_value_to_json,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use unity_asset_binary::object::UnityObject;
use unity_asset_yaml::UnityYamlSerializer;
use unitypackage::{UnityPackageInputEntry, write_unitypackage};

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("asset not found: {file_index}:{path_id}")]
    AssetNotFound { file_index: usize, path_id: i64 },

    #[error("codec error: {0}")]
    Codec(#[from] devx_codecs::CodecError),

    #[error("plugin error: {0}")]
    Plugin(#[from] devx_plugin_api::PluginError),

    #[error("Unity YAML serialization error: {0}")]
    Yaml(String),

    #[error("bundle pack error: {0}")]
    Pack(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type ExportResult<T> = Result<T, ExportError>;

/// User-configurable preferred export formats for batch/single asset extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportAssetOptions {
    pub texture_exporter_id: String,
    pub audio_exporter_id: String,
    pub mesh_exporter_id: String,
    pub export_yaml_for_objects: bool,
    pub preserve_container_paths: bool,
    pub texture_flip_vertical: bool,
}

impl Default for ExportAssetOptions {
    fn default() -> Self {
        Self {
            texture_exporter_id: "builtin.png".to_owned(),
            audio_exporter_id: "builtin.wav".to_owned(),
            mesh_exporter_id: "builtin.obj".to_owned(),
            export_yaml_for_objects: true,
            preserve_container_paths: true,
            texture_flip_vertical: true,
        }
    }
}

/// Summary returned after a batch export job finishes.
#[derive(Debug, Clone, Default)]
pub struct BatchExportReport {
    pub exported_files: Vec<PathBuf>,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Serializes any parsed [`UnityObject`] to Unity YAML (`%YAML 1.1` + `!u!<ClassID> &<PathID>`).
pub fn serialize_object_yaml(obj: &UnityObject, path_id: i64) -> ExportResult<String> {
    let mut class = obj.class.clone();
    class.anchor = path_id.to_string();
    UnityYamlSerializer::new()
        .serialize_to_string(&[class])
        .map_err(|e| ExportError::Yaml(e.to_string()))
}

/// Renders an asset into an in-memory [`ExportArtifact`] using the chosen codec & plugin exporter.
pub fn build_export_artifact(
    catalog: &AssetCatalog,
    asset_ref: AssetRef,
    plugins: &PluginRegistry,
    host: &mut dyn PluginHostContext,
    opts: &ExportAssetOptions,
) -> ExportResult<ExportArtifact> {
    let mut cache = HashMap::new();
    build_export_artifact_with_cache(catalog, asset_ref, plugins, host, opts, &mut cache)
}

fn build_export_artifact_with_cache(
    catalog: &AssetCatalog,
    asset_ref: AssetRef,
    plugins: &PluginRegistry,
    host: &mut dyn PluginHostContext,
    opts: &ExportAssetOptions,
    sprite_tex_cache: &mut HashMap<(usize, i64), DecodedImage>,
) -> ExportResult<ExportArtifact> {
    let entry = catalog
        .entry(asset_ref)
        .ok_or(ExportError::AssetNotFound {
            file_index: asset_ref.file_index,
            path_id: asset_ref.path_id,
        })?;
    let summary = &entry.summary;
    let (loaded, info) = catalog
        .workspace
        .object_info(asset_ref.file_index, asset_ref.path_id)
        .ok_or(ExportError::AssetNotFound {
            file_index: asset_ref.file_index,
            path_id: asset_ref.path_id,
        })?;
    let obj = read_object(loaded, info).map_err(|e| ExportError::Io(std::io::Error::other(e)))?;

    let mut artifact = match summary.class_id {
        28 => {
            // Texture2D
            let mut img = decode_texture2d(
                &obj,
                &summary.unity_version,
                &catalog.workspace.streams,
                TextureDecodeOptions {
                    flip_vertical: opts.texture_flip_vertical,
                    ..Default::default()
                },
            )?;
            plugins.post_process_texture(summary, &mut img, host)?;
            plugins.export_with(
                &opts.texture_exporter_id,
                summary,
                &ExportPayload::Image(&img),
                host,
            )?
        }
        213 => {
            // Sprite
            let tpk = shared_tpk_registry();
            let mut sprite = decode_sprite_cached(
                &obj,
                asset_ref.file_index,
                &summary.unity_version,
                &catalog.workspace,
                &*tpk,
                sprite_tex_cache,
            )?;
            plugins.post_process_texture(summary, &mut sprite.image, host)?;
            plugins.export_with(
                &opts.texture_exporter_id,
                summary,
                &ExportPayload::Image(&sprite.image),
                host,
            )?
        }
        83 => {
            // AudioClip
            let mut clip =
                decode_audio_clip(&obj, &summary.unity_version, &catalog.workspace.streams)?;
            plugins.post_process_audio(summary, &mut clip.primary, host)?;
            plugins.export_with(
                &opts.audio_exporter_id,
                summary,
                &ExportPayload::Audio(&clip.primary),
                host,
            )?
        }
        43 => {
            // Mesh
            let mesh = decode_mesh(&obj, &summary.unity_version, &catalog.workspace.streams)?;
            plugins.export_with(
                &opts.mesh_exporter_id,
                summary,
                &ExportPayload::Mesh(&mesh),
                host,
            )?
        }
        49 => {
            // TextAsset
            let text_asset = decode_text_asset(&obj)?;
            let stem = safe_file_stem(summary);
            ExportArtifact {
                relative_path: format!("{stem}.{}", text_asset.suggested_extension()),
                bytes: text_asset.raw_bytes,
                mime: "text/plain".to_owned(),
            }
        }
        128 => {
            // Font
            let font = decode_font(&obj)?;
            let stem = safe_file_stem(summary);
            ExportArtifact {
                relative_path: format!("{stem}.{}", font.format.extension()),
                bytes: font.data,
                mime: "font/ttf".to_owned(),
            }
        }
        48 => {
            // Shader
            let decoded = decode_shader(&obj)?;
            let stem = safe_file_stem(summary);
            let text = if decoded.decompiled_cg_text.is_empty() {
                decoded.shaderlab_text
            } else {
                decoded.decompiled_cg_text
            };
            ExportArtifact {
                relative_path: format!("{stem}.shader"),
                bytes: text.into_bytes(),
                mime: "text/plain".to_owned(),
            }
        }
        _ => {
            if opts.export_yaml_for_objects {
                let yaml = serialize_object_yaml(&obj, asset_ref.path_id)?;
                let ext = match summary.class_id {
                    1 | 4 => "prefab.yaml",
                    21 => "mat",
                    _ => "asset.yaml",
                };
                ExportArtifact {
                    relative_path: format!("{}_{}.{ext}", safe_file_stem(summary), asset_ref.path_id),
                    bytes: yaml.into_bytes(),
                    mime: "application/yaml".to_owned(),
                }
            } else {
                let json_val = unity_value_to_json(
                    &unity_asset_core::UnityValue::Object(obj.class.properties().clone()),
                    Some(4096),
                );
                plugins.export_with(
                    "builtin.json",
                    summary,
                    &ExportPayload::Json(&json_val),
                    host,
                )?
            }
        }
    };

    if opts.preserve_container_paths
        && let Some(container) = &summary.container_path
        && let Some(parent_rel) = normalize_container_parent(container)
    {
        artifact.relative_path = format!("{parent_rel}/{}", artifact.relative_path);
    } else {
        artifact.relative_path = format!("{}/{}", summary.class_name, artifact.relative_path);
    }

    Ok(artifact)
}

/// Exports a single asset into `out_dir`, creating parent directories automatically.
pub fn export_single_asset(
    catalog: &AssetCatalog,
    asset_ref: AssetRef,
    out_dir: &Path,
    plugins: &PluginRegistry,
    host: &mut dyn PluginHostContext,
    opts: &ExportAssetOptions,
) -> ExportResult<PathBuf> {
    let artifact = build_export_artifact(catalog, asset_ref, plugins, host, opts)?;
    let full_path = unique_output_path(out_dir.join(&artifact.relative_path));
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&full_path, &artifact.bytes)?;
    Ok(full_path)
}

/// Exports multiple assets into `out_dir`, continuing past individual decode failures and reporting them.
pub fn export_batch_directory(
    catalog: &AssetCatalog,
    assets: &[AssetRef],
    out_dir: &Path,
    plugins: &PluginRegistry,
    host: &mut dyn PluginHostContext,
    opts: &ExportAssetOptions,
    mut on_progress: impl FnMut(usize, usize),
) -> ExportResult<BatchExportReport> {
    std::fs::create_dir_all(out_dir)?;
    let mut report = BatchExportReport::default();
    let total = assets.len();
    let mut sprite_tex_cache = HashMap::new();

    for (idx, &asset_ref) in assets.iter().enumerate() {
        on_progress(idx + 1, total);
        let written = build_export_artifact_with_cache(
            catalog,
            asset_ref,
            plugins,
            host,
            opts,
            &mut sprite_tex_cache,
        )
        .and_then(|artifact| {
            let full_path = unique_output_path(out_dir.join(&artifact.relative_path));
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full_path, &artifact.bytes)?;
            Ok(full_path)
        });
        match written {
            Ok(path) => report.exported_files.push(path),
            Err(err) => {
                report.skipped += 1;
                report.errors.push(format!(
                    "{}:{}: {err}",
                    asset_ref.file_index, asset_ref.path_id
                ));
            }
        }
    }

    Ok(report)
}

/// Extracts raw `.ab` / `.bundle` / `.unity3d` files directly onto disk via streaming I/O.
pub fn extract_ab_bundles(
    source_path: &Path,
    out_dir: &Path,
    filter: Option<&str>,
) -> ExportResult<ExtractBundlesReport> {
    extract_raw_bundles(source_path, out_dir, filter)
        .map_err(|e| ExportError::Io(std::io::Error::other(e)))
}

/// Decompiles all `Shader` (`class_id == 48`) assets in `catalog` in parallel and writes `.shader` files.
pub fn decompile_shaders_to_dir(
    catalog: &AssetCatalog,
    out_dir: &Path,
) -> ExportResult<(usize, usize, Vec<String>)> {
    std::fs::create_dir_all(out_dir)?;
    let shader_refs: Vec<_> = catalog
        .entries
        .iter()
        .filter(|e| e.summary.class_id == 48)
        .collect();

    struct DecompiledShaderFile {
        file_name: String,
        bytes: Vec<u8>,
        subprograms: usize,
    }

    let results: Vec<Result<DecompiledShaderFile, String>> = shader_refs
        .par_iter()
        .map(|entry| {
            let Some((loaded, info)) = catalog
                .workspace
                .object_info(entry.asset_ref.file_index, entry.asset_ref.path_id)
            else {
                return Err(format!("missing shader object {:?}", entry.asset_ref));
            };
            let obj = read_object(loaded, info).map_err(|e| format!("{}: {e}", entry.summary.name))?;
            let decoded = decode_shader(&obj).map_err(|e| format!("{}: {e}", entry.summary.name))?;
            let subprograms = decoded.total_subprograms;
            let text = if decoded.decompiled_cg_text.is_empty() {
                decoded.shaderlab_text
            } else {
                decoded.decompiled_cg_text
            };
            Ok(DecompiledShaderFile {
                file_name: format!("{}_{}.shader", safe_file_stem(&entry.summary), entry.asset_ref.path_id),
                bytes: text.into_bytes(),
                subprograms,
            })
        })
        .collect();

    let mut written = 0usize;
    let mut subprograms = 0usize;
    let mut errors = Vec::new();
    for res in results {
        match res {
            Ok(file) => {
                let path = unique_output_path(out_dir.join(file.file_name));
                std::fs::write(&path, file.bytes)?;
                written += 1;
                subprograms += file.subprograms;
            }
            Err(err) => errors.push(err),
        }
    }
    Ok((written, subprograms, errors))
}

/// Exports selected catalog assets into a `.unitypackage` archive at `out_path`.
pub fn export_unitypackage_from_catalog(
    catalog: &AssetCatalog,
    assets: &[AssetRef],
    out_path: &Path,
    plugins: &PluginRegistry,
    host: &mut dyn PluginHostContext,
    opts: &ExportAssetOptions,
) -> ExportResult<usize> {
    let mut pkg_entries = Vec::new();
    for &asset_ref in assets {
        let Ok(artifact) = build_export_artifact(catalog, asset_ref, plugins, host, opts) else {
            continue;
        };
        let clean_rel = artifact.relative_path.trim_start_matches('/');
        let asset_path = if clean_rel.starts_with("Assets/") {
            clean_rel.to_owned()
        } else {
            format!("Assets/Exported/{clean_rel}")
        };
        pkg_entries.push(UnityPackageInputEntry {
            asset_path,
            guid: None,
            asset_bytes: artifact.bytes,
            meta_text: None,
        });
    }
    let count = pkg_entries.len();
    write_unitypackage(out_path, &pkg_entries)?;
    Ok(count)
}

fn safe_file_stem(summary: &AssetSummary) -> String {
    let s: String = summary
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        format!("{}_{}", summary.class_name, summary.asset_ref.path_id)
    } else {
        s
    }
}

fn normalize_container_parent(container: &str) -> Option<String> {
    let normalized = container.replace('\\', "/");
    let dir = normalized.rsplit_once('/')?.0.trim_matches('/');
    if dir.is_empty() {
        None
    } else {
        Some(dir.to_owned())
    }
}

fn unique_output_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset")
        .to_owned();
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();

    for idx in 1..10_000 {
        let file_name = if ext.is_empty() {
            format!("{stem}_{idx}")
        } else {
            format!("{stem}_{idx}.{ext}")
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use unity_asset_core::UnityClass;

    #[test]
    fn serializes_unity_class_to_unity_yaml_header() {
        let mut cls = UnityClass::new(1, "GameObject".into(), "100".into());
        cls.set("m_Name".to_owned(), "Player".to_owned());
        cls.set("m_IsActive".to_owned(), 1i64);
        let out = UnityYamlSerializer::new()
            .serialize_to_string(&[cls])
            .expect("yaml ok");
        assert!(out.contains("%YAML"));
        assert!(out.contains("%TAG !u!"));
        assert!(out.contains("Player"));
    }
}
