//! Indexed workspace asset catalog, name resolution, raw search, and `PPtr` graph.

use devx_plugin_api::{AssetRef, AssetSummary};
use devx_unity_io::{
    PPtrLink, UnityWorkspaceData, class_name_for, read_object, scan_pptrs,
};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use unity_asset_binary::object::ObjectHandle;

/// Single indexed Unity object inside a [`AssetCatalog`].
#[derive(Debug, Clone)]
pub struct AssetEntry {
    pub asset_ref: AssetRef,
    pub summary: AssetSummary,
    pub byte_start: u64,
}

/// Directed `PPtr` edge between two objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceEdge {
    pub from: AssetRef,
    pub to: AssetRef,
    pub field_path: String,
}

/// Forward + reverse `PPtr` graph across all files in an [`AssetCatalog`].
#[derive(Debug, Default, Clone)]
pub struct ReferenceGraph {
    pub forward: HashMap<AssetRef, Vec<ReferenceEdge>>,
    pub reverse: HashMap<AssetRef, Vec<ReferenceEdge>>,
}

/// Result returned by deep content/hex/script searches.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub asset_ref: AssetRef,
    pub name: String,
    pub class_name: String,
    pub reason: String,
}

/// Immutable indexed view over a loaded [`UnityWorkspaceData`].
#[derive(Debug)]
pub struct AssetCatalog {
    pub workspace: Arc<UnityWorkspaceData>,
    pub entries: Vec<AssetEntry>,
    pub by_ref: HashMap<AssetRef, usize>,
    pub class_counts: BTreeMap<(i32, String), usize>,
}

impl AssetCatalog {
    /// Indexes all objects across loaded files in parallel using fast `peek_name`.
    pub fn from_workspace(workspace: Arc<UnityWorkspaceData>) -> Self {
        let mut raw_entries: Vec<(usize, usize, AssetEntry)> = workspace
            .files
            .par_iter()
            .enumerate()
            .flat_map_iter(|(file_index, loaded)| {
                let source_file = loaded
                    .virtual_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&loaded.virtual_path)
                    .to_owned();
                let unity_version = loaded.file.unity_version.clone();

                loaded.file.objects.iter().enumerate().map(move |(obj_idx, info)| {
                    let asset_ref = AssetRef {
                        file_index,
                        path_id: info.path_id,
                    };
                    let class_name = class_name_for(info.type_id);
                    let container_path = loaded.bundle_container_paths.get(&info.path_id).cloned();

                    let peeked = ObjectHandle::new(&loaded.file, info)
                        .peek_name()
                        .ok()
                        .flatten()
                        .filter(|s| !s.trim().is_empty());

                    let fallback_container_name = container_path
                        .as_deref()
                        .and_then(|p| p.rsplit('/').next())
                        .map(|s| {
                            s.rsplit_once('.')
                                .map_or(s, |(stem, _)| stem)
                                .to_owned()
                        });

                    let name = peeked
                        .or(fallback_container_name)
                        .unwrap_or_else(|| format!("{class_name}_{}", info.path_id));

                    let entry = AssetEntry {
                        asset_ref,
                        byte_start: info.byte_start,
                        summary: AssetSummary {
                            asset_ref,
                            class_id: info.type_id,
                            class_name,
                            name,
                            container_path,
                            source_file: source_file.clone(),
                            byte_size: info.byte_size,
                            unity_version: unity_version.clone(),
                        },
                    };
                    (file_index, obj_idx, entry)
                })
            })
            .collect();

        raw_entries.sort_by_key(|(fi, oi, _)| (*fi, *oi));

        let mut entries = Vec::with_capacity(raw_entries.len());
        let mut by_ref = HashMap::with_capacity(raw_entries.len());
        let mut class_counts: BTreeMap<(i32, String), usize> = BTreeMap::new();

        for (_, _, entry) in raw_entries {
            let idx = entries.len();
            by_ref.insert(entry.asset_ref, idx);
            *class_counts
                .entry((entry.summary.class_id, entry.summary.class_name.clone()))
                .or_insert(0) += 1;
            entries.push(entry);
        }

        Self {
            workspace,
            entries,
            by_ref,
            class_counts,
        }
    }

    pub fn entry(&self, asset_ref: AssetRef) -> Option<&AssetEntry> {
        self.by_ref.get(&asset_ref).and_then(|&idx| self.entries.get(idx))
    }

    /// Reads the raw bytes slice of an object directly from its `SerializedFile`.
    pub fn raw_bytes(&self, asset_ref: AssetRef) -> Option<&[u8]> {
        let (loaded, info) = self
            .workspace
            .object_info(asset_ref.file_index, asset_ref.path_id)?;
        ObjectHandle::new(&loaded.file, info).raw_data().ok()
    }

    /// Scans all objects in parallel to build the cross-file `PPtr` reference graph.
    pub fn build_reference_graph(&self) -> ReferenceGraph {
        let edges: Vec<ReferenceEdge> = self
            .entries
            .par_iter()
            .flat_map_iter(|entry| {
                let mut local_edges = Vec::new();
                let Some((loaded, info)) = self
                    .workspace
                    .object_info(entry.asset_ref.file_index, entry.asset_ref.path_id)
                else {
                    return local_edges;
                };
                let Ok(obj) = read_object(loaded, info) else {
                    return local_edges;
                };
                for PPtrLink {
                    field_path,
                    file_id,
                    path_id,
                } in scan_pptrs(&obj)
                {
                    if let Some(target_file) = self
                        .workspace
                        .resolve_external_file(entry.asset_ref.file_index, file_id)
                    {
                        let to = AssetRef {
                            file_index: target_file,
                            path_id,
                        };
                        local_edges.push(ReferenceEdge {
                            from: entry.asset_ref,
                            to,
                            field_path,
                        });
                    }
                }
                local_edges
            })
            .collect();

        let mut graph = ReferenceGraph::default();
        for edge in edges {
            graph.forward.entry(edge.from).or_default().push(edge.clone());
            graph.reverse.entry(edge.to).or_default().push(edge);
        }
        graph
    }

    /// Searches object names, IDs, container paths, or raw byte patterns (ASCII or `AA BB CC` hex).
    pub fn deep_search(&self, query: &str, search_raw_bytes: bool) -> Vec<SearchHit> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let hex_pattern = parse_hex_bytes(trimmed);
        let lower_query = trimmed.to_ascii_lowercase();

        self.entries
            .par_iter()
            .filter_map(|entry| {
                let s = &entry.summary;
                if s.name.to_ascii_lowercase().contains(&lower_query)
                    || s.class_name.to_ascii_lowercase().contains(&lower_query)
                    || s.asset_ref.path_id.to_string() == trimmed
                    || s.container_path
                        .as_deref()
                        .is_some_and(|p| p.to_ascii_lowercase().contains(&lower_query))
                {
                    return Some(SearchHit {
                        asset_ref: s.asset_ref,
                        name: s.name.clone(),
                        class_name: s.class_name.clone(),
                        reason: "Metadata match".into(),
                    });
                }

                if search_raw_bytes && let Some(bytes) = self.raw_bytes(s.asset_ref) {
                    if let Some(ref needle) = hex_pattern
                        && !needle.is_empty()
                        && bytes.windows(needle.len()).any(|w| w == needle.as_slice())
                    {
                        return Some(SearchHit {
                            asset_ref: s.asset_ref,
                            name: s.name.clone(),
                            class_name: s.class_name.clone(),
                            reason: format!("HEX match ({} bytes)", needle.len()),
                        });
                    }
                    let ascii_needle = lower_query.as_bytes();
                    if ascii_needle.len() >= 3
                        && contains_ascii_case_insensitive(bytes, ascii_needle)
                    {
                        return Some(SearchHit {
                            asset_ref: s.asset_ref,
                            name: s.name.clone(),
                            class_name: s.class_name.clone(),
                            reason: "Content ASCII/UTF-8 match".into(),
                        });
                    }
                }
                None
            })
            .collect()
    }
}

fn contains_ascii_case_insensitive(haystack: &[u8], lower_needle: &[u8]) -> bool {
    if haystack.len() < lower_needle.len() {
        return false;
    }
    haystack.windows(lower_needle.len()).any(|win| {
        win.iter()
            .zip(lower_needle)
            .all(|(&a, &b)| a.to_ascii_lowercase() == b)
    })
}

fn parse_hex_bytes(input: &str) -> Option<Vec<u8>> {
    let cleaned = input.trim().strip_prefix("0x").unwrap_or(input.trim());
    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    if parts.len() >= 2 && parts.iter().all(|p| p.len() == 2) {
        let mut out = Vec::with_capacity(parts.len());
        for p in parts {
            out.push(u8::from_str_radix(p, 16).ok()?);
        }
        return Some(out);
    }
    if cleaned.len() >= 4
        && cleaned.len().is_multiple_of(2)
        && cleaned.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let mut out = Vec::with_capacity(cleaned.len() / 2);
        for chunk in cleaned.as_bytes().as_chunks::<2>().0 {
            let s = std::str::from_utf8(chunk).ok()?;
            out.push(u8::from_str_radix(s, 16).ok()?);
        }
        return Some(out);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_patterns() {
        assert_eq!(parse_hex_bytes("89 50 4E 47"), Some(vec![0x89, 0x50, 0x4E, 0x47]));
        assert_eq!(parse_hex_bytes("UnityFS"), None);
    }
}
