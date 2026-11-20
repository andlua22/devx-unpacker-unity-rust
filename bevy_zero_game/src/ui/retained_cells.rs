//! Clones retained source cells while preserving geometry and giving every binding a fresh name.

use super::panel_loader::{UiAssetRefDto, UiPanelNodeDto};
use std::collections::HashMap;

#[derive(Clone)]
pub(crate) struct CloneSpec {
    pub(crate) name: String,
    pub(crate) slots: Vec<String>,
}

pub(crate) fn node_by_ref(
    node: &UiPanelNodeDto,
    reference: UiAssetRefDto,
) -> Option<&UiPanelNodeDto> {
    if node.go_ref == Some(reference) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| node_by_ref(child, reference))
}

pub(crate) fn node_by_ref_mut(
    node: &mut UiPanelNodeDto,
    reference: UiAssetRefDto,
) -> Option<&mut UiPanelNodeDto> {
    if node.go_ref == Some(reference) {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| node_by_ref_mut(child, reference))
}

pub(crate) fn clone_cell(
    template: &UiPanelNodeDto,
    targets: &[UiAssetRefDto],
    name: String,
) -> Result<(UiPanelNodeDto, CloneSpec), String> {
    fn clone_nodes(
        node: &mut UiPanelNodeDto,
        prefix: &str,
        ids: &mut HashMap<UiAssetRefDto, String>,
        next: &mut usize,
    ) {
        let unique = format!("{prefix}_{next}");
        *next += 1;
        if let Some(reference) = node.go_ref.take() {
            ids.insert(reference, unique.clone());
        }
        node.name = unique;
        node.active_self = true;
        for child in &mut node.children {
            clone_nodes(child, prefix, ids, next);
        }
    }
    let mut clone = template.clone();
    let mut names = HashMap::new();
    clone_nodes(&mut clone, &name, &mut names, &mut 0);
    let slots = targets
        .iter()
        .map(|reference| {
            names.get(reference).cloned().ok_or_else(|| {
                format!(
                    "Template {name} does not contain bound node {}",
                    reference.path_id
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let spec = CloneSpec {
        name: clone.name.clone(),
        slots,
    };
    Ok((clone, spec))
}
