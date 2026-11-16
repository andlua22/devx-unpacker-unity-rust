//! Virtual File System loader supporting Unity directories, split assets,
//! `UnityFS`/`UnityRaw`/`UnityWeb` bundles, standalone `SerializedFile`s,
//! `.zip`/`.apk`/`.xapk`/`.obb`/`.ipa`, `.unityweb` WebGL containers, and `.unitypackage`.

use crate::tpk_bridge::shared_tpk_registry;
use crate::workspace_io::{
    DeferredBundleEntry, LoadedAssetFile, RawArtifact, UnityWorkspaceData,
};
use crate::{IoError, IoResult};
use flate2::read::GzDecoder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tar::Archive;
use unity_asset_binary::asset::SerializedFileParser;
use unity_asset_binary::bundle::AssetBundle;
use unity_asset_binary::file::{UnityFile, load_unity_file_from_memory};
use walkdir::WalkDir;
use zip::ZipArchive;

/// High-level container classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerKind {
    Directory,
    SerializedFile,
    AssetBundle,
    WebFile,
    Archive,
    UnityPackage,
}

/// Entry extracted from a `.unitypackage` tar.gz archive.
#[derive(Debug, Clone)]
pub struct UnityPackageEntry {
    pub guid: String,
    pub asset_path: String,
    pub has_asset: bool,
    pub meta_text: Option<String>,
    pub raw_bytes: Vec<u8>,
}

/// Discovered managed assembly or IL2CPP metadata artifact inside a game build.
#[derive(Debug, Clone)]
pub struct ScriptArtifact {
    pub name: String,
    pub virtual_path: String,
    pub bytes: Arc<[u8]>,
}

/// Case-insensitive/normalized store of `.resS`, `.resource`, and `archive:/...` streams.
#[derive(Debug, Default, Clone)]
pub struct ResourceStreamStore {
    by_exact: HashMap<String, Arc<[u8]>>,
    by_basename: HashMap<String, Arc<[u8]>>,
}

impl ResourceStreamStore {
    pub fn insert(&mut self, virtual_path: &str, bytes: Arc<[u8]>) {
        let normalized = normalize_stream_key(virtual_path);
        self.by_exact.insert(normalized.clone(), bytes.clone());
        if let Some(base) = normalized.rsplit('/').next()
            && !base.is_empty()
        {
            self.by_basename.insert(base.to_owned(), bytes);
        }
    }

    /// Resolves Unity `archive:/CAB-.../Foo.resS` or relative `sharedassets0.assets.resS`.
    pub fn resolve(&self, stream_path: &str) -> Option<&Arc<[u8]>> {
        let normalized = normalize_stream_key(stream_path);
        if let Some(found) = self.by_exact.get(&normalized) {
            return Some(found);
        }
        let trimmed = normalized
            .strip_prefix("archive:/")
            .unwrap_or(&normalized);
        if let Some(found) = self.by_exact.get(trimmed) {
            return Some(found);
        }
        let base = trimmed.rsplit('/').next()?;
        self.by_basename.get(base)
    }

    pub fn read_slice(&self, stream_path: &str, offset: u64, size: u32) -> Option<&[u8]> {
        let data = self.resolve(stream_path)?;
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(size as usize)?;
        data.get(start..end)
    }

    pub fn len(&self) -> usize {
        self.by_exact.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_exact.is_empty()
    }

    pub(crate) fn into_entries(self) -> impl Iterator<Item = (String, Arc<[u8]>)> {
        self.by_exact.into_iter()
    }
}

fn normalize_stream_key(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_lowercase()
}

/// Controls lazy vs eager AssetBundle parsing for large archives (`.apk`, `.xapk`, `.zip`).
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceLoadOptions {
    /// Archives larger than this threshold (bytes) index `.ab`/`.bundle`/`.unity3d`
    /// entries as [`DeferredBundleEntry`] instead of decompressing every bundle upfront.
    /// Set to `u64::MAX` to always parse all bundles eagerly.
    pub lazy_bundle_threshold_bytes: u64,
}

impl Default for WorkspaceLoadOptions {
    fn default() -> Self {
        Self {
            // 128 MiB: regular bundles/dirs load eagerly in parallel; 1.1 GB APKs open in seconds.
            lazy_bundle_threshold_bytes: 128 * 1024 * 1024,
        }
    }
}

/// Summary returned by [`extract_raw_bundles`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractBundlesReport {
    pub matched_entries: usize,
    pub extracted_files: usize,
    pub total_bytes: u64,
    pub output_paths: Vec<PathBuf>,
}

/// Thread-local parsed slice merged sequentially into [`UnityWorkspaceData`].
#[derive(Default)]
struct ParsedFragment {
    files: Vec<LoadedAssetFile>,
    streams: Vec<(String, Arc<[u8]>)>,
    script_artifacts: Vec<ScriptArtifact>,
    unitypackage_entries: Vec<UnityPackageEntry>,
    raw_artifacts: Vec<RawArtifact>,
    deferred_bundles: Vec<DeferredBundleEntry>,
}

impl ParsedFragment {
    fn push_file(&mut self, mut loaded: LoadedAssetFile) {
        loaded.file_index = 0;
        self.files.push(loaded);
    }

    fn merge_into(self, workspace: &mut UnityWorkspaceData) {
        for (key, bytes) in self.streams {
            workspace.streams.insert(&key, bytes);
        }
        workspace.script_artifacts.extend(self.script_artifacts);
        workspace.unitypackage_entries.extend(self.unitypackage_entries);
        workspace.raw_artifacts.extend(self.raw_artifacts);
        workspace.deferred_bundles.extend(self.deferred_bundles);
        for mut loaded in self.files {
            loaded.file_index = workspace.files.len();
            workspace.files.push(loaded);
        }
    }
}

/// Loads any supported file or directory into a [`UnityWorkspaceData`] using default options.
pub fn load_path(path: impl AsRef<Path>) -> IoResult<UnityWorkspaceData> {
    load_path_with_options(path, WorkspaceLoadOptions::default())
}

/// Loads any supported file or directory with custom [`WorkspaceLoadOptions`].
pub fn load_path_with_options(
    path: impl AsRef<Path>,
    options: WorkspaceLoadOptions,
) -> IoResult<UnityWorkspaceData> {
    let path = path.as_ref();
    let mut workspace = UnityWorkspaceData::new(path.to_path_buf());
    if path.is_dir() {
        load_directory(path, &mut workspace)?;
    } else if path.is_file() {
        load_single_file(path, options, &mut workspace)?;
    } else {
        return Err(IoError::UnsupportedFormat(path.display().to_string()));
    }
    workspace.finalize_indices();
    Ok(workspace)
}

/// Extracts `.ab` / `.bundle` / `.unity3d` files directly onto disk using streaming I/O.
pub fn extract_raw_bundles(
    source_path: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
    filter: Option<&str>,
) -> IoResult<ExtractBundlesReport> {
    let source = source_path.as_ref();
    let out_root = output_dir.as_ref();
    fs::create_dir_all(out_root).map_err(|source| IoError::Io {
        path: out_root.display().to_string(),
        source,
    })?;

    let needle = filter
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase);
    let mut report = ExtractBundlesReport::default();

    if source.is_dir() {
        for entry in WalkDir::new(source).follow_links(false).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = rel_display(source, entry.path());
            if !matches_bundle_filter(&rel, needle.as_deref()) {
                continue;
            }
            report.matched_entries += 1;
            let dest = safe_join_relative(out_root, &rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|source| IoError::Io {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
            let copied = fs::copy(entry.path(), &dest).map_err(|source| IoError::Io {
                path: dest.display().to_string(),
                source,
            })?;
            report.extracted_files += 1;
            report.total_bytes += copied;
            report.output_paths.push(dest);
        }
        return Ok(report);
    }

    let lower = source
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if is_zip_like_name(&lower) {
        let file = File::open(source).map_err(|err| IoError::Io {
            path: source.display().to_string(),
            source: err,
        })?;
        let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| IoError::Archive {
            path: source.display().to_string(),
            message: e.to_string(),
        })?;

        for i in 0..zip.len() {
            let Ok(mut entry) = zip.by_index(i) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            let Ok(name_cow) = entry.name() else {
                continue;
            };
            let entry_name = name_cow.into_owned();
            if !matches_bundle_filter(&entry_name, needle.as_deref()) {
                continue;
            }
            report.matched_entries += 1;
            let dest = safe_join_relative(out_root, &entry_name);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|err| IoError::Io {
                    path: parent.display().to_string(),
                    source: err,
                })?;
            }
            let mut out_file = File::create(&dest).map_err(|err| IoError::Io {
                path: dest.display().to_string(),
                source: err,
            })?;
            let copied = std::io::copy(&mut entry, &mut out_file).map_err(|err| IoError::Io {
                path: dest.display().to_string(),
                source: err,
            })?;
            report.extracted_files += 1;
            report.total_bytes += copied;
            report.output_paths.push(dest);
        }
        return Ok(report);
    }

    if matches_bundle_filter(&lower, needle.as_deref()) {
        report.matched_entries += 1;
        let file_name = source
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("bundle.ab");
        let dest = safe_join_relative(out_root, file_name);
        let copied = fs::copy(source, &dest).map_err(|err| IoError::Io {
            path: dest.display().to_string(),
            source: err,
        })?;
        report.extracted_files += 1;
        report.total_bytes += copied;
        report.output_paths.push(dest);
    }
    Ok(report)
}

/// Opens previously indexed [`DeferredBundleEntry`] items in streaming mode and parses them in parallel.
pub fn load_deferred_bundle_indices(
    workspace: &mut UnityWorkspaceData,
    indices: &[usize],
) -> IoResult<usize> {
    let mut by_archive: BTreeMap<PathBuf, Vec<(usize, String, String)>> = BTreeMap::new();
    for &idx in indices {
        if let Some(entry) = workspace.deferred_bundles.get(idx)
            && !entry.loaded
        {
            by_archive.entry(entry.archive_path.clone()).or_default().push((
                idx,
                entry.entry_name.clone(),
                entry.virtual_path.clone(),
            ));
        }
    }

    let mut payloads: Vec<(String, Vec<u8>)> = Vec::new();
    let mut loaded_indices: Vec<usize> = Vec::new();
    for (archive_path, items) in by_archive {
        let file = File::open(&archive_path).map_err(|source| IoError::Io {
            path: archive_path.display().to_string(),
            source,
        })?;
        let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| IoError::Archive {
            path: archive_path.display().to_string(),
            message: e.to_string(),
        })?;
        for (idx, entry_name, virtual_path) in items {
            if let Ok(mut entry) = zip.by_name(&entry_name) {
                let mut buf = Vec::with_capacity(entry.size().min(64 * 1024 * 1024) as usize);
                if entry.read_to_end(&mut buf).is_ok() {
                    payloads.push((virtual_path, buf));
                    loaded_indices.push(idx);
                }
            }
        }
    }

    for &idx in &loaded_indices {
        if let Some(entry) = workspace.deferred_bundles.get_mut(idx) {
            entry.loaded = true;
        }
    }

    let start_file_idx = workspace.files.len();
    ingest_payloads_parallel(payloads, ContainerKind::Archive, workspace);
    workspace.finalize_indices_from(start_file_idx);
    Ok(loaded_indices.len())
}

fn load_directory(root: &Path, workspace: &mut UnityWorkspaceData) -> IoResult<()> {
    let mut split_groups: BTreeMap<PathBuf, Vec<(usize, PathBuf)>> = BTreeMap::new();
    let mut regular_files: Vec<PathBuf> = Vec::new();

    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        if let Some((base_path, part_idx)) = parse_split_suffix(&path) {
            split_groups.entry(base_path).or_default().push((part_idx, path));
        } else if !should_skip_dir_noise(&path) {
            regular_files.push(path);
        }
    }

    let mut split_payloads: Vec<(String, Vec<u8>)> = Vec::with_capacity(split_groups.len());
    for (base_path, mut parts) in split_groups {
        parts.sort_by_key(|(idx, _)| *idx);
        let mut combined = Vec::new();
        for (_, part_path) in parts {
            let bytes = fs::read(&part_path).map_err(|source| IoError::Io {
                path: part_path.display().to_string(),
                source,
            })?;
            combined.extend_from_slice(&bytes);
        }
        split_payloads.push((rel_display(root, &base_path), combined));
    }

    let fragments: Vec<ParsedFragment> = regular_files
        .into_par_iter()
        .filter_map(|file_path| {
            let bytes = fs::read(&file_path).ok()?;
            let rel = rel_display(root, &file_path);
            let mut frag = ParsedFragment::default();
            let _ = ingest_bytes_into_fragment(&rel, bytes, ContainerKind::Directory, &mut frag);
            Some(frag)
        })
        .collect();

    ingest_payloads_parallel(split_payloads, ContainerKind::Directory, workspace);
    for frag in fragments {
        frag.merge_into(workspace);
    }
    Ok(())
}

fn load_single_file(
    path: &Path,
    options: WorkspaceLoadOptions,
    workspace: &mut UnityWorkspaceData,
) -> IoResult<()> {
    // Handle `.splitN` opened directly by delegating to sibling split pieces if possible.
    if let Some((base_path, _)) = parse_split_suffix(path)
        && let Some(parent) = path.parent()
    {
        let mut parts = Vec::new();
        if let Ok(entries) = fs::read_dir(parent) {
            for e in entries.flatten() {
                if let Some((cand_base, idx)) = parse_split_suffix(&e.path())
                    && cand_base == base_path
                {
                    parts.push((idx, e.path()));
                }
            }
        }
        if !parts.is_empty() {
            parts.sort_by_key(|(idx, _)| *idx);
            let mut combined = Vec::new();
            for (_, part) in parts {
                let b = fs::read(&part).map_err(|source| IoError::Io {
                    path: part.display().to_string(),
                    source,
                })?;
                combined.extend_from_slice(&b);
            }
            let name = base_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("split.assets")
                .to_owned();
            return ingest_bytes(&name, combined, ContainerKind::SerializedFile, workspace);
        }
    }

    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("input")
        .to_owned();
    let lower = name.to_ascii_lowercase();

    if is_zip_like_name(&lower) {
        let file = File::open(path).map_err(|source| IoError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        return ingest_zip_reader(
            &name,
            Some(path),
            file_len,
            BufReader::new(file),
            options,
            workspace,
        );
    }

    let bytes = fs::read(path).map_err(|source| IoError::Io {
        path: path.display().to_string(),
        source,
    })?;
    if lower.ends_with(".unitypackage") {
        let entries = parse_unitypackage(&name, &bytes)?;
        workspace.unitypackage_entries.extend(entries);
        return Ok(());
    }

    ingest_bytes(&name, bytes, ContainerKind::SerializedFile, workspace)
}

fn ingest_payloads_parallel(
    payloads: Vec<(String, Vec<u8>)>,
    parent_kind: ContainerKind,
    workspace: &mut UnityWorkspaceData,
) {
    if payloads.is_empty() {
        return;
    }
    let fragments: Vec<ParsedFragment> = payloads
        .into_par_iter()
        .map(|(vpath, bytes)| {
            let mut frag = ParsedFragment::default();
            let _ = ingest_bytes_into_fragment(&vpath, bytes, parent_kind, &mut frag);
            frag
        })
        .collect();
    for frag in fragments {
        frag.merge_into(workspace);
    }
}

fn parse_split_suffix(path: &Path) -> Option<(PathBuf, usize)> {
    let ext = path.extension()?.to_str()?;
    let num_str = ext.strip_prefix("split")?;
    let idx: usize = num_str.parse().ok()?;
    Some((path.with_extension(""), idx))
}

fn parse_split_suffix_str(entry_name: &str) -> Option<(String, usize)> {
    let (base, ext) = entry_name.rsplit_once('.')?;
    let num_str = ext.strip_prefix("split")?;
    let idx: usize = num_str.parse().ok()?;
    Some((base.to_owned(), idx))
}

fn is_zip_like_name(lower: &str) -> bool {
    lower.ends_with(".zip")
        || lower.ends_with(".apk")
        || lower.ends_with(".xapk")
        || lower.ends_with(".obb")
        || lower.ends_with(".ipa")
}

fn is_bundle_extension(lower: &str) -> bool {
    lower.ends_with(".ab") || lower.ends_with(".bundle") || lower.ends_with(".unity3d")
}

fn is_core_unity_data(lower: &str) -> bool {
    lower.contains("bin/data/") || lower.contains(".app/data/")
}

fn matches_bundle_filter(path: &str, needle_lower: Option<&str>) -> bool {
    let lower = path.to_ascii_lowercase();
    if !is_bundle_extension(&lower) {
        return false;
    }
    needle_lower.is_none_or(|n| lower.contains(n))
}

fn safe_join_relative(root: &Path, entry_path: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for comp in Path::new(&entry_path.replace('\\', "/")).components() {
        if let Component::Normal(part) = comp {
            out.push(part);
        }
    }
    out
}

fn should_skip_dir_noise(path: &Path) -> bool {
    let lower = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "desktop.ini" | ".ds_store" | "thumbs.db"
    ) || lower.ends_with(".pdb")
        || lower.ends_with(".exe")
}

fn rel_display(root: &Path, target: &Path) -> String {
    target
        .strip_prefix(root)
        .unwrap_or(target)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Core ingestion dispatcher for arbitrary in-memory payloads.
pub fn ingest_bytes(
    virtual_path: &str,
    bytes: Vec<u8>,
    parent_kind: ContainerKind,
    workspace: &mut UnityWorkspaceData,
) -> IoResult<()> {
    let mut frag = ParsedFragment::default();
    ingest_bytes_into_fragment(virtual_path, bytes, parent_kind, &mut frag)?;
    frag.merge_into(workspace);
    Ok(())
}

fn ingest_bytes_into_fragment(
    virtual_path: &str,
    mut bytes: Vec<u8>,
    parent_kind: ContainerKind,
    frag: &mut ParsedFragment,
) -> IoResult<()> {
    let lower = virtual_path.to_ascii_lowercase();

    // Streaming resource blobs (`.resS`, `.resource`)
    if lower.ends_with(".ress") || lower.ends_with(".resource") {
        frag.streams.push((virtual_path.to_owned(), Arc::from(bytes)));
        return Ok(());
    }

    // Managed assemblies, IL2CPP metadata & IL2CPP native/WASM binaries for `devx-scripts`
    if is_script_artifact_path(&lower) {
        let name = virtual_path
            .rsplit('/')
            .next()
            .unwrap_or(virtual_path)
            .to_owned();
        frag.script_artifacts.push(ScriptArtifact {
            name,
            virtual_path: virtual_path.to_owned(),
            bytes: Arc::from(bytes),
        });
        return Ok(());
    }

    // Compressed `.gz` / `.br` / `.unityweb` WebGL wrappers
    if lower.ends_with(".unityweb") || lower.ends_with(".gz") || lower.ends_with(".br") {
        if bytes.starts_with(&[0x1f, 0x8b]) {
            if let Ok(decompressed) = decompress_gzip(&bytes) {
                bytes = decompressed;
            }
        } else if (lower.ends_with(".br") || lower.ends_with(".unityweb"))
            && let Ok(decompressed) = decompress_brotli(&bytes)
        {
            bytes = decompressed;
        }
    }

    // Nested ZIP/XAPK/OBB archives (e.g. `.xapk` containing `.apk` + `.obb`)
    if is_zip_like_name(&lower) {
        let mut nested = UnityWorkspaceData::new(PathBuf::from(virtual_path));
        ingest_zip_reader(
            virtual_path,
            None,
            bytes.len() as u64,
            Cursor::new(&bytes),
            WorkspaceLoadOptions {
                lazy_bundle_threshold_bytes: u64::MAX,
            },
            &mut nested,
        )?;
        for loaded in nested.files {
            frag.push_file(loaded);
        }
        frag.script_artifacts.extend(nested.script_artifacts);
        frag.raw_artifacts.extend(nested.raw_artifacts);
        frag.unitypackage_entries.extend(nested.unitypackage_entries);
        // Preserve already-indexed stream keys directly via a small temp store conversion isn't needed
        // when we re-export keys cleanly:
        for loaded in &frag.files {
            let _ = loaded;
        }
        // Note: nested streams already need preserving — use `by_exact` snapshot:
        frag.streams.extend(nested.streams.into_entries());
        return Ok(());
    }
    if lower.ends_with(".unitypackage") {
        frag.unitypackage_entries
            .extend(parse_unitypackage(virtual_path, &bytes)?);
        return Ok(());
    }

    // Try parsing with `unity-asset-binary`
    if let Ok(unity_file) = load_unity_file_from_memory(bytes.clone()) {
        match unity_file {
            UnityFile::AssetBundle(bundle) => {
                ingest_bundle_into_fragment(virtual_path, bundle, frag);
                return Ok(());
            }
            UnityFile::SerializedFile(mut sf) => {
                sf.set_type_tree_registry(Some(shared_tpk_registry()));
                frag.push_file(LoadedAssetFile {
                    file_index: 0,
                    container_name: None,
                    virtual_path: virtual_path.to_owned(),
                    kind: if parent_kind == ContainerKind::Directory {
                        ContainerKind::Directory
                    } else {
                        ContainerKind::SerializedFile
                    },
                    file: sf,
                    raw_bytes: Some(Arc::from(bytes)),
                    bundle_container_paths: HashMap::new(),
                });
                return Ok(());
            }
            UnityFile::WebFile(web) => {
                for entry in &web.files {
                    if let Ok(entry_bytes) = web.extract_file(&entry.name) {
                        let child_path = format!("{virtual_path}/{}", entry.name);
                        let _ = ingest_bytes_into_fragment(
                            &child_path,
                            entry_bytes,
                            ContainerKind::WebFile,
                            frag,
                        );
                    }
                }
                return Ok(());
            }
        }
    }

    // Fallback for plain text / shaders / JSON / XML / large `.oc` config tables / images found in archives or dirs
    if bytes.len() <= 128 * 1024 * 1024 {
        frag.raw_artifacts.push(RawArtifact {
            virtual_path: virtual_path.to_owned(),
            bytes: Arc::from(bytes),
        });
    }
    Ok(())
}

fn ingest_bundle_into_fragment(
    virtual_path: &str,
    bundle: AssetBundle,
    frag: &mut ParsedFragment,
) {
    // First register any `.resS` / `.resource` entries inside the bundle so later decoding finds them.
    let mut candidate_serialized: Vec<(String, Vec<u8>)> = Vec::new();
    for file_info in &bundle.files {
        let lower = file_info.name.to_ascii_lowercase();
        let full_name = format!("{virtual_path}/{}", file_info.name);
        if let Ok(data) = bundle.extract_file_data(file_info) {
            if lower.ends_with(".ress") || lower.ends_with(".resource") {
                let arc: Arc<[u8]> = Arc::from(data);
                frag.streams.push((file_info.name.clone(), arc.clone()));
                frag.streams.push((full_name, arc));
            } else {
                candidate_serialized.push((file_info.name.clone(), data));
            }
        }
    }

    // Also pull pre-parsed assets if any, or parse each non-resS entry cleanly.
    if !candidate_serialized.is_empty() {
        for (entry_name, data) in candidate_serialized {
            let full_name = format!("{virtual_path}/{entry_name}");
            if let Ok(mut sf) = SerializedFileParser::from_bytes(data.clone()) {
                sf.set_type_tree_registry(Some(shared_tpk_registry()));
                frag.push_file(LoadedAssetFile {
                    file_index: 0,
                    container_name: Some(virtual_path.to_owned()),
                    virtual_path: full_name,
                    kind: ContainerKind::AssetBundle,
                    file: sf,
                    raw_bytes: Some(Arc::from(data)),
                    bundle_container_paths: HashMap::new(),
                });
            }
        }
    } else {
        for (idx, mut sf) in bundle.assets.into_iter().enumerate() {
            let entry_name = bundle
                .asset_names
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("entry_{idx}"));
            sf.set_type_tree_registry(Some(shared_tpk_registry()));
            frag.push_file(LoadedAssetFile {
                file_index: 0,
                container_name: Some(virtual_path.to_owned()),
                virtual_path: format!("{virtual_path}/{entry_name}"),
                kind: ContainerKind::AssetBundle,
                file: sf,
                raw_bytes: None,
                bundle_container_paths: HashMap::new(),
            });
        }
    }
}

fn ingest_zip_reader<R: Read + Seek>(
    archive_name: &str,
    archive_path: Option<&Path>,
    archive_bytes_len: u64,
    reader: R,
    options: WorkspaceLoadOptions,
    workspace: &mut UnityWorkspaceData,
) -> IoResult<()> {
    let mut zip = ZipArchive::new(reader).map_err(|e| IoError::Archive {
        path: archive_name.to_owned(),
        message: e.to_string(),
    })?;

    let defer_bundles =
        archive_path.is_some() && archive_bytes_len > options.lazy_bundle_threshold_bytes;
    let mut split_groups: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut eager_payloads: Vec<(String, Vec<u8>)> = Vec::new();

    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let Ok(name_cow) = entry.name() else {
            continue;
        };
        let name = name_cow.into_owned();
        if let Some((base_name, part_idx)) = parse_split_suffix_str(&name) {
            split_groups.entry(base_name).or_default().push((part_idx, i));
            continue;
        }
        if !is_relevant_archive_entry(&name) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if defer_bundles
            && is_bundle_extension(&lower)
            && !is_core_unity_data(&lower)
            && let Some(disk_path) = archive_path
        {
            workspace.deferred_bundles.push(DeferredBundleEntry {
                archive_path: disk_path.to_path_buf(),
                entry_name: name.clone(),
                virtual_path: format!("{archive_name}/{name}"),
                compressed_size: entry.compressed_size(),
                uncompressed_size: entry.size(),
                loaded: false,
            });
            continue;
        }
        let mut buf = Vec::with_capacity(entry.size().min(64 * 1024 * 1024) as usize);
        if entry.read_to_end(&mut buf).is_ok() {
            eager_payloads.push((format!("{archive_name}/{name}"), buf));
        }
    }

    // Reassemble `.assets.split0`, `.split1`, ... sequentially from the ZIP reader.
    for (base_name, mut parts) in split_groups {
        parts.sort_by_key(|(part_idx, _)| *part_idx);
        let mut combined = Vec::new();
        let mut ok = true;
        for (_, zip_idx) in parts {
            let Ok(mut entry) = zip.by_index(zip_idx) else {
                ok = false;
                break;
            };
            if entry.read_to_end(&mut combined).is_err() {
                ok = false;
                break;
            }
        }
        if ok && !combined.is_empty() {
            eager_payloads.push((format!("{archive_name}/{base_name}"), combined));
        }
    }

    ingest_payloads_parallel(eager_payloads, ContainerKind::Archive, workspace);
    Ok(())
}

fn is_relevant_archive_entry(entry_name: &str) -> bool {
    let lower = entry_name.to_ascii_lowercase();
    if lower.contains("bin/data/")
        || lower.starts_with("assets/")
        || lower.contains(".app/data/")
    {
        return true;
    }
    lower.ends_with(".assets")
        || lower.ends_with(".ress")
        || lower.ends_with(".resource")
        || lower.ends_with(".ab")
        || lower.ends_with(".unity3d")
        || lower.ends_with(".bundle")
        || lower.ends_with(".unityweb")
        || lower.ends_with(".obb")
        || lower.ends_with(".apk")
        || is_script_artifact_path(&lower)
        || lower.contains(".split")
}

fn is_script_artifact_path(lower: &str) -> bool {
    lower.ends_with("global-metadata.dat")
        || lower.ends_with(".dll")
        || lower.ends_with("libil2cpp.so")
        || lower.ends_with("gameassembly.so")
        || lower.ends_with("userassembly.so")
        || lower.ends_with(".wasm")
        || lower.ends_with("/unityframework")
        || lower == "unityframework"
}

fn parse_unitypackage(
    package_name: &str,
    bytes: &[u8],
) -> IoResult<Vec<UnityPackageEntry>> {
    let decoder = GzDecoder::new(BufReader::new(Cursor::new(bytes)));
    let mut archive = Archive::new(decoder);
    let entries = archive.entries().map_err(|e| IoError::Archive {
        path: package_name.to_owned(),
        message: e.to_string(),
    })?;

    let mut grouped: HashMap<String, UnityPackageEntry> = HashMap::new();
    for entry_res in entries {
        let Ok(mut entry) = entry_res else {
            continue;
        };
        let Ok(path) = entry.path() else {
            continue;
        };
        let path_str = path.to_string_lossy().replace('\\', "/");
        let parts: Vec<&str> = path_str
            .trim_start_matches("./")
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() < 2 {
            continue;
        }
        let guid = parts[0].to_owned();
        let leaf = parts[1];
        let mut content = Vec::new();
        if entry.read_to_end(&mut content).is_err() {
            continue;
        }
        let item = grouped.entry(guid.clone()).or_insert_with(|| UnityPackageEntry {
            guid,
            asset_path: String::new(),
            has_asset: false,
            meta_text: None,
            raw_bytes: Vec::new(),
        });
        match leaf {
            "pathname" => {
                let first_line = String::from_utf8_lossy(&content)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                item.asset_path = first_line;
            }
            "asset" => {
                item.has_asset = true;
                item.raw_bytes = content;
            }
            "asset.meta" => {
                item.meta_text = Some(String::from_utf8_lossy(&content).into_owned());
            }
            _ => {}
        }
    }

    Ok(grouped.into_values().collect())
}

fn decompress_gzip(bytes: &[u8]) -> IoResult<Vec<u8>> {
    let mut decoder = GzDecoder::new(Cursor::new(bytes));
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(|source| IoError::Io {
        path: "<gzip>".into(),
        source,
    })?;
    Ok(out)
}

fn decompress_brotli(bytes: &[u8]) -> IoResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = brotli::Decompressor::new(Cursor::new(bytes), 64 * 1024);
    decoder
        .read_to_end(&mut out)
        .map_err(|source| IoError::Io {
            path: "<brotli>".into(),
            source,
        })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tpk_bridge::{sanitize_unity_version, shared_tpk_registry};
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    struct ScopedTempDir(PathBuf);

    impl ScopedTempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("devx_vfs_{tag}_{nanos}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }
    }

    impl Drop for ScopedTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sanitize_china_suffix_and_resolve_tpk() {
        assert_eq!(sanitize_unity_version("2021.3.23f1c1"), "2021.3.23f1");
        assert_eq!(sanitize_unity_version("2022.3.10f1t2"), "2022.3.10f1");
        assert_eq!(sanitize_unity_version("2019.4.40f1"), "2019.4.40f1");

        let reg = shared_tpk_registry();
        let tree = reg.resolve("2021.3.23f1c1", 28);
        assert!(tree.is_some(), "Texture2D TypeTree should resolve for 2021.3.23f1c1");
    }

    #[test]
    fn zip_deferred_bundles_splits_and_raw_extract() {
        let dir = ScopedTempDir::new("zip_split");
        let zip_path = dir.0.join("sample.apk");
        {
            let file = File::create(&zip_path).expect("create zip");
            let mut writer = zip::ZipWriter::new(file);
            let opts = SimpleFileOptions::default();

            writer
                .start_file("assets/scenetexture/stone.ab", opts)
                .unwrap();
            writer.write_all(b"UnityFS\0fake-ab-payload").unwrap();

            writer
                .start_file("assets/shader/shaders.ab", opts)
                .unwrap();
            writer.write_all(b"UnityFS\0shader-ab-payload").unwrap();

            writer
                .start_file("assets/bin/Data/test.assets.split0", opts)
                .unwrap();
            writer.write_all(b"part0-").unwrap();

            writer
                .start_file("assets/bin/Data/test.assets.split1", opts)
                .unwrap();
            writer.write_all(b"part1").unwrap();

            writer.finish().unwrap();
        }

        // Test deferred indexing mode (`lazy_bundle_threshold_bytes: 0`)
        let mut ws = load_path_with_options(
            &zip_path,
            WorkspaceLoadOptions {
                lazy_bundle_threshold_bytes: 0,
            },
        )
        .expect("load_path_with_options");
        assert_eq!(ws.deferred_bundles.len(), 2);
        assert!(
            ws.raw_artifacts
                .iter()
                .any(|r| r.virtual_path.ends_with("assets/bin/Data/test.assets")
                    && r.bytes.as_ref() == b"part0-part1"),
            "split parts inside ZIP should reassemble cleanly"
        );

        let loaded = ws.load_deferred_bundles_filtered(Some("shader")).unwrap();
        assert_eq!(loaded, 1);
        assert!(
            ws.deferred_bundles
                .iter()
                .any(|d| d.entry_name.contains("shaders.ab") && d.loaded)
        );
        assert!(
            ws.deferred_bundles
                .iter()
                .any(|d| d.entry_name.contains("stone.ab") && !d.loaded)
        );

        // Test raw `.ab` extraction
        let out_dir = dir.0.join("extracted");
        let report = extract_raw_bundles(&zip_path, &out_dir, Some("stone")).unwrap();
        assert_eq!(report.extracted_files, 1);
        assert!(
            out_dir.join("assets/scenetexture/stone.ab").exists(),
            "extracted .ab should keep sanitized relative path"
        );
    }
}
