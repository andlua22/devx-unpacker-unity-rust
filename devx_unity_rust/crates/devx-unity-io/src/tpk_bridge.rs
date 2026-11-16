//! Bridge exposing `rabex`'s embedded AssetRipper TPK database as a
//! `unity_asset_binary::typetree::TypeTreeRegistry` so stripped builds still parse.

use parking_lot::RwLock;
use rabex::objects::ClassId;
use rabex::tpk::TpkTypeTreeBlob;
use rabex::typetree::TypeTreeNode as RabexNode;
use rabex::UnityVersion;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use unity_asset_binary::typetree::{TypeTree, TypeTreeNode, TypeTreeRegistry};

static EMBEDDED_REGISTRY: LazyLock<Arc<RabexEmbeddedTpkRegistry>> =
    LazyLock::new(|| Arc::new(RabexEmbeddedTpkRegistry::new()));

/// Shared singleton handle to the embedded AssetRipper TPK registry.
pub fn shared_tpk_registry() -> Arc<dyn TypeTreeRegistry> {
    EMBEDDED_REGISTRY.clone()
}

/// Returns the number of Unity classes contained in the embedded TPK database.
pub fn tpk_class_count() -> usize {
    EMBEDDED_REGISTRY.blob.class_information.len()
}

type TypeTreeCacheMap = HashMap<(String, i32), Option<Arc<TypeTree>>>;

/// [`TypeTreeRegistry`] backed by `rabex::tpk::TpkTypeTreeBlob::embedded()`.
#[derive(Debug)]
pub struct RabexEmbeddedTpkRegistry {
    blob: TpkTypeTreeBlob,
    cache: RwLock<TypeTreeCacheMap>,
}

impl RabexEmbeddedTpkRegistry {
    pub fn new() -> Self {
        Self {
            blob: TpkTypeTreeBlob::embedded(),
            cache: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for RabexEmbeddedTpkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeTreeRegistry for RabexEmbeddedTpkRegistry {
    fn resolve(&self, unity_version: &str, class_id: i32) -> Option<Arc<TypeTree>> {
        let key = (unity_version.to_owned(), class_id);
        if let Some(hit) = self.cache.read().get(&key) {
            return hit.clone();
        }

        let parsed_version = parse_unity_version_with_fallback(unity_version);
        let tree = self
            .blob
            .get_typetree_node(ClassId(class_id), &parsed_version)
            .map(|root| {
                let mut tt = TypeTree::new();
                tt.add_node(convert_node(&root, 0, &mut 0));
                Arc::new(tt)
            });

        self.cache.write().insert(key, tree.clone());
        tree
    }
}

/// Sanitizes Unity version strings (e.g. `"2021.3.23f1c1"` -> `"2021.3.23f1"`).
///
/// Chinese Unity builds (`f1c1`), hotfix suffixes, or embedded newline metadata
/// are stripped so standard `major.minor.patch[a|b|p|f|x]build` parsers succeed.
pub fn sanitize_unity_version(version: &str) -> String {
    let trimmed = version
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut dot_parts = trimmed.splitn(3, '.');
    let (Some(major), Some(minor), Some(rest)) =
        (dot_parts.next(), dot_parts.next(), dot_parts.next())
    else {
        return trimmed.to_owned();
    };
    if !major.chars().all(|c| c.is_ascii_digit()) || !minor.chars().all(|c| c.is_ascii_digit()) {
        return trimmed.to_owned();
    }

    // `rest` starts with patch digits, followed by an optional release letter (`a|b|c|f|p|x`)
    // and build number digits; anything after (`c1`, `-custom`, `_hash`) is variant noise.
    let bytes = rest.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i >= bytes.len() {
        return trimmed.to_owned();
    }
    let ch = bytes[i] as char;
    if matches!(ch, 'a' | 'b' | 'p' | 'f' | 'x') {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        format!("{major}.{minor}.{}", &rest[..i])
    } else if ch == 'c' {
        // Rare older `2019.4.1c1` style without `f1`: normalize `c` -> `f`.
        let patch = &rest[..i];
        i += 1;
        let b_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let b_num = if i > b_start { &rest[b_start..i] } else { "1" };
        format!("{major}.{minor}.{patch}f{b_num}")
    } else {
        trimmed.to_owned()
    }
}

fn parse_unity_version_with_fallback(version: &str) -> UnityVersion {
    let fallback = || {
        "2022.3.0f1"
            .parse::<UnityVersion>()
            .expect("hardcoded fallback UnityVersion is valid")
    };
    let normalized = sanitize_unity_version(version);
    if normalized.is_empty() || normalized.starts_with("0.") {
        return fallback();
    }
    normalized.parse().unwrap_or_else(|_| fallback())
}

fn convert_node(node: &RabexNode, level: i32, next_index: &mut i32) -> TypeTreeNode {
    let index = *next_index;
    *next_index += 1;
    let children = node
        .children
        .iter()
        .map(|child| convert_node(child, level + 1, next_index))
        .collect();
    TypeTreeNode {
        type_name: node.m_Type.clone(),
        name: node.m_Name.clone(),
        byte_size: node.m_ByteSize,
        index,
        type_flags: node.m_TypeFlags,
        version: node.m_Version,
        meta_flags: node.m_MetaFlag.unwrap_or(0),
        level,
        type_str_offset: 0,
        name_str_offset: 0,
        ref_type_hash: node.m_RefTypeHash.unwrap_or(0),
        children,
    }
}
