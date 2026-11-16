//! Parsed Unity workspace representation, PPtr extraction, TypeTree reading,
//! and stream resolution.

use crate::vfs::{ContainerKind, ResourceStreamStore, ScriptArtifact, UnityPackageEntry};
use crate::{IoError, IoResult};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use unity_asset_binary::asset::{ObjectInfo, SerializedFile};
use unity_asset_binary::object::{ObjectHandle, UnityObject};
use unity_asset_binary::typetree::TypeTreeParseOptions;
use unity_asset_core::{UnityValue, constants::GLOBAL_CLASS_ID_MAP};

/// A single opened `.assets` or bundle-embedded `SerializedFile`.
#[derive(Debug)]
pub struct LoadedAssetFile {
    pub file_index: usize,
    pub container_name: Option<String>,
    pub virtual_path: String,
    pub kind: ContainerKind,
    pub file: SerializedFile,
    pub raw_bytes: Option<Arc<[u8]>>,
    /// Maps `path_id` -> asset path inside an `AssetBundle` (`m_Container`).
    pub bundle_container_paths: HashMap<i64, String>,
}

/// Non-SerializedFile loose resource (text, XML, shader, image) found in packages/dirs.
#[derive(Debug, Clone)]
pub struct RawArtifact {
    pub virtual_path: String,
    pub bytes: Arc<[u8]>,
}

/// Indexed but not-yet-parsed `.ab` / `.bundle` / `.unity3d` entry inside a large archive (`.apk`, `.xapk`, `.zip`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredBundleEntry {
    pub archive_path: PathBuf,
    pub entry_name: String,
    pub virtual_path: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub loaded: bool,
}

/// Complete collection of parsed files, external `.resS` streams, script files,
/// and loose artifacts loaded from a path.
#[derive(Debug)]
pub struct UnityWorkspaceData {
    pub root_path: PathBuf,
    pub files: Vec<LoadedAssetFile>,
    pub streams: ResourceStreamStore,
    pub script_artifacts: Vec<ScriptArtifact>,
    pub unitypackage_entries: Vec<UnityPackageEntry>,
    pub raw_artifacts: Vec<RawArtifact>,
    pub deferred_bundles: Vec<DeferredBundleEntry>,
    /// Fast O(1) lookup from lowercased virtual-path basename (e.g. `cab-...`) to `files` index.
    pub(crate) file_basename_index: HashMap<String, usize>,
    /// Lazy cache mapping lowercased internal `CAB-*` names to unloaded `deferred_bundles` indices.
    pub(crate) deferred_cab_index: Option<HashMap<String, usize>>,
}

impl UnityWorkspaceData {
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            root_path,
            files: Vec::new(),
            streams: ResourceStreamStore::default(),
            script_artifacts: Vec::new(),
            unitypackage_entries: Vec::new(),
            raw_artifacts: Vec::new(),
            deferred_bundles: Vec::new(),
            file_basename_index: HashMap::new(),
            deferred_cab_index: None,
        }
    }

    /// Populates `bundle_container_paths` from `AssetBundle` (`class_id == 142`) objects.
    pub fn finalize_indices(&mut self) {
        self.file_basename_index.clear();
        self.finalize_indices_from(0);
    }

    /// Incremental variant of [`Self::finalize_indices`] processing files from `start_idx..`.
    pub fn finalize_indices_from(&mut self, start_idx: usize) {
        for (idx, loaded) in self.files.iter().enumerate().skip(start_idx) {
            let vbase = path_basename_lower(&loaded.virtual_path);
            if !vbase.is_empty() {
                self.file_basename_index.entry(vbase).or_insert(idx);
            }
        }
        let mut cross_file_paths = Vec::new();
        for (src_idx, loaded) in self.files.iter_mut().enumerate().skip(start_idx) {
            let entries_per_obj: Vec<_> = loaded
                .file
                .objects
                .iter()
                .filter(|info| info.type_id == 142)
                .filter_map(|info| loaded.file.assetbundle_container_raw(info).ok())
                .flatten()
                .collect();
            for (path, file_id, path_id) in entries_per_obj {
                if file_id == 0 {
                    loaded.bundle_container_paths.insert(path_id, path);
                } else {
                    cross_file_paths.push((src_idx, file_id, path_id, path));
                }
            }
        }
        for (src_idx, file_id, path_id, path) in cross_file_paths {
            if let Some(target_idx) = self.resolve_external_file(src_idx, file_id)
                && let Some(target_file) = self.files.get_mut(target_idx)
            {
                target_file.bundle_container_paths.insert(path_id, path);
            }
        }
    }

    /// Loads one indexed [`DeferredBundleEntry`] from disk into `self.files`.
    pub fn load_deferred_bundle(&mut self, deferred_idx: usize) -> IoResult<usize> {
        crate::vfs::load_deferred_bundle_indices(self, &[deferred_idx])
    }

    /// Loads all indexed [`DeferredBundleEntry`] items whose `virtual_path` matches `filter`
    /// (case-insensitive substring, or all unloaded bundles when `filter` is `None`).
    pub fn load_deferred_bundles_filtered(&mut self, filter: Option<&str>) -> IoResult<usize> {
        let needle = filter.map(str::trim).filter(|s| !s.is_empty()).map(str::to_ascii_lowercase);
        let indices: Vec<usize> = self
            .deferred_bundles
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                !d.loaded
                    && needle
                        .as_ref()
                        .is_none_or(|n| d.virtual_path.to_ascii_lowercase().contains(n))
            })
            .map(|(idx, _)| idx)
            .collect();
        crate::vfs::load_deferred_bundle_indices(self, &indices)
    }

    /// Loads all indexed [`DeferredBundleEntry`] items matching `filter` AND recursively
    /// resolves their transitive `.ab` / `CAB-*` dependencies.
    pub fn load_deferred_bundles_with_deps(
        &mut self,
        filter: Option<&str>,
    ) -> IoResult<(usize, crate::deps::DependencyResolutionReport)> {
        let needle = filter.map(str::trim).filter(|s| !s.is_empty()).map(str::to_ascii_lowercase);
        let indices: Vec<usize> = self
            .deferred_bundles
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                !d.loaded
                    && needle
                        .as_ref()
                        .is_none_or(|n| d.virtual_path.to_ascii_lowercase().contains(n))
            })
            .map(|(idx, _)| idx)
            .collect();
        crate::deps::load_deferred_bundles_with_dependencies(self, &indices)
    }

    pub fn get_file(&self, file_index: usize) -> Option<&LoadedAssetFile> {
        self.files.get(file_index)
    }

    pub fn object_info(&self, file_index: usize, path_id: i64) -> Option<(&LoadedAssetFile, &ObjectInfo)> {
        let file = self.files.get(file_index)?;
        let info = file.file.find_object(path_id)?;
        Some((file, info))
    }

    /// Resolves `m_FileID` relative to `source_file_index` to a global `file_index`
    /// using `SerializedFile.externals` matched against loaded `virtual_path`s.
    pub fn resolve_external_file(&self, source_file_index: usize, m_file_id: i32) -> Option<usize> {
        if m_file_id == 0 {
            return Some(source_file_index);
        }
        let src = self.files.get(source_file_index)?;
        let ext_idx = usize::try_from(m_file_id.checked_sub(1)?).ok()?;
        let external = src.file.externals.get(ext_idx)?;
        let target_name = path_basename_lower(&external.path);
        let target_fallback = path_basename_lower(&external.temp_empty);

        if !target_name.is_empty()
            && let Some(&idx) = self.file_basename_index.get(&target_name)
        {
            return Some(idx);
        }
        if !target_fallback.is_empty()
            && let Some(&idx) = self.file_basename_index.get(&target_fallback)
        {
            return Some(idx);
        }

        self.files.iter().position(|f| {
            let vbase = path_basename_lower(&f.virtual_path);
            (!target_name.is_empty() && vbase == target_name)
                || (!target_fallback.is_empty() && vbase == target_fallback)
        })
    }

    /// Returns the declared external dependency path for a non-zero `m_FileID` on `source_file_index`.
    pub fn external_file_name(&self, source_file_index: usize, m_file_id: i32) -> Option<&str> {
        if m_file_id <= 0 {
            return None;
        }
        let src = self.files.get(source_file_index)?;
        let ext_idx = usize::try_from(m_file_id - 1).ok()?;
        let external = src.file.externals.get(ext_idx)?;
        if !external.path.is_empty() {
            Some(external.path.as_str())
        } else if !external.temp_empty.is_empty() {
            Some(external.temp_empty.as_str())
        } else {
            None
        }
    }
}

fn path_basename_lower(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .next()
        .unwrap_or(&normalized)
        .to_ascii_lowercase()
}

/// Extracted `PPtr(m_FileID, m_PathID)` link discovered inside an object's TypeTree properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PPtrLink {
    pub field_path: String,
    pub file_id: i32,
    pub path_id: i64,
}

/// Parses an object cleanly into [`UnityObject`] (falling back to raw bytes if needed).
pub fn read_object(loaded: &LoadedAssetFile, info: &ObjectInfo) -> IoResult<UnityObject> {
    let mut obj = ObjectHandle::new(&loaded.file, info)
        .read_with_options(TypeTreeParseOptions::default())
        .map_err(|e| IoError::UnityParse {
            path: loaded.virtual_path.clone(),
            message: e.to_string(),
        })?;
    recover_oversized_typeless_data(&mut obj, info.type_id);
    Ok(obj)
}

/// `unity-asset-binary` caps `TypelessData` arrays at 1,000,000 bytes in lenient mode, dropping
/// inline `"image data"` on large `Texture2D` (`28`) and `Cubemap` (`89`) objects. This helper
/// recovers the exact inline byte slice from `obj.raw_data()` when `m_CompleteImageSize > 1_000_000`.
fn recover_oversized_typeless_data(obj: &mut UnityObject, type_id: i32) {
    if !matches!(type_id, 28 | 89) {
        return;
    }
    let has_inline_bytes = obj
        .class
        .properties()
        .get("image data")
        .or_else(|| obj.class.properties().get("image_data"))
        .is_some_and(|v| match v {
            UnityValue::Bytes(b) => !b.is_empty(),
            UnityValue::Array(a) => !a.is_empty(),
            _ => false,
        });
    if has_inline_bytes {
        return;
    }
    let raw = obj.raw_data();
    if raw.len() <= 1_000_000 || raw.len() < 32 {
        return;
    }
    let expected_len = obj
        .class
        .properties()
        .get("m_CompleteImageSize")
        .and_then(UnityValue::as_i64)
        .unwrap_or(0)
        .max(0) as usize;

    let name_len = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    let start_scan = if name_len <= 256 {
        (4 + name_len + 3) & !3
    } else {
        4
    };
    let end_scan = raw.len().saturating_sub(4).min(512);
    let mut matched_pos: Option<(usize, usize)> = None;
    let mut pos = start_scan;
    while pos + 4 <= end_scan {
        let len = u32::from_le_bytes([raw[pos], raw[pos + 1], raw[pos + 2], raw[pos + 3]]) as usize;
        if len > 1_000_000
            && pos + 4 + len <= raw.len()
            && (expected_len == 0 || len == expected_len)
        {
            matched_pos = Some((pos, len));
        }
        pos += 4;
    }

    if let Some((pos, len)) = matched_pos {
        let payload = raw[pos + 4..pos + 4 + len].to_vec();
        let props = obj.class.properties_mut();
        props.insert("image data".to_owned(), UnityValue::Bytes(payload));
        if let Some(UnityValue::Object(sd)) = props.get_mut("m_StreamData") {
            sd.insert("offset".to_owned(), UnityValue::Integer(0));
            sd.insert("size".to_owned(), UnityValue::Integer(0));
            sd.insert("path".to_owned(), UnityValue::String(String::new()));
        }
    }
}

/// Resolves streamed bytes (`m_StreamData` or `m_Resource`) from [`ResourceStreamStore`]
/// using either the `UnityObject` field path or an external offset/size tuple.
pub fn resolve_object_stream<'a>(
    streams: &'a ResourceStreamStore,
    stream_path: &str,
    offset: u64,
    size: u32,
) -> Option<&'a [u8]> {
    if stream_path.is_empty() || size == 0 {
        return None;
    }
    streams.read_slice(stream_path, offset, size)
}

/// Recursively walks a parsed [`UnityObject`] to collect every non-null `PPtr` reference.
pub fn scan_pptrs(obj: &UnityObject) -> Vec<PPtrLink> {
    let mut out = Vec::new();
    for (k, v) in obj.class.properties() {
        collect_pptrs_in_value(k, v, &mut out);
    }
    out
}

fn collect_pptrs_in_value(prefix: &str, value: &UnityValue, out: &mut Vec<PPtrLink>) {
    match value {
        UnityValue::Object(map) => {
            if let (Some(fid), Some(pid)) = (
                map.get("m_FileID").and_then(UnityValue::as_i64),
                map.get("m_PathID").and_then(UnityValue::as_i64),
            ) {
                if pid != 0 {
                    out.push(PPtrLink {
                        field_path: prefix.to_owned(),
                        file_id: fid as i32,
                        path_id: pid,
                    });
                }
                return;
            }
            for (k, v) in map {
                let next = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                collect_pptrs_in_value(&next, v, out);
            }
        }
        UnityValue::Array(items) => {
            for (idx, v) in items.iter().enumerate() {
                let next = format!("{prefix}[{idx}]");
                collect_pptrs_in_value(&next, v, out);
            }
        }
        _ => {}
    }
}

/// Converts a [`UnityValue`] into a serde `serde_json::Value` (truncating huge byte blobs for inspector display).
pub fn unity_value_to_json(val: &UnityValue, truncate_bytes_at: Option<usize>) -> Value {
    match val {
        UnityValue::Null => Value::Null,
        UnityValue::Bool(b) => Value::Bool(*b),
        UnityValue::Integer(i) => Value::Number(Number::from(*i)),
        UnityValue::Float(f) => Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        UnityValue::String(s) => Value::String(s.clone()),
        UnityValue::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| unity_value_to_json(v, truncate_bytes_at))
                .collect(),
        ),
        UnityValue::Bytes(bytes) => {
            if let Some(limit) = truncate_bytes_at
                && bytes.len() > limit
            {
                Value::String(format!("<{} bytes>", bytes.len()))
            } else {
                Value::Array(bytes.iter().map(|&b| Value::Number(Number::from(b))).collect())
            }
        }
        UnityValue::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                out.insert(k.clone(), unity_value_to_json(v, truncate_bytes_at));
            }
        Value::Object(out)
        }
    }
}

/// Extracts byte arrays stored either as `UnityValue::Bytes` or `UnityValue::Array<Integer>`.
pub fn extract_bytes_field(val: &UnityValue) -> Option<Vec<u8>> {
    match val {
        UnityValue::Bytes(b) => Some(b.clone()),
        UnityValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let n = item.as_i64()?;
                out.push(u8::try_from(n).ok()?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Extracts an `i32` from a [`UnityValue::Integer`].
pub fn extract_i32_field(val: &UnityValue) -> Option<i32> {
    val.as_i64().and_then(|n| i32::try_from(n).ok())
}

/// Extracts an `i64` from a [`UnityValue::Integer`].
pub fn extract_i64_field(val: &UnityValue) -> Option<i64> {
    val.as_i64()
}

/// Returns human-readable Unity class name for a numeric `ClassID`.
///
/// Queries `rabex`'s full `ClassId` table (covering Unity 3.x–6000.x classes such as
/// `SkinnedMeshRenderer`, `RectTransform`, `Canvas`, `ParticleSystem`, `LODGroup`,
/// `SpriteAtlas`, etc.), then falls back to `unity-asset-core` and a supplemental table.
pub fn class_name_for(class_id: i32) -> String {
    if let Some(name) = rabex::objects::ClassId(class_id).name() {
        return name.to_owned();
    }
    if let Some(name) = GLOBAL_CLASS_ID_MAP.get_class_name_str(class_id) {
        return name.to_owned();
    }
    supplemental_class_name(class_id)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Class_{class_id}"))
}

fn supplemental_class_name(class_id: i32) -> Option<&'static str> {
    match class_id {
        19 => Some("Physics2DSettings"),
        30 => Some("GraphicsSettings"),
        47 => Some("QualitySettings"),
        55 => Some("PhysicsManager"),
        65 => Some("BoxCollider"),
        72 => Some("ComputeShader"),
        78 => Some("TagManager"),
        81 => Some("AudioListener"),
        94 => Some("ShaderNameRegistry"),
        98 => Some("DelayedCallManager"),
        104 => Some("RenderSettings"),
        116 => Some("MonoManager"),
        126 => Some("NavMeshProjectSettings"),
        129 => Some("PlayerSettings"),
        136 => Some("CapsuleCollider"),
        137 => Some("SkinnedMeshRenderer"),
        141 => Some("BuildSettings"),
        147 => Some("ResourceManager"),
        150 => Some("PreloadData"),
        157 => Some("LightmapSettings"),
        196 => Some("NavMeshSettings"),
        198 => Some("ParticleSystem"),
        199 => Some("ParticleSystemRenderer"),
        205 => Some("LODGroup"),
        222 => Some("CanvasRenderer"),
        223 => Some("Canvas"),
        224 => Some("RectTransform"),
        258 => Some("LightProbes"),
        290 => Some("AssetBundleManifest"),
        300 => Some("RuntimeInitializeOnLoadManager"),
        687078895 => Some("SpriteAtlas"),
        850595691 => Some("LightingSettings"),
        937362698 => Some("ShaderVariantCollection"),
        1403656975 => Some("StreamingController"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_store_normalizes_archive_prefixes() {
        let mut store = ResourceStreamStore::default();
        store.insert("level0.resS", Arc::from(&b"HELLO_WORLD"[..]));
        assert_eq!(
            store.read_slice("archive:/CAB-1234abcd/level0.resS", 6, 5),
            Some(&b"WORLD"[..])
        );
    }

    #[test]
    fn resolves_official_unity_class_names() {
        assert_eq!(class_name_for(137), "SkinnedMeshRenderer");
        assert_eq!(class_name_for(224), "RectTransform");
        assert_eq!(class_name_for(223), "Canvas");
        assert_eq!(class_name_for(198), "ParticleSystem");
        assert_eq!(class_name_for(205), "LODGroup");
        assert_eq!(class_name_for(687078895), "SpriteAtlas");
    }

    #[test]
    fn tpk_registry_resolves_standard_texture2d() {
        let reg = crate::shared_tpk_registry();
        let tt = reg
            .resolve("2022.3.12f1", 28)
            .expect("Texture2D typetree should exist in embedded TPK");
        assert!(!tt.is_empty());
        assert_eq!(tt.nodes[0].type_name, "Texture2D");
    }
}
