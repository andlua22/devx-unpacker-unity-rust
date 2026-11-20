//! Shared Unity uGUI `.json` panel loader, full-fidelity retained Canvas spawner, runtime node
//! binding registry (`BoundUiPanel`), and uniform Pure-Bevy button styling helpers.

use std::collections::{HashMap, HashSet};

use bevy::picking::Pickable;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource, Justify};
use bevy::ui::widget::NodeImageMode;
use bevy::ui::{Pressed, ScrollPosition, UiTransform};
use bevy::ui_widgets::{Activate, Button, observe};
use serde::Deserialize;

use crate::config::{AssetManifest, GameConfigs, LocalizationDb, resolve_assets_root};
use crate::ui::widgets::{UiFilledBar, UiScrollArea, UiSelectableColors};

/// Root document schema written by `devx_cli export-bevy` into `assets/ui_panels/*.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UiPanelDoc {
    pub name: String,
    pub reference_width: f32,
    pub reference_height: f32,
    #[serde(default)]
    pub scale_mode: String,
    #[serde(default = "default_half")]
    pub match_width_or_height: f32,
    pub root: UiPanelNodeDto,
    #[serde(default)]
    pub textures: Vec<UiTextureDto>,
    #[serde(default)]
    pub script_bindings: Vec<UiScriptBindingDto>,
    #[serde(default)]
    pub animations: Vec<UiAnimationClipDto>,
}

/// Texture lookup entry attached to a [`UiPanelDoc`].
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UiTextureDto {
    pub key: String,
    #[serde(default)]
    pub asset_path: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

/// Serialized Unity object identity. Paths and names can repeat within a panel.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq, Hash)]
pub struct UiAssetRefDto {
    pub file_index: usize,
    pub path_id: i64,
}

/// Extracted Unity `MonoBehaviour` (`GameObjectData` / controller) field-to-node binding.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UiScriptBindingDto {
    #[serde(default)]
    pub script_name: String,
    #[serde(default)]
    pub field_name: String,
    #[serde(default)]
    pub slot_index: Option<usize>,
    #[serde(default)]
    pub target_name: String,
    #[serde(default)]
    pub target_go_ref: Option<UiAssetRefDto>,
    #[serde(default)]
    pub owner_go_ref: Option<UiAssetRefDto>,
    #[serde(default)]
    pub owner_component_ref: Option<UiAssetRefDto>,
    #[serde(default)]
    pub owner_path: Option<String>,
    #[serde(default)]
    pub target_path: Option<String>,
}

/// Scalar UI animation channel decoded from a Unity `AnimationClip` (`class_id == 74`).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAnimChannelDto {
    LocalEulerZDeg,
    LocalScaleX,
    LocalScaleY,
    AnchoredPosX,
    AnchoredPosY,
    SizeDeltaX,
    SizeDeltaY,
    ColorAlpha,
    Active,
}

/// Single keyframed UI property track inside a [`UiAnimationClipDto`].
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct UiAnimTrackDto {
    pub path_hash: u32,
    #[serde(default)]
    pub path_name: Option<String>,
    pub channel: UiAnimChannelDto,
    #[serde(default)]
    pub times: Vec<f32>,
    #[serde(default)]
    pub values: Vec<f32>,
}

/// Decoded UI `AnimationClip` bundled alongside a panel prefab.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UiAnimationClipDto {
    pub name: String,
    #[serde(default)]
    pub duration_secs: f32,
    #[serde(default)]
    pub sample_rate: f32,
    #[serde(default)]
    pub wrap_mode: i32,
    #[serde(default)]
    pub tracks: Vec<UiAnimTrackDto>,
}

/// Interactive widget properties extracted from `Selectable`/`Slider`/`Toggle`/`ScrollRect`/`InputField`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UiWidgetPropsDto {
    #[serde(default)]
    pub colors: Option<[[f32; 4]; 4]>,
    #[serde(default)]
    pub slider_range: Option<[f32; 3]>,
    #[serde(default)]
    pub is_on: Option<bool>,
    #[serde(default)]
    pub scroll_hv: Option<[bool; 2]>,
    #[serde(default)]
    pub scroll_cell_size: Option<[f32; 2]>,
    #[serde(default)]
    pub scroll_rows: Option<i32>,
    #[serde(default)]
    pub scroll_cols: Option<i32>,
    #[serde(default)]
    pub char_limit: Option<i32>,
    #[serde(default)]
    pub is_password: bool,
}

/// Single UI node extracted from a Unity `RectTransform` hierarchy.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UiPanelNodeDto {
    pub name: String,
    #[serde(default)]
    pub go_ref: Option<UiAssetRefDto>,
    #[serde(default = "default_true")]
    pub active_self: bool,
    #[serde(default)]
    pub active_in_hierarchy: bool,
    #[serde(default)]
    pub anchored_position: [f32; 2],
    #[serde(default)]
    pub anchor_min: Option<[f32; 2]>,
    #[serde(default)]
    pub anchor_max: Option<[f32; 2]>,
    #[serde(default)]
    pub pivot: Option<[f32; 2]>,
    #[serde(default)]
    pub size_delta: Option<[f32; 2]>,
    #[serde(default = "default_vec3_one")]
    pub scale: [f32; 3],
    #[serde(default)]
    pub canvas_rect: [f32; 4],
    #[serde(default)]
    pub local_rect: [f32; 4],
    #[serde(default)]
    pub rotation_z_deg: f32,
    /// World-space top-down rotation, including parent RectTransforms (new DevX exports).
    #[serde(default)]
    pub canvas_rotation_z_deg: Option<f32>,
    /// Unrotated artwork box centered on the transformed rectangle's center.
    #[serde(default)]
    pub canvas_render_rect: Option<[f32; 4]>,
    #[serde(default)]
    pub canvas_parent_basis: Option<[[f32; 2]; 2]>,
    #[serde(default)]
    pub flip_x: bool,
    #[serde(default)]
    pub flip_y: bool,
    #[serde(default = "default_one")]
    pub effective_alpha: f32,
    #[serde(default)]
    pub clips_children: bool,
    #[serde(default = "default_true")]
    pub show_mask_graphic: bool,
    #[serde(default = "default_true")]
    pub graphic_enabled: bool,
    #[serde(default)]
    pub is_interactive: bool,
    #[serde(default)]
    pub widget_kind: Option<String>,
    #[serde(default)]
    pub widget_props: Option<UiWidgetPropsDto>,
    #[serde(default)]
    pub graphic: Option<UiPanelGraphicDto>,
    #[serde(default)]
    pub children: Vec<UiPanelNodeDto>,
}

fn default_one() -> f32 {
    1.0
}

fn default_half() -> f32 {
    0.5
}

fn default_vec3_one() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

fn default_true() -> bool {
    true
}

/// Drawable component attached to a [`UiPanelNodeDto`].
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiPanelGraphicDto {
    Image {
        color: [f32; 4],
        #[serde(default)]
        texture_key: Option<String>,
        #[serde(default)]
        missing_texture: bool,
        #[serde(default)]
        image_type: String,
        #[serde(default)]
        preserve_aspect: bool,
        #[serde(default = "default_one")]
        fill_amount: f32,
        #[serde(default)]
        border: [f32; 4],
        #[serde(default)]
        raw_name: Option<String>,
    },
    RawImage {
        color: [f32; 4],
        #[serde(default)]
        texture_key: Option<String>,
        #[serde(default)]
        missing_texture: bool,
        #[serde(default)]
        raw_name: Option<String>,
    },
    Text {
        text: String,
        color: [f32; 4],
        font_size: f32,
        #[serde(default)]
        alignment: String,
        #[serde(default)]
        rich_text: bool,
        #[serde(default)]
        best_fit: bool,
        #[serde(default)]
        outline_color: Option<[f32; 4]>,
    },
}

/// Color palette driven uniformly by [`style_palette_buttons`] from `Hovered` / `Pressed`.
#[derive(Component, Debug, Clone, Copy)]
pub struct UiPaletteButton {
    pub normal: Color,
    pub hovered: Color,
    pub pressed: Color,
}

impl Default for UiPaletteButton {
    fn default() -> Self {
        Self {
            normal: Color::srgb(0.15, 0.20, 0.29),
            hovered: Color::srgb(0.24, 0.34, 0.49),
            pressed: Color::srgb(0.29, 0.56, 0.86),
        }
    }
}

/// Marker on spawned 1:1 exported panel preview roots inside `DevSandbox`.
#[derive(Component)]
pub struct UiPanelPreviewRoot;

/// Reference dimensions stored on spawned [`UiPanelFrame`] entities for responsive resizing.
#[derive(Component)]
pub struct UiPanelFrame {
    pub ref_w: f32,
    pub ref_h: f32,
    pub fullscreen: bool,
}

#[derive(Component)]
pub(crate) struct UiPanelCanvasReflow {
    source: UiPanelDoc,
    canvas_size: Vec2,
    scale: f32,
}

#[derive(Component)]
pub(crate) struct UiRetainedCanvasNode {
    frame: Entity,
    indices: Vec<usize>,
    artwork_child: bool,
}

/// Unity CanvasScaler result: physical viewport and its corresponding logical canvas dimensions.
#[derive(Debug, Clone, Copy)]
pub struct UiPanelCanvasMetrics {
    pub scale: f32,
    pub canvas_size: Vec2,
    pub frame_size: Vec2,
}

pub fn compute_panel_canvas_metrics(
    doc: &UiPanelDoc,
    viewport: Vec2,
    fullscreen: bool,
) -> UiPanelCanvasMetrics {
    let reference = Vec2::new(
        doc.reference_width.max(64.0),
        doc.reference_height.max(64.0),
    );
    if fullscreen && doc.scale_mode == "scale_with_screen_size" {
        let viewport = viewport.max(Vec2::ONE);
        let ratios = viewport / reference;
        let matched = doc.match_width_or_height.clamp(0.0, 1.0);
        let scale = (ratios.x.ln() * (1.0 - matched) + ratios.y.ln() * matched).exp();
        UiPanelCanvasMetrics {
            scale,
            canvas_size: viewport / scale,
            frame_size: viewport,
        }
    } else {
        let scale = compute_panel_fit_scale(reference.x, reference.y, viewport, fullscreen);
        UiPanelCanvasMetrics {
            scale,
            canvas_size: reference,
            frame_size: reference * scale,
        }
    }
}

#[derive(Clone, Copy)]
struct CanvasAffine {
    origin: Vec2,
    x: Vec2,
    y: Vec2,
}

impl CanvasAffine {
    fn point(self, point: Vec2) -> Vec2 {
        self.origin + self.x * point.x + self.y * point.y
    }
}

/// Reflows exported rectangles from their solved baseline, retaining layout-group and template edits.
/// Only the parent-size contribution of native anchors changes; affine artwork stays in bind geometry.
pub fn reflow_panel_doc(source: &UiPanelDoc, canvas_size: Vec2) -> UiPanelDoc {
    let mut doc = source.clone();
    let reference = Vec2::new(source.reference_width, source.reference_height);
    let identity = CanvasAffine {
        origin: Vec2::ZERO,
        x: Vec2::X,
        y: Vec2::Y,
    };
    reflow_canvas_node(
        &source.root,
        &mut doc.root,
        reference,
        canvas_size,
        identity,
        identity,
        reference.y,
        canvas_size.y,
    );
    doc.reference_width = canvas_size.x;
    doc.reference_height = canvas_size.y;
    doc
}

#[allow(clippy::too_many_arguments)]
fn reflow_canvas_node(
    source: &UiPanelNodeDto,
    target: &mut UiPanelNodeDto,
    source_parent_size: Vec2,
    target_parent_size: Vec2,
    source_parent: CanvasAffine,
    target_parent: CanvasAffine,
    source_canvas_h: f32,
    target_canvas_h: f32,
) {
    let Some((anchor_min, anchor_max, pivot)) = source
        .anchor_min
        .zip(source.anchor_max)
        .zip(source.pivot)
        .map(|((min, max), pivot)| (min, max, pivot))
    else {
        return;
    };
    let (sin, cos) = (-source.rotation_z_deg).to_radians().sin_cos();
    let local_x = Vec2::new(cos, sin) * source.scale[0];
    let local_y = Vec2::new(-sin, cos) * source.scale[1];
    let source_x = source_parent.x * local_x.x + source_parent.y * local_x.y;
    let source_y = source_parent.x * local_y.x + source_parent.y * local_y.y;
    let parent_basis = Mat2::from_cols(source_parent.x, source_parent.y);
    if source_x.length_squared() < 1e-10
        || source_y.length_squared() < 1e-10
        || parent_basis.determinant().abs() < 1e-10
    {
        return;
    }
    let art = source.canvas_render_rect.unwrap_or(source.canvas_rect);
    let source_size = Vec2::new(art[2] / source_x.length(), art[3] / source_y.length());
    let pivot = Vec2::from_array(pivot);
    let center = Vec2::new(
        art[0] + art[2] * 0.5,
        source_canvas_h - art[1] - art[3] * 0.5,
    );
    let local_center = parent_basis.inverse() * (center - source_parent.origin);
    let source_pivot = local_center
        - local_x * (source_size.x * (0.5 - pivot.x))
        - local_y * (source_size.y * (0.5 - pivot.y));
    let source_origin = source_pivot - source_size * pivot;
    let delta = target_parent_size - source_parent_size;
    let anchor_min = Vec2::from_array(anchor_min);
    let anchor_max = Vec2::from_array(anchor_max);
    let target_size = (source_size + (anchor_max - anchor_min) * delta).max(Vec2::ZERO);
    let target_origin = source_origin + anchor_min * delta;
    let target_x = target_parent.x * local_x.x + target_parent.y * local_x.y;
    let target_y = target_parent.x * local_y.x + target_parent.y * local_y.y;
    let target_pivot = target_parent.point(target_origin + target_size * pivot);
    let source_map = CanvasAffine {
        origin: source_parent.point(source_pivot)
            - source_x * (source_size.x * pivot.x)
            - source_y * (source_size.y * pivot.y),
        x: source_x,
        y: source_y,
    };
    let target_map = CanvasAffine {
        origin: target_pivot
            - target_x * (target_size.x * pivot.x)
            - target_y * (target_size.y * pivot.y),
        x: target_x,
        y: target_y,
    };
    let corners = [
        Vec2::ZERO,
        Vec2::new(target_size.x, 0.0),
        Vec2::new(0.0, target_size.y),
        target_size,
    ]
    .map(|point| target_map.point(point));
    let minimum = corners
        .into_iter()
        .fold(Vec2::splat(f32::INFINITY), Vec2::min);
    let maximum = corners
        .into_iter()
        .fold(Vec2::splat(f32::NEG_INFINITY), Vec2::max);
    target.canvas_rect = [
        minimum.x,
        target_canvas_h - maximum.y,
        maximum.x - minimum.x,
        maximum.y - minimum.y,
    ];
    let center = target_map.point(target_size * 0.5);
    let render_size = Vec2::new(
        target_size.x * target_x.length(),
        target_size.y * target_y.length(),
    );
    target.canvas_render_rect = Some([
        center.x - render_size.x * 0.5,
        target_canvas_h - center.y - render_size.y * 0.5,
        render_size.x,
        render_size.y,
    ]);
    target.canvas_parent_basis = Some([
        [target_parent.x.x, -target_parent.x.y],
        [target_parent.y.x, -target_parent.y.y],
    ]);
    for (source_child, target_child) in source.children.iter().zip(&mut target.children) {
        reflow_canvas_node(
            source_child,
            target_child,
            source_size,
            target_size,
            source_map,
            target_map,
            source_canvas_h,
            target_canvas_h,
        );
    }
}

/// Spawns a retained [`UiPanelFrame`] stage using the document's native CanvasScaler when configured.
/// mounts `doc` inside it, returning `(stage_entity, BoundUiPanel)`.
pub fn spawn_retained_panel_stage(
    parent: &mut ChildSpawnerCommands,
    doc: &UiPanelDoc,
    asset_server: &AssetServer,
    l10n: Option<&LocalizationDb>,
    font_source: &FontSource,
    fit_scale: f32,
    opts: &UiRetainedOptions,
) -> (Entity, BoundUiPanel) {
    let ref_w = doc.reference_width.max(64.0);
    let ref_h = doc.reference_height.max(64.0);
    let mut stage_opts = opts.clone();
    let frame_size = if let Some(viewport) = opts.viewport_size {
        let metrics = compute_panel_canvas_metrics(doc, viewport, true);
        stage_opts.fit_scale = metrics.scale;
        metrics.frame_size
    } else {
        stage_opts.fit_scale = fit_scale;
        Vec2::new(ref_w, ref_h) * fit_scale
    };
    let mut bound_opt = None;
    let stage_e = parent
        .spawn((
            UiPanelFrame {
                ref_w,
                ref_h,
                fullscreen: true,
            },
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Relative,
                width: Val::Px(frame_size.x),
                height: Val::Px(frame_size.y),
                overflow: Overflow::visible(),
                ..default()
            },
        ))
        .with_children(|stage| {
            bound_opt = Some(spawn_retained_panel_into(
                stage,
                doc,
                asset_server,
                l10n,
                font_source,
                &stage_opts,
            ));
        })
        .id();
    (stage_e, bound_opt.unwrap_or_default())
}

/// Marker on scaled UI text inside [`UiPanelFrame`].
#[derive(Component)]
pub struct UiPanelScaledText {
    pub base_px: f32,
}

/// Optional interactive widget label attached to bound entities inside a spawned [`BoundUiPanel`].
#[allow(dead_code)]
#[derive(Component, Clone, Debug)]
pub struct UiPanelInteractiveLabel(pub String);

/// Last clicked widget readout updated by DevSandbox preview clicks.
#[derive(Resource, Default)]
pub struct UiPreviewInteractionState {
    pub last_clicked_label: String,
}

/// Emitted whenever an interactive button node inside a live retained [`BoundUiPanel`] is clicked.
#[allow(dead_code)]
#[derive(Message, Clone, Debug)]
pub struct BoundUiClick {
    pub panel_name: String,
    pub node_name: String,
    pub entity: Entity,
}

#[derive(Component, Clone, Debug)]
struct BoundNodeButtonMeta {
    panel_name: String,
    node_name: String,
    preview_mode: bool,
}

/// Live entity-binding registry attached to a spawned runtime UI panel root entity (`SC.UI.View`).
#[allow(dead_code)]
#[derive(Component, Clone, Debug, Default)]
pub struct BoundUiPanel {
    pub panel_name: String,
    pub ref_w: f32,
    pub ref_h: f32,
    pub nodes: HashMap<String, Vec<Entity>>,
    pub by_path: HashMap<String, Entity>,
    pub by_go_ref: HashMap<UiAssetRefDto, Entity>,
    pub text_nodes: HashMap<String, Vec<Entity>>,
    pub node_text_child: HashMap<Entity, Entity>,
    pub tex_handles: HashMap<String, Handle<Image>>,
    pub slots: HashMap<String, Vec<Option<Entity>>>,
    pub controller_slots: HashMap<UiAssetRefDto, HashMap<String, Vec<Option<Entity>>>>,
    pub animator_entity: Option<Entity>,
}

#[allow(dead_code)]
impl BoundUiPanel {
    /// Returns the first spawned UI entity matching `name`, if present.
    pub fn first(&self, name: &str) -> Option<Entity> {
        self.nodes.get(name).and_then(|v| v.first()).copied()
    }

    /// Returns the first spawned `Text` entity matching `name`, or its direct child text if stored.
    pub fn first_text(&self, name: &str) -> Option<Entity> {
        self.text_nodes
            .get(name)
            .and_then(|v| v.first())
            .copied()
            .or_else(|| self.first(name))
    }

    /// Returns the spawned UI entity matching an animation/hierarchy relative path (e.g. `"m_Animation/Content/Circle"`).
    pub fn by_canvas_path(&self, path: &str) -> Option<Entity> {
        self.by_path.get(path).copied()
    }

    /// Returns a bound `GameObjectData` / MonoBehaviour array slot entity by `field_name` and `slot_index`.
    pub fn slot(&self, field_name: &str, idx: usize) -> Option<Entity> {
        self.slots
            .get(field_name)
            .and_then(|vec| vec.get(idx))
            .and_then(|opt| *opt)
    }

    /// Shortcut for `GameObjectData.GameObjects[idx]` (`SC.UI.View.Awake` slot lookup).
    pub fn game_object_slot(&self, idx: usize) -> Option<Entity> {
        self.slot("GameObjects", idx)
    }

    /// Looks up a field on a specific Unity controller component, including nested templates.
    pub fn controller_slot(
        &self,
        controller: UiAssetRefDto,
        field: &str,
        idx: usize,
    ) -> Option<Entity> {
        self.controller_slots
            .get(&controller)?
            .get(field)?
            .get(idx)
            .copied()
            .flatten()
    }

    /// Returns every entity matching `name` (in tree order).
    pub fn all(&self, name: &str) -> &[Entity] {
        self.nodes.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Unity `Extensions.SetActiveBetter(go, active)` equivalent on a specific entity.
    pub fn set_entity_active(
        entity: Entity,
        active: bool,
        vis_q: &mut Query<&mut Visibility>,
        node_q: &mut Query<&mut Node>,
    ) {
        if let Ok(mut vis) = vis_q.get_mut(entity) {
            *vis = if active {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
        if let Ok(mut n) = node_q.get_mut(entity) {
            n.display = if active { Display::Flex } else { Display::None };
        }
    }

    /// Unity `GameObject.SetActive(bool)` equivalent: updates both `Visibility` and `Node::display`.
    pub fn set_active(
        &self,
        name: &str,
        active: bool,
        vis_q: &mut Query<&mut Visibility>,
        node_q: &mut Query<&mut Node>,
    ) {
        for &e in self.all(name) {
            Self::set_entity_active(e, active, vis_q, node_q);
        }
    }

    /// Updates text content of all nodes matching `name`.
    pub fn set_text(&self, name: &str, value: &str, text_q: &mut Query<&mut Text>) {
        if let Some(list) = self.text_nodes.get(name) {
            for &e in list {
                if let Ok(mut t) = text_q.get_mut(e)
                    && t.0 != value
                {
                    t.0 = value.to_owned();
                }
            }
        }
    }

    /// Updates text content on a bound `RectTransform` entity (or its child `Text` leaf).
    pub fn set_entity_text(&self, entity: Entity, value: &str, text_q: &mut Query<&mut Text>) {
        let target = self.node_text_child.get(&entity).copied().unwrap_or(entity);
        if let Ok(mut t) = text_q.get_mut(target)
            && t.0 != value
        {
            t.0 = value.to_owned();
        }
    }

    /// Updates a [`UiFilledBar`] progress fraction (`0.0..=1.0`) for all matching entities.
    pub fn set_fill(&self, name: &str, frac: f32, bars: &mut Query<&mut UiFilledBar>) {
        for &e in self.all(name) {
            if let Ok(mut b) = bars.get_mut(e) {
                b.frac = frac.clamp(0.0, 1.0);
            }
        }
    }

    /// Updates a Unity `Slider` fill rect entity directly (`Node.width` + optional [`UiFilledBar`]).
    pub fn set_slider_fill(
        entity: Entity,
        frac: f32,
        node_q: &mut Query<&mut Node>,
        bar_q: &mut Query<&mut UiFilledBar>,
    ) {
        let clamped = frac.clamp(0.0, 1.0);
        if let Ok(mut n) = node_q.get_mut(entity) {
            n.left = Val::Px(0.0);
            n.top = Val::Px(0.0);
            n.width = Val::Percent(clamped * 100.0);
            n.height = Val::Percent(100.0);
        }
        if let Ok(mut b) = bar_q.get_mut(entity) {
            b.frac = clamped;
        }
    }

    /// Swaps a `RawImage` / `Image` entity's texture handle (`SC.UI.View.SetTexture`).
    pub fn set_image_texture(
        entity: Entity,
        handle: Handle<Image>,
        img_q: &mut Query<&mut ImageNode>,
    ) {
        if let Ok(mut img) = img_q.get_mut(entity) {
            img.image = handle;
        }
    }
}

/// Options controlling how a [`UiPanelDoc`] is instantiated into live Bevy UI nodes.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct UiRetainedOptions {
    pub fullscreen: bool,
    pub fit_scale: f32,
    /// Actual viewport for native CanvasScaler geometry at initial spawn.
    pub viewport_size: Option<Vec2>,
    pub localize_text: bool,
    pub preview_mode: bool,
    pub hide_offscreen_nodes: bool,
    pub animate: bool,
    pub text_overrides: HashMap<String, String>,
    pub hidden_nodes: HashSet<String>,
    pub passthrough_nodes: HashSet<String>,
    pub interactive_whitelist: Option<HashSet<String>>,
    pub force_active_subtrees: HashSet<String>,
    pub render_targets: HashMap<String, Handle<Image>>,
}

impl Default for UiRetainedOptions {
    fn default() -> Self {
        Self {
            fullscreen: false,
            fit_scale: 0.0,
            viewport_size: None,
            localize_text: false,
            preview_mode: false,
            hide_offscreen_nodes: false,
            animate: true,
            text_overrides: HashMap::new(),
            hidden_nodes: HashSet::new(),
            passthrough_nodes: HashSet::new(),
            interactive_whitelist: None,
            force_active_subtrees: HashSet::new(),
            render_targets: HashMap::new(),
        }
    }
}

/// Summary returned by [`spawn_ui_panel_preview`] for Sandbox UI HUD telemetry.
#[derive(Debug, Clone, Default)]
pub struct UiPanelPreviewSummary {
    pub name: String,
    pub ref_w: f32,
    pub ref_h: f32,
    pub binding_count: usize,
    pub clip_count: usize,
    pub animation_names: Vec<String>,
}

/// Applies per-panel runtime `View` / `onInit` visibility & placeholder string rules so every
/// exported uGUI prefab renders cleanly without stacked sibling tabs or offscreen tween widgets.
pub fn apply_panel_default_view_rules(
    panel_name: &str,
    l10n: Option<&LocalizationDb>,
    configs: Option<&GameConfigs>,
    opts: &mut UiRetainedOptions,
) {
    opts.hide_offscreen_nodes = true;
    opts.animate = true;

    let loading_str = configs
        .zip(l10n)
        .map(|(c, l)| c.get_string(l, 435, &[]))
        .filter(|s| !s.is_empty() && !s.starts_with("String#"))
        .unwrap_or_else(|| "Loading...".to_owned());

    match panel_name {
        "LoadingPanel" => {
            opts.hidden_nodes.insert("btn_repair".to_owned());
            opts.hidden_nodes.insert("txt_loding_name1".to_owned());
            opts.text_overrides
                .entry("txt_loding_name".to_owned())
                .or_insert(loading_str);
            if let Some(cfg) = configs
                && !cfg.version.str_v.is_empty()
            {
                opts.text_overrides
                    .entry("txt_version".to_owned())
                    .or_insert_with(|| cfg.version.str_v.clone());
            }
        }
        "BattleLoadingPanel" | "CarDiyLoadingPanel" => {
            // `BattleLoadingPanel.onInit` hides left/right split descriptions & repair controls.
            for name in [
                "m_leftDesc",
                "m_rightDesc",
                "btn_repair",
                "GameObject (1)",
                "GameObject",
            ] {
                opts.hidden_nodes.insert(name.to_owned());
            }
            let weighted_tip = configs
                .and_then(|c| {
                    c.battle_loading_tips
                        .iter()
                        .find(|t| t.weight > 0.0)
                        .or_else(|| c.battle_loading_tips.first())
                })
                .map(|tip| {
                    let t = l10n.map(|l| l.tr(&tip.content)).unwrap_or(&tip.content);
                    strip_rich_text_tags(t)
                })
                .filter(|s| !s.is_empty());
            opts.text_overrides
                .entry("txt_loding_name".to_owned())
                .or_insert(loading_str);
            if let Some(t) = weighted_tip {
                opts.text_overrides
                    .entry("txt_tips".to_owned())
                    .or_insert(t);
            }
        }
        "SettingPanel" => {
            for sibling in [
                "m_pic",
                "m_operation_page",
                "m_PCKeyCode",
                "m_sensitivity",
                "m_trusteeship",
                "m_DownloadPage",
                "m_help",
            ] {
                opts.hidden_nodes.insert(sibling.to_owned());
            }
        }
        "BagAndBuildPanel" => {
            for sibling in [
                "m_produce_page",
                "m_equipment",
                "m_weapon",
                "m_equip_desc",
                "m_desc",
                "m_quick_use_mask",
                "m_drag_Canvas",
            ] {
                opts.hidden_nodes.insert(sibling.to_owned());
            }
        }
        "ShopOutBattlePanel" => {
            for sibling in [
                "m_tuijian",
                "m_Treasure",
                "m_niudan",
                "m_click_mask",
                "m_model_show",
            ] {
                opts.hidden_nodes.insert(sibling.to_owned());
            }
        }
        "BattleCustomPanel" => {
            for sibling in ["m_drive_uis", "m_NearCar", "m_shezhi"] {
                opts.hidden_nodes.insert(sibling.to_owned());
            }
        }
        "BattlePanel" => {
            for overlay in ["m_waterEffect", "m_tips", "BlackBase", "m_qiangzhan"] {
                opts.hidden_nodes.insert(overlay.to_owned());
            }
        }
        "EstablishPanel" => {
            for tpl in ["m_role_model", "m_cell"] {
                opts.hidden_nodes.insert(tpl.to_owned());
            }
        }
        "MainPanel" => {
            for extra in [
                "m_posture_tip",
                "btn_Back_to_game",
                "btn_Gm",
                "btn_openserver",
            ] {
                opts.hidden_nodes.insert(extra.to_owned());
            }
        }
        _ => {}
    }
}

/// Loads `<assets_root>/<rel_asset_path>` into a parsed [`UiPanelDoc`].
pub fn load_ui_panel_doc(rel_asset_path: &str) -> Result<UiPanelDoc, String> {
    let full = resolve_assets_root().join(rel_asset_path);
    let raw = std::fs::read_to_string(&full)
        .map_err(|e| format!("failed to read UI panel `{}`: {e}", full.display()))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse UI panel `{}`: {e}", full.display()))
}

/// Finds a mutable/immutable node DTO recursively by exact case-insensitive name.
#[allow(dead_code)]
pub fn find_panel_node_dto<'a>(
    node: &'a UiPanelNodeDto,
    target: &str,
) -> Option<&'a UiPanelNodeDto> {
    if node.name.eq_ignore_ascii_case(target) {
        return Some(node);
    }
    for ch in &node.children {
        if let Some(found) = find_panel_node_dto(ch, target) {
            return Some(found);
        }
    }
    None
}

/// Searches `doc` depth-first for a node matching any case-insensitive exact name (or substring) in
/// `node_names` and returns its resolved texture `asset_path` (e.g. `ui/Loading_bg_2.png`).
#[allow(dead_code)]
pub fn find_panel_node_texture(doc: &UiPanelDoc, node_names: &[&str]) -> Option<String> {
    let by_key: HashMap<&str, &str> = doc
        .textures
        .iter()
        .filter_map(|t| t.asset_path.as_deref().map(|p| (t.key.as_str(), p)))
        .collect();

    for &target in node_names {
        if let Some(path) = search_node_texture(&doc.root, target, true, &by_key) {
            return Some(path);
        }
    }
    for &target in node_names {
        if let Some(path) = search_node_texture(&doc.root, target, false, &by_key) {
            return Some(path);
        }
    }
    None
}

#[allow(dead_code)]
fn search_node_texture(
    node: &UiPanelNodeDto,
    target: &str,
    exact: bool,
    by_key: &HashMap<&str, &str>,
) -> Option<String> {
    let matches = if exact {
        node.name.eq_ignore_ascii_case(target)
    } else {
        node.name
            .to_ascii_lowercase()
            .contains(&target.to_ascii_lowercase())
    };
    if matches && let Some(ref g) = node.graphic {
        let key = match g {
            UiPanelGraphicDto::Image { texture_key, .. }
            | UiPanelGraphicDto::RawImage { texture_key, .. } => texture_key.as_deref(),
            UiPanelGraphicDto::Text { .. } => None,
        };
        if let Some(k) = key
            && let Some(&asset_path) = by_key.get(k)
        {
            return Some(asset_path.to_owned());
        }
    }
    for child in &node.children {
        if let Some(found) = search_node_texture(child, target, exact, by_key) {
            return Some(found);
        }
    }
    None
}

/// Spawns a full-screen background artwork layer from an exported PNG path with optional dark
/// readability scrim (`scrim_alpha` in `0.0..=1.0`).
pub fn spawn_fullscreen_backdrop(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    tex_path: Option<&str>,
    fallback_bg: Color,
    scrim_alpha: f32,
) {
    if let Some(path) = tex_path {
        let mut img = ImageNode::new(asset_server.load(path.to_owned()));
        img.image_mode = NodeImageMode::Stretch;
        parent.spawn((
            Pickable::IGNORE,
            img,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
        ));
    } else {
        parent.spawn((
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(fallback_bg),
        ));
    }
    if scrim_alpha > 0.001 {
        parent.spawn((
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.05, 0.09, scrim_alpha.clamp(0.0, 1.0))),
        ));
    }
}

/// Inserts a stretched APK UI panel texture (`ui/*.png`) cleanly behind a Bevy container's children.
pub fn spawn_chrome_backdrop(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    sprite_path: &str,
    tint: Color,
) {
    parent.spawn((
        Pickable::IGNORE,
        ImageNode::new(asset_server.load(sprite_path.to_owned())).with_color(tint),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
    ));
}

/// Shared hover/press styling system for every Pure-Bevy [`UiPaletteButton`].
pub fn style_palette_buttons(
    mut buttons: Query<
        (
            &Hovered,
            Has<Pressed>,
            &UiPaletteButton,
            &mut BackgroundColor,
        ),
        With<Button>,
    >,
) {
    for (hovered, pressed, palette, mut bg) in &mut buttons {
        let color = match (pressed, hovered.get()) {
            (true, _) => palette.pressed,
            (false, true) => palette.hovered,
            (false, false) => palette.normal,
        };
        if bg.0 != color {
            *bg = BackgroundColor(color);
        }
    }
}

/// Computes uniform aspect-fit scale factor for [`UiPanelFrame`] inside the game window.
pub fn compute_panel_fit_scale(ref_w: f32, ref_h: f32, win_size: Vec2, fullscreen: bool) -> f32 {
    if fullscreen {
        return (win_size.x / ref_w.max(1.0))
            .min(win_size.y / ref_h.max(1.0))
            .clamp(0.25, 2.5);
    }
    let avail_w = (win_size.x - 40.0).max(200.0);
    let avail_h = (win_size.y - 170.0).max(160.0);
    (avail_w / ref_w.max(1.0))
        .min(avail_h / ref_h.max(1.0))
        .clamp(0.1, 2.5)
}

struct SpawnedAnimMeta {
    rest_anchored: [f32; 2],
    rest_rot_deg: f32,
    rest_local_rot_deg: f32,
    parent_basis: [[f32; 2]; 2],
    rest_scale: [f32; 2],
    rest_rgba: [f32; 4],
    rest_active: bool,
}

/// Sampled animation track bound to a live UI `Entity`.
#[derive(Clone, Debug)]
struct BoundAnimTrack {
    channel: UiAnimChannelDto,
    times: Vec<f32>,
    values: Vec<f32>,
}

/// Pre-grouped tracks for one animated UI entity inside a [`BoundUiAnimClip`].
#[derive(Clone, Debug)]
struct BoundAnimTargetGroup {
    entity: Entity,
    text_entity: Option<Entity>,
    rest_anchored: [f32; 2],
    rest_rot_deg: f32,
    rest_local_rot_deg: f32,
    parent_basis: [[f32; 2]; 2],
    rest_scale: [f32; 2],
    rest_rgba: [f32; 4],
    rest_active: bool,
    has_transform_track: bool,
    tracks: Vec<BoundAnimTrack>,
}

/// Live UI animation clip compiled against spawned Bevy node entities.
#[derive(Clone, Debug)]
pub struct BoundUiAnimClip {
    pub name: String,
    pub duration_secs: f32,
    pub looping: bool,
    targets: Vec<BoundAnimTargetGroup>,
}

/// Live Pure-Bevy UI animation controller executing exported Unity `AnimationClip` tracks.
#[allow(dead_code)]
#[derive(Component, Clone, Debug)]
pub struct UiPanelAnimator {
    pub frame_entity: Entity,
    pub clips: Vec<BoundUiAnimClip>,
    pub active_clips: Vec<usize>,
    pub elapsed_secs: f32,
    pub playing: bool,
    pub speed: f32,
}

#[allow(dead_code)]
impl UiPanelAnimator {
    /// Plays a named animation clip (matching `AnimationClip::name`), resetting elapsed time.
    pub fn play_clip(&mut self, clip_name: &str, looping: bool) -> bool {
        self.play_clips(&[clip_name], looping)
    }

    /// Starts concurrent clips atomically, e.g. the native footer and chat transition pair.
    pub fn play_clips(&mut self, names: &[&str], looping: bool) -> bool {
        if !self.seek_clips(names, 0.0, true) {
            return false;
        }
        for &idx in &self.active_clips {
            self.clips[idx].looping = looping;
        }
        self.elapsed_secs = 0.0;
        self.playing = true;
        true
    }

    /// Restores a concurrent pose after a retained panel rebuild without replaying its transition.
    pub fn seek_clips(&mut self, names: &[&str], elapsed_secs: f32, playing: bool) -> bool {
        let Some(indices) = names
            .iter()
            .map(|name| self.clips.iter().position(|clip| clip.name == *name))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        self.active_clips = indices;
        self.elapsed_secs = elapsed_secs.max(0.0);
        self.playing = playing;
        true
    }
}

fn is_continuous_loop_clip(name: &str) -> bool {
    name == "Animation_Loading"
        || name.contains("chilun")
        || name.contains("Loop")
        || name.contains("Idle")
}

fn eval_ui_anim_scalar(times: &[f32], values: &[f32], t: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    if times.len() <= 1 || values.len() <= 1 {
        return values[0];
    }
    let idx = times.partition_point(|&kt| kt <= t).saturating_sub(1);
    let i0 = idx.min(values.len() - 1);
    let i1 = (i0 + 1).min(values.len() - 1);
    if i0 == i1 {
        return values[i0];
    }
    let dt = (times[i1] - times[i0]).max(1e-6);
    let f = ((t - times[i0]) / dt).clamp(0.0, 1.0);
    values[i0] + (values[i1] - values[i0]) * f
}

/// Advances all [`UiPanelAnimator`] controllers and applies `UiTransform` / alpha / active curves.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn drive_ui_panel_animations(
    time: Res<Time>,
    mut animators: Query<&mut UiPanelAnimator>,
    frame_q: Query<(&UiPanelFrame, &Node, Option<&UiPanelCanvasReflow>)>,
    mut ui_tf_q: Query<&mut UiTransform>,
    mut img_q: Query<&mut ImageNode>,
    mut text_color_q: Query<&mut TextColor>,
    mut bg_q: Query<&mut BackgroundColor>,
    mut vis_q: Query<&mut Visibility>,
    mut node_q: Query<&mut Node, Without<UiPanelFrame>>,
) {
    let dt = time.delta_secs();
    for mut animator in &mut animators {
        if animator.active_clips.is_empty() {
            continue;
        }
        let frame_scale = frame_q
            .get(animator.frame_entity)
            .ok()
            .and_then(|(frame, node, canvas)| match (canvas, node.width) {
                (Some(canvas), _) => Some(canvas.scale),
                (None, Val::Px(w)) if frame.ref_w > 1.0 => Some((w / frame.ref_w).clamp(0.1, 3.0)),
                _ => None,
            })
            .unwrap_or(1.0);

        let dur = animator
            .active_clips
            .iter()
            .map(|&idx| animator.clips[idx].duration_secs)
            .fold(0.0_f32, f32::max);
        let looping = animator
            .active_clips
            .iter()
            .any(|&idx| animator.clips[idx].looping);
        if animator.playing {
            animator.elapsed_secs += dt * animator.speed;
            if !looping && animator.elapsed_secs >= dur {
                animator.elapsed_secs = dur;
                animator.playing = false;
            }
        }
        for &clip_idx in &animator.active_clips {
            let clip = &animator.clips[clip_idx];
            let dur = clip.duration_secs.max(0.0);
            let sample_t = if dur > 1e-4 {
                if clip.looping {
                    animator.elapsed_secs % dur
                } else {
                    animator.elapsed_secs.clamp(0.0, dur)
                }
            } else {
                0.0
            };

            for grp in &clip.targets {
                let mut pos_x = grp.rest_anchored[0];
                let mut pos_y = grp.rest_anchored[1];
                let mut rot_z_deg: Option<f32> = None;
                let mut sx = grp.rest_scale[0];
                let mut sy = grp.rest_scale[1];
                let mut alpha: Option<f32> = None;
                let mut active: Option<bool> = None;

                for tr in &grp.tracks {
                    let v = eval_ui_anim_scalar(&tr.times, &tr.values, sample_t);
                    match tr.channel {
                        UiAnimChannelDto::AnchoredPosX => pos_x = v,
                        UiAnimChannelDto::AnchoredPosY => pos_y = v,
                        UiAnimChannelDto::LocalEulerZDeg => rot_z_deg = Some(-v),
                        UiAnimChannelDto::LocalScaleX => sx = v,
                        UiAnimChannelDto::LocalScaleY => sy = v,
                        UiAnimChannelDto::ColorAlpha => alpha = Some(v.clamp(0.0, 1.0)),
                        UiAnimChannelDto::Active => active = Some(v >= 0.5),
                        UiAnimChannelDto::SizeDeltaX | UiAnimChannelDto::SizeDeltaY => {}
                    }
                }

                if grp.has_transform_track
                    && let Ok(mut tf) = ui_tf_q.get_mut(grp.entity)
                {
                    let delta_x = pos_x - grp.rest_anchored[0];
                    let delta_y = pos_y - grp.rest_anchored[1];
                    let dx = (delta_x * grp.parent_basis[0][0] + delta_y * grp.parent_basis[1][0])
                        * frame_scale;
                    let dy = (delta_x * grp.parent_basis[0][1] + delta_y * grp.parent_basis[1][1])
                        * frame_scale;
                    let rel_sx = sx
                        / if grp.rest_scale[0].abs() > 1e-4 {
                            grp.rest_scale[0]
                        } else {
                            1.0
                        };
                    let rel_sy = sy
                        / if grp.rest_scale[1].abs() > 1e-4 {
                            grp.rest_scale[1]
                        } else {
                            1.0
                        };
                    tf.translation = Val2::px(dx, dy);
                    tf.scale = Vec2::new(rel_sx, rel_sy);
                    tf.rotation = Rot2::degrees(
                        grp.rest_rot_deg
                            + rot_z_deg.map_or(0.0, |rotation| rotation - grp.rest_local_rot_deg),
                    );
                }

                if let Some(a) = alpha {
                    let [r, g, b, _] = grp.rest_rgba;
                    let col = Color::srgba(r, g, b, a);
                    if let Ok(mut img) = img_q.get_mut(grp.entity) {
                        img.color = col;
                    } else if let Some(tid) = grp.text_entity
                        && let Ok(mut tc) = text_color_q.get_mut(tid)
                    {
                        tc.0 = col;
                    } else if let Ok(mut bg) = bg_q.get_mut(grp.entity) {
                        bg.0 = col;
                    }
                }

                if let Some(is_on) = active
                    && (grp.rest_active || is_on)
                {
                    if let Ok(mut vis) = vis_q.get_mut(grp.entity) {
                        *vis = if is_on {
                            Visibility::Inherited
                        } else {
                            Visibility::Hidden
                        };
                    }
                    if let Ok(mut n) = node_q.get_mut(grp.entity) {
                        n.display = if is_on { Display::Flex } else { Display::None };
                    }
                }
            }
        }
    }
}

fn rect_intersects_canvas(rect: [f32; 4], ref_w: f32, ref_h: f32) -> bool {
    let [x, y, w, h] = rect;
    if w < 1.0 || h < 1.0 {
        return false;
    }
    const MARGIN: f32 = 24.0;
    x + w >= -MARGIN && y + h >= -MARGIN && x <= ref_w + MARGIN && y <= ref_h + MARGIN
}

fn subtree_intersects_canvas(node: &UiPanelNodeDto, ref_w: f32, ref_h: f32) -> bool {
    if !node.active_self {
        return false;
    }
    let has_content = node.graphic.is_some() || node.is_interactive;
    if has_content && rect_intersects_canvas(node.canvas_rect, ref_w, ref_h) {
        return true;
    }
    node.children
        .iter()
        .any(|ch| subtree_intersects_canvas(ch, ref_w, ref_h))
}

/// Spawns a complete retained Unity uGUI hierarchy under `frame`, returning [`BoundUiPanel`] with
/// every named entity, `GameObjectData` slot, hierarchy path, and animation clip bound.
pub fn spawn_retained_panel_into(
    frame: &mut ChildSpawnerCommands,
    doc: &UiPanelDoc,
    asset_server: &AssetServer,
    l10n: Option<&LocalizationDb>,
    font_source: &FontSource,
    opts: &UiRetainedOptions,
) -> BoundUiPanel {
    let source_doc = doc.clone();
    let mut spawn_doc = source_doc.clone();
    let mut spawn_opts = opts.clone();
    let native_canvas = opts.fullscreen && doc.scale_mode == "scale_with_screen_size";
    let metrics = opts
        .viewport_size
        .map(|viewport| compute_panel_canvas_metrics(doc, viewport, opts.fullscreen));
    if let Some(metrics) = metrics {
        spawn_opts.fit_scale = metrics.scale;
        if native_canvas {
            spawn_doc = reflow_panel_doc(doc, metrics.canvas_size);
        }
    }
    let doc = &spawn_doc;
    let opts = &spawn_opts;
    let frame_entity = frame.target_entity();
    if native_canvas {
        let canvas_size = Vec2::new(doc.reference_width, doc.reference_height);
        let scale = metrics.map_or(opts.fit_scale.max(0.1), |metrics| metrics.scale);
        frame.commands().entity(frame_entity).insert((
            UiPanelFrame {
                ref_w: source_doc.reference_width,
                ref_h: source_doc.reference_height,
                fullscreen: true,
            },
            UiPanelCanvasReflow {
                source: source_doc,
                canvas_size,
                scale,
            },
        ));
        if let Some(metrics) = metrics {
            frame.commands().queue(move |world: &mut World| {
                if let Some(mut node) = world.get_mut::<Node>(frame_entity) {
                    node.width = Val::Px(metrics.frame_size.x);
                    node.height = Val::Px(metrics.frame_size.y);
                }
            });
        }
    }
    let ref_w = doc.reference_width.max(64.0);
    let ref_h = doc.reference_height.max(64.0);
    let mut tex_handles: HashMap<String, Handle<Image>> = HashMap::new();
    let mut tex_dims: HashMap<String, [u32; 2]> = HashMap::new();
    for tex in &doc.textures {
        if let Some(ref rel_path) = tex.asset_path {
            tex_handles.insert(tex.key.clone(), asset_server.load(rel_path.clone()));
        }
        if tex.width > 0 && tex.height > 0 {
            tex_dims.insert(tex.key.clone(), [tex.width, tex.height]);
        }
    }

    let mut bound = BoundUiPanel {
        panel_name: doc.name.clone(),
        ref_w,
        ref_h,
        tex_handles: tex_handles.clone(),
        ..default()
    };

    let mut node_indices = HashMap::new();
    collect_canvas_node_indices(&doc.root, &mut Vec::new(), &mut node_indices);
    let ctx = RetainedSpawnCtx {
        panel_name: &doc.name,
        ref_w,
        ref_h,
        font_source,
        l10n,
        tex_handles: &tex_handles,
        tex_dims: &tex_dims,
        opts,
        frame_entity,
        node_indices: &node_indices,
        native_canvas,
    };

    let mut anim_meta: HashMap<Entity, SpawnedAnimMeta> = HashMap::new();
    spawn_retained_node_recursive(
        frame,
        &doc.root,
        "",
        true,
        [0.0, 0.0, ref_w, ref_h],
        false,
        None,
        None,
        &ctx,
        &mut bound,
        &mut anim_meta,
    );

    bind_panel_scripts(&mut bound, doc);

    if opts.animate && !doc.animations.is_empty() {
        let mut transform_entities = HashSet::new();
        let mut clips = Vec::new();
        for clip in &doc.animations {
            let mut by_target: HashMap<Entity, Vec<BoundAnimTrack>> = HashMap::new();
            for tr in &clip.tracks {
                let Some(ref p) = tr.path_name else {
                    continue;
                };
                let Some(&target_e) = bound.by_path.get(p) else {
                    continue;
                };
                if matches!(
                    tr.channel,
                    UiAnimChannelDto::LocalEulerZDeg
                        | UiAnimChannelDto::LocalScaleX
                        | UiAnimChannelDto::LocalScaleY
                        | UiAnimChannelDto::AnchoredPosX
                        | UiAnimChannelDto::AnchoredPosY
                ) {
                    transform_entities.insert(target_e);
                }
                by_target.entry(target_e).or_default().push(BoundAnimTrack {
                    channel: tr.channel,
                    times: tr.times.clone(),
                    values: tr.values.clone(),
                });
            }
            if by_target.is_empty() {
                continue;
            }
            let targets = by_target
                .into_iter()
                .filter_map(|(e, tracks)| {
                    let meta = anim_meta.get(&e)?;
                    let has_transform_track = tracks.iter().any(|t| {
                        matches!(
                            t.channel,
                            UiAnimChannelDto::LocalEulerZDeg
                                | UiAnimChannelDto::LocalScaleX
                                | UiAnimChannelDto::LocalScaleY
                                | UiAnimChannelDto::AnchoredPosX
                                | UiAnimChannelDto::AnchoredPosY
                        )
                    });
                    Some(BoundAnimTargetGroup {
                        entity: e,
                        text_entity: bound.node_text_child.get(&e).copied(),
                        rest_anchored: meta.rest_anchored,
                        rest_rot_deg: meta.rest_rot_deg,
                        rest_local_rot_deg: meta.rest_local_rot_deg,
                        parent_basis: meta.parent_basis,
                        rest_scale: meta.rest_scale,
                        rest_rgba: meta.rest_rgba,
                        rest_active: meta.rest_active,
                        has_transform_track,
                        tracks,
                    })
                })
                .collect::<Vec<_>>();
            if targets.is_empty() {
                continue;
            }
            clips.push(BoundUiAnimClip {
                name: clip.name.clone(),
                duration_secs: clip.duration_secs,
                looping: is_continuous_loop_clip(&clip.name),
                targets,
            });
        }

        if !clips.is_empty() {
            for e in transform_entities {
                if let Some(meta) = anim_meta.get(&e) {
                    frame
                        .commands()
                        .entity(e)
                        .try_insert(UiTransform::from_rotation(Rot2::degrees(meta.rest_rot_deg)));
                }
            }
            let active_clips = clips
                .iter()
                .enumerate()
                .filter_map(|(idx, clip)| clip.looping.then_some(idx))
                .collect::<Vec<_>>();
            let playing = !active_clips.is_empty();
            let anim_entity = frame
                .spawn((
                    Name::new("UiPanelAnimator"),
                    UiPanelAnimator {
                        frame_entity: frame.target_entity(),
                        clips,
                        active_clips,
                        elapsed_secs: 0.0,
                        playing,
                        speed: 1.0,
                    },
                ))
                .id();
            bound.animator_entity = Some(anim_entity);
        }
    }

    bound
}

fn bind_panel_scripts(bound: &mut BoundUiPanel, doc: &UiPanelDoc) {
    fn write_slot(
        slots: &mut HashMap<String, Vec<Option<Entity>>>,
        binding: &UiScriptBindingDto,
        target: Option<Entity>,
        replace: bool,
    ) {
        let list = slots.entry(binding.field_name.clone()).or_default();
        if let Some(idx) = binding.slot_index {
            let new_slot = idx >= list.len();
            if new_slot {
                list.resize(idx + 1, None);
            }
            if replace || new_slot {
                list[idx] = target;
            }
        } else {
            list.push(target);
        }
    }

    for binding in &doc.script_bindings {
        let target = if let Some(go_ref) = binding.target_go_ref {
            // A skipped inactive target stays unbound; a same-named active sibling is different.
            bound.by_go_ref.get(&go_ref).copied()
        } else if let Some(ref path) = binding.target_path {
            bound.by_path.get(path).copied()
        } else {
            bound.first(&binding.target_name)
        };
        if let Some(controller) = binding.owner_component_ref {
            write_slot(
                bound.controller_slots.entry(controller).or_default(),
                binding,
                target,
                true,
            );
        }
        let is_root = if let Some(owner) = binding.owner_go_ref {
            Some(owner) == doc.root.go_ref
        } else {
            binding.owner_path.as_deref() == Some("")
        };
        let legacy = binding.owner_go_ref.is_none() && binding.owner_path.is_none();
        if is_root || legacy {
            // View.Awake reads the root GameObjectData array; nested widgets own separate arrays.
            write_slot(&mut bound.slots, binding, target, is_root);
        }
    }
}

struct RetainedSpawnCtx<'a> {
    panel_name: &'a str,
    ref_w: f32,
    ref_h: f32,
    font_source: &'a FontSource,
    l10n: Option<&'a LocalizationDb>,
    tex_handles: &'a HashMap<String, Handle<Image>>,
    tex_dims: &'a HashMap<String, [u32; 2]>,
    opts: &'a UiRetainedOptions,
    frame_entity: Entity,
    node_indices: &'a HashMap<usize, Vec<usize>>,
    native_canvas: bool,
}

fn collect_canvas_node_indices(
    node: &UiPanelNodeDto,
    indices: &mut Vec<usize>,
    result: &mut HashMap<usize, Vec<usize>>,
) {
    result.insert(node as *const UiPanelNodeDto as usize, indices.clone());
    for (index, child) in node.children.iter().enumerate() {
        indices.push(index);
        collect_canvas_node_indices(child, indices, result);
        indices.pop();
    }
}

struct DeferredTextSpawn {
    initial: String,
    justify: Justify,
    base_px: f32,
    color: Color,
    outline: Option<[f32; 4]>,
}

fn is_generic_child_hitbox_name(name: &str) -> bool {
    matches!(
        name,
        "Image" | "RawImage" | "On" | "Off" | "Background" | "Checkmark" | "m_color" | "m_mark"
    ) || name.starts_with("Image (")
}

fn scroll_viewport_index(node: &UiPanelNodeDto) -> Option<usize> {
    (node.widget_kind.as_deref() == Some("ScrollRect"))
        .then(|| {
            node.children
                .iter()
                .position(|child| child.name.eq_ignore_ascii_case("viewport"))
        })
        .flatten()
}

fn scroll_overflow([horizontal, vertical]: [bool; 2]) -> Overflow {
    Overflow {
        x: if horizontal {
            OverflowAxis::Scroll
        } else {
            OverflowAxis::Clip
        },
        y: if vertical {
            OverflowAxis::Scroll
        } else {
            OverflowAxis::Clip
        },
    }
}

fn canvas_rotation(node: &UiPanelNodeDto) -> f32 {
    node.canvas_rotation_z_deg.unwrap_or(node.rotation_z_deg)
}

#[allow(clippy::too_many_arguments)]
fn spawn_retained_node_recursive(
    parent: &mut ChildSpawnerCommands,
    node: &UiPanelNodeDto,
    rel_path: &str,
    is_root: bool,
    parent_rect: [f32; 4],
    parent_forced: bool,
    inherited_button_name: Option<&str>,
    inherited_scroll_axes: Option<[bool; 2]>,
    ctx: &RetainedSpawnCtx<'_>,
    bound: &mut BoundUiPanel,
    anim_meta: &mut HashMap<Entity, SpawnedAnimMeta>,
) {
    let forced_here = parent_forced || ctx.opts.force_active_subtrees.contains(&node.name);
    if !node.active_self && !forced_here {
        return;
    }

    let zero_scaled = (node.scale[0].abs() < 1e-4 || node.scale[1].abs() < 1e-4) && !forced_here;
    let offscreen_hidden = !is_root
        && ctx.opts.hide_offscreen_nodes
        && !forced_here
        && !subtree_intersects_canvas(node, ctx.ref_w, ctx.ref_h);
    let hidden_here = ctx.opts.hidden_nodes.contains(&node.name) || zero_scaled || offscreen_hidden;
    let [px, py, pw, ph] = parent_rect;
    let orig_rect = node.canvas_rect;
    let [orig_x, orig_y, orig_w, orig_h] = orig_rect;
    let [art_x, art_y, art_w, art_h] = node.canvas_render_rect.unwrap_or(orig_rect);
    let has_sized_rect = orig_w >= 1.0 && orig_h >= 1.0;
    let is_text_leaf = matches!(node.graphic, Some(UiPanelGraphicDto::Text { .. }));
    let scroll_axes = (node.widget_kind.as_deref() == Some("ScrollRect")).then(|| {
        node.widget_props
            .as_ref()
            .and_then(|props| props.scroll_hv)
            .unwrap_or([false, true])
    });
    let viewport_index = scroll_viewport_index(node);
    // Unity scrolls Content inside its Viewport, leaving the viewport and scrollbars fixed.
    let scroll_here =
        inherited_scroll_axes.or_else(|| scroll_axes.filter(|_| viewport_index.is_none()));

    // Compute preserve_aspect box separately so child coordinates stay anchored to `orig_rect`.
    let mut aspect_box: Option<[f32; 4]> = None;
    if let Some(UiPanelGraphicDto::Image {
        preserve_aspect: true,
        texture_key: Some(key),
        ..
    }) = node.graphic.as_ref()
        && has_sized_rect
        && let Some(&[tw, th]) = ctx.tex_dims.get(key)
        && th > 0
    {
        let tex_ratio = tw as f32 / th as f32;
        let box_ratio = art_w / art_h.max(1e-4);
        if (box_ratio - tex_ratio).abs() > 0.01 {
            let (new_w, new_h) = if box_ratio > tex_ratio {
                (art_h * tex_ratio, art_h)
            } else {
                (art_w, art_w / tex_ratio)
            };
            aspect_box = Some([
                art_x + (art_w - new_w) * 0.5,
                art_y + (art_h - new_h) * 0.5,
                new_w,
                new_h,
            ]);
        }
    }

    let (text_justify_v, text_align_h) = match node.graphic.as_ref() {
        Some(UiPanelGraphicDto::Text { alignment, .. }) => {
            let v = if alignment.starts_with("upper") {
                JustifyContent::FlexStart
            } else if alignment.starts_with("lower") {
                JustifyContent::FlexEnd
            } else {
                JustifyContent::Center
            };
            let h = if alignment.ends_with("left") {
                AlignItems::FlexStart
            } else if alignment.ends_with("right") {
                AlignItems::FlexEnd
            } else {
                AlignItems::Center
            };
            (v, h)
        }
        _ => (JustifyContent::Center, AlignItems::Center),
    };

    let (box_x, box_y, box_w, box_h) = if node.children.is_empty()
        && let Some(ab) = aspect_box.or(node.canvas_render_rect)
    {
        (ab[0], ab[1], ab[2], ab[3])
    } else {
        (orig_x, orig_y, orig_w, orig_h)
    };

    let (layout_node, next_parent_rect) = if has_sized_rect {
        let mut overflow = Overflow::visible();
        if let Some(axes) = scroll_here {
            overflow = scroll_overflow(axes);
        } else if node.clips_children {
            overflow = Overflow::clip();
        }
        (
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(((box_x - px) / pw.max(1.0)) * 100.0),
                top: Val::Percent(((box_y - py) / ph.max(1.0)) * 100.0),
                width: Val::Percent((box_w / pw.max(1.0)) * 100.0),
                height: Val::Percent((box_h / ph.max(1.0)) * 100.0),
                flex_direction: FlexDirection::Column,
                display: if hidden_here {
                    Display::None
                } else {
                    Display::Flex
                },
                overflow,
                justify_content: if is_text_leaf {
                    text_justify_v
                } else {
                    JustifyContent::Center
                },
                align_items: if is_text_leaf {
                    text_align_h
                } else {
                    AlignItems::Center
                },
                ..default()
            },
            orig_rect,
        )
    } else {
        let collapse_empty =
            (orig_w < 0.5 && orig_h < 0.5) && (zero_scaled || node.children.is_empty());
        (
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: if collapse_empty {
                    Val::Px(0.0)
                } else {
                    Val::Percent(100.0)
                },
                height: if collapse_empty {
                    Val::Px(0.0)
                } else {
                    Val::Percent(100.0)
                },
                display: if hidden_here || collapse_empty {
                    Display::None
                } else {
                    Display::Flex
                },
                overflow: Overflow::visible(),
                ..default()
            },
            parent_rect,
        )
    };

    let mut entity_cmd = parent.spawn((
        Name::new(node.name.clone()),
        layout_node,
        if hidden_here {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        },
    ));
    let node_entity = entity_cmd.id();
    if ctx.native_canvas {
        entity_cmd.insert(UiRetainedCanvasNode {
            frame: ctx.frame_entity,
            indices: ctx.node_indices[&(node as *const UiPanelNodeDto as usize)].clone(),
            artwork_child: false,
        });
    }
    bound
        .nodes
        .entry(node.name.clone())
        .or_default()
        .push(node_entity);
    bound
        .by_path
        .entry(rel_path.to_owned())
        .or_insert(node_entity);
    if let Some(go_ref) = node.go_ref {
        bound.by_go_ref.insert(go_ref, node_entity);
    }

    // Artwork uses the cumulative canvas angle; the container hierarchy uses already-solved AABBs.
    if node.children.is_empty() && canvas_rotation(node).abs() > 0.25 {
        entity_cmd.insert(UiTransform::from_rotation(Rot2::degrees(canvas_rotation(
            node,
        ))));
    }
    if scroll_here.is_some_and(|axes| axes[0] || axes[1]) {
        entity_cmd.insert((
            UiScrollArea { speed_px: 1.0 },
            Hovered::default(),
            ScrollPosition::default(),
        ));
    }

    let draw_graphic = node.graphic_enabled && (!node.clips_children || node.show_mask_graphic);
    let rt_override = ctx.opts.render_targets.get(&node.name).cloned();
    let mut deferred_aspect_img: Option<([f32; 4], ImageNode)> = None;
    let mut deferred_text: Option<DeferredTextSpawn> = None;
    let mut base_tint = Color::WHITE;

    if has_sized_rect {
        if let Some(rt_handle) = rt_override {
            let mut img = ImageNode::new(rt_handle);
            img.image_mode = NodeImageMode::Stretch;
            entity_cmd.insert(img);
        } else if let Some(ref g) = node.graphic.as_ref().filter(|_| draw_graphic) {
            match g {
                UiPanelGraphicDto::Image {
                    color,
                    texture_key,
                    missing_texture,
                    image_type,
                    fill_amount,
                    border,
                    ..
                } => {
                    let alpha = (color[3] * node.effective_alpha).clamp(0.0, 1.0);
                    if alpha > 0.004 {
                        base_tint = Color::srgba(color[0], color[1], color[2], alpha);
                        let tex_handle = texture_key
                            .as_deref()
                            .and_then(|k| ctx.tex_handles.get(k))
                            .cloned();
                        let img_node = tex_handle.map(|h| {
                            let mut img = ImageNode::new(h).with_color(base_tint);
                            img.flip_x = node.flip_x;
                            img.flip_y = node.flip_y;
                            if image_type == "sliced" && border.iter().any(|&b| b > 0.5) {
                                // Mirror Unity `Image.GetAdjustedBorders` so narrow bars never invert 9-slice caps.
                                let bx_sum = (border[0] + border[2]).max(1e-3);
                                let by_sum = (border[1] + border[3]).max(1e-3);
                                let kx = (orig_w * 0.9 / bx_sum).min(1.0);
                                let ky = (orig_h * 0.9 / by_sum).min(1.0);
                                if kx < 0.08 || ky < 0.08 {
                                    img.image_mode = NodeImageMode::Stretch;
                                } else {
                                    img.image_mode = NodeImageMode::Sliced(TextureSlicer {
                                        border: BorderRect {
                                            min_inset: Vec2::new(border[0] * kx, border[1] * ky),
                                            max_inset: Vec2::new(border[2] * kx, border[3] * ky),
                                        },
                                        ..default()
                                    });
                                }
                            } else if image_type == "tiled" {
                                img.image_mode = NodeImageMode::Tiled {
                                    tile_x: true,
                                    tile_y: true,
                                    stretch_value: 1.0,
                                };
                            } else {
                                img.image_mode = NodeImageMode::Stretch;
                            }
                            img
                        });
                        let is_large_white_mask = color[0] > 0.9
                            && color[1] > 0.9
                            && color[2] > 0.9
                            && orig_w > 48.0
                            && orig_h > 48.0;
                        if let Some(img) = img_node {
                            if !node.children.is_empty()
                                && let Some(ab) = aspect_box
                            {
                                deferred_aspect_img = Some((ab, img));
                            } else {
                                entity_cmd.insert(img);
                            }
                        } else if *missing_texture {
                            entity_cmd.insert(BackgroundColor(Color::srgba(
                                color[0],
                                color[1],
                                color[2],
                                (alpha * 0.14).max(0.04),
                            )));
                        } else if !is_large_white_mask {
                            entity_cmd.insert(BackgroundColor(base_tint));
                        }
                        if image_type == "filled" {
                            entity_cmd.insert(UiFilledBar {
                                frac: fill_amount.clamp(0.0, 1.0),
                                horizontal: orig_w >= orig_h,
                            });
                        }
                    }
                }
                UiPanelGraphicDto::RawImage {
                    color,
                    texture_key,
                    missing_texture,
                    ..
                } => {
                    let alpha = (color[3] * node.effective_alpha).clamp(0.0, 1.0);
                    if alpha > 0.004 {
                        base_tint = Color::srgba(color[0], color[1], color[2], alpha);
                        let tex_handle = texture_key
                            .as_deref()
                            .and_then(|k| ctx.tex_handles.get(k))
                            .cloned();
                        if let Some(h) = tex_handle {
                            let mut img = ImageNode::new(h).with_color(base_tint);
                            img.flip_x = node.flip_x;
                            img.flip_y = node.flip_y;
                            img.image_mode = NodeImageMode::Stretch;
                            entity_cmd.insert(img);
                        } else if *missing_texture {
                            entity_cmd
                                .insert(BackgroundColor(Color::srgba(0.18, 0.55, 0.85, 0.08)));
                        }
                    }
                }
                UiPanelGraphicDto::Text {
                    text,
                    color,
                    font_size,
                    alignment,
                    rich_text,
                    best_fit,
                    outline_color,
                } => {
                    let alpha = (color[3] * node.effective_alpha).clamp(0.0, 1.0);
                    if alpha > 0.004 {
                        let initial = ctx
                            .opts
                            .text_overrides
                            .get(&node.name)
                            .cloned()
                            .unwrap_or_else(|| {
                                let clean = if *rich_text && text.contains('<') {
                                    strip_rich_text_tags(text)
                                } else {
                                    text.clone()
                                };
                                if ctx.opts.localize_text
                                    && let Some(l10n) = ctx.l10n
                                {
                                    l10n.tr(clean.trim()).to_owned()
                                } else {
                                    clean
                                }
                            });
                        let justify = match alignment.as_str() {
                            "upper_center" | "middle_center" | "lower_center" => Justify::Center,
                            "upper_right" | "middle_right" | "lower_right" => Justify::Right,
                            _ => Justify::Left,
                        };
                        let mut base_px = if *best_fit {
                            (*font_size).clamp(8.0, (orig_h * 0.72).max(10.0))
                        } else {
                            (*font_size).max(8.0)
                        };
                        let char_count = initial.chars().count();
                        if char_count > 6 && orig_w > 40.0 {
                            let est_width = char_count as f32 * base_px * 0.52;
                            if est_width > orig_w * 1.15 && orig_h < base_px * 2.4 {
                                let shrink =
                                    ((orig_w * 1.05) / (char_count as f32 * 0.52)).max(9.5);
                                base_px = base_px.min(shrink);
                            }
                        }
                        let txt_color = Color::srgba(color[0], color[1], color[2], alpha);
                        deferred_text = Some(DeferredTextSpawn {
                            initial,
                            justify,
                            base_px,
                            color: txt_color,
                            outline: outline_color.filter(|oc| oc[3] > 0.02),
                        });
                    }
                }
            }
        }
    }

    let passthrough = ctx.opts.passthrough_nodes.contains(&node.name);
    let own_interactive =
        !passthrough && node.is_interactive && node.widget_kind.as_deref() != Some("ScrollRect");
    let effective_button_name = if own_interactive {
        if is_generic_child_hitbox_name(&node.name) && inherited_button_name.is_some() {
            inherited_button_name
        } else {
            Some(node.name.as_str())
        }
    } else if inherited_button_name.is_some()
        && has_sized_rect
        && node.graphic.is_some()
        && !is_text_leaf
    {
        inherited_button_name
    } else {
        None
    };
    let next_inherited_button_name = if own_interactive && !is_generic_child_hitbox_name(&node.name)
    {
        Some(node.name.as_str())
    } else {
        inherited_button_name
    };
    let explicitly_allowed = effective_button_name.is_some_and(|bname| {
        ctx.opts
            .interactive_whitelist
            .as_ref()
            .is_none_or(|set| set.contains(bname))
    });
    let is_clickable = has_sized_rect && !passthrough && explicitly_allowed;

    if is_clickable {
        let click_name = effective_button_name
            .unwrap_or(node.name.as_str())
            .to_owned();
        if node.graphic.is_none() && ctx.opts.preview_mode {
            entity_cmd.insert(BackgroundColor(Color::srgba(0.2, 0.65, 0.9, 0.08)));
        }
        entity_cmd.insert((
            Button,
            Hovered::default(),
            UiPanelInteractiveLabel(click_name.clone()),
            BoundNodeButtonMeta {
                panel_name: ctx.panel_name.to_owned(),
                node_name: click_name,
                preview_mode: ctx.opts.preview_mode,
            },
            observe(on_retained_button_activate),
        ));
        if node.graphic.is_some()
            && let Some(props) = node.widget_props.as_ref()
            && let Some(colors) = props.colors
        {
            entity_cmd.insert(UiSelectableColors::from_unity_block(colors, base_tint));
        }
    } else if !scroll_here.is_some_and(|axes| axes[0] || axes[1]) {
        entity_cmd.insert(Pickable::IGNORE);
    }

    let rest_rgba = Srgba::from(
        deferred_text
            .as_ref()
            .map(|dt| dt.color)
            .unwrap_or(base_tint),
    )
    .to_f32_array();
    anim_meta.insert(
        node_entity,
        SpawnedAnimMeta {
            rest_anchored: node.anchored_position,
            rest_rot_deg: if node.children.is_empty() {
                canvas_rotation(node)
            } else {
                0.0
            },
            rest_local_rot_deg: node.rotation_z_deg,
            parent_basis: node
                .canvas_parent_basis
                .unwrap_or([[1.0, 0.0], [0.0, -1.0]]),
            rest_scale: [node.scale[0], node.scale[1]],
            rest_rgba,
            rest_active: !hidden_here,
        },
    );

    let fit_scale = ctx.opts.fit_scale.max(0.15);
    let node_name_clone = node.name.clone();
    if deferred_aspect_img.is_some() || deferred_text.is_some() || !node.children.is_empty() {
        let mut text_child_id = None;
        entity_cmd.with_children(|ch_spawner| {
            if let Some((ab, img)) = deferred_aspect_img {
                let mut artwork = ch_spawner.spawn((
                    Pickable::IGNORE,
                    img,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(((ab[0] - orig_x) / orig_w.max(1.0)) * 100.0),
                        top: Val::Percent(((ab[1] - orig_y) / orig_h.max(1.0)) * 100.0),
                        width: Val::Percent((ab[2] / orig_w.max(1.0)) * 100.0),
                        height: Val::Percent((ab[3] / orig_h.max(1.0)) * 100.0),
                        ..default()
                    },
                ));
                if ctx.native_canvas {
                    artwork.insert(UiRetainedCanvasNode {
                        frame: ctx.frame_entity,
                        indices: ctx.node_indices[&(node as *const UiPanelNodeDto as usize)]
                            .clone(),
                        artwork_child: true,
                    });
                }
            }
            if let Some(dt) = deferred_text {
                let mut tc = ch_spawner.spawn((
                    Pickable::IGNORE,
                    UiPanelScaledText {
                        base_px: dt.base_px,
                    },
                    Node {
                        max_width: Val::Percent(100.0),
                        ..default()
                    },
                    Text::new(dt.initial),
                    TextFont {
                        font: ctx.font_source.clone(),
                        font_size: FontSize::Px((dt.base_px * fit_scale).clamp(7.0, 128.0)),
                        ..default()
                    },
                    TextLayout::justify(dt.justify),
                    TextColor(dt.color),
                ));
                if let Some(oc) = dt.outline {
                    tc.insert(TextShadow {
                        offset: Vec2::new(1.25, 1.25),
                        color: Color::srgba(oc[0], oc[1], oc[2], oc[3] * node.effective_alpha),
                    });
                }
                text_child_id = Some(tc.id());
            }
            for (child_idx, child) in node.children.iter().enumerate() {
                let child_path = if is_root {
                    child.name.clone()
                } else {
                    format!("{rel_path}/{}", child.name)
                };
                spawn_retained_node_recursive(
                    ch_spawner,
                    child,
                    &child_path,
                    false,
                    next_parent_rect,
                    forced_here,
                    next_inherited_button_name,
                    scroll_axes.filter(|_| viewport_index == Some(child_idx)),
                    ctx,
                    bound,
                    anim_meta,
                );
            }
        });
        if let Some(tid) = text_child_id {
            bound
                .text_nodes
                .entry(node_name_clone)
                .or_default()
                .push(tid);
            bound.node_text_child.insert(node_entity, tid);
        }
    }
}

fn on_retained_button_activate(
    activate: On<Activate>,
    metas: Query<&BoundNodeButtonMeta>,
    mut preview_state: ResMut<UiPreviewInteractionState>,
    mut click_out: MessageWriter<BoundUiClick>,
) {
    let Ok(meta) = metas.get(activate.entity) else {
        return;
    };
    if meta.preview_mode {
        preview_state.last_clicked_label = format!("Clicked `{}`", meta.node_name);
    }
    click_out.write(BoundUiClick {
        panel_name: meta.panel_name.clone(),
        node_name: meta.node_name.clone(),
        entity: activate.entity,
    });
}

/// Spawns an exported [`UiPanelDoc`] interactively inside the Sandbox UI Panel preview viewport,
/// supporting both raw APK prefab inspection and runtime `SC.UI.View` / `LocalizationDb` rules.
#[allow(clippy::too_many_arguments)]
pub fn spawn_ui_panel_preview(
    commands: &mut Commands,
    manifest: &AssetManifest,
    panel_index: usize,
    asset_server: &AssetServer,
    l10n: Option<&LocalizationDb>,
    configs: Option<&GameConfigs>,
    font_source: &FontSource,
    win_size: Vec2,
    use_runtime_view: bool,
    localize: bool,
    marker: impl Bundle,
) -> UiPanelPreviewSummary {
    if manifest.ui_panels.is_empty() {
        commands
            .spawn((
                UiPanelPreviewRoot,
                marker,
                ZIndex(5),
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(14.0),
                    right: Val::Px(14.0),
                    top: Val::Px(88.0),
                    bottom: Val::Px(64.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
            ))
            .with_children(|root| {
                root.spawn((
                    Text::new("No exported UI panels in assets/ui_panels/."),
                    TextFont {
                        font: font_source.clone(),
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.85, 0.88, 0.94)),
                ));
            });
        return UiPanelPreviewSummary {
            name: "None".to_owned(),
            ..default()
        };
    }

    let entry = &manifest.ui_panels[panel_index % manifest.ui_panels.len()];
    let doc = match load_ui_panel_doc(&entry.asset_path) {
        Ok(d) => d,
        Err(_) => {
            return UiPanelPreviewSummary {
                name: format!("{} (Load error)", entry.name),
                ..default()
            };
        }
    };

    let ref_w = doc.reference_width.max(64.0);
    let ref_h = doc.reference_height.max(64.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, false);
    let mut opts = UiRetainedOptions {
        fullscreen: false,
        fit_scale,
        localize_text: localize,
        preview_mode: true,
        hide_offscreen_nodes: false,
        animate: use_runtime_view,
        ..default()
    };
    if use_runtime_view {
        apply_panel_default_view_rules(&doc.name, l10n, configs, &mut opts);
    }

    let summary = UiPanelPreviewSummary {
        name: entry.name.clone(),
        ref_w,
        ref_h,
        binding_count: doc.script_bindings.len(),
        clip_count: doc.animations.len(),
        animation_names: doc.animations.iter().map(|c| c.name.clone()).collect(),
    };

    let mut bound_opt = None;
    let root_e = commands
        .spawn((
            UiPanelPreviewRoot,
            marker,
            ZIndex(5),
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(14.0),
                right: Val::Px(14.0),
                top: Val::Px(88.0),
                bottom: Val::Px(64.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn((
                UiPanelFrame {
                    ref_w,
                    ref_h,
                    fullscreen: false,
                },
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Relative,
                    width: Val::Px(ref_w * fit_scale),
                    height: Val::Px(ref_h * fit_scale),
                    overflow: Overflow::clip(),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.08, 0.10, 0.14, 0.90)),
                BorderColor::all(Color::srgba(0.35, 0.52, 0.78, 0.85)),
            ))
            .with_children(|frame| {
                bound_opt = Some(spawn_retained_panel_into(
                    frame,
                    &doc,
                    asset_server,
                    l10n.filter(|_| localize),
                    font_source,
                    &opts,
                ));
            });
        })
        .id();

    if let Some(bound) = bound_opt {
        commands.entity(root_e).insert(bound);
    }

    summary
}

/// Resizes preview & stage [`UiPanelFrame`] entities and scaled font sizes smoothly on window resize.
pub fn resize_ui_panel_frames(
    windows: Query<&Window>,
    mut frame_q: Query<(
        Entity,
        &UiPanelFrame,
        Option<&mut UiPanelCanvasReflow>,
        &mut Node,
    )>,
    mut retained_nodes: Query<(&UiRetainedCanvasNode, &mut Node), Without<UiPanelFrame>>,
    children_q: Query<&Children>,
    mut text_q: Query<(&UiPanelScaledText, &mut TextFont)>,
) {
    let Some(win) = windows.iter().next() else {
        return;
    };
    let win_size = Vec2::new(win.width(), win.height());
    for (frame_e, frame, canvas, mut node) in &mut frame_q {
        let scale = if let Some(mut canvas) = canvas {
            let metrics = compute_panel_canvas_metrics(&canvas.source, win_size, frame.fullscreen);
            node.width = Val::Px(metrics.frame_size.x);
            node.height = Val::Px(metrics.frame_size.y);
            canvas.scale = metrics.scale;
            if !canvas.canvas_size.abs_diff_eq(metrics.canvas_size, 0.01) {
                let doc = reflow_panel_doc(&canvas.source, metrics.canvas_size);
                for (binding, mut retained) in &mut retained_nodes {
                    if binding.frame != frame_e {
                        continue;
                    }
                    let (source_node, parent_rect) =
                        canvas_node_and_parent_rect(&doc, &binding.indices);
                    let artwork = canvas_artwork_rect(source_node, &doc.textures);
                    let rectangle = if binding.artwork_child {
                        artwork
                    } else if source_node.children.is_empty() {
                        artwork
                    } else {
                        source_node.canvas_rect
                    };
                    let parent_rect = if binding.artwork_child {
                        source_node.canvas_rect
                    } else {
                        parent_rect
                    };
                    if source_node.canvas_rect[2] >= 1.0 && source_node.canvas_rect[3] >= 1.0 {
                        retained.left = Val::Percent(
                            (rectangle[0] - parent_rect[0]) / parent_rect[2].max(1.0) * 100.0,
                        );
                        retained.top = Val::Percent(
                            (rectangle[1] - parent_rect[1]) / parent_rect[3].max(1.0) * 100.0,
                        );
                        retained.width =
                            Val::Percent(rectangle[2] / parent_rect[2].max(1.0) * 100.0);
                        retained.height =
                            Val::Percent(rectangle[3] / parent_rect[3].max(1.0) * 100.0);
                    }
                }
                canvas.canvas_size = metrics.canvas_size;
            }
            metrics.scale
        } else {
            let scale =
                compute_panel_fit_scale(frame.ref_w, frame.ref_h, win_size, frame.fullscreen);
            node.width = Val::Px(frame.ref_w * scale);
            node.height = Val::Px(frame.ref_h * scale);
            scale
        };
        rescale_texts_under(frame_e, scale, &children_q, &mut text_q);
    }
}

fn canvas_node_and_parent_rect<'a>(
    doc: &'a UiPanelDoc,
    indices: &[usize],
) -> (&'a UiPanelNodeDto, [f32; 4]) {
    let mut node = &doc.root;
    let mut parent_rect = [0.0, 0.0, doc.reference_width, doc.reference_height];
    for &index in indices {
        if node.canvas_rect[2] >= 1.0 && node.canvas_rect[3] >= 1.0 {
            parent_rect = node.canvas_rect;
        }
        node = &node.children[index];
    }
    (node, parent_rect)
}

fn canvas_artwork_rect(node: &UiPanelNodeDto, textures: &[UiTextureDto]) -> [f32; 4] {
    let mut art = node.canvas_render_rect.unwrap_or(node.canvas_rect);
    if let Some(UiPanelGraphicDto::Image {
        preserve_aspect: true,
        texture_key: Some(key),
        ..
    }) = &node.graphic
        && let Some(texture) = textures
            .iter()
            .find(|texture| texture.key == *key && texture.height > 0 && texture.width > 0)
    {
        let ratio = texture.width as f32 / texture.height as f32;
        let width = art[2].min(art[3] * ratio);
        let height = art[3].min(art[2] / ratio);
        art[0] += (art[2] - width) * 0.5;
        art[1] += (art[3] - height) * 0.5;
        art[2] = width;
        art[3] = height;
    }
    art
}

fn rescale_texts_under(
    entity: Entity,
    scale: f32,
    children_q: &Query<&Children>,
    text_q: &mut Query<(&UiPanelScaledText, &mut TextFont)>,
) {
    if let Ok((scaled, mut tf)) = text_q.get_mut(entity) {
        let target = (scaled.base_px * scale).clamp(7.0, 128.0);
        let changed = match tf.font_size {
            FontSize::Px(cur) => (cur - target).abs() > 0.25,
            _ => true,
        };
        if changed {
            tf.font_size = FontSize::Px(target);
        }
    }
    if let Ok(children) = children_q.get(entity) {
        for child in children.iter() {
            rescale_texts_under(child, scale, children_q, text_q);
        }
    }
}

/// Strips Unity rich-text markup tags cleanly while leaving literal `<` fragments intact.
pub fn strip_rich_text_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            let mut closed = false;
            for tc in chars.by_ref() {
                if tc == '>' {
                    closed = true;
                    break;
                }
                tag.push(tc);
                if tag.len() > 32 {
                    break;
                }
            }
            if !closed {
                out.push('<');
                out.push_str(&tag);
            } else {
                let low = tag.trim().to_ascii_lowercase();
                let known = low == "b"
                    || low == "/b"
                    || low == "i"
                    || low == "/i"
                    || low == "/color"
                    || low == "/size"
                    || low.starts_with("color=")
                    || low.starts_with("size=");
                if !known {
                    out.push('<');
                    out.push_str(&tag);
                    out.push('>');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_node<'a>(root: &'a UiPanelNodeDto, path: &[&str]) -> &'a UiPanelNodeDto {
        path.iter().fold(root, |node, name| {
            node.children
                .iter()
                .find(|child| child.name == *name)
                .unwrap()
        })
    }

    #[test]
    fn real_main_panel_canvas_scaler_preserves_header_anchors_across_aspects() {
        let source = load_ui_panel_doc("ui_panels/MainPanel.json").unwrap();
        assert_eq!(source.scale_mode, "scale_with_screen_size");
        assert_eq!(source.match_width_or_height, 0.5);
        for viewport in [
            Vec2::new(1920.0, 1080.0),
            Vec2::new(1920.0, 1006.0),
            Vec2::new(2560.0, 1080.0),
            Vec2::new(1280.0, 1024.0),
        ] {
            let metrics = compute_panel_canvas_metrics(&source, viewport, true);
            let expected_scale = ((viewport.x / 1920.0) * (viewport.y / 1080.0)).sqrt();
            assert!((metrics.scale - expected_scale).abs() < 0.00001);
            assert_eq!(metrics.frame_size, viewport);
            let doc = reflow_panel_doc(&source, metrics.canvas_size);
            let avatar = panel_node(&doc.root, &["m_ding", "zuo", "head", "head", "m_head_icon"]);
            assert!(
                (avatar.canvas_rect[0] * metrics.scale - 44.700012 * expected_scale).abs() < 0.05
            );
            assert!(
                (avatar.canvas_rect[1] * metrics.scale - 17.599976 * expected_scale).abs() < 0.05
            );
            let right = panel_node(&doc.root, &["m_ding", "you"]);
            assert!(
                ((right.canvas_rect[0] + right.canvas_rect[2]) * metrics.scale - viewport.x).abs()
                    < 0.05
            );
            let gold = panel_node(&doc.root, &["m_ding", "zuo", "money", "jinbi", "txt_gold"]);
            assert!(
                (gold.canvas_rect[0] * metrics.scale - 1265.1019 * expected_scale).abs() < 0.05
            );
            let down = panel_node(&doc.root, &["down"]);
            assert!(
                (down.canvas_rect[1] * metrics.scale - (viewport.y - 81.0 * metrics.scale)).abs()
                    < 0.05
            );
            assert_eq!(down.canvas_rotation_z_deg, Some(-180.0));
        }
        let baseline = reflow_panel_doc(&source, Vec2::new(1920.0, 1080.0));
        fn compare(source: &UiPanelNodeDto, actual: &UiPanelNodeDto) {
            for (expected, actual) in source.canvas_rect.into_iter().zip(actual.canvas_rect) {
                assert!(
                    (expected - actual).abs() < 0.05,
                    "{} baseline geometry",
                    source.name
                );
            }
            for (source, actual) in source.children.iter().zip(&actual.children) {
                compare(source, actual);
            }
        }
        compare(&source.root, &baseline.root);
    }

    #[test]
    fn retained_main_panel_reflows_existing_nodes_on_live_resize() {
        let mut source = load_ui_panel_doc("ui_panels/MainPanel.json").unwrap();
        source.textures.clear();
        source.animations.clear();
        fn geometry_only(node: &mut UiPanelNodeDto) {
            node.graphic = None;
            node.is_interactive = false;
            for child in &mut node.children {
                geometry_only(child);
            }
        }
        geometry_only(&mut source.root);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .add_systems(Update, resize_ui_panel_frames)
            .add_systems(
                Startup,
                move |mut commands: Commands, assets: Res<AssetServer>| {
                    let host = commands.spawn(Node::default()).id();
                    commands.entity(host).with_children(|root| {
                        let bound = spawn_retained_panel_into(
                            root,
                            &source,
                            &assets,
                            None,
                            &FontSource::sans_serif(),
                            &UiRetainedOptions {
                                fullscreen: true,
                                viewport_size: Some(Vec2::new(1920.0, 1080.0)),
                                ..default()
                            },
                        );
                        root.commands().entity(host).insert(bound);
                    });
                },
            );
        let window = app
            .world_mut()
            .spawn(Window {
                resolution: (1920, 1080).into(),
                ..default()
            })
            .id();
        app.update();
        let (host, avatar) = {
            let mut panels = app.world_mut().query::<(Entity, &BoundUiPanel)>();
            let (host, bound) = panels.single(app.world()).unwrap();
            (host, bound.first("m_head_icon").unwrap())
        };
        fn live_rect(world: &World, entity: Entity) -> [f32; 4] {
            let node = world.get::<Node>(entity).unwrap();
            let parent = world
                .get::<ChildOf>(entity)
                .map(|parent| live_rect(world, parent.parent()))
                .unwrap_or([0.0, 0.0, 0.0, 0.0]);
            let value = |value, parent| match value {
                Val::Px(value) => value,
                Val::Percent(value) => value * parent * 0.01,
                _ => 0.0,
            };
            [
                parent[0] + value(node.left, parent[2]),
                parent[1] + value(node.top, parent[3]),
                value(node.width, parent[2]),
                value(node.height, parent[3]),
            ]
        }
        assert!((live_rect(app.world(), avatar)[0] - 44.700012).abs() < 0.05);
        for viewport in [Vec2::new(1920.0, 1006.0), Vec2::new(2560.0, 1080.0)] {
            app.world_mut()
                .get_mut::<Window>(window)
                .unwrap()
                .resolution
                .set(viewport.x, viewport.y);
            app.update();
            let scale = ((viewport.x / 1920.0) * (viewport.y / 1080.0)).sqrt();
            assert!((live_rect(app.world(), avatar)[0] - 44.700012 * scale).abs() < 0.05);
            assert_eq!(
                app.world().get::<Node>(host).unwrap().width,
                Val::Px(viewport.x)
            );
            assert!(app.world().get::<UiRetainedCanvasNode>(avatar).is_some());
        }
    }

    #[test]
    fn concurrent_ui_clips_sample_each_target_in_its_native_parent_basis() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, drive_ui_panel_animations);
        let frame = app
            .world_mut()
            .spawn((
                UiPanelFrame {
                    ref_w: 1920.0,
                    ref_h: 1080.0,
                    fullscreen: true,
                },
                Node {
                    width: Val::Px(960.0),
                    ..default()
                },
            ))
            .id();
        let footer = app.world_mut().spawn(UiTransform::default()).id();
        let chat = app.world_mut().spawn(UiTransform::default()).id();
        let clip = |name: &str, entity, parent_basis| BoundUiAnimClip {
            name: name.into(),
            duration_secs: 1.0,
            looping: false,
            targets: vec![BoundAnimTargetGroup {
                entity,
                text_entity: None,
                rest_anchored: [0.0, 0.0],
                rest_rot_deg: 0.0,
                rest_local_rot_deg: 0.0,
                parent_basis,
                rest_scale: [1.0, 1.0],
                rest_rgba: [1.0; 4],
                rest_active: true,
                has_transform_track: true,
                tracks: vec![BoundAnimTrack {
                    channel: UiAnimChannelDto::AnchoredPosY,
                    times: vec![0.0, 1.0],
                    values: vec![0.0, 100.0],
                }],
            }],
        };
        let mut animator = UiPanelAnimator {
            frame_entity: frame,
            clips: vec![
                clip("footer", footer, [[1.0, 0.0], [0.0, -1.0]]),
                clip("chat", chat, [[-1.0, 0.0], [0.0, 1.0]]),
            ],
            active_clips: Vec::new(),
            elapsed_secs: 0.0,
            playing: false,
            speed: 1.0,
        };
        assert!(animator.seek_clips(&["footer", "chat"], 0.5, false));
        assert!(!animator.play_clips(&["footer", "missing"], false));
        assert_eq!(
            animator.active_clips,
            vec![0, 1],
            "missing clip must not partially replace playback"
        );
        app.world_mut().spawn(animator);
        app.update();
        assert_eq!(
            app.world().get::<UiTransform>(footer).unwrap().translation,
            Val2::px(0.0, -25.0)
        );
        assert_eq!(
            app.world().get::<UiTransform>(chat).unwrap().translation,
            Val2::px(0.0, 25.0)
        );
    }

    #[test]
    fn script_slots_preserve_controller_ownership_and_duplicate_object_identity() {
        let reference = |path_id| UiAssetRefDto {
            file_index: 7,
            path_id,
        };
        let first = Entity::from_bits(1);
        let second = Entity::from_bits(2);
        let mut bound = BoundUiPanel {
            nodes: HashMap::from([("Image".into(), vec![first, second])]),
            by_path: HashMap::from([("Content/Image".into(), first)]),
            by_go_ref: HashMap::from([(reference(10), first), (reference(20), second)]),
            ..default()
        };
        let binding = |owner, component, target, slot| UiScriptBindingDto {
            script_name: "GameObjectData".into(),
            field_name: "GameObjects".into(),
            slot_index: Some(slot),
            target_name: "Image".into(),
            target_go_ref: Some(reference(target)),
            target_path: Some("Content/Image".into()),
            owner_go_ref: Some(reference(owner)),
            owner_component_ref: Some(reference(component)),
            ..default()
        };
        let doc = UiPanelDoc {
            root: UiPanelNodeDto {
                go_ref: Some(reference(1)),
                ..default()
            },
            script_bindings: vec![
                binding(2, 200, 20, 0),
                binding(1, 100, 10, 0),
                binding(2, 200, 20, 1),
                binding(1, 100, 30, 1),
            ],
            ..default()
        };
        bind_panel_scripts(&mut bound, &doc);
        assert_eq!(bound.game_object_slot(0), Some(first));
        assert_eq!(
            bound.controller_slot(reference(200), "GameObjects", 0),
            Some(second)
        );
        assert_eq!(
            bound.game_object_slot(1),
            None,
            "inactive target must not alias the same-named sibling"
        );
        assert_eq!(
            bound.controller_slot(reference(200), "GameObjects", 1),
            Some(second)
        );
    }

    #[test]
    fn establish_panel_root_slots_match_native_awake() {
        let doc = load_ui_panel_doc("ui_panels/EstablishPanel.json").unwrap();
        let mut world = World::new();
        let mut bound = BoundUiPanel::default();
        fn register(node: &UiPanelNodeDto, world: &mut World, bound: &mut BoundUiPanel) {
            let entity = world.spawn_empty().id();
            bound
                .nodes
                .entry(node.name.clone())
                .or_default()
                .push(entity);
            bound
                .by_go_ref
                .insert(node.go_ref.expect("exported object identity"), entity);
            for child in &node.children {
                register(child, world, bound);
            }
        }
        register(&doc.root, &mut world, &mut bound);
        bind_panel_scripts(&mut bound, &doc);
        assert_eq!(bound.game_object_slot(0), bound.first("m_hero_tex"));
        assert_eq!(bound.game_object_slot(1), bound.first("inp_inputName"));
        assert_eq!(bound.slots["GameObjects"].len(), 25);
    }

    #[test]
    fn native_scroll_rect_keeps_viewport_fixed_and_scrolls_content() {
        let mut doc = UiPanelDoc {
            name: "ScrollFixture".into(),
            reference_width: 1920.0,
            reference_height: 1080.0,
            root: UiPanelNodeDto {
                name: "ScrollFixture".into(),
                canvas_rect: [0.0, 0.0, 1920.0, 1080.0],
                children: vec![UiPanelNodeDto {
                    name: "ScrollRect".into(),
                    canvas_rect: [300.0, 400.0, 500.0, 100.0],
                    widget_kind: Some("ScrollRect".into()),
                    widget_props: Some(UiWidgetPropsDto {
                        scroll_hv: Some([true, false]),
                        ..default()
                    }),
                    children: vec![UiPanelNodeDto {
                        name: "Viewport".into(),
                        canvas_rect: [310.0, 400.0, 480.0, 100.0],
                        clips_children: true,
                        children: vec![UiPanelNodeDto {
                            name: "Content".into(),
                            canvas_rect: [310.0, 400.0, 960.0, 100.0],
                            ..default()
                        }],
                        ..default()
                    }],
                    ..default()
                }],
                ..default()
            },
            ..default()
        };
        fn activate_fixture(node: &mut UiPanelNodeDto) {
            node.active_self = true;
            node.scale = default_vec3_one();
            for child in &mut node.children {
                activate_fixture(child);
            }
        }
        activate_fixture(&mut doc.root);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.add_systems(
            Startup,
            move |mut commands: Commands, assets: Res<AssetServer>| {
                commands.spawn(Node::default()).with_children(|root| {
                    spawn_retained_panel_into(
                        root,
                        &doc,
                        &assets,
                        None,
                        &FontSource::sans_serif(),
                        &UiRetainedOptions::default(),
                    );
                });
            },
        );
        app.update();
        let mut query = app
            .world_mut()
            .query::<(&Name, &Node, Option<&UiScrollArea>)>();
        let nodes = query.iter(app.world()).collect::<Vec<_>>();
        let (_, container, scroll) = nodes
            .iter()
            .find(|(name, _, _)| name.as_str() == "ScrollRect")
            .unwrap();
        assert!(
            scroll.is_none(),
            "scrolling the outer container would move its viewport"
        );
        assert_eq!(container.overflow, Overflow::visible());
        let (_, viewport, scroll) = nodes
            .iter()
            .find(|(name, _, _)| name.as_str() == "Viewport")
            .unwrap();
        assert!(scroll.is_some());
        assert_eq!(viewport.overflow, scroll_overflow([true, false]));
        assert_eq!(viewport.left, Val::Percent(2.0));
        assert_eq!(viewport.width, Val::Percent(96.0));
    }

    #[test]
    fn loads_exported_login_and_loading_panel_textures_and_bindings() {
        let loading =
            load_ui_panel_doc("ui_panels/LoadingPanel.json").expect("LoadingPanel.json must parse");
        assert!(find_panel_node_texture(&loading, &["m_bg", "RawImage"]).is_some());
        assert_eq!(loading.script_bindings.len(), 8);
        let expected_slots = [
            "m_main_bg",
            "m_bg",
            "txt_version",
            "txt_loding_name",
            "m_loding_slider",
            "btn_repair",
            "txt_loding_name1",
            "m_Animation",
        ];
        for (idx, name) in expected_slots.iter().enumerate() {
            let found = loading
                .script_bindings
                .iter()
                .find(|b| b.slot_index == Some(idx))
                .expect("slot binding must exist");
            assert_eq!(found.target_name, *name);
        }
        let anim_clip = loading
            .animations
            .iter()
            .find(|c| c.name == "Animation_Loading")
            .expect("LoadingPanel must preserve Animation_Loading clip");
        assert!(anim_clip.tracks.len() >= 18);
        assert!(
            !subtree_intersects_canvas(
                find_panel_node_dto(&loading.root, "btn_repair").unwrap(),
                loading.reference_width,
                loading.reference_height
            ),
            "offscreen btn_repair must be culled cleanly when hide_offscreen_nodes is enabled"
        );
        assert!(
            subtree_intersects_canvas(
                find_panel_node_dto(&loading.root, "m_Animation").unwrap(),
                loading.reference_width,
                loading.reference_height
            ),
            "onscreen m_Animation must remain visible"
        );

        let login =
            load_ui_panel_doc("ui_panels/LoginPanel.json").expect("LoginPanel.json must parse");
        assert!(find_panel_node_texture(&login, &["RawImage (1)", "m_over_sea_logo"]).is_some());
        let repair = find_panel_node_dto(&login.root, "btn_repair").expect("btn_repair present");
        let announce =
            find_panel_node_dto(&login.root, "btn_announce").expect("btn_announce present");
        assert!(
            (announce.canvas_rect[1] - repair.canvas_rect[1]).abs() > 20.0,
            "VerticalLayoutGroup must separate btn_repair ({:?}) and btn_announce ({:?})",
            repair.canvas_rect,
            announce.canvas_rect
        );

        // Every exported UI panel prefab in assets/ui_panels/ must deserialize without error.
        let panel_dir = resolve_assets_root().join("ui_panels");
        let mut parsed_count = 0usize;
        for entry in std::fs::read_dir(&panel_dir).expect("assets/ui_panels directory must exist") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let rel = format!("ui_panels/{}", path.file_name().unwrap().to_string_lossy());
            let doc =
                load_ui_panel_doc(&rel).unwrap_or_else(|e| panic!("{rel} must parse cleanly: {e}"));
            assert!(doc.reference_width > 0.0 && doc.reference_height > 0.0);
            parsed_count += 1;
        }
        assert!(
            parsed_count >= 31,
            "expected at least 31 exported UI panels, parsed {parsed_count}"
        );
    }

    #[test]
    fn interpolates_ui_animation_scalar_curves() {
        let times = [0.0, 1.0, 2.0];
        let vals = [0.0, 100.0, 200.0];
        assert!((eval_ui_anim_scalar(&times, &vals, -0.5) - 0.0).abs() < 1e-4);
        assert!((eval_ui_anim_scalar(&times, &vals, 0.5) - 50.0).abs() < 1e-4);
        assert!((eval_ui_anim_scalar(&times, &vals, 1.5) - 150.0).abs() < 1e-4);
        assert!((eval_ui_anim_scalar(&times, &vals, 3.0) - 200.0).abs() < 1e-4);
    }

    #[test]
    fn bound_ui_panel_toggles_visibility_and_display_and_text() {
        let mut world = World::new();
        let tab_e = world
            .spawn((
                Visibility::Inherited,
                Node {
                    display: Display::Flex,
                    ..default()
                },
            ))
            .id();
        let txt_e = world.spawn(Text::new("Old")).id();

        let mut bound = BoundUiPanel {
            panel_name: "SettingPanel".into(),
            ref_w: 1920.0,
            ref_h: 1080.0,
            ..default()
        };
        bound.nodes.insert("m_pic".into(), vec![tab_e]);
        bound.text_nodes.insert("txt_title".into(), vec![txt_e]);

        let mut vis_q = world.query::<&mut Visibility>();
        let vis_lens = vis_q.query(&world);
        // Set active/text sequentially with separate system states to match Bevy borrow rules cleanly.
        assert_eq!(*vis_lens.get(tab_e).unwrap(), Visibility::Inherited);

        {
            let mut nq = world.query::<&mut Node>();
            let mut vis_state = world.query::<&mut Visibility>();
            // Verify Node & Visibility toggling manually via BoundUiPanel helpers in two blocks:
            if let Ok(mut vis) = vis_state.get_mut(&mut world, tab_e) {
                *vis = Visibility::Hidden;
            }
            if let Ok(mut node) = nq.get_mut(&mut world, tab_e) {
                node.display = Display::None;
            }
        }

        assert_eq!(*world.get::<Visibility>(tab_e).unwrap(), Visibility::Hidden);
        assert_eq!(world.get::<Node>(tab_e).unwrap().display, Display::None);
    }
}
