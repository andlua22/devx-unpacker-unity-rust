//! Shared Unity uGUI (`Canvas` / `RectTransform` / `Image` / `RawImage` / `Text` / `TMP_Text`)
//! layout extractor, `RectTransform` solver, and sprite/texture decoder.
//!
//! Used by both `devx-app`'s interactive 2D UI Canvas preview and `devx-exporter::bevy_export`
//! so Unity UI traversal, PPtr resolution, texture decoding, and coordinate math exist in one place.

use crate::prefab::resolve_pptr_ref;
use devx_codecs::animation::{
    DecodedUiAnimationClip, decode_ui_animation_clip, unity_path_crc32,
};
use devx_codecs::texture::{TextureDecodeOptions, decode_sprite_cached, decode_texture2d};
use devx_plugin_api::{AssetRef, DecodedImage};
use devx_unity_io::{
    UnityObject, UnityValue, UnityWorkspaceData, extract_i32_field, extract_i64_field, read_object,
    shared_tpk_registry,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use unity_asset_binary::typetree::TypeTreeRegistry;

/// Default canonical reference canvas resolution when a prefab has no `CanvasScaler`.
pub const DEFAULT_UI_REF_WIDTH: f32 = 1920.0;
pub const DEFAULT_UI_REF_HEIGHT: f32 = 1080.0;

/// Default maximum UI nodes extracted per canvas to bound pathological hierarchies.
pub const DEFAULT_MAX_UI_NODES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiScaleMode {
    ConstantPixelSize,
    #[default]
    ScaleWithScreenSize,
    ConstantPhysicalSize,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiImageType {
    #[default]
    Simple,
    Sliced,
    Tiled,
    Filled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiTextAlignment {
    UpperLeft,
    UpperCenter,
    UpperRight,
    MiddleLeft,
    #[default]
    MiddleCenter,
    MiddleRight,
    LowerLeft,
    LowerCenter,
    LowerRight,
}

impl UiTextAlignment {
    /// Horizontal normalized alignment factor in `0.0..=1.0` (`0` = left, `0.5` = center, `1` = right).
    pub fn horizontal_factor(self) -> f32 {
        match self {
            Self::UpperLeft | Self::MiddleLeft | Self::LowerLeft => 0.0,
            Self::UpperCenter | Self::MiddleCenter | Self::LowerCenter => 0.5,
            Self::UpperRight | Self::MiddleRight | Self::LowerRight => 1.0,
        }
    }

    /// Vertical normalized alignment factor in `0.0..=1.0` (`0` = top, `0.5` = middle, `1` = bottom).
    pub fn vertical_factor(self) -> f32 {
        match self {
            Self::UpperLeft | Self::UpperCenter | Self::UpperRight => 0.0,
            Self::MiddleLeft | Self::MiddleCenter | Self::MiddleRight => 0.5,
            Self::LowerLeft | Self::LowerCenter | Self::LowerRight => 1.0,
        }
    }
}

/// Decoded sprite/texture metadata referenced by [`UiGraphic`] entries via `key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiTextureAssetMeta {
    pub key: String,
    pub name: String,
    pub source_ref: AssetRef,
    pub is_sprite: bool,
    pub width: u32,
    pub height: u32,
    /// 9-slice border in texture pixels: `[left, top, right, bottom]`.
    pub border: [f32; 4],
    /// Relative asset path inside an exported project (e.g. `"ui/btn_123.png"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_path: Option<String>,
}

/// Visual graphic component attached to a UI [`UiCanvasNode`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiGraphic {
    Image {
        color: [f32; 4],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        texture_key: Option<String>,
        #[serde(default)]
        missing_texture: bool,
        image_type: UiImageType,
        preserve_aspect: bool,
        fill_amount: f32,
        /// 9-slice border in pixels `[left, top, right, bottom]`.
        border: [f32; 4],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_name: Option<String>,
    },
    RawImage {
        color: [f32; 4],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        texture_key: Option<String>,
        #[serde(default)]
        missing_texture: bool,
        uv_rect: [f32; 4],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_name: Option<String>,
    },
    Text {
        text: String,
        color: [f32; 4],
        font_size: f32,
        alignment: UiTextAlignment,
        rich_text: bool,
        best_fit: bool,
        min_size: f32,
        max_size: f32,
        line_spacing: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_ref: Option<AssetRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outline_color: Option<[f32; 4]>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLayoutGroupKind {
    Horizontal,
    Vertical,
    Grid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiLayoutGroupDto {
    pub kind: UiLayoutGroupKind,
    pub padding: [f32; 4],
    pub spacing: [f32; 2],
    pub child_alignment: u8,
    pub child_control_width: bool,
    pub child_control_height: bool,
    pub child_force_expand_width: bool,
    pub child_force_expand_height: bool,
    pub child_scale_width: bool,
    pub child_scale_height: bool,
    pub reverse_arrangement: bool,
    pub cell_size: [f32; 2],
    pub start_corner: u8,
    pub start_axis: u8,
    pub constraint: u8,
    pub constraint_count: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UiContentSizeFitterDto {
    pub horizontal_fit: u8,
    pub vertical_fit: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UiLayoutElementDto {
    pub ignore_layout: bool,
    pub min_width: f32,
    pub min_height: f32,
    pub preferred_width: f32,
    pub preferred_height: f32,
    pub flexible_width: f32,
    pub flexible_height: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UiAspectRatioFitterDto {
    pub mode: u8,
    pub ratio: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiWidgetPropsDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<[[f32; 4]; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slider_range: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_hv: Option<[bool; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_limit: Option<i32>,
    #[serde(default)]
    pub is_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiScriptBindingDto {
    pub script_name: String,
    pub field_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_index: Option<usize>,
    pub target_name: String,
    pub target_go_ref: AssetRef,
}

/// One node in an extracted uGUI `RectTransform` hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiCanvasNode {
    pub name: String,
    pub go_ref: AssetRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect_ref: Option<AssetRef>,
    pub active_self: bool,
    pub active_in_hierarchy: bool,
    pub anchor_min: [f32; 2],
    pub anchor_max: [f32; 2],
    pub anchored_position: [f32; 2],
    pub size_delta: [f32; 2],
    pub pivot: [f32; 2],
    pub scale: [f32; 3],
    pub rotation_z_deg: f32,
    /// Resolved rectangle `[x, y, width, height]` in top-down canvas coordinates.
    pub canvas_rect: [f32; 4],
    /// Resolved rectangle `[x, y, width, height]` relative to parent's top-left corner.
    pub local_rect: [f32; 4],
    pub flip_x: bool,
    pub flip_y: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_group_alpha: Option<f32>,
    #[serde(default)]
    pub ignore_parent_groups: bool,
    pub effective_alpha: f32,
    pub clips_children: bool,
    pub show_mask_graphic: bool,
    pub has_canvas: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_sorting_order: Option<i32>,
    pub graphic_enabled: bool,
    pub is_interactive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_props: Option<UiWidgetPropsDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_group: Option<UiLayoutGroupDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_size_fitter: Option<UiContentSizeFitterDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_element: Option<UiLayoutElementDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio_fitter: Option<UiAspectRatioFitterDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphic: Option<UiGraphic>,
    #[serde(default)]
    pub children: Vec<UiCanvasNode>,
}

impl Default for UiCanvasNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            go_ref: AssetRef {
                file_index: 0,
                path_id: 0,
            },
            rect_ref: None,
            active_self: true,
            active_in_hierarchy: true,
            anchor_min: [0.5, 0.5],
            anchor_max: [0.5, 0.5],
            anchored_position: [0.0, 0.0],
            size_delta: [100.0, 100.0],
            pivot: [0.5, 0.5],
            scale: [1.0, 1.0, 1.0],
            rotation_z_deg: 0.0,
            canvas_rect: [0.0; 4],
            local_rect: [0.0; 4],
            flip_x: false,
            flip_y: false,
            canvas_group_alpha: None,
            ignore_parent_groups: false,
            effective_alpha: 1.0,
            clips_children: false,
            show_mask_graphic: true,
            has_canvas: false,
            canvas_sorting_order: None,
            graphic_enabled: true,
            is_interactive: false,
            widget_kind: None,
            widget_props: None,
            layout_group: None,
            content_size_fitter: None,
            layout_element: None,
            aspect_ratio_fitter: None,
            graphic: None,
            children: Vec::new(),
        }
    }
}

/// Summary counts for quick UI validation and HUD reporting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiCanvasStats {
    pub node_count: usize,
    pub active_node_count: usize,
    pub rect_transform_count: usize,
    pub canvas_renderer_count: usize,
    pub has_canvas_component: bool,
    pub graphic_count: usize,
    pub active_graphic_count: usize,
    pub image_count: usize,
    pub raw_image_count: usize,
    pub text_count: usize,
    pub interactive_count: usize,
    pub resolved_texture_count: usize,
    pub missing_texture_count: usize,
    pub truncated: bool,
}

/// Complete serializable representation of a Unity uGUI Canvas / panel prefab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiCanvasDocument {
    pub name: String,
    pub root_ref: AssetRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_ref: Option<AssetRef>,
    pub source_bundle: String,
    pub reference_width: f32,
    pub reference_height: f32,
    pub scale_mode: UiScaleMode,
    pub match_width_or_height: f32,
    pub root: UiCanvasNode,
    #[serde(default)]
    pub textures: Vec<UiTextureAssetMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_bindings: Vec<UiScriptBindingDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<DecodedUiAnimationClip>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub stats: UiCanvasStats,
}

impl UiCanvasDocument {
    /// Returns `true` when the hierarchy contains enough uGUI structure to merit 2D Canvas preview.
    pub fn has_ui_content(&self) -> bool {
        let s = &self.stats;
        s.has_canvas_component
            || s.graphic_count > 0
            || s.interactive_count > 0
            || (s.rect_transform_count > 0 && (s.canvas_renderer_count > 0 || s.node_count > 1))
    }
}

/// Extracted UI layout paired with decoded RGBA8 textures/sprites keyed by `UiTextureAssetMeta::key`.
pub struct ExtractedUiCanvas {
    pub document: UiCanvasDocument,
    pub images: HashMap<String, DecodedImage>,
}

/// Reusable texture/sprite decode cache across extracting multiple UI panels in the same workspace.
#[derive(Default)]
pub struct UiExtractionCache {
    backing_textures: HashMap<(usize, i64), DecodedImage>,
    resolved_textures: HashMap<(AssetRef, bool), Result<(UiTextureAssetMeta, DecodedImage), String>>,
    script_names: HashMap<AssetRef, String>,
}

/// Extracts a 2D uGUI canvas starting from any UI-related object (`GameObject`, `RectTransform`,
/// `Canvas`, `CanvasGroup`, `CanvasRenderer`, or UI `MonoBehaviour`), walking up `m_Father` to the
/// enclosing `Canvas` or topmost `RectTransform` root.
pub fn extract_ui_canvas(
    workspace: &UnityWorkspaceData,
    start_ref: AssetRef,
    max_nodes: usize,
) -> Option<ExtractedUiCanvas> {
    let mut cache = UiExtractionCache::default();
    extract_ui_canvas_cached(workspace, start_ref, max_nodes, &mut cache)
}

/// Stateful variant of [`extract_ui_canvas`] that reuses decoded backing textures and `MonoScript`
/// metadata across multiple UI panels.
pub fn extract_ui_canvas_cached(
    workspace: &UnityWorkspaceData,
    start_ref: AssetRef,
    max_nodes: usize,
    cache: &mut UiExtractionCache,
) -> Option<ExtractedUiCanvas> {
    let start_go = resolve_owning_game_object(workspace, start_ref)?;
    let root_go = walk_up_to_ui_root(workspace, start_go);

    let tpk = shared_tpk_registry();
    let max_nodes = max_nodes.max(1);
    let mut ctx = ExtractContext {
        workspace,
        tpk: tpk.as_ref(),
        cache,
        max_nodes,
        visited_gos: HashSet::new(),
        textures: Vec::new(),
        tex_index_by_key: HashMap::new(),
        images: HashMap::new(),
        warnings: Vec::new(),
        seen_warnings: HashSet::new(),
        stats: UiCanvasStats::default(),
        scale_mode: UiScaleMode::ScaleWithScreenSize,
        match_width_or_height: 0.5,
        scaler_ref_size: None,
    };

    let mut root = ctx.extract_node(root_go, true)?;
    let stats = &ctx.stats;
    let has_ui = stats.has_canvas_component
        || stats.graphic_count > 0
        || stats.interactive_count > 0
        || (stats.rect_transform_count > 0 && (stats.canvas_renderer_count > 0 || stats.node_count > 1));
    if !has_ui {
        return None;
    }

    let (ref_w, ref_h) = infer_reference_resolution(ctx.scaler_ref_size, &root);
    let source_bundle = workspace
        .get_file(root_go.file_index)
        .map(|f| f.virtual_path.clone())
        .unwrap_or_default();

    let mut document = UiCanvasDocument {
        name: if root.name.is_empty() {
            format!("Canvas_{}", root_go.path_id)
        } else {
            root.name.clone()
        },
        root_ref: root_go,
        selected_ref: Some(start_go),
        source_bundle,
        reference_width: ref_w,
        reference_height: ref_h,
        scale_mode: ctx.scale_mode,
        match_width_or_height: ctx.match_width_or_height,
        root: std::mem::take(&mut root),
        textures: ctx.textures,
        script_bindings: Vec::new(),
        animations: Vec::new(),
        warnings: ctx.warnings,
        stats: ctx.stats,
    };

    populate_script_bindings(workspace, &mut document);
    populate_ui_animations(workspace, &mut document);
    recompute_ui_layout(&mut document, ref_w, ref_h);

    Some(ExtractedUiCanvas {
        document,
        images: ctx.images,
    })
}

/// Recomputes `canvas_rect`, `local_rect`, `effective_alpha`, `active_in_hierarchy`, `flip_x`,
/// and `flip_y` across the entire UI node tree for a given reference resolution `(width, height)`.
pub fn recompute_ui_layout(doc: &mut UiCanvasDocument, width: f32, height: f32) {
    let ref_w = width.max(64.0);
    let ref_h = height.max(64.0);
    doc.reference_width = ref_w;
    doc.reference_height = ref_h;
    let mut pref_memo: HashMap<usize, [f32; 2]> = HashMap::new();
    populate_preferred_sizes_tree(&doc.root, &mut pref_memo);
    let root_map = UiCanvasMapping {
        x0: 0.0,
        y0_bu: 0.0,
        sx: 1.0,
        sy: 1.0,
        canvas_h: ref_h,
    };
    recompute_node_recursive(
        &mut doc.root,
        [ref_w, ref_h],
        [0.0, 0.0, ref_w, ref_h],
        root_map,
        None,
        true,
        1.0,
        true,
        &pref_memo,
    );
}

#[derive(Clone, Copy)]
struct UiCanvasMapping {
    x0: f32,
    y0_bu: f32,
    sx: f32,
    sy: f32,
    canvas_h: f32,
}

fn norm_ui_scale(s: f32) -> f32 {
    if s.abs() < 1e-5 { 0.0 } else { s }
}

fn measure_text_intrinsic(text: &str, font_size: f32, fallback: [f32; 2]) -> [f32; 2] {
    let lines = text.lines().count().max(1) as f32;
    let max_chars = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    let w = (max_chars * font_size * 0.58).max(fallback[0]).max(4.0);
    let h = (lines * font_size * 1.25).max(fallback[1]).max(font_size);
    [w, h]
}

fn populate_preferred_sizes_tree(node: &UiCanvasNode, memo: &mut HashMap<usize, [f32; 2]>) {
    for ch in &node.children {
        populate_preferred_sizes_tree(ch, memo);
    }
    measure_preferred_size_memo(node, memo);
}

fn measure_preferred_size_memo(node: &UiCanvasNode, memo: &mut HashMap<usize, [f32; 2]>) -> [f32; 2] {
    let key = (node as *const UiCanvasNode) as usize;
    if let Some(&v) = memo.get(&key) {
        return v;
    }

    let base_sd = [node.size_delta[0].max(0.0), node.size_delta[1].max(0.0)];
    let mut measured = if let Some(ref lg) = node.layout_group {
        let mut child_sizes: Vec<[f32; 2]> = Vec::new();
        for ch in &node.children {
            if ch.active_self
                && !ch.layout_element.as_ref().is_some_and(|e| e.ignore_layout)
            {
                let mut sz = measure_preferred_size_memo(ch, memo);
                if lg.child_scale_width {
                    sz[0] *= ch.scale[0].abs();
                }
                if lg.child_scale_height {
                    sz[1] *= ch.scale[1].abs();
                }
                child_sizes.push(sz);
            }
        }
        let [pl, pt, pr, pb] = lg.padding;
        let [sx, sy] = lg.spacing;
        let n = child_sizes.len();
        let (iw, ih) = match lg.kind {
            UiLayoutGroupKind::Vertical => {
                let w = child_sizes.iter().map(|s| s[0]).fold(0.0_f32, f32::max);
                let h_sum: f32 = child_sizes.iter().map(|s| s[1]).sum();
                let gaps = if n > 1 { sy * (n - 1) as f32 } else { 0.0 };
                (w, h_sum + gaps)
            }
            UiLayoutGroupKind::Horizontal => {
                let w_sum: f32 = child_sizes.iter().map(|s| s[0]).sum();
                let gaps = if n > 1 { sx * (n - 1) as f32 } else { 0.0 };
                let h = child_sizes.iter().map(|s| s[1]).fold(0.0_f32, f32::max);
                (w_sum + gaps, h)
            }
            UiLayoutGroupKind::Grid => {
                let cols = match lg.constraint {
                    1 => lg.constraint_count.max(1),
                    2 => {
                        let rows = lg.constraint_count.max(1);
                        n.div_ceil(rows).max(1)
                    }
                    _ => {
                        let cw = (lg.cell_size[0] + sx).max(1.0);
                        (((base_sd[0] - pl - pr + sx).max(lg.cell_size[0])) / cw)
                            .floor()
                            .max(1.0) as usize
                    }
                };
                let rows = n.div_ceil(cols.max(1)).max(1);
                let gw = cols as f32 * lg.cell_size[0] + (cols.saturating_sub(1)) as f32 * sx;
                let gh = rows as f32 * lg.cell_size[1] + (rows.saturating_sub(1)) as f32 * sy;
                (gw, gh)
            }
        };
        [pl + pr + iw, pt + pb + ih]
    } else if let Some(UiGraphic::Text {
        text,
        font_size,
        ..
    }) = node.graphic.as_ref()
    {
        measure_text_intrinsic(text, *font_size, base_sd)
    } else {
        base_sd
    };

    if let Some(el) = node.layout_element {
        if el.preferred_width >= 0.0 {
            measured[0] = el.preferred_width;
        } else if el.min_width >= 0.0 {
            measured[0] = measured[0].max(el.min_width);
        }
        if el.preferred_height >= 0.0 {
            measured[1] = el.preferred_height;
        } else if el.min_height >= 0.0 {
            measured[1] = measured[1].max(el.min_height);
        }
    }

    memo.insert(key, measured);
    measured
}

#[allow(clippy::too_many_arguments)]
fn recompute_node_recursive(
    node: &mut UiCanvasNode,
    parent_unscaled_size: [f32; 2],
    parent_canvas_rect: [f32; 4],
    parent_map: UiCanvasMapping,
    override_local_rect: Option<[f32; 4]>,
    parent_active: bool,
    parent_alpha: f32,
    is_root: bool,
    pref_memo: &HashMap<usize, [f32; 2]>,
) {
    node.active_in_hierarchy = parent_active && node.active_self;
    let base_alpha = if node.ignore_parent_groups {
        1.0
    } else {
        parent_alpha
    };
    node.effective_alpha =
        (base_alpha * node.canvas_group_alpha.unwrap_or(1.0)).clamp(0.0, 1.0);

    let [pw, ph] = parent_unscaled_size;
    // Solve node rectangle in parent's unscaled local coordinates (top-down [x, y, w, h]).
    let mut unscaled_local = if let Some(ovr) = override_local_rect {
        ovr
    } else if is_root && (node.has_canvas || is_full_stretch_root(node)) {
        [0.0, 0.0, pw, ph]
    } else {
        let ax0 = node.anchor_min[0] * pw;
        let ax1 = node.anchor_max[0] * pw;
        let ay0 = node.anchor_min[1] * ph;
        let ay1 = node.anchor_max[1] * ph;
        let left = ax0 + node.anchored_position[0] - node.size_delta[0] * node.pivot[0];
        let right = ax1 + node.anchored_position[0] + node.size_delta[0] * (1.0 - node.pivot[0]);
        let bottom = ay0 + node.anchored_position[1] - node.size_delta[1] * node.pivot[1];
        let top_bu = ay1 + node.anchored_position[1] + node.size_delta[1] * (1.0 - node.pivot[1]);
        [
            left.min(right),
            ph - top_bu.max(bottom),
            (right - left).max(0.0),
            (top_bu - bottom).max(0.0),
        ]
    };

    // Apply ContentSizeFitter (Min/Preferred) expanding cleanly around Unity RectTransform pivot.
    if override_local_rect.is_none()
        && let Some(fitter) = node.content_size_fitter
        && (fitter.horizontal_fit != 0 || fitter.vertical_fit != 0)
    {
        let key = (node as *const UiCanvasNode) as usize;
        let pref = pref_memo
            .get(&key)
            .copied()
            .unwrap_or([unscaled_local[2], unscaled_local[3]]);
        if fitter.horizontal_fit != 0 {
            let new_w = pref[0];
            unscaled_local[0] -= (new_w - unscaled_local[2]) * node.pivot[0];
            unscaled_local[2] = new_w;
        }
        if fitter.vertical_fit != 0 {
            let new_h = pref[1];
            unscaled_local[1] -= (new_h - unscaled_local[3]) * (1.0 - node.pivot[1]);
            unscaled_local[3] = new_h;
        }
    }

    // Apply AspectRatioFitter when active (`1=W->H`, `2=H->W`, `3=FitInParent`, `4=EnvelopeParent`).
    if let Some(ar) = node.aspect_ratio_fitter
        && ar.ratio > 0.01
    {
        let (nw, nh) = match ar.mode {
            1 => (unscaled_local[2], unscaled_local[2] / ar.ratio),
            2 => (unscaled_local[3] * ar.ratio, unscaled_local[3]),
            3 if unscaled_local[2] > 0.0 && unscaled_local[3] > 0.0 => {
                if unscaled_local[2] / unscaled_local[3] > ar.ratio {
                    (unscaled_local[3] * ar.ratio, unscaled_local[3])
                } else {
                    (unscaled_local[2], unscaled_local[2] / ar.ratio)
                }
            }
            4 if unscaled_local[2] > 0.0 && unscaled_local[3] > 0.0 => {
                if unscaled_local[2] / unscaled_local[3] < ar.ratio {
                    (unscaled_local[3] * ar.ratio, unscaled_local[3])
                } else {
                    (unscaled_local[2], unscaled_local[2] / ar.ratio)
                }
            }
            _ => (unscaled_local[2], unscaled_local[3]),
        };
        unscaled_local[0] -= (nw - unscaled_local[2]) * node.pivot[0];
        unscaled_local[1] -= (nh - unscaled_local[3]) * (1.0 - node.pivot[1]);
        unscaled_local[2] = nw;
        unscaled_local[3] = nh;
    }

    let cw = unscaled_local[2].max(0.0);
    let ch = unscaled_local[3].max(0.0);
    let lu = unscaled_local[0];
    let bu = ph - unscaled_local[1] - ch;
    let piv_ux = lu + cw * node.pivot[0];
    let piv_uy_bu = bu + ch * node.pivot[1];

    let piv_cx = parent_map.x0 + piv_ux * parent_map.sx;
    let piv_cy_bu = parent_map.y0_bu + piv_uy_bu * parent_map.sy;
    let csx = norm_ui_scale(parent_map.sx * node.scale[0]);
    let csy = norm_ui_scale(parent_map.sy * node.scale[1]);
    let node_map = UiCanvasMapping {
        x0: piv_cx - (node.pivot[0] * cw) * csx,
        y0_bu: piv_cy_bu - (node.pivot[1] * ch) * csy,
        sx: csx,
        sy: csy,
        canvas_h: parent_map.canvas_h,
    };

    let cx0 = node_map.x0;
    let cx1 = node_map.x0 + cw * csx;
    let cy0_bu = node_map.y0_bu;
    let cy1_bu = node_map.y0_bu + ch * csy;
    let left = cx0.min(cx1);
    let right = cx0.max(cx1);
    let bottom = cy0_bu.min(cy1_bu);
    let top_bu = cy0_bu.max(cy1_bu);
    let canvas_rect = [
        left,
        parent_map.canvas_h - top_bu,
        (right - left).max(0.0),
        (top_bu - bottom).max(0.0),
    ];

    node.canvas_rect = canvas_rect;
    node.local_rect = if is_root {
        canvas_rect
    } else {
        [
            canvas_rect[0] - parent_canvas_rect[0],
            canvas_rect[1] - parent_canvas_rect[1],
            canvas_rect[2],
            canvas_rect[3],
        ]
    };
    node.flip_x = csx < 0.0;
    node.flip_y = csy < 0.0;

    let child_overrides = compute_layout_group_child_rects(node, [0.0, 0.0, cw, ch], pref_memo);
    let active_here = node.active_in_hierarchy;
    let alpha_here = node.effective_alpha;
    for (idx, child) in node.children.iter_mut().enumerate() {
        let ch_ovr = child_overrides.get(idx).copied().flatten();
        recompute_node_recursive(
            child,
            [cw, ch],
            canvas_rect,
            node_map,
            ch_ovr,
            active_here,
            alpha_here,
            false,
            pref_memo,
        );
    }
}

fn compute_layout_group_child_rects(
    parent: &UiCanvasNode,
    rect: [f32; 4],
    pref_memo: &HashMap<usize, [f32; 2]>,
) -> Vec<Option<[f32; 4]>> {
    let mut out = vec![None; parent.children.len()];
    let Some(ref lg) = parent.layout_group else {
        return out;
    };

    let mut active_indices: Vec<usize> = parent
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.active_self && !c.layout_element.as_ref().is_some_and(|e| e.ignore_layout)
        })
        .map(|(i, _)| i)
        .collect();
    if active_indices.is_empty() {
        return out;
    }
    if lg.reverse_arrangement && lg.kind != UiLayoutGroupKind::Grid {
        active_indices.reverse();
    }

    let [pl, pt, pr, pb] = lg.padding;
    let ix = rect[0] + pl;
    let iy = rect[1] + pt;
    let iw = (rect[2] - pl - pr).max(0.0);
    let ih = (rect[3] - pt - pb).max(0.0);
    let [sx, sy] = lg.spacing;
    let ax = f32::from(lg.child_alignment % 3) * 0.5;
    let ay = f32::from(lg.child_alignment / 3) * 0.5;
    let n = active_indices.len();

    if lg.kind == UiLayoutGroupKind::Grid {
        let cell_w = lg.cell_size[0].max(1.0);
        let cell_h = lg.cell_size[1].max(1.0);
        let cols = match lg.constraint {
            1 => lg.constraint_count.max(1),
            2 => n.div_ceil(lg.constraint_count.max(1)).max(1),
            _ => (((iw + sx) / (cell_w + sx).max(1.0)).floor().max(1.0)) as usize,
        };
        let rows = n.div_ceil(cols).max(1);
        let grid_w = cols as f32 * cell_w + cols.saturating_sub(1) as f32 * sx;
        let grid_h = rows as f32 * cell_h + rows.saturating_sub(1) as f32 * sy;
        let start_x = ix + ax * (iw - grid_w).max(0.0);
        let start_y = iy + ay * (ih - grid_h).max(0.0);
        let flip_h = (lg.start_corner & 1) != 0;
        let flip_v = (lg.start_corner & 2) != 0;

        for (seq, &ci) in active_indices.iter().enumerate() {
            let (mut col, mut row) = if lg.start_axis == 1 {
                (seq / rows, seq % rows)
            } else {
                (seq % cols, seq / cols)
            };
            if flip_h {
                col = cols.saturating_sub(1).saturating_sub(col);
            }
            if flip_v {
                row = rows.saturating_sub(1).saturating_sub(row);
            }
            let cx = start_x + col as f32 * (cell_w + sx);
            let cy = start_y + row as f32 * (cell_h + sy);
            out[ci] = Some([cx, cy, cell_w, cell_h]);
        }
        return out;
    }

    // Horizontal / Vertical linear layout group
    let mut sizes: Vec<[f32; 2]> = active_indices
        .iter()
        .map(|&ci| {
            let ch = &parent.children[ci];
            let ckey = (ch as *const UiCanvasNode) as usize;
            let pref = pref_memo.get(&ckey).copied().unwrap_or([
                ch.size_delta[0].max(0.0),
                ch.size_delta[1].max(0.0),
            ]);
            let solved = solve_rect_transform_top_down(ch, iw.max(pref[0]), ih.max(pref[1]));
            let mut w = if lg.child_control_width { pref[0] } else { solved[2] };
            let mut h = if lg.child_control_height { pref[1] } else { solved[3] };
            if lg.child_scale_width {
                w *= ch.scale[0].abs();
            }
            if lg.child_scale_height {
                h *= ch.scale[1].abs();
            }
            [w, h]
        })
        .collect();

    if lg.kind == UiLayoutGroupKind::Vertical {
        if lg.child_control_width && lg.child_force_expand_width && iw > 0.5 {
            for sz in &mut sizes {
                sz[0] = sz[0].max(iw);
            }
        }
        let sum_h: f32 = sizes.iter().map(|s| s[1]).sum();
        let total_h = sum_h + if n > 1 { sy * (n - 1) as f32 } else { 0.0 };
        let extra_h = (ih - total_h).max(0.0);
        let mut cursor_y = if lg.child_control_height && lg.child_force_expand_height && extra_h > 0.5 {
            let add = extra_h / n as f32;
            for sz in &mut sizes {
                sz[1] += add;
            }
            iy
        } else {
            iy + ay * extra_h
        };
        for (&ci, sz) in active_indices.iter().zip(sizes.iter()) {
            let cx = ix + ax * (iw - sz[0]);
            out[ci] = Some([cx, cursor_y, sz[0], sz[1]]);
            cursor_y += sz[1] + sy;
        }
    } else {
        if lg.child_control_height && lg.child_force_expand_height && ih > 0.5 {
            for sz in &mut sizes {
                sz[1] = sz[1].max(ih);
            }
        }
        let sum_w: f32 = sizes.iter().map(|s| s[0]).sum();
        let total_w = sum_w + if n > 1 { sx * (n - 1) as f32 } else { 0.0 };
        let extra_w = (iw - total_w).max(0.0);
        let mut cursor_x = if lg.child_control_width && lg.child_force_expand_width && extra_w > 0.5 {
            let add = extra_w / n as f32;
            for sz in &mut sizes {
                sz[0] += add;
            }
            ix
        } else {
            ix + ax * extra_w
        };
        for (&ci, sz) in active_indices.iter().zip(sizes.iter()) {
            let cy = iy + ay * (ih - sz[1]);
            out[ci] = Some([cursor_x, cy, sz[0], sz[1]]);
            cursor_x += sz[0] + sx;
        }
    }

    out
}

fn is_full_stretch_root(node: &UiCanvasNode) -> bool {
    (node.anchor_min[0] - 0.0).abs() < 1e-4
        && (node.anchor_min[1] - 0.0).abs() < 1e-4
        && (node.anchor_max[0] - 1.0).abs() < 1e-4
        && (node.anchor_max[1] - 1.0).abs() < 1e-4
        && node.size_delta[0].abs() < 1e-2
        && node.size_delta[1].abs() < 1e-2
}

/// Converts a single Unity `RectTransform` into top-down `[x, y, width, height]` relative to a
/// parent of size `(pw, ph)` pixels.
pub fn solve_rect_transform_top_down(node: &UiCanvasNode, pw: f32, ph: f32) -> [f32; 4] {
    let ax0 = node.anchor_min[0] * pw;
    let ax1 = node.anchor_max[0] * pw;
    let ay0 = node.anchor_min[1] * ph;
    let ay1 = node.anchor_max[1] * ph;

    // Unity UI bottom-up bounds before local scale around pivot.
    let left_unscaled = ax0 + node.anchored_position[0] - node.size_delta[0] * node.pivot[0];
    let right_unscaled =
        ax1 + node.anchored_position[0] + node.size_delta[0] * (1.0 - node.pivot[0]);
    let bottom_unscaled = ay0 + node.anchored_position[1] - node.size_delta[1] * node.pivot[1];
    let top_bu_unscaled =
        ay1 + node.anchored_position[1] + node.size_delta[1] * (1.0 - node.pivot[1]);

    let pivot_x = left_unscaled + (right_unscaled - left_unscaled) * node.pivot[0];
    let pivot_y_bu = bottom_unscaled + (top_bu_unscaled - bottom_unscaled) * node.pivot[1];

    let sx = if node.scale[0].abs() < 1e-5 {
        0.0
    } else {
        node.scale[0]
    };
    let sy = if node.scale[1].abs() < 1e-5 {
        0.0
    } else {
        node.scale[1]
    };

    let sx0 = pivot_x + (left_unscaled - pivot_x) * sx;
    let sx1 = pivot_x + (right_unscaled - pivot_x) * sx;
    let sy0 = pivot_y_bu + (bottom_unscaled - pivot_y_bu) * sy;
    let sy1 = pivot_y_bu + (top_bu_unscaled - pivot_y_bu) * sy;

    let left = sx0.min(sx1);
    let right = sx0.max(sx1);
    let bottom = sy0.min(sy1);
    let top_bu = sy0.max(sy1);

    let top = ph - top_bu;
    let width = (right - left).max(0.0);
    let height = (top_bu - bottom).max(0.0);
    [left, top, width, height]
}

fn infer_reference_resolution(
    scaler_ref: Option<[f32; 2]>,
    root: &UiCanvasNode,
) -> (f32, f32) {
    if let Some([w, h]) = scaler_ref
        && w >= 64.0
        && h >= 64.0
    {
        return (w, h);
    }
    if (root.anchor_min[0] - root.anchor_max[0]).abs() < 1e-4
        && (root.anchor_min[1] - root.anchor_max[1]).abs() < 1e-4
        && root.size_delta[0] >= 200.0
        && root.size_delta[1] >= 150.0
        && root.size_delta[0] <= 4096.0
        && root.size_delta[1] <= 4096.0
    {
        return (root.size_delta[0], root.size_delta[1]);
    }
    (DEFAULT_UI_REF_WIDTH, DEFAULT_UI_REF_HEIGHT)
}

#[derive(Default)]
struct CollectedUiCompMeta {
    clips_children: bool,
    show_mask_graphic: bool,
    is_interactive: bool,
    widget_kind: Option<String>,
    widget_props: Option<UiWidgetPropsDto>,
    layout_group: Option<UiLayoutGroupDto>,
    content_size_fitter: Option<UiContentSizeFitterDto>,
    layout_element: Option<UiLayoutElementDto>,
    aspect_ratio_fitter: Option<UiAspectRatioFitterDto>,
    graphic: Option<UiGraphic>,
    graphic_enabled: bool,
    outline_color: Option<[f32; 4]>,
}

fn parse_rect_offset(val: Option<&UnityValue>) -> [f32; 4] {
    let Some(map) = val.and_then(UnityValue::as_object) else {
        return [0.0; 4];
    };
    let left = map.get("m_Left").and_then(extract_f32).unwrap_or(0.0);
    let top = map.get("m_Top").and_then(extract_f32).unwrap_or(0.0);
    let right = map.get("m_Right").and_then(extract_f32).unwrap_or(0.0);
    let bottom = map.get("m_Bottom").and_then(extract_f32).unwrap_or(0.0);
    [left, top, right, bottom]
}

fn collect_node_go_map(node: &UiCanvasNode, out: &mut HashMap<AssetRef, String>) {
    out.insert(node.go_ref, node.name.clone());
    for ch in &node.children {
        collect_node_go_map(ch, out);
    }
}

fn collect_subtree_bindings_recursive(
    workspace: &UnityWorkspaceData,
    node: &UiCanvasNode,
    go_map: &HashMap<AssetRef, String>,
    bindings: &mut Vec<UiScriptBindingDto>,
    seen: &mut HashSet<(String, String, Option<usize>, AssetRef)>,
) {
    if bindings.len() >= 2048 {
        return;
    }
    if let Some(go_obj) = read_workspace_object(workspace, node.go_ref) {
        for comp_ref in collect_go_component_refs(workspace, node.go_ref.file_index, &go_obj) {
            let Some(comp_obj) = read_workspace_object(workspace, comp_ref) else {
                continue;
            };
            if comp_obj.info.type_id != 114 {
                continue;
            }
            let script_name = comp_obj
                .get("m_Script")
                .and_then(|v| resolve_pptr_ref(workspace, comp_ref.file_index, v))
                .and_then(|sr| read_workspace_object(workspace, sr))
                .and_then(|so| {
                    extract_string_prop(&so, "m_ClassName")
                        .or_else(|| extract_string_prop(&so, "m_Name"))
                })
                .unwrap_or_default();
            if script_name.is_empty()
                || matches!(
                    script_name.as_str(),
                    "CanvasScaler"
                        | "GraphicRaycaster"
                        | "Image"
                        | "RawImage"
                        | "Text"
                        | "TextMeshProUGUI"
                        | "TMP_Text"
                        | "Button"
                        | "EmptyButton"
                        | "RadioButton"
                        | "Toggle"
                        | "Slider"
                        | "Scrollbar"
                        | "InputField"
                        | "TMP_InputField"
                        | "Dropdown"
                        | "TMP_Dropdown"
                        | "ScrollRect"
                        | "Mask"
                        | "RectMask2D"
                        | "Outline"
                        | "Shadow"
                        | "HorizontalLayoutGroup"
                        | "VerticalLayoutGroup"
                        | "GridLayoutGroup"
                        | "ContentSizeFitter"
                        | "LayoutElement"
                        | "AspectRatioFitter"
                )
            {
                continue;
            }
            for (field, val) in comp_obj.class.properties() {
                if field.starts_with("m_GameObject")
                    || field == "m_Script"
                    || field == "m_Enabled"
                    || field == "m_Name"
                {
                    continue;
                }
                let p_refs: Vec<(Option<usize>, AssetRef)> = match val {
                    UnityValue::Object(_) => resolve_pptr_ref(workspace, comp_ref.file_index, val)
                        .into_iter()
                        .map(|r| (None, r))
                        .collect(),
                    UnityValue::Array(arr) => arr
                        .iter()
                        .enumerate()
                        .take(512)
                        .filter_map(|(idx, item)| {
                            resolve_pptr_ref(workspace, comp_ref.file_index, item)
                                .map(|r| (Some(idx), r))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                for (slot_index, target_ref) in p_refs {
                    if let Some(go_target) = resolve_game_object_from_comp(workspace, target_ref)
                        && let Some(t_name) = go_map.get(&go_target)
                        && seen.insert((script_name.clone(), field.clone(), slot_index, go_target))
                    {
                        bindings.push(UiScriptBindingDto {
                            script_name: script_name.clone(),
                            field_name: field.clone(),
                            slot_index,
                            target_name: t_name.clone(),
                            target_go_ref: go_target,
                        });
                        if bindings.len() >= 2048 {
                            return;
                        }
                    }
                }
            }
        }
    }
    for ch in &node.children {
        collect_subtree_bindings_recursive(workspace, ch, go_map, bindings, seen);
    }
}

fn populate_script_bindings(workspace: &UnityWorkspaceData, doc: &mut UiCanvasDocument) {
    let mut go_map = HashMap::new();
    collect_node_go_map(&doc.root, &mut go_map);
    let mut bindings = Vec::new();
    let mut seen = HashSet::new();
    collect_subtree_bindings_recursive(workspace, &doc.root, &go_map, &mut bindings, &mut seen);
    doc.script_bindings = bindings;
}

fn collect_relative_paths_crc_map(
    node: &UiCanvasNode,
    prefix: &str,
    root_prefix: &str,
    out: &mut HashMap<u32, String>,
) {
    for ch in &node.children {
        let rel_anim = if prefix.is_empty() {
            ch.name.clone()
        } else {
            format!("{prefix}/{}", ch.name)
        };
        let rel_root = if root_prefix.is_empty() {
            ch.name.clone()
        } else {
            format!("{root_prefix}/{}", ch.name)
        };
        out.entry(unity_path_crc32(&rel_anim))
            .or_insert_with(|| rel_root.clone());
        collect_relative_paths_crc_map(ch, &rel_anim, &rel_root, out);
    }
}

fn collect_ui_clips_recursive(
    workspace: &UnityWorkspaceData,
    node: &UiCanvasNode,
    root_prefix: &str,
    seen_clips: &mut HashSet<AssetRef>,
    out: &mut Vec<DecodedUiAnimationClip>,
) {
    let mut clip_refs: Vec<(AssetRef, bool)> = Vec::new();
    if let Some(go_obj) = read_workspace_object(workspace, node.go_ref) {
        for comp_ref in collect_go_component_refs(workspace, node.go_ref.file_index, &go_obj) {
            let Some(comp_obj) = read_workspace_object(workspace, comp_ref) else {
                continue;
            };
            match comp_obj.info.type_id {
                95 => {
                    // Animator -> m_Controller (91 AnimatorController) -> m_AnimationClips
                    if let Some(ctrl_val) = comp_obj.get("m_Controller")
                        && let Some(ctrl_ref) =
                            resolve_pptr_ref(workspace, comp_ref.file_index, ctrl_val)
                        && let Some(ctrl_obj) = read_workspace_object(workspace, ctrl_ref)
                        && let Some(arr) = ctrl_obj
                            .get("m_AnimationClips")
                            .and_then(UnityValue::as_array)
                    {
                        for item in arr {
                            if let Some(cr) = resolve_pptr_ref(workspace, ctrl_ref.file_index, item)
                            {
                                clip_refs.push((cr, false));
                            }
                        }
                    }
                }
                111 => {
                    // Legacy Animation -> m_Clip / m_Animations
                    if let Some(cval) = comp_obj.get("m_Clip")
                        && let Some(cr) = resolve_pptr_ref(workspace, comp_ref.file_index, cval)
                    {
                        clip_refs.push((cr, true));
                    }
                    if let Some(arr) = comp_obj.get("m_Animations").and_then(UnityValue::as_array)
                    {
                        for item in arr {
                            if let Some(cr) = resolve_pptr_ref(workspace, comp_ref.file_index, item)
                            {
                                clip_refs.push((cr, true));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if !clip_refs.is_empty() {
        let mut crc_to_root_path: HashMap<u32, String> = HashMap::new();
        // Path hash 0 targets the Animator component owner GameObject itself.
        crc_to_root_path.insert(0, root_prefix.to_owned());
        crc_to_root_path.insert(unity_path_crc32(""), root_prefix.to_owned());
        collect_relative_paths_crc_map(node, "", root_prefix, &mut crc_to_root_path);

        for (clip_ref, legacy_hint) in clip_refs {
            if out.len() >= 16 || !seen_clips.insert(clip_ref) {
                continue;
            }
            let Some(clip_obj) = read_workspace_object(workspace, clip_ref) else {
                continue;
            };
            if clip_obj.info.type_id != 74 {
                continue;
            }
            if let Ok(mut decoded) = decode_ui_animation_clip(&clip_obj)
                && !decoded.tracks.is_empty()
            {
                // Animator loops by default when wrap_mode == 0 inside looping UI emblems.
                if !legacy_hint && decoded.wrap_mode == 0 {
                    decoded.wrap_mode = 2;
                }
                for track in &mut decoded.tracks {
                    if let Some(resolved) = crc_to_root_path.get(&track.path_hash)
                        && !resolved.is_empty()
                    {
                        track.path_name = Some(resolved.clone());
                    }
                }
                out.push(decoded);
            }
        }
    }

    for ch in &node.children {
        let next_prefix = if root_prefix.is_empty() {
            ch.name.clone()
        } else {
            format!("{root_prefix}/{}", ch.name)
        };
        collect_ui_clips_recursive(workspace, ch, &next_prefix, seen_clips, out);
    }
}

fn populate_ui_animations(workspace: &UnityWorkspaceData, doc: &mut UiCanvasDocument) {
    let mut seen = HashSet::new();
    let mut clips = Vec::new();
    collect_ui_clips_recursive(workspace, &doc.root, "", &mut seen, &mut clips);
    doc.animations = clips;
}

struct ExtractContext<'a> {
    workspace: &'a UnityWorkspaceData,
    tpk: &'a dyn TypeTreeRegistry,
    cache: &'a mut UiExtractionCache,
    max_nodes: usize,
    visited_gos: HashSet<AssetRef>,
    textures: Vec<UiTextureAssetMeta>,
    tex_index_by_key: HashMap<String, usize>,
    images: HashMap<String, DecodedImage>,
    warnings: Vec<String>,
    seen_warnings: HashSet<String>,
    stats: UiCanvasStats,
    scale_mode: UiScaleMode,
    match_width_or_height: f32,
    scaler_ref_size: Option<[f32; 2]>,
}

impl<'a> ExtractContext<'a> {
    fn warn_once(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.seen_warnings.insert(msg.clone()) && self.warnings.len() < 64 {
            self.warnings.push(msg);
        }
    }

    fn extract_node(&mut self, go_ref: AssetRef, is_root: bool) -> Option<UiCanvasNode> {
        if self.stats.node_count >= self.max_nodes {
            self.stats.truncated = true;
            return None;
        }
        if !self.visited_gos.insert(go_ref) {
            return None;
        }
        let go_obj = read_workspace_object(self.workspace, go_ref)?;
        if go_obj.info.type_id != 1 {
            return None;
        }

        let name = extract_string_prop(&go_obj, "m_Name")
            .unwrap_or_else(|| format!("GameObject_{}", go_ref.path_id));
        let active_self = extract_bool_prop(&go_obj, "m_IsActive").unwrap_or(true);

        let mut rect_ref: Option<AssetRef> = None;
        let mut rect_obj: Option<UnityObject> = None;
        let mut child_transform_refs: Vec<AssetRef> = Vec::new();
        let mut has_canvas = false;
        let mut canvas_sorting_order = None;
        let mut canvas_group_alpha = None;
        let mut ignore_parent_groups = false;
        let mut meta = CollectedUiCompMeta {
            show_mask_graphic: true,
            graphic_enabled: true,
            ..Default::default()
        };

        for comp_ref in collect_go_component_refs(self.workspace, go_ref.file_index, &go_obj) {
            let Some(comp_obj) = read_workspace_object(self.workspace, comp_ref) else {
                continue;
            };
            match comp_obj.info.type_id {
                224 | 4 => {
                    if rect_obj.is_none() || comp_obj.info.type_id == 224 {
                        if comp_obj.info.type_id == 224 {
                            self.stats.rect_transform_count += 1;
                        }
                        rect_ref = Some(comp_ref);
                        child_transform_refs = collect_children_refs(
                            self.workspace,
                            comp_ref.file_index,
                            &comp_obj,
                        );
                        rect_obj = Some(comp_obj);
                    }
                }
                222 => {
                    self.stats.canvas_renderer_count += 1;
                }
                223 => {
                    has_canvas = true;
                    self.stats.has_canvas_component = true;
                    if let Some(order) = comp_obj.get("m_SortingOrder").and_then(extract_i32_field) {
                        canvas_sorting_order = Some(order);
                    }
                }
                225 => {
                    if extract_bool_prop(&comp_obj, "m_Enabled").unwrap_or(true) {
                        canvas_group_alpha = comp_obj
                            .get("m_Alpha")
                            .and_then(extract_f32)
                            .map(|a| a.clamp(0.0, 1.0));
                        ignore_parent_groups =
                            extract_bool_prop(&comp_obj, "m_IgnoreParentGroups").unwrap_or(false);
                    }
                }
                114 => {
                    let script_name =
                        self.resolve_script_name(comp_ref.file_index, &comp_obj);
                    let enabled = extract_bool_prop(&comp_obj, "m_Enabled").unwrap_or(true);
                    self.classify_ui_monobehaviour(
                        comp_ref.file_index,
                        &comp_obj,
                        script_name.as_deref(),
                        enabled,
                        is_root,
                        &mut meta,
                    );
                }
                _ => {}
            }
        }

        if let Some(UiGraphic::Text {
            outline_color: ref mut text_outline,
            ..
        }) = meta.graphic
            && text_outline.is_none()
        {
            *text_outline = meta.outline_color;
        }

        self.stats.node_count += 1;
        if active_self {
            self.stats.active_node_count += 1;
        }
        if meta.is_interactive {
            self.stats.interactive_count += 1;
        }
        if let Some(ref g) = meta.graphic {
            self.stats.graphic_count += 1;
            if active_self && meta.graphic_enabled {
                self.stats.active_graphic_count += 1;
            }
            match g {
                UiGraphic::Image { .. } => self.stats.image_count += 1,
                UiGraphic::RawImage { .. } => self.stats.raw_image_count += 1,
                UiGraphic::Text { .. } => self.stats.text_count += 1,
            }
        }

        let RectTransformFields {
            anchor_min,
            anchor_max,
            anchored_position,
            size_delta,
            pivot,
            scale,
            rotation_z_deg,
        } = parse_rect_transform_fields(rect_obj.as_ref());

        let mut children = Vec::with_capacity(child_transform_refs.len());
        for child_tr_ref in child_transform_refs {
            if self.stats.node_count >= self.max_nodes {
                self.stats.truncated = true;
                break;
            }
            let Some(child_go_ref) = resolve_game_object_from_comp(self.workspace, child_tr_ref)
            else {
                continue;
            };
            if let Some(child_node) = self.extract_node(child_go_ref, false) {
                children.push(child_node);
            }
        }

        Some(UiCanvasNode {
            name,
            go_ref,
            rect_ref,
            active_self,
            active_in_hierarchy: active_self,
            anchor_min,
            anchor_max,
            anchored_position,
            size_delta,
            pivot,
            scale,
            rotation_z_deg,
            canvas_rect: [0.0; 4],
            local_rect: [0.0; 4],
            flip_x: scale[0] < 0.0,
            flip_y: scale[1] < 0.0,
            canvas_group_alpha,
            ignore_parent_groups,
            effective_alpha: canvas_group_alpha.unwrap_or(1.0),
            clips_children: meta.clips_children,
            show_mask_graphic: meta.show_mask_graphic,
            has_canvas,
            canvas_sorting_order,
            graphic_enabled: meta.graphic_enabled,
            is_interactive: meta.is_interactive,
            widget_kind: meta.widget_kind,
            widget_props: meta.widget_props,
            layout_group: meta.layout_group,
            content_size_fitter: meta.content_size_fitter,
            layout_element: meta.layout_element,
            aspect_ratio_fitter: meta.aspect_ratio_fitter,
            graphic: meta.graphic,
            children,
        })
    }

    fn classify_ui_monobehaviour(
        &mut self,
        file_index: usize,
        obj: &UnityObject,
        script_name: Option<&str>,
        enabled: bool,
        is_root: bool,
        meta: &mut CollectedUiCompMeta,
    ) {
        let sname = script_name.unwrap_or("");

        // 1. CanvasScaler (record reference resolution when found near/at root)
        if sname == "CanvasScaler"
            || (obj.get("m_ReferenceResolution").is_some() && obj.get("m_UiScaleMode").is_some())
        {
            if is_root || self.scaler_ref_size.is_none() {
                let mode = match obj.get("m_UiScaleMode").and_then(extract_i32_field) {
                    Some(0) => UiScaleMode::ConstantPixelSize,
                    Some(1) => UiScaleMode::ScaleWithScreenSize,
                    Some(2) => UiScaleMode::ConstantPhysicalSize,
                    _ => UiScaleMode::Unknown,
                };
                self.scale_mode = mode;
                if let Some(m) = obj.get("m_MatchWidthOrHeight").and_then(extract_f32) {
                    self.match_width_or_height = m.clamp(0.0, 1.0);
                }
                if let Some(res) = obj.get("m_ReferenceResolution").and_then(extract_vec2)
                    && res[0] >= 64.0
                    && res[1] >= 64.0
                {
                    self.scaler_ref_size = Some(res);
                }
            }
            return;
        }

        // 2. Mask / RectMask2D
        if sname == "Mask" || obj.get("m_ShowMaskGraphic").is_some() {
            if enabled {
                meta.clips_children = true;
                meta.show_mask_graphic = extract_bool_prop(obj, "m_ShowMaskGraphic").unwrap_or(true);
            }
            return;
        }
        if sname == "RectMask2D" {
            if enabled {
                meta.clips_children = true;
                meta.show_mask_graphic = false;
            }
            return;
        }

        // 3. Outline / Shadow effect tint
        if sname == "Outline" || sname == "Shadow" || obj.get("m_EffectColor").is_some() {
            if enabled
                && meta.outline_color.is_none()
                && let Some(c) = obj.get("m_EffectColor").and_then(extract_rgba)
            {
                meta.outline_color = Some(c);
            }
            return;
        }

        // 4a. Auto-Layout & Size Fitters
        if sname == "VerticalLayoutGroup"
            || sname == "HorizontalLayoutGroup"
            || (obj.get("m_ChildControlWidth").is_some() && obj.get("m_Spacing").is_some())
        {
            if enabled && meta.layout_group.is_none() {
                let kind = if sname == "HorizontalLayoutGroup" {
                    UiLayoutGroupKind::Horizontal
                } else {
                    UiLayoutGroupKind::Vertical
                };
                let sp = obj.get("m_Spacing").and_then(extract_f32).unwrap_or(0.0);
                meta.layout_group = Some(UiLayoutGroupDto {
                    kind,
                    padding: parse_rect_offset(obj.get("m_Padding")),
                    spacing: [sp, sp],
                    child_alignment: obj
                        .get("m_ChildAlignment")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 8) as u8,
                    child_control_width: extract_bool_prop(obj, "m_ChildControlWidth").unwrap_or(true),
                    child_control_height: extract_bool_prop(obj, "m_ChildControlHeight").unwrap_or(true),
                    child_force_expand_width: extract_bool_prop(obj, "m_ChildForceExpandWidth")
                        .unwrap_or(true),
                    child_force_expand_height: extract_bool_prop(obj, "m_ChildForceExpandHeight")
                        .unwrap_or(true),
                    child_scale_width: extract_bool_prop(obj, "m_ChildScaleWidth").unwrap_or(false),
                    child_scale_height: extract_bool_prop(obj, "m_ChildScaleHeight").unwrap_or(false),
                    reverse_arrangement: extract_bool_prop(obj, "m_ReverseArrangement")
                        .unwrap_or(false),
                    cell_size: [100.0, 100.0],
                    start_corner: 0,
                    start_axis: 0,
                    constraint: 0,
                    constraint_count: 1,
                });
                if meta.widget_kind.is_none() {
                    meta.widget_kind = Some(if sname.is_empty() {
                        "VerticalLayoutGroup".to_owned()
                    } else {
                        sname.to_owned()
                    });
                }
            }
            return;
        }
        if sname == "GridLayoutGroup"
            || (obj.get("m_CellSize").is_some() && obj.get("m_Constraint").is_some())
        {
            if enabled && meta.layout_group.is_none() {
                meta.layout_group = Some(UiLayoutGroupDto {
                    kind: UiLayoutGroupKind::Grid,
                    padding: parse_rect_offset(obj.get("m_Padding")),
                    spacing: obj.get("m_Spacing").and_then(extract_vec2).unwrap_or([0.0, 0.0]),
                    child_alignment: obj
                        .get("m_ChildAlignment")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 8) as u8,
                    child_control_width: false,
                    child_control_height: false,
                    child_force_expand_width: false,
                    child_force_expand_height: false,
                    child_scale_width: false,
                    child_scale_height: false,
                    reverse_arrangement: false,
                    cell_size: obj
                        .get("m_CellSize")
                        .and_then(extract_vec2)
                        .unwrap_or([100.0, 100.0]),
                    start_corner: obj
                        .get("m_StartCorner")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 3) as u8,
                    start_axis: obj
                        .get("m_StartAxis")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 1) as u8,
                    constraint: obj
                        .get("m_Constraint")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 2) as u8,
                    constraint_count: obj
                        .get("m_ConstraintCount")
                        .and_then(extract_i32_field)
                        .unwrap_or(2)
                        .max(1) as usize,
                });
                if meta.widget_kind.is_none() {
                    meta.widget_kind = Some("GridLayoutGroup".to_owned());
                }
            }
            return;
        }
        if sname == "ContentSizeFitter"
            || (obj.get("m_HorizontalFit").is_some() && obj.get("m_VerticalFit").is_some())
        {
            if enabled {
                meta.content_size_fitter = Some(UiContentSizeFitterDto {
                    horizontal_fit: obj
                        .get("m_HorizontalFit")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 2) as u8,
                    vertical_fit: obj
                        .get("m_VerticalFit")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 2) as u8,
                });
            }
            return;
        }
        if sname == "LayoutElement"
            || (obj.get("m_IgnoreLayout").is_some() && obj.get("m_PreferredWidth").is_some())
        {
            if enabled {
                meta.layout_element = Some(UiLayoutElementDto {
                    ignore_layout: extract_bool_prop(obj, "m_IgnoreLayout").unwrap_or(false),
                    min_width: obj.get("m_MinWidth").and_then(extract_f32).unwrap_or(-1.0),
                    min_height: obj.get("m_MinHeight").and_then(extract_f32).unwrap_or(-1.0),
                    preferred_width: obj
                        .get("m_PreferredWidth")
                        .and_then(extract_f32)
                        .unwrap_or(-1.0),
                    preferred_height: obj
                        .get("m_PreferredHeight")
                        .and_then(extract_f32)
                        .unwrap_or(-1.0),
                    flexible_width: obj
                        .get("m_FlexibleWidth")
                        .and_then(extract_f32)
                        .unwrap_or(-1.0),
                    flexible_height: obj
                        .get("m_FlexibleHeight")
                        .and_then(extract_f32)
                        .unwrap_or(-1.0),
                });
            }
            return;
        }
        if sname == "AspectRatioFitter" || obj.get("m_AspectMode").is_some() {
            if enabled {
                meta.aspect_ratio_fitter = Some(UiAspectRatioFitterDto {
                    mode: obj
                        .get("m_AspectMode")
                        .and_then(extract_i32_field)
                        .unwrap_or(0)
                        .clamp(0, 4) as u8,
                    ratio: obj.get("m_AspectRatio").and_then(extract_f32).unwrap_or(1.0),
                });
            }
            return;
        }

        // 4b. Interactive widgets & Selectable states
        if matches!(
            sname,
            "Button" | "EmptyButton" | "RadioButton" | "Toggle" | "Slider" | "Scrollbar"
                | "InputField" | "TMP_InputField" | "Dropdown" | "TMP_Dropdown" | "ScrollRect"
        ) || (obj.get("m_Transition").is_some() && obj.get("m_Colors").is_some())
        {
            if enabled {
                meta.is_interactive = true;
            }
            if meta.widget_kind.is_none() {
                meta.widget_kind = Some(if sname.is_empty() {
                    "Button".to_owned()
                } else {
                    sname.to_owned()
                });
            }
            let props = meta.widget_props.get_or_insert_with(Default::default);
            if let Some(c) = obj.get("m_Colors").and_then(UnityValue::as_object) {
                let norm = c.get("m_NormalColor").and_then(extract_rgba).unwrap_or([1.0; 4]);
                let high = c
                    .get("m_HighlightedColor")
                    .and_then(extract_rgba)
                    .unwrap_or([0.96, 0.96, 0.96, 1.0]);
                let press = c
                    .get("m_PressedColor")
                    .and_then(extract_rgba)
                    .unwrap_or([0.78, 0.78, 0.78, 1.0]);
                let dis = c
                    .get("m_DisabledColor")
                    .and_then(extract_rgba)
                    .unwrap_or([0.78, 0.78, 0.78, 0.5]);
                props.colors = Some([norm, high, press, dis]);
            }
            if obj.get("m_MaxValue").is_some() && obj.get("m_Value").is_some() {
                let min_v = obj.get("m_MinValue").and_then(extract_f32).unwrap_or(0.0);
                let max_v = obj.get("m_MaxValue").and_then(extract_f32).unwrap_or(1.0);
                let cur_v = obj.get("m_Value").and_then(extract_f32).unwrap_or(0.5);
                props.slider_range = Some([min_v, max_v, cur_v]);
            }
            if let Some(on) = extract_bool_prop(obj, "m_IsOn") {
                props.is_on = Some(on);
            }
            if obj.get("m_Content").is_some() && obj.get("m_Horizontal").is_some() {
                props.scroll_hv = Some([
                    extract_bool_prop(obj, "m_Horizontal").unwrap_or(false),
                    extract_bool_prop(obj, "m_Vertical").unwrap_or(true),
                ]);
            }
            if let Some(lim) = obj.get("m_CharacterLimit").and_then(extract_i32_field) {
                props.char_limit = Some(lim);
                props.is_password = obj
                    .get("m_ContentType")
                    .and_then(extract_i32_field)
                    .is_some_and(|ct| ct == 7);
            }
            return;
        }
        if sname.ends_with("LayoutGroup") && meta.widget_kind.is_none() {
            meta.widget_kind = Some(sname.to_owned());
            return;
        }

        // 5. RawImage (must come before Image!)
        if sname == "RawImage"
            || (obj.get("m_Texture").is_some() && obj.get("m_UVRect").is_some())
        {
            if meta.graphic.is_none() {
                let color = obj
                    .get("m_Color")
                    .and_then(extract_rgba)
                    .unwrap_or([1.0, 1.0, 1.0, 1.0]);
                let uv_rect = obj
                    .get("m_UVRect")
                    .and_then(extract_rect4)
                    .unwrap_or([0.0, 0.0, 1.0, 1.0]);
                let raw_name = extract_string_prop(obj, "RawName").filter(|s| !s.is_empty());
                let tex_val = obj.get("m_Texture");
                let has_pptr = has_nonzero_pptr(tex_val);
                let tex_ref = tex_val.and_then(|v| resolve_pptr_ref(self.workspace, file_index, v));
                if has_pptr && tex_ref.is_none() {
                    self.stats.missing_texture_count += 1;
                    self.warn_once("External UI Texture2D bundle is not loaded in workspace");
                }
                let texture_key = tex_ref
                    .and_then(|r| self.load_ui_texture(r, false))
                    .map(|m| m.key);
                let missing_texture = (has_pptr || raw_name.is_some()) && texture_key.is_none();
                meta.graphic_enabled = enabled;
                meta.graphic = Some(UiGraphic::RawImage {
                    color,
                    texture_key,
                    missing_texture,
                    uv_rect,
                    raw_name,
                });
            }
            return;
        }

        // 6. uGUI Text
        if sname == "Text" || (obj.get("m_Text").is_some() && obj.get("m_FontData").is_some()) {
            if meta.graphic.is_none() {
                let parsed = self.parse_ugui_text(file_index, obj);
                meta.graphic_enabled = enabled;
                meta.graphic = Some(parsed);
            }
            return;
        }

        // 7. TextMeshProUGUI / TMP_Text
        if sname.contains("TextMeshPro")
            || sname == "TMP_Text"
            || (obj.get("m_text").is_some() && obj.get("m_fontSize").is_some())
        {
            if meta.graphic.is_none() {
                let parsed = parse_tmp_text(obj);
                meta.graphic_enabled = enabled;
                meta.graphic = Some(parsed);
            }
            return;
        }

        // 8. uGUI Image / custom Image subclass
        if meta.graphic.is_none()
            && (sname == "Image"
                || sname.ends_with("Image")
                || obj.get("m_Sprite").is_some()
                || (obj.get("RawName").is_some() && obj.get("m_Color").is_some()))
        {
            let color = obj
                .get("m_Color")
                .and_then(extract_rgba)
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);
            let image_type = match obj.get("m_Type").and_then(extract_i32_field) {
                Some(1) => UiImageType::Sliced,
                Some(2) => UiImageType::Tiled,
                Some(3) => UiImageType::Filled,
                _ => UiImageType::Simple,
            };
            let preserve_aspect = extract_bool_prop(obj, "m_PreserveAspect").unwrap_or(false);
            let fill_amount = obj
                .get("m_FillAmount")
                .and_then(extract_f32)
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            let raw_name = extract_string_prop(obj, "RawName").filter(|s| !s.is_empty());

            let sprite_val = obj.get("m_Sprite");
            let has_pptr = has_nonzero_pptr(sprite_val);
            let sprite_ref =
                sprite_val.and_then(|v| resolve_pptr_ref(self.workspace, file_index, v));
            if has_pptr && sprite_ref.is_none() {
                self.stats.missing_texture_count += 1;
                self.warn_once("External UI Sprite/SpriteAtlas bundle is not loaded in workspace");
            }
            let mut border = [0.0; 4];
            let texture_key = sprite_ref
                .and_then(|r| self.load_ui_texture(r, true))
                .map(|m| {
                    border = m.border;
                    m.key
                });
            let missing_texture = (has_pptr || raw_name.is_some()) && texture_key.is_none();
            meta.graphic_enabled = enabled;
            meta.graphic = Some(UiGraphic::Image {
                color,
                texture_key,
                missing_texture,
                image_type,
                preserve_aspect,
                fill_amount,
                border,
                raw_name,
            });
        }
    }

    fn parse_ugui_text(&mut self, file_index: usize, obj: &UnityObject) -> UiGraphic {
        let text = extract_string_prop(obj, "m_Text").unwrap_or_default();
        let color = obj
            .get("m_Color")
            .and_then(extract_rgba)
            .unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let fd = obj.get("m_FontData").and_then(UnityValue::as_object);
        let font_size = fd
            .and_then(|m| m.get("m_FontSize"))
            .and_then(extract_f32)
            .unwrap_or(20.0)
            .max(1.0);
        let alignment = fd
            .and_then(|m| m.get("m_Alignment"))
            .and_then(extract_i32_field)
            .map(unity_text_alignment_from_i32)
            .unwrap_or(UiTextAlignment::UpperLeft);
        let rich_text = fd
            .and_then(|m| m.get("m_RichText"))
            .and_then(extract_bool_val)
            .unwrap_or(true);
        let best_fit = fd
            .and_then(|m| m.get("m_BestFit"))
            .and_then(extract_bool_val)
            .unwrap_or(false);
        let min_size = fd
            .and_then(|m| m.get("m_MinSize"))
            .and_then(extract_f32)
            .unwrap_or(10.0);
        let max_size = fd
            .and_then(|m| m.get("m_MaxSize"))
            .and_then(extract_f32)
            .unwrap_or(40.0);
        let line_spacing = fd
            .and_then(|m| m.get("m_LineSpacing"))
            .and_then(extract_f32)
            .unwrap_or(1.0);

        let font_ref = fd
            .and_then(|m| m.get("m_Font"))
            .and_then(|v| resolve_pptr_ref(self.workspace, file_index, v));
        let font_name = font_ref.and_then(|fr| {
            read_workspace_object(self.workspace, fr)
                .and_then(|fo| extract_string_prop(&fo, "m_Name"))
                .filter(|n| !n.is_empty())
        });

        UiGraphic::Text {
            text,
            color,
            font_size,
            alignment,
            rich_text,
            best_fit,
            min_size,
            max_size,
            line_spacing,
            font_name,
            font_ref,
            outline_color: None,
        }
    }

    fn resolve_script_name(&mut self, file_index: usize, obj: &UnityObject) -> Option<String> {
        let script_val = obj.get("m_Script")?;
        let script_ref = resolve_pptr_ref(self.workspace, file_index, script_val)?;
        if let Some(cached) = self.cache.script_names.get(&script_ref) {
            return Some(cached.clone());
        }
        let script_obj = read_workspace_object(self.workspace, script_ref)?;
        let cls = extract_string_prop(&script_obj, "m_ClassName")
            .or_else(|| extract_string_prop(&script_obj, "m_Name"))
            .unwrap_or_default();
        self.cache.script_names.insert(script_ref, cls.clone());
        Some(cls)
    }

    fn load_ui_texture(
        &mut self,
        target: AssetRef,
        prefer_sprite: bool,
    ) -> Option<UiTextureAssetMeta> {
        let key = make_texture_key(target);
        if let Some(&idx) = self.tex_index_by_key.get(&key) {
            return self.textures.get(idx).cloned();
        }

        let cache_key = (target, prefer_sprite);
        if !self.cache.resolved_textures.contains_key(&cache_key) {
            let decoded = self.decode_texture_or_sprite(target, prefer_sprite);
            self.cache.resolved_textures.insert(cache_key, decoded);
        }

        match self.cache.resolved_textures.get(&cache_key) {
            Some(Ok((meta, img))) => {
                let idx = self.textures.len();
                self.textures.push(meta.clone());
                self.tex_index_by_key.insert(meta.key.clone(), idx);
                self.images.insert(meta.key.clone(), img.clone());
                self.stats.resolved_texture_count += 1;
                Some(meta.clone())
            }
            Some(Err(err)) => {
                self.stats.missing_texture_count += 1;
                self.warn_once(err.clone());
                None
            }
            None => None,
        }
    }

    fn decode_texture_or_sprite(
        &mut self,
        target: AssetRef,
        prefer_sprite: bool,
    ) -> Result<(UiTextureAssetMeta, DecodedImage), String> {
        let Some(loaded) = self.workspace.get_file(target.file_index) else {
            return Err(format!(
                "UI sprite/texture file #{} is not loaded",
                target.file_index
            ));
        };
        let Some(info) = loaded.file.objects.iter().find(|o| o.path_id == target.path_id) else {
            return Err(format!(
                "UI asset {}:{} not found in loaded bundle '{}'",
                target.file_index, target.path_id, loaded.virtual_path
            ));
        };
        let obj = read_object(loaded, info)
            .map_err(|e| format!("UI asset {}:{} read error: {e}", target.file_index, target.path_id))?;
        let ver = loaded.file.unity_version.as_str();
        let key = make_texture_key(target);

        if prefer_sprite && obj.info.type_id == 213 {
            let border = extract_sprite_border(&obj);
            let sp = decode_sprite_cached(
                &obj,
                target.file_index,
                ver,
                self.workspace,
                self.tpk,
                &mut self.cache.backing_textures,
            )
            .map_err(|e| format!("Sprite '{}' ({}:{}): {e}", sp_label(&obj), target.file_index, target.path_id))?;
            let meta = UiTextureAssetMeta {
                key,
                name: if sp.name.is_empty() {
                    format!("Sprite_{}", target.path_id)
                } else {
                    sp.name
                },
                source_ref: target,
                is_sprite: true,
                width: sp.image.width,
                height: sp.image.height,
                border,
                asset_path: None,
            };
            Ok((meta, sp.image))
        } else if obj.info.type_id == 28 {
            let img = decode_texture2d(&obj, ver, &self.workspace.streams, TextureDecodeOptions::default())
                .map_err(|e| format!("Texture2D {}:{}: {e}", target.file_index, target.path_id))?;
            let name = extract_string_prop(&obj, "m_Name")
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("Texture_{}", target.path_id));
            let meta = UiTextureAssetMeta {
                key,
                name,
                source_ref: target,
                is_sprite: false,
                width: img.width,
                height: img.height,
                border: [0.0; 4],
                asset_path: None,
            };
            Ok((meta, img))
        } else if obj.info.type_id == 213 {
            let border = extract_sprite_border(&obj);
            let sp = decode_sprite_cached(
                &obj,
                target.file_index,
                ver,
                self.workspace,
                self.tpk,
                &mut self.cache.backing_textures,
            )
            .map_err(|e| format!("Sprite {}:{}: {e}", target.file_index, target.path_id))?;
            let meta = UiTextureAssetMeta {
                key,
                name: sp.name,
                source_ref: target,
                is_sprite: true,
                width: sp.image.width,
                height: sp.image.height,
                border,
                asset_path: None,
            };
            Ok((meta, sp.image))
        } else {
            Err(format!(
                "UI asset {}:{} has unsupported texture class {}",
                target.file_index, target.path_id, obj.info.type_id
            ))
        }
    }
}

fn sp_label(obj: &UnityObject) -> String {
    extract_string_prop(obj, "m_Name").unwrap_or_else(|| format!("Sprite_{}", obj.info.path_id))
}

fn make_texture_key(target: AssetRef) -> String {
    format!("ui_{}_{}", target.file_index, target.path_id)
}

fn read_workspace_object(workspace: &UnityWorkspaceData, target: AssetRef) -> Option<UnityObject> {
    let loaded = workspace.get_file(target.file_index)?;
    let info = loaded
        .file
        .objects
        .iter()
        .find(|o| o.path_id == target.path_id)?;
    read_object(loaded, info).ok()
}

fn resolve_owning_game_object(
    workspace: &UnityWorkspaceData,
    start_ref: AssetRef,
) -> Option<AssetRef> {
    let obj = read_workspace_object(workspace, start_ref)?;
    if obj.info.type_id == 1 {
        return Some(start_ref);
    }
    let go_val = obj.get("m_GameObject")?;
    resolve_pptr_ref(workspace, start_ref.file_index, go_val)
}

fn resolve_game_object_from_comp(
    workspace: &UnityWorkspaceData,
    comp_ref: AssetRef,
) -> Option<AssetRef> {
    let obj = read_workspace_object(workspace, comp_ref)?;
    if obj.info.type_id == 1 {
        return Some(comp_ref);
    }
    let go_val = obj.get("m_GameObject")?;
    resolve_pptr_ref(workspace, comp_ref.file_index, go_val)
}

/// Walks up `m_Father` chain to find the highest ancestor owning a `Canvas` (`223`), or if no
/// ancestor owns a `Canvas`, returns the highest `RectTransform` (`224`) / `Transform` root.
fn walk_up_to_ui_root(workspace: &UnityWorkspaceData, start_go: AssetRef) -> AssetRef {
    let mut visited = HashSet::new();
    let mut cur_go = start_go;
    let mut best_canvas_go: Option<AssetRef> = None;
    let mut top_go = start_go;

    for _ in 0..64 {
        if !visited.insert(cur_go) {
            break;
        }
        let Some(go_obj) = read_workspace_object(workspace, cur_go) else {
            break;
        };
        top_go = cur_go;

        let mut father_tr: Option<AssetRef> = None;
        for comp_ref in collect_go_component_refs(workspace, cur_go.file_index, &go_obj) {
            let Some(comp_obj) = read_workspace_object(workspace, comp_ref) else {
                continue;
            };
            if comp_obj.info.type_id == 223 {
                best_canvas_go = Some(cur_go);
            } else if matches!(comp_obj.info.type_id, 224 | 4)
                && let Some(fval) = comp_obj.get("m_Father")
            {
                father_tr = resolve_pptr_ref(workspace, comp_ref.file_index, fval);
            }
        }

        let Some(parent_tr) = father_tr else {
            break;
        };
        let Some(parent_go) = resolve_game_object_from_comp(workspace, parent_tr) else {
            break;
        };
        cur_go = parent_go;
    }

    best_canvas_go.unwrap_or(top_go)
}

fn collect_go_component_refs(
    workspace: &UnityWorkspaceData,
    file_index: usize,
    go_obj: &UnityObject,
) -> Vec<AssetRef> {
    let Some(arr) = go_obj.get("m_Component").and_then(UnityValue::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(pptr_val) = item.as_object().and_then(|m| m.get("component")).or(Some(item))
            && let Some(r) = resolve_pptr_ref(workspace, file_index, pptr_val)
        {
            out.push(r);
        }
    }
    out
}

fn collect_children_refs(
    workspace: &UnityWorkspaceData,
    file_index: usize,
    tr_obj: &UnityObject,
) -> Vec<AssetRef> {
    let Some(arr) = tr_obj.get("m_Children").and_then(UnityValue::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| resolve_pptr_ref(workspace, file_index, v))
        .collect()
}

struct RectTransformFields {
    anchor_min: [f32; 2],
    anchor_max: [f32; 2],
    anchored_position: [f32; 2],
    size_delta: [f32; 2],
    pivot: [f32; 2],
    scale: [f32; 3],
    rotation_z_deg: f32,
}

fn parse_rect_transform_fields(rect_obj: Option<&UnityObject>) -> RectTransformFields {
    let Some(obj) = rect_obj else {
        return RectTransformFields {
            anchor_min: [0.5, 0.5],
            anchor_max: [0.5, 0.5],
            anchored_position: [0.0, 0.0],
            size_delta: [100.0, 100.0],
            pivot: [0.5, 0.5],
            scale: [1.0, 1.0, 1.0],
            rotation_z_deg: 0.0,
        };
    };
    if obj.info.type_id == 4 {
        // Fallback Transform embedded inside a UI hierarchy
        let pos = obj
            .get("m_LocalPosition")
            .and_then(extract_vec3)
            .unwrap_or([0.0, 0.0, 0.0]);
        let scale = obj
            .get("m_LocalScale")
            .and_then(extract_vec3)
            .unwrap_or([1.0, 1.0, 1.0]);
        return RectTransformFields {
            anchor_min: [0.5, 0.5],
            anchor_max: [0.5, 0.5],
            anchored_position: [pos[0], pos[1]],
            size_delta: [0.0, 0.0],
            pivot: [0.5, 0.5],
            scale,
            rotation_z_deg: 0.0,
        };
    }

    let anchor_min = obj
        .get("m_AnchorMin")
        .and_then(extract_vec2)
        .unwrap_or([0.5, 0.5]);
    let anchor_max = obj
        .get("m_AnchorMax")
        .and_then(extract_vec2)
        .unwrap_or([0.5, 0.5]);
    let anchored_position = obj
        .get("m_AnchoredPosition")
        .and_then(extract_vec2)
        .unwrap_or([0.0, 0.0]);
    let size_delta = obj
        .get("m_SizeDelta")
        .and_then(extract_vec2)
        .unwrap_or([100.0, 100.0]);
    let pivot = obj
        .get("m_Pivot")
        .and_then(extract_vec2)
        .unwrap_or([0.5, 0.5]);
    let scale = obj
        .get("m_LocalScale")
        .and_then(extract_vec3)
        .unwrap_or([1.0, 1.0, 1.0]);
    let rotation_z_deg = obj
        .get("m_LocalRotation")
        .and_then(extract_quaternion_z_deg)
        .unwrap_or(0.0);

    RectTransformFields {
        anchor_min,
        anchor_max,
        anchored_position,
        size_delta,
        pivot,
        scale,
        rotation_z_deg,
    }
}

fn parse_tmp_text(obj: &UnityObject) -> UiGraphic {
    let text = extract_string_prop(obj, "m_text").unwrap_or_default();
    let color = obj
        .get("m_fontColor")
        .or_else(|| obj.get("m_Color"))
        .and_then(extract_rgba)
        .unwrap_or([1.0, 1.0, 1.0, 1.0]);
    let font_size = obj
        .get("m_fontSize")
        .and_then(extract_f32)
        .unwrap_or(24.0)
        .max(1.0);
    let align_raw = obj
        .get("m_textAlignment")
        .and_then(extract_i32_field)
        .unwrap_or(257);
    let alignment = tmp_text_alignment_from_i32(align_raw);
    let best_fit = extract_bool_prop(obj, "m_enableAutoSizing").unwrap_or(false);
    let min_size = obj
        .get("m_fontSizeMin")
        .and_then(extract_f32)
        .unwrap_or(10.0);
    let max_size = obj
        .get("m_fontSizeMax")
        .and_then(extract_f32)
        .unwrap_or(72.0);
    let line_spacing = obj
        .get("m_lineSpacing")
        .and_then(extract_f32)
        .unwrap_or(0.0);

    UiGraphic::Text {
        text,
        color,
        font_size,
        alignment,
        rich_text: extract_bool_prop(obj, "m_isRichText").unwrap_or(true),
        best_fit,
        min_size,
        max_size,
        line_spacing,
        font_name: None,
        font_ref: None,
        outline_color: None,
    }
}

fn extract_sprite_border(obj: &UnityObject) -> [f32; 4] {
    let Some(map) = obj.get("m_Border").and_then(UnityValue::as_object) else {
        return [0.0; 4];
    };
    let left = map.get("x").and_then(extract_f32).unwrap_or(0.0).max(0.0);
    let bottom = map.get("y").and_then(extract_f32).unwrap_or(0.0).max(0.0);
    let right = map.get("z").and_then(extract_f32).unwrap_or(0.0).max(0.0);
    let top = map.get("w").and_then(extract_f32).unwrap_or(0.0).max(0.0);
    [left, top, right, bottom]
}

fn unity_text_alignment_from_i32(v: i32) -> UiTextAlignment {
    match v {
        0 => UiTextAlignment::UpperLeft,
        1 => UiTextAlignment::UpperCenter,
        2 => UiTextAlignment::UpperRight,
        3 => UiTextAlignment::MiddleLeft,
        4 => UiTextAlignment::MiddleCenter,
        5 => UiTextAlignment::MiddleRight,
        6 => UiTextAlignment::LowerLeft,
        7 => UiTextAlignment::LowerCenter,
        8 => UiTextAlignment::LowerRight,
        _ => UiTextAlignment::UpperLeft,
    }
}

fn tmp_text_alignment_from_i32(v: i32) -> UiTextAlignment {
    // TextMeshPro bitmask: lower byte horizontal (1=Left,2=Center,4=Right,8=Justified),
    // upper bits vertical (256=Top,512=Middle,1024=Bottom).
    let horiz = match v & 0x0F {
        2 => 1,
        4 => 2,
        _ => 0,
    };
    let vert = if (v & 1024) != 0 {
        2
    } else if (v & 512) != 0 {
        1
    } else {
        0
    };
    unity_text_alignment_from_i32(vert * 3 + horiz)
}

fn extract_string_prop(obj: &UnityObject, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str().map(ToOwned::to_owned))
}

fn extract_bool_val(v: &UnityValue) -> Option<bool> {
    match v {
        UnityValue::Bool(b) => Some(*b),
        UnityValue::Integer(i) => Some(*i != 0),
        _ => None,
    }
}

fn extract_bool_prop(obj: &UnityObject, key: &str) -> Option<bool> {
    obj.get(key).and_then(extract_bool_val)
}

fn extract_f32(v: &UnityValue) -> Option<f32> {
    match v {
        UnityValue::Float(f) => Some(*f as f32),
        UnityValue::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

fn extract_vec2(v: &UnityValue) -> Option<[f32; 2]> {
    let map = v.as_object()?;
    Some([
        map.get("x").and_then(extract_f32).unwrap_or(0.0),
        map.get("y").and_then(extract_f32).unwrap_or(0.0),
    ])
}

fn extract_vec3(v: &UnityValue) -> Option<[f32; 3]> {
    let map = v.as_object()?;
    Some([
        map.get("x").and_then(extract_f32).unwrap_or(0.0),
        map.get("y").and_then(extract_f32).unwrap_or(0.0),
        map.get("z").and_then(extract_f32).unwrap_or(0.0),
    ])
}

fn extract_rgba(v: &UnityValue) -> Option<[f32; 4]> {
    let map = v.as_object()?;
    Some([
        map.get("r").and_then(extract_f32).unwrap_or(1.0).clamp(0.0, 1.0),
        map.get("g").and_then(extract_f32).unwrap_or(1.0).clamp(0.0, 1.0),
        map.get("b").and_then(extract_f32).unwrap_or(1.0).clamp(0.0, 1.0),
        map.get("a").and_then(extract_f32).unwrap_or(1.0).clamp(0.0, 1.0),
    ])
}

fn extract_rect4(v: &UnityValue) -> Option<[f32; 4]> {
    let map = v.as_object()?;
    Some([
        map.get("x").and_then(extract_f32).unwrap_or(0.0),
        map.get("y").and_then(extract_f32).unwrap_or(0.0),
        map.get("width")
            .or_else(|| map.get("w"))
            .and_then(extract_f32)
            .unwrap_or(1.0),
        map.get("height")
            .or_else(|| map.get("h"))
            .and_then(extract_f32)
            .unwrap_or(1.0),
    ])
}

fn extract_quaternion_z_deg(v: &UnityValue) -> Option<f32> {
    let map = v.as_object()?;
    let x = map.get("x").and_then(extract_f32).unwrap_or(0.0);
    let y = map.get("y").and_then(extract_f32).unwrap_or(0.0);
    let z = map.get("z").and_then(extract_f32).unwrap_or(0.0);
    let w = map.get("w").and_then(extract_f32).unwrap_or(1.0);
    let siny_cosp = 2.0 * (w * z + x * y);
    let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
    Some((-siny_cosp.atan2(cosy_cosp)).to_degrees())
}

// Keep extract_i64_field cleanly used if future components read i64 IDs directly.
fn has_nonzero_pptr(val: Option<&UnityValue>) -> bool {
    val.and_then(UnityValue::as_object)
        .and_then(|m| m.get("m_PathID"))
        .and_then(extract_i64_field)
        .is_some_and(|id| id != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_node(name: &str) -> UiCanvasNode {
        UiCanvasNode {
            name: name.to_owned(),
            go_ref: AssetRef {
                file_index: 0,
                path_id: 1,
            },
            rect_ref: Some(AssetRef {
                file_index: 0,
                path_id: 2,
            }),
            size_delta: [100.0, 100.0],
            ..Default::default()
        }
    }

    #[test]
    fn rect_transform_fixed_anchor_and_pivot_top_down() {
        let mut node = dummy_node("CenterOffset");
        node.anchor_min = [0.5, 0.5];
        node.anchor_max = [0.5, 0.5];
        node.anchored_position = [100.0, 50.0]; // +50 UP in Unity
        node.size_delta = [200.0, 80.0];
        node.pivot = [0.5, 0.5];

        let rect = solve_rect_transform_top_down(&node, 1920.0, 1080.0);
        // x: 960 + 100 - 100 = 960
        // top_bu: 540 + 50 + 40 = 630 -> top = 1080 - 630 = 450
        assert!((rect[0] - 960.0).abs() < 1e-3, "got {:?}", rect);
        assert!((rect[1] - 450.0).abs() < 1e-3, "got {:?}", rect);
        assert!((rect[2] - 200.0).abs() < 1e-3, "got {:?}", rect);
        assert!((rect[3] - 80.0).abs() < 1e-3, "got {:?}", rect);
    }

    #[test]
    fn rect_transform_stretch_anchor_margins() {
        let mut node = dummy_node("Stretched");
        node.anchor_min = [0.0, 0.0];
        node.anchor_max = [1.0, 1.0];
        node.anchored_position = [0.0, 0.0];
        node.size_delta = [-40.0, -20.0];
        node.pivot = [0.5, 0.5];

        let rect = solve_rect_transform_top_down(&node, 1920.0, 1080.0);
        assert!((rect[0] - 20.0).abs() < 1e-3, "got {:?}", rect);
        assert!((rect[1] - 10.0).abs() < 1e-3, "got {:?}", rect);
        assert!((rect[2] - 1880.0).abs() < 1e-3, "got {:?}", rect);
        assert!((rect[3] - 1060.0).abs() < 1e-3, "got {:?}", rect);
    }

    #[test]
    fn recompute_ui_layout_propagates_groups_and_active_flags() {
        let mut root = dummy_node("Canvas");
        root.has_canvas = true;
        root.canvas_group_alpha = Some(0.5);

        let mut child = dummy_node("ChildPanel");
        child.anchor_min = [0.0, 1.0]; // Top-left anchor!
        child.anchor_max = [0.0, 1.0];
        child.pivot = [0.0, 1.0];
        child.anchored_position = [30.0, -25.0];
        child.size_delta = [400.0, 120.0];
        child.canvas_group_alpha = Some(0.5);
        child.scale = [-1.0, 1.0, 1.0];

        let mut grandchild = dummy_node("IndependentChild");
        grandchild.ignore_parent_groups = true;
        grandchild.canvas_group_alpha = Some(0.8);
        grandchild.active_self = false;

        child.children.push(grandchild);
        root.children.push(child);

        let mut doc = UiCanvasDocument {
            name: "TestCanvas".into(),
            root_ref: AssetRef { file_index: 0, path_id: 1 },
            selected_ref: None,
            source_bundle: "ui/testpanel.ab".into(),
            reference_width: 1280.0,
            reference_height: 720.0,
            scale_mode: UiScaleMode::ScaleWithScreenSize,
            match_width_or_height: 0.5,
            root,
            textures: Vec::new(),
            script_bindings: Vec::new(),
            animations: Vec::new(),
            warnings: Vec::new(),
            stats: UiCanvasStats::default(),
        };

        recompute_ui_layout(&mut doc, 1280.0, 720.0);

        assert_eq!(doc.root.canvas_rect, [0.0, 0.0, 1280.0, 720.0]);
        let ch = &doc.root.children[0];
        assert!((ch.effective_alpha - 0.25).abs() < 1e-4);
        assert!(ch.flip_x && !ch.flip_y);
        // pivot (0, 1) mirrored by scale.x = -1 flips box left by width 400
        assert!((ch.canvas_rect[1] - 25.0).abs() < 1e-3, "rect={:?}", ch.canvas_rect);
        assert!((ch.canvas_rect[2] - 400.0).abs() < 1e-3);
        assert!((ch.canvas_rect[3] - 120.0).abs() < 1e-3);

        let gch = &ch.children[0];
        assert!((gch.effective_alpha - 0.8).abs() < 1e-4);
        assert!(!gch.active_in_hierarchy);
    }

    #[test]
    fn vertical_layout_group_and_content_size_fitter_pivot_expansion() {
        let mut root = dummy_node("Canvas");
        root.has_canvas = true;

        let mut list = dummy_node("RightMenu");
        list.anchor_min = [1.0, 1.0];
        list.anchor_max = [1.0, 1.0];
        list.pivot = [1.0, 1.0];
        list.anchored_position = [-27.0, -9.0];
        list.size_delta = [0.0, 0.0];
        list.layout_group = Some(UiLayoutGroupDto {
            kind: UiLayoutGroupKind::Vertical,
            padding: [0.0; 4],
            spacing: [0.0, 50.0],
            child_alignment: 0,
            child_control_width: false,
            child_control_height: false,
            child_force_expand_width: true,
            child_force_expand_height: true,
            child_scale_width: false,
            child_scale_height: false,
            reverse_arrangement: false,
            cell_size: [100.0, 100.0],
            start_corner: 0,
            start_axis: 0,
            constraint: 0,
            constraint_count: 1,
        });
        list.content_size_fitter = Some(UiContentSizeFitterDto {
            horizontal_fit: 2,
            vertical_fit: 2,
        });

        for name in ["btn_repair", "btn_announce", "btn_cs"] {
            let mut b = dummy_node(name);
            b.anchor_min = [0.0, 0.0];
            b.anchor_max = [0.0, 0.0];
            b.size_delta = [100.0, 100.0];
            list.children.push(b);
        }
        root.children.push(list);

        let mut doc = UiCanvasDocument {
            name: "LoginStack".into(),
            root_ref: AssetRef { file_index: 0, path_id: 1 },
            selected_ref: None,
            source_bundle: "ui/loginpanel.ab".into(),
            reference_width: 1920.0,
            reference_height: 1080.0,
            scale_mode: UiScaleMode::ScaleWithScreenSize,
            match_width_or_height: 0.5,
            root,
            textures: Vec::new(),
            script_bindings: Vec::new(),
            animations: Vec::new(),
            warnings: Vec::new(),
            stats: UiCanvasStats::default(),
        };

        recompute_ui_layout(&mut doc, 1920.0, 1080.0);
        let l = &doc.root.children[0];
        assert!((l.canvas_rect[0] - 1793.0).abs() < 1e-3, "got {:?}", l.canvas_rect);
        assert!((l.canvas_rect[1] - 9.0).abs() < 1e-3);
        assert!((l.canvas_rect[2] - 100.0).abs() < 1e-3);
        assert!((l.canvas_rect[3] - 400.0).abs() < 1e-3);
        assert!((l.children[0].canvas_rect[1] - 9.0).abs() < 1e-3);
        assert!((l.children[1].canvas_rect[1] - 159.0).abs() < 1e-3);
        assert!((l.children[2].canvas_rect[1] - 309.0).abs() < 1e-3);
    }

    #[test]
    fn grid_layout_group_places_cells_in_rows_and_cols() {
        let mut root = dummy_node("Canvas");
        root.has_canvas = true;

        let mut grid = dummy_node("FaceGrid");
        grid.anchor_min = [0.0, 1.0];
        grid.anchor_max = [0.0, 1.0];
        grid.pivot = [0.0, 1.0];
        grid.anchored_position = [20.0, -40.0];
        grid.size_delta = [260.0, 300.0];
        grid.layout_group = Some(UiLayoutGroupDto {
            kind: UiLayoutGroupKind::Grid,
            padding: [10.0, 10.0, 10.0, 10.0],
            spacing: [10.0, 20.0],
            child_alignment: 0,
            child_control_width: false,
            child_control_height: false,
            child_force_expand_width: false,
            child_force_expand_height: false,
            child_scale_width: false,
            child_scale_height: false,
            reverse_arrangement: false,
            cell_size: [100.0, 80.0],
            start_corner: 0,
            start_axis: 0,
            constraint: 1,
            constraint_count: 2,
        });
        for i in 0..3 {
            grid.children.push(dummy_node(&format!("Cell_{i}")));
        }
        root.children.push(grid);

        let mut doc = UiCanvasDocument {
            name: "GridTest".into(),
            root_ref: AssetRef { file_index: 0, path_id: 1 },
            selected_ref: None,
            source_bundle: "ui/establishpanel.ab".into(),
            reference_width: 1920.0,
            reference_height: 1080.0,
            scale_mode: UiScaleMode::ScaleWithScreenSize,
            match_width_or_height: 0.5,
            root,
            textures: Vec::new(),
            script_bindings: Vec::new(),
            animations: Vec::new(),
            warnings: Vec::new(),
            stats: UiCanvasStats::default(),
        };

        recompute_ui_layout(&mut doc, 1920.0, 1080.0);
        let g = &doc.root.children[0];
        assert_eq!(g.children[0].canvas_rect, [30.0, 50.0, 100.0, 80.0]);
        assert_eq!(g.children[1].canvas_rect, [140.0, 50.0, 100.0, 80.0]);
        assert_eq!(g.children[2].canvas_rect, [30.0, 150.0, 100.0, 80.0]);
    }

    #[test]
    fn parent_scale_and_envelope_aspect_propagate_cleanly_to_descendants() {
        let mut root = dummy_node("Canvas");
        root.has_canvas = true;

        let mut scaled = dummy_node("ScaledContainer");
        scaled.size_delta = [400.0, 200.0];
        scaled.scale = [0.5, 0.5, 1.0];

        let mut child = dummy_node("InnerIcon");
        child.size_delta = [200.0, 100.0];
        child.anchored_position = [40.0, 0.0];
        scaled.children.push(child);

        let mut collapsed = dummy_node("ZeroContainer");
        collapsed.size_delta = [400.0, 200.0];
        collapsed.scale = [0.0, 0.0, 0.0];
        let mut zero_child = dummy_node("CollapsedChild");
        zero_child.size_delta = [300.0, 150.0];
        collapsed.children.push(zero_child);

        let mut env = dummy_node("EnvelopeBg");
        env.anchor_min = [0.0, 0.0];
        env.anchor_max = [1.0, 1.0];
        env.size_delta = [0.0, 0.0];
        env.aspect_ratio_fitter = Some(UiAspectRatioFitterDto {
            mode: 4,
            ratio: 2.0,
        });

        root.children.push(scaled);
        root.children.push(collapsed);
        root.children.push(env);

        let mut doc = UiCanvasDocument {
            name: "ScaleAndAspect".into(),
            root_ref: AssetRef { file_index: 0, path_id: 1 },
            selected_ref: None,
            source_bundle: "ui/test.ab".into(),
            reference_width: 1000.0,
            reference_height: 1000.0,
            scale_mode: UiScaleMode::ScaleWithScreenSize,
            match_width_or_height: 0.5,
            root,
            textures: Vec::new(),
            script_bindings: Vec::new(),
            animations: Vec::new(),
            warnings: Vec::new(),
            stats: UiCanvasStats::default(),
        };

        recompute_ui_layout(&mut doc, 1000.0, 1000.0);
        let scaled_ch = &doc.root.children[0].children[0];
        // Child size 200x100 scaled by parent 0.5 -> 100x50, shifted right by 40*0.5 = 20
        assert!((scaled_ch.canvas_rect[2] - 100.0).abs() < 1e-3, "got {:?}", scaled_ch.canvas_rect);
        assert!((scaled_ch.canvas_rect[3] - 50.0).abs() < 1e-3, "got {:?}", scaled_ch.canvas_rect);
        assert!((scaled_ch.canvas_rect[0] - 470.0).abs() < 1e-3, "got {:?}", scaled_ch.canvas_rect);

        let collapsed_ch = &doc.root.children[1].children[0];
        assert_eq!(collapsed_ch.canvas_rect[2], 0.0);
        assert_eq!(collapsed_ch.canvas_rect[3], 0.0);

        let env_node = &doc.root.children[2];
        // 1000x1000 container with aspect 2.0 envelope -> 2000x1000 centered at [-500, 0]
        assert!((env_node.canvas_rect[2] - 2000.0).abs() < 1e-3, "got {:?}", env_node.canvas_rect);
        assert!((env_node.canvas_rect[3] - 1000.0).abs() < 1e-3);
        assert!((env_node.canvas_rect[0] + 500.0).abs() < 1e-3);
    }
}
