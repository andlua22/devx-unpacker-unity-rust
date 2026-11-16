//! Multi-mode asset tree builder supporting By Asset Class, By Container/File,
//! By Scene/GameObject Hierarchy, and Flat filtered views.

use crate::catalog::{AssetCatalog, AssetEntry};
use devx_plugin_api::AssetRef;
use devx_unity_io::read_object;
use std::collections::{BTreeMap, HashMap, HashSet};
use unity_asset_core::UnityValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeGroupingMode {
    ByClass,
    ByContainer,
    BySceneHierarchy,
    Flat,
}

impl TreeGroupingMode {
    pub fn label(self) -> &'static str {
        match self {
            TreeGroupingMode::ByClass => "By Asset Type",
            TreeGroupingMode::ByContainer => "By Container / File",
            TreeGroupingMode::BySceneHierarchy => "By Scene Hierarchy",
            TreeGroupingMode::Flat => "Flat List",
        }
    }
}

#[derive(Debug, Clone)]
pub enum TreeNodeKind {
    Group { count: usize },
    Asset(AssetRef),
}

#[derive(Debug, Clone)]
pub struct AssetTreeNode {
    pub label: String,
    pub subtitle: Option<String>,
    pub kind: TreeNodeKind,
    pub children: Vec<AssetTreeNode>,
}

/// Filter applied to the asset tree.
#[derive(Debug, Default, Clone)]
pub struct TreeFilter {
    pub search_text: String,
    pub class_id: Option<i32>,
    pub file_index: Option<usize>,
}

impl TreeFilter {
    pub fn matches(&self, entry: &AssetEntry) -> bool {
        if let Some(cid) = self.class_id
            && entry.summary.class_id != cid
        {
            return false;
        }
        if let Some(fi) = self.file_index
            && entry.asset_ref.file_index != fi
        {
            return false;
        }
        let q = self.search_text.trim().to_ascii_lowercase();
        if q.is_empty() {
            return true;
        }
        entry.summary.name.to_ascii_lowercase().contains(&q)
            || entry.summary.class_name.to_ascii_lowercase().contains(&q)
            || entry.asset_ref.path_id.to_string().contains(&q)
            || entry
                .summary
                .container_path
                .as_deref()
                .is_some_and(|c| c.to_ascii_lowercase().contains(&q))
    }
}

/// Builds a tree hierarchy matching the selected [`TreeGroupingMode`] and [`TreeFilter`].
pub fn build_asset_tree(
    catalog: &AssetCatalog,
    mode: TreeGroupingMode,
    filter: &TreeFilter,
) -> Vec<AssetTreeNode> {
    let filtered: Vec<&AssetEntry> = catalog
        .entries
        .iter()
        .filter(|e| filter.matches(e))
        .collect();

    match mode {
        TreeGroupingMode::ByClass => build_by_class(&filtered),
        TreeGroupingMode::ByContainer => build_by_container(catalog, &filtered),
        TreeGroupingMode::BySceneHierarchy => build_by_scene_hierarchy(catalog, &filtered),
        TreeGroupingMode::Flat => filtered
            .into_iter()
            .map(asset_leaf_node)
            .collect(),
    }
}

fn asset_leaf_node(entry: &AssetEntry) -> AssetTreeNode {
    AssetTreeNode {
        label: entry.summary.name.clone(),
        subtitle: Some(format!(
            "{} (#{})",
            entry.summary.class_name, entry.asset_ref.path_id
        )),
        kind: TreeNodeKind::Asset(entry.asset_ref),
        children: Vec::new(),
    }
}

fn build_by_class(entries: &[&AssetEntry]) -> Vec<AssetTreeNode> {
    let mut groups: BTreeMap<String, Vec<&AssetEntry>> = BTreeMap::new();
    for &entry in entries {
        groups
            .entry(entry.summary.class_name.clone())
            .or_default()
            .push(entry);
    }

    groups
        .into_iter()
        .map(|(class_name, mut items)| {
            items.sort_by(|a, b| a.summary.name.cmp(&b.summary.name));
            let count = items.len();
            AssetTreeNode {
                label: format!("{class_name} ({count})"),
                subtitle: None,
                kind: TreeNodeKind::Group { count },
                children: items.into_iter().map(asset_leaf_node).collect(),
            }
        })
        .collect()
}

fn build_by_container(catalog: &AssetCatalog, entries: &[&AssetEntry]) -> Vec<AssetTreeNode> {
    let mut groups: BTreeMap<usize, Vec<&AssetEntry>> = BTreeMap::new();
    for &entry in entries {
        groups
            .entry(entry.asset_ref.file_index)
            .or_default()
            .push(entry);
    }

    groups
        .into_iter()
        .map(|(file_idx, mut items)| {
            items.sort_by(|a, b| {
                a.summary
                    .class_name
                    .cmp(&b.summary.class_name)
                    .then(a.summary.name.cmp(&b.summary.name))
            });
            let file_label = catalog
                .workspace
                .get_file(file_idx)
                .map_or_else(|| format!("File #{file_idx}"), |f| f.virtual_path.clone());
            let count = items.len();
            AssetTreeNode {
                label: format!("{file_label} ({count})"),
                subtitle: None,
                kind: TreeNodeKind::Group { count },
                children: items.into_iter().map(asset_leaf_node).collect(),
            }
        })
        .collect()
}

fn build_by_scene_hierarchy(catalog: &AssetCatalog, entries: &[&AssetEntry]) -> Vec<AssetTreeNode> {
    let visible_set: HashSet<AssetRef> = entries.iter().map(|e| e.asset_ref).collect();
    let mut per_file_roots: Vec<AssetTreeNode> = Vec::new();

    for loaded in &catalog.workspace.files {
        let mut go_names: HashMap<i64, String> = HashMap::new();
        let mut go_components: HashMap<i64, Vec<AssetRef>> = HashMap::new();
        let mut transform_go: HashMap<i64, i64> = HashMap::new();
        let mut transform_children: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut transform_father: HashMap<i64, i64> = HashMap::new();
        let mut all_transforms: Vec<i64> = Vec::new();

        for info in &loaded.file.objects {
            if info.type_id == 1 || info.type_id == 4 || info.type_id == 224 {
                let Ok(obj) = read_object(loaded, info) else {
                    continue;
                };
                if info.type_id == 1 {
                    let name = obj
                        .class
                        .get("m_Name")
                        .and_then(UnityValue::as_str)
                        .unwrap_or("GameObject")
                        .to_owned();
                    go_names.insert(info.path_id, name);
                    if let Some(UnityValue::Array(comps)) = obj.class.get("m_Component") {
                        for comp in comps {
                            if let Some(UnityValue::Object(map)) = comp
                                .as_object()
                                .and_then(|m| m.get("component"))
                                .or(Some(comp))
                                && let Some(pid) = map.get("m_PathID").and_then(UnityValue::as_i64)
                            {
                                let fid = map
                                    .get("m_FileID")
                                    .and_then(UnityValue::as_i64)
                                    .unwrap_or(0) as i32;
                                if let Some(target_fi) = catalog
                                    .workspace
                                    .resolve_external_file(loaded.file_index, fid)
                                {
                                    go_components
                                        .entry(info.path_id)
                                        .or_default()
                                        .push(AssetRef {
                                            file_index: target_fi,
                                            path_id: pid,
                                        });
                                }
                            }
                        }
                    }
                } else {
                    // Transform / RectTransform
                    all_transforms.push(info.path_id);
                    if let Some(go_pid) = obj
                        .class
                        .get("m_GameObject")
                        .and_then(UnityValue::as_object)
                        .and_then(|m| m.get("m_PathID"))
                        .and_then(UnityValue::as_i64)
                    {
                        transform_go.insert(info.path_id, go_pid);
                    }
                    if let Some(father_pid) = obj
                        .class
                        .get("m_Father")
                        .and_then(UnityValue::as_object)
                        .and_then(|m| m.get("m_PathID"))
                        .and_then(UnityValue::as_i64)
                    {
                        transform_father.insert(info.path_id, father_pid);
                    }
                    if let Some(UnityValue::Array(kids)) = obj.class.get("m_Children") {
                        let list: Vec<i64> = kids
                            .iter()
                            .filter_map(|k| {
                                k.as_object()
                                    .and_then(|m| m.get("m_PathID"))
                                    .and_then(UnityValue::as_i64)
                            })
                            .collect();
                        transform_children.insert(info.path_id, list);
                    }
                }
            }
        }

        if all_transforms.is_empty() {
            continue;
        }

        let mut visited = HashSet::new();
        let mut go_nodes = Vec::new();
        for &tid in &all_transforms {
            let father = transform_father.get(&tid).copied().unwrap_or(0);
            if father == 0
                && let Some(node) = build_transform_node(
                    loaded.file_index,
                    tid,
                    catalog,
                    &visible_set,
                    &go_names,
                    &go_components,
                    &transform_go,
                    &transform_children,
                    &mut visited,
                )
            {
                go_nodes.push(node);
            }
        }

        if !go_nodes.is_empty() {
            let count = go_nodes.len();
            per_file_roots.push(AssetTreeNode {
                label: format!("{} ({count} roots)", loaded.virtual_path),
                subtitle: None,
                kind: TreeNodeKind::Group { count },
                children: go_nodes,
            });
        }
    }

    if per_file_roots.is_empty() {
        return build_by_class(entries);
    }
    per_file_roots
}

#[allow(clippy::too_many_arguments)]
fn build_transform_node(
    file_index: usize,
    transform_id: i64,
    catalog: &AssetCatalog,
    visible_set: &HashSet<AssetRef>,
    go_names: &HashMap<i64, String>,
    go_components: &HashMap<i64, Vec<AssetRef>>,
    transform_go: &HashMap<i64, i64>,
    transform_children: &HashMap<i64, Vec<i64>>,
    visited: &mut HashSet<i64>,
) -> Option<AssetTreeNode> {
    if !visited.insert(transform_id) {
        return None;
    }
    let go_id = transform_go.get(&transform_id).copied().unwrap_or(0);
    let go_ref = AssetRef {
        file_index,
        path_id: go_id,
    };
    let name = go_names
        .get(&go_id)
        .cloned()
        .unwrap_or_else(|| format!("Transform_{transform_id}"));

    let mut children = Vec::new();
    if let Some(comps) = go_components.get(&go_id) {
        for &comp_ref in comps {
            if let Some(entry) = catalog.entry(comp_ref)
                && visible_set.contains(&comp_ref)
            {
                children.push(asset_leaf_node(entry));
            }
        }
    }
    if let Some(kids) = transform_children.get(&transform_id) {
        for &kid_tid in kids {
            if let Some(child_node) = build_transform_node(
                file_index,
                kid_tid,
                catalog,
                visible_set,
                go_names,
                go_components,
                transform_go,
                transform_children,
                visited,
            ) {
                children.push(child_node);
            }
        }
    }

    let matches_self = visible_set.contains(&go_ref);
    if !matches_self && children.is_empty() {
        return None;
    }

    let kind = if go_id != 0 {
        TreeNodeKind::Asset(go_ref)
    } else {
        TreeNodeKind::Asset(AssetRef {
            file_index,
            path_id: transform_id,
        })
    };

    Some(AssetTreeNode {
        label: name,
        subtitle: Some("GameObject".into()),
        kind,
        children,
    })
}
