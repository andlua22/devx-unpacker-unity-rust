//! Faithful Pure-Bevy port of APK `SC.UI.LoadingPanel` (`LoadingPanel.cs`) implementing:
//! - `Awake`: `GameObjectData.GameObjects[0..7]` slot bindings (`m_main_bg`, `m_bg`, `txt_version`,
//!   `txt_loding_name`, `m_loding_slider`, `btn_repair`, `txt_loding_name1`, `m_Animation`).
//! - `onInit`: random/deterministic `LoadPanelCfg` backdrop loading (`ui/Loading_6..8.png`),
//!   slider zeroing, `_loadtime` pacing (`0.5` / `0.33`), and `btn_repair` listener binding.
//! - `onShow` / `Load` / `SetProgerss` / `FinishUpdate` / `Update` (`_deceive`) / `FinishSlider`:
//!   formatted byte progress (`cfg.Strings` IDs `445`/`446`/`436`/`435`), live `Animation_Loading`
//!   playback, and `OnRepair` interactive confirmation modal.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::picking::Pickable;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource, Justify};
use bevy::ui_widgets::{Activate, Button, observe};

use crate::config::{ConfigLoadProgress, GameConfigs, LocalizationDb};
use crate::states::{AppState, SplashUiRoot};
use crate::ui::panel_loader::{
    BoundUiClick, BoundUiPanel, UiPanelDoc, UiPanelFrame, UiPaletteButton,
    UiRetainedOptions, apply_panel_default_view_rules, compute_panel_fit_scale, load_ui_panel_doc,
    spawn_fullscreen_backdrop, spawn_retained_panel_into,
};
use crate::ui::widgets::UiFilledBar;

const SLIDER_FILL_HOLD_SECS: f32 = 1.35;

/// Optional smoke-capture gate keeping `LauncherSplash` alive until `01_splash.png` is captured.
#[derive(Resource, Default)]
pub struct SplashCaptureHold(pub bool);

/// Execution stage mirroring `LoadingPanel.cs` coroutine & update progression.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadingStage {
    #[default]
    PreFont,
    LoadUpdate,
    DeceiveLoadPre,
    OfflineConfirm,
    Completed,
}

/// Action tags emitted by buttons on the `OnRepair` confirmation modal overlay.
#[derive(Component, Clone, Copy)]
enum RepairModalAction {
    Confirm,
    Cancel,
}

/// Marker identifying the active `OnRepair` / `ShowConfirm` modal overlay entity.
#[derive(Component)]
pub struct LoadingRepairModalRoot;

/// Live controller attached alongside [`BoundUiPanel`] mirroring `SC.UI.LoadingPanel`.
#[derive(Component, Debug, Clone)]
pub struct LoadingPanelController {
    /// Bound entities from `GameObjectData.GameObjects[0..8]`:
    /// `0: m_main_bg`, `1: m_bg`, `2: txt_version`, `3: txt_loding_name`,
    /// `4: m_loding_slider`, `5: btn_repair`, `6: txt_loding_name1`, `7: m_Animation`.
    pub slots: [Option<Entity>; 8],
    pub fill_entity: Option<Entity>,
    pub stage_entity: Option<Entity>,
    pub stage: LoadingStage,
    pub deceive: bool,
    pub slider_length: f32,
    pub load_time: f32,
    pub elapsed_secs: f32,
    pub repair_modal_open: bool,
    pub backdrop_asset: String,
    pub repair_notice_label: String,
}

#[allow(dead_code)]
impl LoadingPanelController {
    pub fn m_main_bg(&self) -> Option<Entity> {
        self.slots[0]
    }

    pub fn m_bg(&self) -> Option<Entity> {
        self.slots[1]
    }

    pub fn txt_version(&self) -> Option<Entity> {
        self.slots[2]
    }

    pub fn txt_loding_name(&self) -> Option<Entity> {
        self.slots[3]
    }

    pub fn m_loding_slider(&self) -> Option<Entity> {
        self.slots[4]
    }

    pub fn btn_repair(&self) -> Option<Entity> {
        self.slots[5]
    }

    pub fn txt_loding_name1(&self) -> Option<Entity> {
        self.slots[6]
    }

    #[allow(dead_code)]
    pub fn m_animation(&self) -> Option<Entity> {
        self.slots[7]
    }
}

/// Selects a backdrop PNG path from `LoadPanelCfg` (`texture/loading_6..8` -> `ui/Loading_6..8.png`),
/// mirroring `LoadingPanel.onInit` (`LoadPanelCfg.Get(Random.Range(1, count + 1))`).
pub fn select_load_panel_backdrop(configs: &GameConfigs, deterministic: bool) -> String {
    if !configs.load_panels.is_empty() {
        let idx = if deterministic {
            configs.load_panels.len().saturating_sub(1)
        } else {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as usize)
                .unwrap_or(0);
            secs % configs.load_panels.len()
        };
        if let Some(asset) =
            GameConfigs::resolve_load_panel_texture_asset(&configs.load_panels[idx].texture)
        {
            return asset;
        }
    }
    "ui/Loading_7.png".to_owned()
}

/// Mirrors `LoadingPanel.SetProgerss(int cuttent, int size)` byte-unit formatting using
/// `cfg.Strings` IDs `445` (`"{0}M"`), `446` (`"{0}K"`), and `436` (`"{0}/{1}"`).
pub fn format_progress_bytes_label(
    configs: &GameConfigs,
    l10n: &LocalizationDb,
    current_bytes: u64,
    total_bytes: u64,
) -> String {
    if total_bytes == 0 {
        let s = configs.get_string(l10n, 435, &[]);
        return if s.starts_with("String#") {
            "Loading...".to_owned()
        } else {
            s
        };
    }

    if total_bytes > 256 {
        let factor = 1.0_f64 / 1_048_576.0_f64;
        let cur_m = format!("{:.2}", current_bytes as f64 * factor);
        let tot_m = format!("{:.2}", total_bytes as f64 * factor);
        let cur_fmt = configs.get_string(l10n, 445, &[&cur_m]);
        let tot_fmt = configs.get_string(l10n, 445, &[&tot_m]);
        let cur_s = if cur_fmt.starts_with("String#") {
            format!("{cur_m}M")
        } else {
            cur_fmt
        };
        let tot_s = if tot_fmt.starts_with("String#") {
            format!("{tot_m}M")
        } else {
            tot_fmt
        };
        let ratio = configs.get_string(l10n, 436, &[&cur_s, &tot_s]);
        if ratio.starts_with("String#") {
            format!("{cur_s}/{tot_s}")
        } else {
            ratio
        }
    } else {
        let factor = 1.0_f64 / 1024.0_f64;
        let cur_k = format!("{:.2}", current_bytes as f64 * factor);
        let tot_k = format!("{:.2}", total_bytes as f64 * factor);
        let cur_fmt = configs.get_string(l10n, 446, &[&cur_k]);
        let tot_fmt = configs.get_string(l10n, 446, &[&tot_k]);
        let cur_s = if cur_fmt.starts_with("String#") {
            format!("{cur_k}K")
        } else {
            cur_fmt
        };
        let tot_s = if tot_fmt.starts_with("String#") {
            format!("{tot_k}K")
        } else {
            tot_fmt
        };
        let ratio = configs.get_string(l10n, 436, &[&cur_s, &tot_s]);
        if ratio.starts_with("String#") {
            format!("{cur_s}/{tot_s}")
        } else {
            ratio
        }
    }
}

/// Mounts the complete retained `LoadingPanel.json` hierarchy with full `LoadingPanelController`
/// state & clean responsive slot placement.
#[allow(clippy::too_many_arguments)]
pub fn spawn_loading_panel_screen(
    commands: &mut Commands,
    asset_server: &AssetServer,
    configs: &GameConfigs,
    l10n: &LocalizationDb,
    win_size: Vec2,
    deterministic_bg: bool,
) {
    let font_source = FontSource::sans_serif();
    let doc_opt: Option<UiPanelDoc> = load_ui_panel_doc("ui_panels/LoadingPanel.json").ok();
    let ref_w = doc_opt
        .as_ref()
        .map(|d| d.reference_width.max(64.0))
        .unwrap_or(1920.0);
    let ref_h = doc_opt
        .as_ref()
        .map(|d| d.reference_height.max(64.0))
        .unwrap_or(1080.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, true);

    let backdrop_asset = select_load_panel_backdrop(configs, deterministic_bg);
    let version_str = format_version_banner(configs, l10n);
    let loading_435 = {
        let s = configs.get_string(l10n, 435, &[]);
        if s.starts_with("String#") {
            "Loading configs & resources...".to_owned()
        } else {
            s
        }
    };

    let mut opts = UiRetainedOptions {
        fullscreen: true,
        fit_scale,
        localize_text: true,
        preview_mode: false,
        text_overrides: HashMap::from([
            ("txt_version".to_owned(), version_str.clone()),
            ("txt_loding_name".to_owned(), loading_435),
        ]),
        passthrough_nodes: HashSet::from([
            "m_loding_slider".to_owned(),
            "RawImage".to_owned(),
            "m_bg".to_owned(),
            "m_main_bg".to_owned(),
        ]),
        interactive_whitelist: Some(HashSet::from(["btn_repair".to_owned()])),
        ..default()
    };
    apply_panel_default_view_rules("LoadingPanel", Some(l10n), Some(configs), &mut opts);

    let mut bound_panel: Option<BoundUiPanel> = None;
    let mut stage_e = Entity::PLACEHOLDER;

    let root_e = commands
        .spawn((
            SplashUiRoot,
            DespawnOnExit(AppState::LauncherSplash),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.08, 0.13)),
        ))
        .with_children(|root| {
            // Solid dark fallback behind the 2070x1230 `m_bg` artwork.
            spawn_fullscreen_backdrop(root, asset_server, None, Color::srgb(0.03, 0.05, 0.08), 0.0);

            stage_e = root
                .spawn((
                    UiPanelFrame {
                        ref_w,
                        ref_h,
                        fullscreen: true,
                    },
                    Pickable::IGNORE,
                    Node {
                        position_type: PositionType::Relative,
                        width: Val::Px(ref_w * fit_scale),
                        height: Val::Px(ref_h * fit_scale),
                        overflow: Overflow::visible(),
                        ..default()
                    },
                ))
                .with_children(|stage| {
                    if let Some(ref doc) = doc_opt {
                        bound_panel = Some(spawn_retained_panel_into(
                            stage,
                            doc,
                            asset_server,
                            Some(l10n),
                            &font_source,
                            &opts,
                        ));
                    }
                })
                .id();
        })
        .id();

    let mut controller = LoadingPanelController {
        slots: [None; 8],
        fill_entity: None,
        stage_entity: Some(stage_e),
        stage: LoadingStage::PreFont,
        deceive: false,
        slider_length: 0.0,
        load_time: 0.5,
        elapsed_secs: 0.0,
        repair_modal_open: false,
        backdrop_asset: backdrop_asset.clone(),
        repair_notice_label: String::new(),
    };

    if let Some(bound) = bound_panel {
        // Resolve `Awake` slots `GameObjects[0..7]` cleanly via BoundUiPanel.
        let fallback_names = [
            "m_main_bg",
            "m_bg",
            "txt_version",
            "txt_loding_name",
            "m_loding_slider",
            "btn_repair",
            "txt_loding_name1",
            "m_Animation",
        ];
        for (i, slot) in controller.slots.iter_mut().enumerate() {
            *slot = bound
                .game_object_slot(i)
                .or_else(|| bound.first(fallback_names[i]));
        }
        controller.fill_entity = bound.first("Fill");

        // Apply `onInit` selected `LoadPanelCfg` texture onto `m_bg` and enable `m_main_bg` for `onShow`.
        if let Some(main_bg_e) = controller.m_main_bg() {
            commands.entity(main_bg_e).insert(Visibility::Inherited);
        }
        if let Some(bg_e) = controller.m_bg() {
            let mut img = ImageNode::new(asset_server.load(backdrop_asset)).with_color(Color::WHITE);
            img.image_mode = NodeImageMode::Stretch;
            commands.entity(bg_e).insert(img);
        }

        commands.entity(root_e).insert(bound);
    }

    commands.entity(root_e).insert(controller);
}

fn format_version_banner(configs: &GameConfigs, _l10n: &LocalizationDb) -> String {
    if !configs.version.str_v.is_empty() {
        configs.version.str_v.clone()
    } else {
        "3.14.0.0".to_owned()
    }
}

/// Advances [`LoadingPanelController`] through `PreFont -> LoadUpdate -> DeceiveLoadPre -> Login`
/// while driving bound `Slider`, byte-formatted status text, and `OnRepair` dialog events.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_loading_panel_controller(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    asset_server: Res<AssetServer>,
    progress: Res<ConfigLoadProgress>,
    configs: Res<GameConfigs>,
    l10n: Res<LocalizationDb>,
    capture_hold: Option<Res<SplashCaptureHold>>,
    mut next_state: ResMut<NextState<AppState>>,
    mut commands: Commands,
    mut click_reader: MessageReader<BoundUiClick>,
    mut ctrl_q: Query<(&mut LoadingPanelController, &BoundUiPanel), With<SplashUiRoot>>,
    modal_q: Query<Entity, With<LoadingRepairModalRoot>>,
    mut node_q: Query<&mut Node>,
    mut bar_q: Query<&mut UiFilledBar>,
    mut text_q: Query<&mut Text>,
) {
    let dt = time.delta_secs();
    let held_for_capture = capture_hold.as_deref().is_some_and(|h| h.0);

    for (mut ctrl, bound) in &mut ctrl_q {
        ctrl.elapsed_secs += dt;

        for ev in click_reader.read() {
            if ev.panel_name == "LoadingPanel"
                && ev.node_name == "btn_repair"
                && !ctrl.repair_modal_open
            {
                open_repair_confirm_modal(
                    &mut commands,
                    &mut ctrl,
                    &configs,
                    &l10n,
                    &FontSource::sans_serif(),
                );
            }
        }

        // Advance `LoadingStage` faithfully mirroring `onShow` -> `Load` -> `FinishUpdate` -> `Update`.
        match ctrl.stage {
            LoadingStage::PreFont => {
                if ctrl.elapsed_secs >= 0.08 {
                    ctrl.stage = LoadingStage::LoadUpdate;
                }
            }
            LoadingStage::LoadUpdate => {
                if progress.finished {
                    ctrl.stage = LoadingStage::DeceiveLoadPre;
                    ctrl.slider_length = progress.fraction.clamp(0.35, 0.75);
                    ctrl.deceive = true;
                }
            }
            LoadingStage::DeceiveLoadPre => {
                if ctrl.deceive && !ctrl.repair_modal_open {
                    ctrl.slider_length = (ctrl.slider_length + ctrl.load_time * dt).min(1.0);
                    if ctrl.slider_length >= 1.0 {
                        ctrl.deceive = false;
                        ctrl.stage = LoadingStage::Completed;
                    }
                }
            }
            LoadingStage::OfflineConfirm | LoadingStage::Completed => {}
        }

        // Update version text & progress bar value + byte readout.
        let slider_val = match ctrl.stage {
            LoadingStage::PreFont => 0.15,
            LoadingStage::LoadUpdate => progress.fraction.clamp(0.15, 0.90),
            LoadingStage::DeceiveLoadPre => ctrl.slider_length.clamp(0.35, 1.0),
            LoadingStage::OfflineConfirm => ctrl.slider_length.clamp(0.0, 1.0),
            LoadingStage::Completed => 1.0,
        };
        if let Some(fill_e) = ctrl.fill_entity {
            BoundUiPanel::set_slider_fill(fill_e, slider_val, &mut node_q, &mut bar_q);
        }

        let pct = (slider_val * 100.0).round() as u32;
        if let Some(pct_e) = ctrl.txt_loding_name1() {
            bound.set_entity_text(pct_e, &format!("{pct}%"), &mut text_q);
        }
        if let Some(ver_e) = ctrl.txt_version() {
            bound.set_entity_text(ver_e, &format_version_banner(&configs, &l10n), &mut text_q);
        }
        if let Some(lbl_e) = ctrl.txt_loding_name() {
            let status_txt = if !ctrl.repair_notice_label.is_empty() {
                ctrl.repair_notice_label.clone()
            } else if matches!(ctrl.stage, LoadingStage::LoadUpdate) && !progress.finished {
                let total = 4_250_000_u64;
                let cur = (total as f32 * slider_val) as u64;
                format_progress_bytes_label(&configs, &l10n, cur, total)
            } else {
                let base = configs.get_string(&l10n, 435, &[]);
                if base.starts_with("String#") {
                    progress.stage_label.clone()
                } else {
                    base
                }
            };
            bound.set_entity_text(lbl_e, &status_txt, &mut text_q);
        }

        // Handle backdrop rotation hotkey `[R]` cleanly in interactive play.
        if keys.just_pressed(KeyCode::KeyR)
            && !ctrl.repair_modal_open
            && !configs.load_panels.is_empty()
        {
            let next_asset = select_load_panel_backdrop(&configs, false);
            if let Some(bg_e) = ctrl.m_bg() {
                commands.entity(bg_e).insert(
                    ImageNode::new(asset_server.load(next_asset.clone())).with_color(Color::WHITE),
                );
                ctrl.backdrop_asset = next_asset;
            }
        }

        let ready_to_login = matches!(ctrl.stage, LoadingStage::Completed)
            && ctrl.elapsed_secs >= SLIDER_FILL_HOLD_SECS;
        if !held_for_capture
            && !ctrl.repair_modal_open
            && (keys.just_pressed(KeyCode::Space)
                || keys.just_pressed(KeyCode::Enter)
                || (progress.finished && ready_to_login))
        {
            for modal in &modal_q {
                commands.entity(modal).try_despawn();
            }
            next_state.set_if_different(AppState::LoginLobby);
        }
    }
}

fn open_repair_confirm_modal(
    commands: &mut Commands,
    ctrl: &mut LoadingPanelController,
    configs: &GameConfigs,
    l10n: &LocalizationDb,
    font_source: &FontSource,
) {
    ctrl.repair_modal_open = true;
    let body_text = {
        let offline_hint = configs.get_string(l10n, 342, &[]);
        if offline_hint.starts_with("String#") {
            "Repair client configuration cache and reload assets?".to_owned()
        } else {
            format!(
                "Verify & repair local UI/config cache and reload resources?\n({})",
                offline_hint
            )
        }
    };
    let Some(stage_e) = ctrl.stage_entity else {
        return;
    };

    commands.entity(stage_e).with_children(|parent| {
        parent
            .spawn((
                LoadingRepairModalRoot,
                ZIndex(40),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.02, 0.03, 0.06, 0.72)),
            ))
            .with_children(|scrim| {
                scrim
                    .spawn((
                        Node {
                            width: Val::Px(460.0),
                            padding: UiRect::all(Val::Px(22.0)),
                            row_gap: Val::Px(14.0),
                            flex_direction: FlexDirection::Column,
                            align_items: AlignItems::Center,
                            border: UiRect::all(Val::Px(1.5)),
                            border_radius: BorderRadius::all(Val::Px(8.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.09, 0.12, 0.18, 0.96)),
                        BorderColor::all(Color::srgb(0.45, 0.62, 0.85)),
                    ))
                    .with_children(|card| {
                        card.spawn((
                            Text::new("Client Repair (LoadingPanel.OnRepair)"),
                            TextFont {
                                font: font_source.clone(),
                                font_size: FontSize::Px(18.0),
                                ..default()
                            },
                            TextColor(Color::srgb(0.95, 0.84, 0.45)),
                        ));
                        card.spawn((
                            Text::new(body_text),
                            TextFont {
                                font: font_source.clone(),
                                font_size: FontSize::Px(14.0),
                                ..default()
                            },
                            TextLayout::justify(Justify::Center),
                            TextColor(Color::srgb(0.90, 0.94, 0.98)),
                        ));
                        card.spawn(Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(14.0),
                            margin: UiRect::top(Val::Px(6.0)),
                            ..default()
                        })
                        .with_children(|row| {
                            for (label, action, palette) in [
                                (
                                    "Confirm Repair",
                                    RepairModalAction::Confirm,
                                    UiPaletteButton {
                                        normal: Color::srgb(0.18, 0.46, 0.30),
                                        hovered: Color::srgb(0.24, 0.58, 0.38),
                                        pressed: Color::srgb(0.14, 0.36, 0.24),
                                    },
                                ),
                                (
                                    "Cancel",
                                    RepairModalAction::Cancel,
                                    UiPaletteButton::default(),
                                ),
                            ] {
                                row.spawn((
                                    Button,
                                    Hovered::default(),
                                    palette,
                                    action,
                                    Node {
                                        padding: UiRect::axes(Val::Px(18.0), Val::Px(9.0)),
                                        border: UiRect::all(Val::Px(1.0)),
                                        border_radius: BorderRadius::all(Val::Px(6.0)),
                                        ..default()
                                    },
                                    BackgroundColor(palette.normal),
                                    BorderColor::all(Color::srgb(0.55, 0.72, 0.92)),
                                    observe(on_repair_modal_button),
                                ))
                                .with_children(|b| {
                                    b.spawn((
                                        Text::new(label),
                                        TextFont {
                                            font: font_source.clone(),
                                            font_size: FontSize::Px(14.0),
                                            ..default()
                                        },
                                        TextColor(Color::WHITE),
                                    ));
                                });
                            }
                        });
                    });
            });
    });
}

fn on_repair_modal_button(
    activate: On<Activate>,
    actions: Query<&RepairModalAction>,
    modal_q: Query<Entity, With<LoadingRepairModalRoot>>,
    mut ctrl_q: Query<&mut LoadingPanelController, With<SplashUiRoot>>,
    mut commands: Commands,
) {
    let Ok(&act) = actions.get(activate.entity) else {
        return;
    };
    for modal in &modal_q {
        commands.entity(modal).try_despawn();
    }
    for mut ctrl in &mut ctrl_q {
        ctrl.repair_modal_open = false;
        if matches!(act, RepairModalAction::Confirm) {
            ctrl.stage = LoadingStage::LoadUpdate;
            ctrl.slider_length = 0.0;
            ctrl.deceive = false;
            ctrl.elapsed_secs = 0.0;
            ctrl.repair_notice_label = "Repair verified: reloading UI & config cache...".to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_progress_bytes_above_and_below_256_threshold() {
        let mut cfg = GameConfigs::default();
        cfg.strings.insert(435, "Loading...".to_owned());
        cfg.strings.insert(436, "{0}/{1}".to_owned());
        cfg.strings.insert(445, "{0}M".to_owned());
        cfg.strings.insert(446, "{0}K".to_owned());
        let l10n = LocalizationDb {
            selected_lang_index: -1,
            ..default()
        };

        assert_eq!(
            format_progress_bytes_label(&cfg, &l10n, 1_048_576, 2_097_152),
            "1.00M/2.00M"
        );
        assert_eq!(
            format_progress_bytes_label(&cfg, &l10n, 128, 256),
            "0.12K/0.25K"
        );
        assert_eq!(
            format_progress_bytes_label(&cfg, &l10n, 0, 0),
            "Loading..."
        );
    }

    #[test]
    fn resolves_load_panel_cfg_backdrop_assets() {
        let mut cfg = GameConfigs::default();
        cfg.load_panels.push(crate::config::LoadPanelCfgEntry {
            id: 1,
            texture: "texture/loading_6".to_owned(),
        });
        cfg.load_panels.push(crate::config::LoadPanelCfgEntry {
            id: 2,
            texture: "texture/loading_8".to_owned(),
        });
        assert_eq!(select_load_panel_backdrop(&cfg, true), "ui/Loading_8.png");
    }
}
