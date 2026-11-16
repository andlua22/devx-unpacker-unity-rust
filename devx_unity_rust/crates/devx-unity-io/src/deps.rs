//! Transitive cross-bundle dependency resolver (`AssetBundle.m_Dependencies` + lazy `CAB-*` header fallback).

use crate::workspace_io::UnityWorkspaceData;
use crate::{IoResult, read_object};
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::PathBuf;
use unity_asset_core::UnityValue;
use zip::ZipArchive;

/// Summary returned after resolving cross-bundle dependencies for one or more loaded files.
#[derive(Debug, Clone, Default)]
pub struct DependencyResolutionReport {
    pub newly_loaded_bundles: usize,
    pub newly_loaded_files: usize,
    pub resolved_dependencies: Vec<String>,
    pub unresolved_dependencies: Vec<String>,
}

/// Loads `indices` from `workspace.deferred_bundles` and transitively resolves any referenced
/// dependency bundles (`AssetBundle.m_Dependencies` and `SerializedFile.externals`).
pub fn load_deferred_bundles_with_dependencies(
    workspace: &mut UnityWorkspaceData,
    indices: &[usize],
) -> IoResult<(usize, DependencyResolutionReport)> {
    let start_file = workspace.files.len();
    let loaded_primary = crate::vfs::load_deferred_bundle_indices(workspace, indices)?;
    let end_file = workspace.files.len();
    let seeds: Vec<usize> = (start_file..end_file).collect();
    let report = resolve_dependencies_for_files(workspace, &seeds, 6)?;
    Ok((loaded_primary, report))
}

/// Performs bounded BFS over `seed_file_indices`, loading any unloaded deferred `.ab` bundles
/// required by `AssetBundle.m_Dependencies` or non-builtin `SerializedFile.externals`.
pub fn resolve_dependencies_for_files(
    workspace: &mut UnityWorkspaceData,
    seed_file_indices: &[usize],
    max_depth: usize,
) -> IoResult<DependencyResolutionReport> {
    let mut report = DependencyResolutionReport::default();
    if workspace.deferred_bundles.is_empty() || seed_file_indices.is_empty() {
        return Ok(report);
    }

    let mut queue: VecDeque<(usize, usize)> = seed_file_indices.iter().map(|&fi| (fi, 0)).collect();
    let mut visited_files: HashSet<usize> = HashSet::new();
    let mut seen_resolved = BTreeSet::new();
    let mut seen_unresolved = BTreeSet::new();

    while !queue.is_empty() {
        let mut wave_dep_names: Vec<String> = Vec::new();
        let mut wave_unresolved_cabs: Vec<String> = Vec::new();
        let mut next_depth = usize::MAX;

        while let Some((fi, depth)) = queue.pop_front() {
            if !visited_files.insert(fi) {
                continue;
            }
            if depth >= max_depth {
                continue;
            }
            next_depth = next_depth.min(depth + 1);

            let Some(loaded) = workspace.files.get(fi) else {
                continue;
            };

            // 1. Collect explicit `AssetBundle` (`class_id == 142`) `m_Dependencies` strings.
            for info in &loaded.file.objects {
                if info.type_id != 142 {
                    continue;
                }
                let Ok(obj) = read_object(loaded, info) else {
                    continue;
                };
                if let Some(UnityValue::Array(deps)) = obj.class.get("m_Dependencies") {
                    for item in deps {
                        if let Some(dep_name) = item.as_str() {
                            let trimmed = dep_name.trim();
                            if !trimmed.is_empty() {
                                wave_dep_names.push(trimmed.to_owned());
                            }
                        }
                    }
                }
            }

            // 2. Also inspect `SerializedFile.externals` for any cross-bundle `CAB-*` refs.
            for ext_idx in 0..loaded.file.externals.len() {
                let m_file_id = (ext_idx + 1) as i32;
                if workspace.resolve_external_file(fi, m_file_id).is_some() {
                    continue;
                }
                if let Some(raw_name) = workspace.external_file_name(fi, m_file_id) {
                    let base = raw_name
                        .replace('\\', "/")
                        .rsplit('/')
                        .next()
                        .unwrap_or(raw_name)
                        .to_ascii_lowercase();
                    if !is_builtin_unity_resource(&base) && !base.is_empty() {
                        wave_unresolved_cabs.push(base);
                    }
                }
            }
        }

        if wave_dep_names.is_empty() && wave_unresolved_cabs.is_empty() {
            break;
        }

        let mut batch_indices: BTreeSet<usize> = BTreeSet::new();

        // Match `AssetBundle.m_Dependencies` against `deferred_bundles`.
        if !wave_dep_names.is_empty() {
            let lookup = build_deferred_path_lookup(workspace);
            for dep in wave_dep_names {
                let key = normalize_bundle_rel(&dep);
                let stripped = key.strip_prefix("assets/").unwrap_or(&key);
                if let Some(&idx) = lookup.get(&key).or_else(|| lookup.get(stripped)) {
                    if let Some(d) = workspace.deferred_bundles.get(idx)
                        && !d.loaded
                    {
                        batch_indices.insert(idx);
                    }
                    seen_resolved.insert(dep);
                } else if !is_loaded_bundle_path(workspace, stripped) {
                    seen_unresolved.insert(dep);
                }
            }
        }

        // Load dependencies discovered via `m_Dependencies` first so we don't scan zip CABs unnecessarily.
        let before_files = workspace.files.len();
        if !batch_indices.is_empty() {
            let idxs: Vec<usize> = batch_indices.iter().copied().collect();
            let loaded_cnt = crate::vfs::load_deferred_bundle_indices(workspace, &idxs)?;
            report.newly_loaded_bundles += loaded_cnt;
        }

        // Re-check any remaining unresolved `CAB-*` externals using lazy UnityFS directory scan.
        let still_missing_cabs: Vec<String> = wave_unresolved_cabs
            .into_iter()
            .filter(|cab| !workspace.file_basename_index.contains_key(cab.as_str()))
            .collect();
        if !still_missing_cabs.is_empty() {
            ensure_deferred_cab_index(workspace);
            let mut cab_indices = Vec::new();
            if let Some(ref map) = workspace.deferred_cab_index {
                for cab in still_missing_cabs {
                    if let Some(&idx) = map.get(&cab) {
                        if let Some(d) = workspace.deferred_bundles.get(idx)
                            && !d.loaded
                            && batch_indices.insert(idx)
                        {
                            cab_indices.push(idx);
                        }
                        seen_resolved.insert(cab);
                    } else {
                        seen_unresolved.insert(cab);
                    }
                }
            }
            if !cab_indices.is_empty() {
                let loaded_cnt = crate::vfs::load_deferred_bundle_indices(workspace, &cab_indices)?;
                report.newly_loaded_bundles += loaded_cnt;
            }
        }

        let after_files = workspace.files.len();
        report.newly_loaded_files += after_files.saturating_sub(before_files);
        for new_fi in before_files..after_files {
            if next_depth <= max_depth {
                queue.push_back((new_fi, next_depth));
            }
        }
    }

    report.resolved_dependencies = seen_resolved.into_iter().collect();
    report.unresolved_dependencies = seen_unresolved.into_iter().collect();
    Ok(report)
}

fn normalize_bundle_rel(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

fn is_builtin_unity_resource(base_lower: &str) -> bool {
    base_lower.is_empty()
        || base_lower == "unity default resources"
        || base_lower == "unity_builtin_extra"
        || base_lower.ends_with(".ress")
        || base_lower.ends_with(".resource")
}

fn is_loaded_bundle_path(workspace: &UnityWorkspaceData, dep_stripped: &str) -> bool {
    workspace.deferred_bundles.iter().any(|d| {
        if !d.loaded {
            return false;
        }
        let n = normalize_bundle_rel(&d.entry_name);
        let s = n.strip_prefix("assets/").unwrap_or(&n);
        n == dep_stripped || s == dep_stripped
    })
}

fn build_deferred_path_lookup(workspace: &UnityWorkspaceData) -> HashMap<String, usize> {
    let mut map = HashMap::with_capacity(workspace.deferred_bundles.len() * 2);
    for (idx, d) in workspace.deferred_bundles.iter().enumerate() {
        let entry_norm = normalize_bundle_rel(&d.entry_name);
        if let Some(stripped) = entry_norm.strip_prefix("assets/") {
            map.entry(stripped.to_owned()).or_insert(idx);
        }
        map.entry(entry_norm).or_insert(idx);
        let virt_norm = normalize_bundle_rel(&d.virtual_path);
        if let Some((_, tail)) = virt_norm.split_once("/assets/") {
            map.entry(tail.to_owned()).or_insert(idx);
        }
    }
    map
}

/// Peeks ONLY the UnityFS header & directory table of unloaded deferred `.ab` entries (without
/// decompressing asset payload blocks) so unresolved `CAB-*` externals can be located on demand.
fn ensure_deferred_cab_index(workspace: &mut UnityWorkspaceData) {
    if workspace.deferred_cab_index.is_some() {
        return;
    }

    let mut by_archive: HashMap<PathBuf, Vec<(usize, String)>> = HashMap::new();
    for (idx, entry) in workspace.deferred_bundles.iter().enumerate() {
        by_archive
            .entry(entry.archive_path.clone())
            .or_default()
            .push((idx, entry.entry_name.clone()));
    }

    let mut cab_map = HashMap::new();
    let cfg = ExtractionConfig::default();

    for (archive_path, items) in by_archive {
        let Ok(file) = File::open(&archive_path) else {
            continue;
        };
        let Ok(mut zip) = ZipArchive::new(BufReader::new(file)) else {
            continue;
        };
        for (idx, entry_name) in items {
            let (head, total_size) = {
                let Ok(mut ze) = zip.by_name(&entry_name) else {
                    continue;
                };
                let size = ze.size();
                // Read up to 64 KiB for UnityFS directory header; retry full entry only if header > 64 KiB.
                let mut head = Vec::with_capacity((size as usize).min(65536));
                let mut take_reader = (&mut ze).take(65536);
                if take_reader.read_to_end(&mut head).is_err() {
                    continue;
                }
                (head, size)
            };
            if !head.starts_with(b"UnityFS\0") {
                continue;
            }
            if let Ok(bundle) = BundleFileReader::from_reader(Cursor::new(&head), &cfg) {
                for fe in bundle.files() {
                    let key = fe
                        .path
                        .replace('\\', "/")
                        .rsplit('/')
                        .next()
                        .unwrap_or(&fe.path)
                        .to_ascii_lowercase();
                    if !key.is_empty() {
                        cab_map.entry(key).or_insert(idx);
                    }
                }
            } else if total_size > 65536
                && let Ok(mut ze_full) = zip.by_name(&entry_name)
            {
                let mut full = Vec::new();
                if ze_full.read_to_end(&mut full).is_ok()
                    && let Ok(bundle) = BundleFileReader::from_reader(Cursor::new(&full), &cfg)
                {
                    for fe in bundle.files() {
                        let key = fe
                            .path
                            .replace('\\', "/")
                            .rsplit('/')
                            .next()
                            .unwrap_or(&fe.path)
                            .to_ascii_lowercase();
                        if !key.is_empty() {
                            cab_map.entry(key).or_insert(idx);
                        }
                    }
                }
            }
        }
    }

    workspace.deferred_cab_index = Some(cab_map);
}
