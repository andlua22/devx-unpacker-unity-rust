//! LoadingScene transition screen (`BattleLoadingPanel` artwork + UDP Welcome gate) and
//! Phase 3 InGame 100-player survival HUD (crosshair, hotbar, resources, and crafting menu).

use bevy::ecs::system::SystemParam;
use bevy::picking::Pickable;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource};
use bevy::ui::ScrollPosition;
use bevy::ui_widgets::{Activate, Button, observe};

use crate::config::{BaseSaveData, GameConfigs, LocalizationDb, PlayerProfileSave, PlayerSessionPrefs};
use crate::net::protocol::{MAX_PLAYERS, OP_CONSUME_BANDAGE, OP_CRAFT_RECIPE};
use crate::net::transport::{ClientLinkPhase, NetClientState, NetServerHandle};
use crate::states::{AppState, InGameRoot, LoadingSceneUiRoot};
use crate::survival::{CRAFT_RECIPES, HotbarItem, SurvivalSessionState};
use crate::ui::panel_loader::{
    BoundUiPanel, UiPaletteButton, UiRetainedOptions, apply_panel_default_view_rules,
    compute_panel_fit_scale, load_ui_panel_doc, spawn_chrome_backdrop,
    spawn_fullscreen_backdrop, spawn_retained_panel_stage, strip_rich_text_tags,
};
use crate::ui::widgets::UiScrollArea;
use crate::world_physics::{BuildPieceKind, EnemyKind};

#[derive(Clone, Copy)]
struct HudTheme {
    card_bg: Color,
    border_col: Color,
    border_px: f32,
    font_delta: f32,
}

impl HudTheme {
    fn from_profile(profile: &PlayerProfileSave) -> Self {
        if profile.high_contrast_hud {
            Self {
                card_bg: Color::srgba(0.02, 0.03, 0.05, 0.96),
                border_col: Color::srgb(0.98, 0.92, 0.28),
                border_px: 2.0,
                font_delta: 1.5,
            }
        } else {
            Self {
                card_bg: Color::srgba(0.05, 0.08, 0.13, 0.86),
                border_col: Color::srgba(0.35, 0.52, 0.78, 0.78),
                border_px: 1.0,
                font_delta: 0.0,
            }
        }
    }
}

const MINIMAP_SIZE_PX: f32 = 136.0;
const MINIMAP_RANGE_M: f32 = 105.0;
const MINIMAP_BLIP_POOL: usize = 24;

#[derive(Resource, Default)]
struct LoadingSceneTimer(f32);

#[derive(Component)]
struct InGameHudText;

#[derive(Component)]
struct CrosshairPromptText;

#[derive(Component)]
struct SurvivalStatsText;

#[derive(Component)]
struct HealthBarFill;

#[derive(Component)]
struct CraftOverlayRoot;

#[derive(Component, Clone, Copy)]
struct HotbarSlot(usize);

#[derive(Component)]
struct TurretQuickSlot;

#[derive(Component)]
struct GeneratorQuickSlot;

#[derive(Component)]
struct CrosshairHintPill;

#[derive(Component, Clone, Copy)]
struct MinimapBlip(usize);

#[derive(Component, Clone, Copy)]
enum InGameHudAction {
    ToggleBots,
    ToggleCraftMenu,
    DisconnectToLobby,
    OpenSandbox,
    SelectHotbar(usize),
    SelectTurretBuild,
    SelectGeneratorBuild,
    SaveBaseNow,
    CraftRecipe(u8),
    UseBandage,
}

/// Registers UI systems for `LoadingScene` and `InGame` HUD overlays.
pub struct InGameHudPlugin;

impl Plugin for InGameHudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LoadingSceneTimer>()
            .add_systems(OnEnter(AppState::LoadingScene), spawn_loading_scene_ui)
            .add_systems(
                Update,
                update_loading_scene_ui.run_if(in_state(AppState::LoadingScene)),
            )
            .add_systems(OnEnter(AppState::InGame), spawn_ingame_hud)
            .add_systems(
                Update,
                (
                    update_ingame_hud_text,
                    update_ingame_survival_widgets,
                    update_battle_minimap_blips,
                )
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

fn spawn_loading_scene_ui(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    configs: Res<GameConfigs>,
    l10n: Res<LocalizationDb>,
    windows: Query<&Window>,
    mut timer: ResMut<LoadingSceneTimer>,
) {
    timer.0 = 0.0;
    let doc = load_ui_panel_doc("ui_panels/BattleLoadingPanel.json").ok();
    let font_source = FontSource::sans_serif();
    let win_size = windows
        .iter()
        .next()
        .map(|w| Vec2::new(w.width(), w.height()))
        .unwrap_or(Vec2::new(1280.0, 720.0));
    let ref_w = doc.as_ref().map(|d| d.reference_width.max(64.0)).unwrap_or(1920.0);
    let ref_h = doc.as_ref().map(|d| d.reference_height.max(64.0)).unwrap_or(1080.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, true);

    let backdrop = crate::ui::loading_panel::select_load_panel_backdrop(&configs, true);

    let mut bound_panel = None;
    let root_e = commands
        .spawn((
            LoadingSceneUiRoot,
            DespawnOnExit(AppState::LoadingScene),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                overflow: Overflow::clip(),
                ..default()
            },
        ))
        .with_children(|outer| {
            spawn_fullscreen_backdrop(outer, &asset_server, None, Color::srgb(0.03, 0.05, 0.08), 0.0);
            if let Some(ref panel_doc) = doc {
                let mut opts = UiRetainedOptions {
                    fullscreen: true,
                    fit_scale,
                    localize_text: true,
                    preview_mode: false,
                    ..default()
                };
                apply_panel_default_view_rules(
                    "BattleLoadingPanel",
                    Some(&l10n),
                    Some(&configs),
                    &mut opts,
                );
                let (_stage_e, bound) = spawn_retained_panel_stage(
                    outer,
                    panel_doc,
                    &asset_server,
                    Some(&l10n),
                    &font_source,
                    fit_scale,
                    &opts,
                );
                bound_panel = Some(bound);
            }
        })
        .id();

    if let Some(bound) = bound_panel {
        if let Some(bg_e) = bound.first("m_bg") {
            let mut img = ImageNode::new(asset_server.load(backdrop)).with_color(Color::WHITE);
            img.image_mode = NodeImageMode::Stretch;
            commands.entity(bg_e).insert(img);
        }
        commands.entity(root_e).insert(bound);
    }
}

#[allow(clippy::too_many_arguments)]
fn update_loading_scene_ui(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut timer: ResMut<LoadingSceneTimer>,
    configs: Res<GameConfigs>,
    l10n: Res<LocalizationDb>,
    client: Res<NetClientState>,
    mut next_state: ResMut<NextState<AppState>>,
    bound_q: Query<&BoundUiPanel, With<LoadingSceneUiRoot>>,
    mut text_q: Query<&mut Text>,
) {
    timer.0 += time.delta_secs();

    if keys.just_pressed(KeyCode::Escape) {
        next_state.set_if_different(AppState::LoginLobby);
        return;
    }

    if !configs.battle_loading_tips.is_empty()
        && let Ok(bound) = bound_q.single()
    {
        let weighted: Vec<&str> = configs
            .battle_loading_tips
            .iter()
            .filter(|t| t.weight > 0.0)
            .map(|t| t.content.as_str())
            .collect();
        let raw_tip = if !weighted.is_empty() {
            let idx = ((timer.0 / 3.5) as usize) % weighted.len();
            weighted[idx]
        } else {
            &configs.battle_loading_tips[0].content
        };
        let tip = strip_rich_text_tags(l10n.tr(raw_tip));
        bound.set_text("txt_tips", &tip, &mut text_q);
    }

    if client.phase == ClientLinkPhase::Connected
        && client.local_player_state().is_some()
        && timer.0 >= 0.25
    {
        next_state.set_if_different(AppState::InGame);
    }
}

fn spawn_ingame_hud(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    profile: Res<PlayerProfileSave>,
) {
    let font_source = FontSource::sans_serif();
    let theme = HudTheme::from_profile(&profile);
    commands
        .spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(12.0)),
                ..default()
            },
        ))
        .with_children(|root| {
            spawn_top_bar(root, &asset_server, &font_source, theme);
            spawn_center_hud_and_crafting(root, &asset_server, &font_source, theme);
            spawn_bottom_hud(root, &asset_server, &font_source, theme);
        });
}

fn spawn_top_bar(
    root: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    font_source: &FontSource,
    theme: HudTheme,
) {
    root.spawn((
        Pickable::IGNORE,
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::FlexStart,
            column_gap: Val::Px(12.0),
            ..default()
        },
    ))
    .with_children(|top_row| {
        // Left: APK BattlePanel status banner (`1BattlePanel_zuo_shang_infoBG_1.png`).
        top_row
            .spawn((
                Node {
                    padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                    flex_shrink: 1.0,
                    max_width: Val::Percent(52.0),
                    border: UiRect::all(Val::Px(theme.border_px)),
                    border_radius: BorderRadius::all(Val::Px(7.0)),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(10.0),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(theme.card_bg),
                BorderColor::all(theme.border_col),
            ))
            .with_children(|info_card| {
                spawn_chrome_backdrop(
                    info_card,
                    asset_server,
                    "ui/1BattlePanel_zuo_shang_infoBG_1.png",
                    Color::srgba(0.92, 0.96, 1.0, 0.36),
                );
                info_card.spawn((
                    Pickable::IGNORE,
                    ImageNode::new(asset_server.load("ui/1BattlePanel_zhouye_ty.png")),
                    Node {
                        width: Val::Px(22.0),
                        height: Val::Px(22.0),
                        ..default()
                    },
                ));
                info_card.spawn((
                    InGameHudText,
                    Pickable::IGNORE,
                    Text::new("Connected"),
                    TextFont {
                        font: font_source.clone(),
                        font_size: FontSize::Px(12.5),
                        ..default()
                    },
                    TextColor(Color::srgb(0.92, 0.96, 1.0)),
                ));
            });

        // Right: BattlePanel action strip + Live BattleMinimap radar (`1BattlePanel_map_di_2_1.png`).
        top_row
            .spawn((
                Pickable::IGNORE,
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::FlexStart,
                    column_gap: Val::Px(10.0),
                    ..default()
                },
            ))
            .with_children(|right| {
                right
                    .spawn((
                        Pickable::IGNORE,
                        Node {
                            flex_direction: FlexDirection::Column,
                            align_items: AlignItems::FlexEnd,
                            row_gap: Val::Px(6.0),
                            ..default()
                        },
                    ))
                    .with_children(|btns_col| {
                        btns_col
                            .spawn((
                                Pickable::IGNORE,
                                Node {
                                    flex_direction: FlexDirection::Row,
                                    column_gap: Val::Px(6.0),
                                    ..default()
                                },
                            ))
                            .with_children(|row1| {
                                spawn_ingame_btn(
                                    row1,
                                    "Craft [Tab]",
                                    InGameHudAction::ToggleCraftMenu,
                                    font_source,
                                );
                                spawn_ingame_btn(
                                    row1,
                                    "Turret/Tesla [T]",
                                    InGameHudAction::SelectTurretBuild,
                                    font_source,
                                );
                                spawn_ingame_btn(
                                    row1,
                                    "Gen/Bat [G]",
                                    InGameHudAction::SelectGeneratorBuild,
                                    font_source,
                                );
                                spawn_ingame_btn(
                                    row1,
                                    "100P Bots [B]",
                                    InGameHudAction::ToggleBots,
                                    font_source,
                                );
                            });
                        btns_col
                            .spawn((
                                Pickable::IGNORE,
                                Node {
                                    flex_direction: FlexDirection::Row,
                                    column_gap: Val::Px(6.0),
                                    ..default()
                                },
                            ))
                            .with_children(|row2| {
                                spawn_ingame_btn(
                                    row2,
                                    "Save Base [F5]",
                                    InGameHudAction::SaveBaseNow,
                                    font_source,
                                );
                                spawn_ingame_btn(
                                    row2,
                                    "Sandbox [F12]",
                                    InGameHudAction::OpenSandbox,
                                    font_source,
                                );
                                spawn_ingame_btn(
                                    row2,
                                    "Lobby [Esc]",
                                    InGameHudAction::DisconnectToLobby,
                                    font_source,
                                );
                            });
                    });

                // Live BattleMinimap radar widget
                right
                    .spawn((
                        Node {
                            width: Val::Px(MINIMAP_SIZE_PX),
                            height: Val::Px(MINIMAP_SIZE_PX),
                            border: UiRect::all(Val::Px(1.5)),
                            border_radius: BorderRadius::all(Val::Px(8.0)),
                            overflow: Overflow::clip(),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.05, 0.09, 0.14, 0.90)),
                        BorderColor::all(Color::srgba(0.45, 0.68, 0.92, 0.85)),
                    ))
                    .with_children(|map| {
                        spawn_chrome_backdrop(
                            map,
                            asset_server,
                            "ui/1BattlePanel_map_di_2_1.png",
                            Color::srgba(0.95, 0.98, 1.0, 0.42),
                        );
                        // Pooled blips for enemies, boss, turrets, structures, and loot drops
                        for i in 0..MINIMAP_BLIP_POOL {
                            map.spawn((
                                MinimapBlip(i),
                                Pickable::IGNORE,
                                Node {
                                    position_type: PositionType::Absolute,
                                    display: Display::None,
                                    left: Val::Px(0.0),
                                    top: Val::Px(0.0),
                                    width: Val::Px(6.0),
                                    height: Val::Px(6.0),
                                    border_radius: BorderRadius::all(Val::Px(3.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgb(0.95, 0.25, 0.25)),
                            ));
                        }
                        // Local player heading marker centered in radar
                        map.spawn((
                            Pickable::IGNORE,
                            ImageNode::new(asset_server.load("ui/1BattlePanel_map_xiaojianjiao.png")),
                            Node {
                                position_type: PositionType::Absolute,
                                left: Val::Px(MINIMAP_SIZE_PX * 0.5 - 7.0),
                                top: Val::Px(MINIMAP_SIZE_PX * 0.5 - 7.0),
                                width: Val::Px(14.0),
                                height: Val::Px(14.0),
                                ..default()
                            },
                        ));
                    });
            });
    });
}

fn spawn_center_hud_and_crafting(
    root: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    font_source: &FontSource,
    theme: HudTheme,
) {
    root.spawn((
        Pickable::IGNORE,
        Node {
            width: Val::Percent(100.0),
            flex_grow: 1.0,
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            row_gap: Val::Px(10.0),
            ..default()
        },
    ))
    .with_children(|center| {
        // Aim reticle using APK crosshair accent + center dot
        center.spawn((
            Pickable::IGNORE,
            ImageNode::new(asset_server.load("ui/1BattlePanel_zunxing_2.png"))
                .with_color(Color::srgba(0.95, 0.98, 1.0, 0.72)),
            Node {
                width: Val::Px(22.0),
                height: Val::Px(22.0),
                ..default()
            },
        ));

        center
            .spawn((
                CrosshairHintPill,
                Pickable::IGNORE,
                Node {
                    margin: UiRect::top(Val::Px(26.0)),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                    border: UiRect::all(Val::Px(theme.border_px)),
                    border_radius: BorderRadius::all(Val::Px(5.0)),
                    ..default()
                },
                BackgroundColor(theme.card_bg),
                BorderColor::all(theme.border_col),
            ))
            .with_children(|pill| {
                pill.spawn((
                    CrosshairPromptText,
                    Pickable::IGNORE,
                    Text::new("LMB: Shoot / Gather | [4-0/T/G] Build | [F] Board/Use | [U] Upgrade"),
                    TextFont {
                        font: font_source.clone(),
                        font_size: FontSize::Px(12.0 + theme.font_delta),
                        ..default()
                    },
                    TextColor(Color::srgb(0.92, 0.96, 0.99)),
                ));
            });

        // `[Tab]` Interactive Crafting & Survival Supply Modal backed by BagAndBuildPanel art
        center
            .spawn((
                CraftOverlayRoot,
                UiScrollArea { speed_px: 1.0 },
                Hovered::default(),
                ScrollPosition::default(),
                Node {
                    display: Display::None,
                    width: Val::Px(470.0),
                    max_height: Val::Percent(64.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(8.0),
                    padding: UiRect::all(Val::Px(14.0)),
                    border: UiRect::all(Val::Px(1.5)),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.06, 0.09, 0.15, 0.95)),
                BorderColor::all(Color::srgb(0.42, 0.68, 0.94)),
            ))
            .with_children(|panel| {
                spawn_chrome_backdrop(
                    panel,
                    asset_server,
                    "ui/BagAndBuildPanel_bg_1.png",
                    Color::srgba(0.85, 0.92, 1.0, 0.25),
                );
                panel
                    .spawn(Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|hdr| {
                        hdr.spawn((
                            Text::new("Field Crafting & Supplies"),
                            TextFont {
                                font: font_source.clone(),
                                font_size: FontSize::Px(16.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                        ));
                        spawn_ingame_btn(
                            hdr,
                            "Close [Tab]",
                            InGameHudAction::ToggleCraftMenu,
                            font_source,
                        );
                    });

                for recipe in CRAFT_RECIPES {
                    let label = format!("{}  ({})", recipe.title, recipe.cost_text);
                    spawn_ingame_btn(
                        panel,
                        &label,
                        InGameHudAction::CraftRecipe(recipe.id),
                        font_source,
                    );
                }

                spawn_ingame_btn(
                    panel,
                    "Use Bandage (+35 HP) [H]",
                    InGameHudAction::UseBandage,
                    font_source,
                );
            });
    });
}

fn spawn_bottom_hud(
    root: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    font_source: &FontSource,
    theme: HudTheme,
) {
    root.spawn((
        Pickable::IGNORE,
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::FlexEnd,
            column_gap: Val::Px(12.0),
            ..default()
        },
    ))
    .with_children(|bottom| {
        // Bottom-left survival vitals, power grid, & vehicle dashboard backed by BattlePanel art
        bottom
            .spawn((
                Node {
                    width: Val::Px(340.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    padding: UiRect::all(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(theme.border_px)),
                    border_radius: BorderRadius::all(Val::Px(7.0)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(theme.card_bg),
                BorderColor::all(theme.border_col),
            ))
            .with_children(|vitals| {
                spawn_chrome_backdrop(
                    vitals,
                    asset_server,
                    "ui/1BattlePanel_xue_bg_1.png",
                    Color::srgba(0.92, 0.96, 1.0, 0.30),
                );
                vitals
                    .spawn((
                        Pickable::IGNORE,
                        Node {
                            width: Val::Percent(100.0),
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(6.0),
                            ..default()
                        },
                    ))
                    .with_children(|hp_row| {
                        hp_row.spawn((
                            Pickable::IGNORE,
                            ImageNode::new(asset_server.load("ui/1BattlePanel_buff_xue.png")),
                            Node {
                                width: Val::Px(16.0),
                                height: Val::Px(16.0),
                                ..default()
                            },
                        ));
                        hp_row
                            .spawn((
                                Node {
                                    flex_grow: 1.0,
                                    height: Val::Px(11.0),
                                    border_radius: BorderRadius::all(Val::Px(4.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgb(0.14, 0.18, 0.24)),
                            ))
                            .with_children(|track| {
                                track.spawn((
                                    HealthBarFill,
                                    Node {
                                        width: Val::Percent(100.0),
                                        height: Val::Percent(100.0),
                                        border_radius: BorderRadius::all(Val::Px(4.0)),
                                        ..default()
                                    },
                                    BackgroundColor(Color::srgb(0.24, 0.84, 0.44)),
                                ));
                            });
                    });

                vitals.spawn((
                    SurvivalStatsText,
                    Pickable::IGNORE,
                    Text::new("HP 100 | Ammo 60 | Bandages 2\nWood 120 | Stone 60 | Iron 25 | Fiber 30"),
                    TextFont {
                        font: font_source.clone(),
                        font_size: FontSize::Px(11.5 + theme.font_delta),
                        ..default()
                    },
                    TextColor(Color::srgb(0.92, 0.96, 1.0)),
                ));
            });

        // Bottom-center 10-slot BattlePanel hotbar + `[T] Turret/Tesla` + `[G] Gen/Bat`
        bottom
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(4.0),
                    padding: UiRect::all(Val::Px(7.0)),
                    border: UiRect::all(Val::Px(theme.border_px)),
                    border_radius: BorderRadius::all(Val::Px(7.0)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(theme.card_bg),
                BorderColor::all(theme.border_col),
            ))
            .with_children(|bar| {
                spawn_chrome_backdrop(
                    bar,
                    asset_server,
                    "ui/1BattlePanel_buttle_diban.png",
                    Color::srgba(0.90, 0.95, 1.0, 0.26),
                );
                for (idx, item) in HotbarItem::SLOTS.iter().enumerate() {
                    let icon_path = match item {
                        HotbarItem::Rifle => "ui/qbz97.png",
                        HotbarItem::Hatchet => "ui/tiechui.png",
                        HotbarItem::Knife => "ui/tb_junyongbishou_da_1.png",
                        HotbarItem::Build(_) => "ui/1BattlePanel_jz_di_1.png",
                    };
                    bar.spawn((
                        Button,
                        Hovered::default(),
                        HotbarSlot(idx),
                        InGameHudAction::SelectHotbar(idx),
                        Node {
                            width: Val::Px(64.0),
                            height: Val::Px(50.0),
                            flex_direction: FlexDirection::Column,
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::Center,
                            padding: UiRect::all(Val::Px(3.0)),
                            border: UiRect::all(Val::Px(1.5)),
                            border_radius: BorderRadius::all(Val::Px(5.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.14, 0.19, 0.27)),
                        BorderColor::all(Color::srgb(0.38, 0.52, 0.74)),
                        observe(on_ingame_hud_action),
                    ))
                    .with_children(|slot| {
                        slot.spawn((
                            Pickable::IGNORE,
                            ImageNode::new(asset_server.load(icon_path)),
                            Node {
                                width: Val::Px(22.0),
                                height: Val::Px(22.0),
                                ..default()
                            },
                        ));
                        slot.spawn((
                            Pickable::IGNORE,
                            Text::new(format!("[{}] {}", HotbarItem::key_hint(idx), item.label())),
                            TextFont {
                                font: font_source.clone(),
                                font_size: FontSize::Px(10.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                        ));
                    });
                }

                // Dedicated `[T] Auto / Tesla Turret` hotbar slot
                bar.spawn((
                    Button,
                    Hovered::default(),
                    TurretQuickSlot,
                    InGameHudAction::SelectTurretBuild,
                    Node {
                        width: Val::Px(64.0),
                        height: Val::Px(50.0),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        padding: UiRect::all(Val::Px(3.0)),
                        border: UiRect::all(Val::Px(1.5)),
                        border_radius: BorderRadius::all(Val::Px(5.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.14, 0.19, 0.27)),
                    BorderColor::all(Color::srgb(0.38, 0.52, 0.74)),
                    observe(on_ingame_hud_action),
                ))
                .with_children(|slot| {
                    slot.spawn((
                        Pickable::IGNORE,
                        ImageNode::new(asset_server.load("ui/tankepao_01.png")),
                        Node {
                            width: Val::Px(22.0),
                            height: Val::Px(22.0),
                            ..default()
                        },
                    ));
                    slot.spawn((
                        Pickable::IGNORE,
                        Text::new("[T] Turret".to_owned()),
                        TextFont {
                            font: font_source.clone(),
                            font_size: FontSize::Px(10.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });

                // Dedicated `[G] Generator / Battery` hotbar slot
                bar.spawn((
                    Button,
                    Hovered::default(),
                    GeneratorQuickSlot,
                    InGameHudAction::SelectGeneratorBuild,
                    Node {
                        width: Val::Px(64.0),
                        height: Val::Px(50.0),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        padding: UiRect::all(Val::Px(3.0)),
                        border: UiRect::all(Val::Px(1.5)),
                        border_radius: BorderRadius::all(Val::Px(5.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.14, 0.19, 0.27)),
                    BorderColor::all(Color::srgb(0.38, 0.52, 0.74)),
                    observe(on_ingame_hud_action),
                ))
                .with_children(|slot| {
                    slot.spawn((
                        Pickable::IGNORE,
                        ImageNode::new(asset_server.load("ui/1BattlePanel_jz_di_1.png")),
                        Node {
                            width: Val::Px(22.0),
                            height: Val::Px(22.0),
                            ..default()
                        },
                    ));
                    slot.spawn((
                        Pickable::IGNORE,
                        Text::new("[G] Power".to_owned()),
                        TextFont {
                            font: font_source.clone(),
                            font_size: FontSize::Px(10.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            });

        bottom.spawn((Pickable::IGNORE, Node { width: Val::Px(20.0), ..default() }));
    });
}

fn spawn_ingame_btn(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    action: InGameHudAction,
    font: &FontSource,
) {
    let palette = UiPaletteButton::default();
    parent
        .spawn((
            Button,
            Hovered::default(),
            action,
            palette,
            Node {
                padding: UiRect::axes(Val::Px(11.0), Val::Px(6.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(5.0)),
                ..default()
            },
            BackgroundColor(palette.normal),
            BorderColor::all(Color::srgb(0.40, 0.56, 0.78)),
            observe(on_ingame_hud_action),
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new(label.to_owned()),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(13.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

#[derive(SystemParam)]
struct HudActionCtx<'w> {
    server_opt: Option<ResMut<'w, NetServerHandle>>,
    client: ResMut<'w, NetClientState>,
    survival: ResMut<'w, SurvivalSessionState>,
    next_state: ResMut<'w, NextState<AppState>>,
}

fn on_ingame_hud_action(
    activate: On<Activate>,
    actions: Query<&InGameHudAction>,
    mut ctx: HudActionCtx,
) {
    let Ok(&act) = actions.get(activate.entity) else {
        return;
    };
    match act {
        InGameHudAction::ToggleBots => {
            if let Some(ref mut srv) = ctx.server_opt {
                let enable = srv.world.bot_count() == 0;
                srv.world.set_stress_bots_enabled(enable);
            }
        }
        InGameHudAction::ToggleCraftMenu => {
            ctx.survival.craft_menu_open = !ctx.survival.craft_menu_open;
        }
        InGameHudAction::OpenSandbox => {
            ctx.next_state.set_if_different(AppState::DevSandbox);
        }
        InGameHudAction::DisconnectToLobby => {
            ctx.next_state.set_if_different(AppState::MainLobby);
        }
        InGameHudAction::SelectHotbar(slot) => {
            ctx.survival.custom_build_override = None;
            ctx.survival.active_slot = slot % HotbarItem::SLOTS.len();
        }
        InGameHudAction::SelectTurretBuild => {
            ctx.survival.custom_build_override = match ctx.survival.custom_build_override {
                Some(BuildPieceKind::Turret) => Some(BuildPieceKind::ElectricTurret),
                Some(BuildPieceKind::ElectricTurret) => None,
                _ => Some(BuildPieceKind::Turret),
            };
        }
        InGameHudAction::SelectGeneratorBuild => {
            ctx.survival.custom_build_override = match ctx.survival.custom_build_override {
                Some(BuildPieceKind::Generator) => Some(BuildPieceKind::Battery),
                Some(BuildPieceKind::Battery) => None,
                _ => Some(BuildPieceKind::Generator),
            };
        }
        InGameHudAction::SaveBaseNow => {
            if let Some(ref srv) = ctx.server_opt {
                let owner = ctx.client.local_player_id.unwrap_or(1);
                let save = srv.world.export_saved_base(owner);
                ctx.survival.status_banner =
                    match save.save_atomic(&BaseSaveData::resolve_save_path()) {
                        Ok(()) => format!("Saved base ({} structures)", save.structures.len()),
                        Err(e) => format!("Base save failed: {e}"),
                    };
                ctx.survival.status_banner_secs = 3.5;
            }
        }
        InGameHudAction::CraftRecipe(recipe_id) => {
            ctx.client
                .send_world_op(OP_CRAFT_RECIPE, recipe_id as u32, 0, 0, Vec3::ZERO);
        }
        InGameHudAction::UseBandage => {
            ctx.client
                .send_world_op(OP_CONSUME_BANDAGE, 0, 0, 0, Vec3::ZERO);
        }
    }
}

fn update_ingame_hud_text(
    client: Res<NetClientState>,
    prefs: Res<PlayerSessionPrefs>,
    configs: Res<GameConfigs>,
    survival: Res<SurvivalSessionState>,
    mut q: Query<&mut Text, With<InGameHudText>>,
) {
    let Ok(mut text) = q.single_mut() else {
        return;
    };
    let pos = client
        .local_player_state()
        .map(|p| format!("({:.1}, {:.1}, {:.1})", p.pos[0], p.pos[1], p.pos[2]))
        .unwrap_or_else(|| "(sync)".to_owned());
    let slot = client
        .local_player_id
        .map(|id| format!("#{id}"))
        .unwrap_or_else(|| "?".to_owned());
    let srv_name = if client.server_name.is_empty() {
        configs
            .active_server(prefs.selected_server_idx)
            .map(|s| s.name.as_str())
            .unwrap_or("Local")
    } else {
        client.server_name.as_str()
    };

    let enemies_alive = client.enemies.values().filter(|e| e.hp > 0).count();
    let boss_hp = client
        .enemies
        .values()
        .find(|e| e.kind == EnemyKind::PortBossMech)
        .map(|b| format!("{}/{}", b.hp, b.max_hp))
        .unwrap_or_else(|| "-".to_owned());

    **text = format!(
        "{srv_name} {slot} | {chunk} | {online}/{max_p} | Builds {builds} | Pwr {pwr}% | Veh {veh} | Threats {threats} | {rtt:.0}ms {pos}",
        chunk = survival.active_chunk,
        online = client.total_online,
        max_p = MAX_PLAYERS,
        builds = client.structures.len(),
        pwr = client.base_power_pct,
        veh = client.vehicles.len(),
        threats = enemies_alive,
        rtt = client.rtt_ms,
    );
    let _ = (prefs, boss_hp);
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_ingame_survival_widgets(
    client: Res<NetClientState>,
    survival: Res<SurvivalSessionState>,
    profile: Res<PlayerProfileSave>,
    mut prompt_q: Query<
        &mut Text,
        (
            With<CrosshairPromptText>,
            Without<InGameHudText>,
            Without<SurvivalStatsText>,
        ),
    >,
    mut hint_pill_q: Query<
        &mut Node,
        (
            With<CrosshairHintPill>,
            Without<HealthBarFill>,
            Without<CraftOverlayRoot>,
        ),
    >,
    mut stats_q: Query<
        &mut Text,
        (
            With<SurvivalStatsText>,
            Without<InGameHudText>,
            Without<CrosshairPromptText>,
        ),
    >,
    mut hp_fill_q: Query<
        (&mut Node, &mut BackgroundColor),
        (
            With<HealthBarFill>,
            Without<CraftOverlayRoot>,
            Without<CrosshairHintPill>,
            Without<HotbarSlot>,
            Without<TurretQuickSlot>,
            Without<GeneratorQuickSlot>,
        ),
    >,
    mut craft_q: Query<
        &mut Node,
        (
            With<CraftOverlayRoot>,
            Without<HealthBarFill>,
            Without<CrosshairHintPill>,
        ),
    >,
    mut hotbar_q: Query<
        (&HotbarSlot, &Hovered, &mut BackgroundColor, &mut BorderColor),
        (
            Without<HealthBarFill>,
            Without<TurretQuickSlot>,
            Without<GeneratorQuickSlot>,
        ),
    >,
    mut turret_q: Query<
        (&Hovered, &mut BackgroundColor, &mut BorderColor),
        (
            With<TurretQuickSlot>,
            Without<HealthBarFill>,
            Without<HotbarSlot>,
            Without<GeneratorQuickSlot>,
        ),
    >,
    mut gen_q: Query<
        (&Hovered, &mut BackgroundColor, &mut BorderColor),
        (
            With<GeneratorQuickSlot>,
            Without<HealthBarFill>,
            Without<HotbarSlot>,
            Without<TurretQuickSlot>,
        ),
    >,
) {
    if let Ok(mut text) = prompt_q.single_mut() {
        **text = if !survival.status_banner.is_empty() {
            format!("{}  |  {}", survival.status_banner, survival.hovered_prompt)
        } else {
            survival.hovered_prompt.clone()
        };
    }
    if let Ok(mut pill) = hint_pill_q.single_mut() {
        let show = profile.show_control_hints
            || survival.driving_vehicle_id != 0
            || survival.custom_build_override.is_some()
            || !survival.status_banner.is_empty();
        pill.display = if show { Display::Flex } else { Display::None };
    }
    if let Ok(mut stats) = stats_q.single_mut() {
        let grid_line = if client.base_power_pct > 0 {
            format!("Grid Power: ON ({}%)", client.base_power_pct)
        } else {
            "Grid Power: OFF (Build [G] Generator)".to_owned()
        };
        let veh_line = if let Some(vk) = survival.active_vehicle_kind {
            format!(
                "\n{}: {:.0} km/h | Fuel {}% | HP {}/{}",
                vk.label(),
                survival.active_vehicle_speed * 3.6,
                survival.active_vehicle_fuel_pct,
                survival.active_vehicle_hp,
                survival.active_vehicle_max_hp,
            )
        } else {
            String::new()
        };
        **stats = format!(
            "HP {}/100 | Ammo {} | Bandages {} | {grid_line}\nWood {} | Stone {} | Iron {} | Fiber {}{veh_line}",
            client.hp,
            client.ammo,
            client.bandages,
            client.wood,
            client.stone,
            client.iron,
            client.fiber,
        );
    }
    if let Ok((mut node, mut bg)) = hp_fill_q.single_mut() {
        let pct = (client.hp as f32).clamp(0.0, 100.0);
        node.width = Val::Percent(pct);
        *bg = BackgroundColor(if pct > 55.0 {
            Color::srgb(0.22, 0.84, 0.44)
        } else if pct > 25.0 {
            Color::srgb(0.92, 0.68, 0.22)
        } else {
            Color::srgb(0.92, 0.26, 0.24)
        });
    }
    if let Ok(mut craft_node) = craft_q.single_mut() {
        craft_node.display = if survival.craft_menu_open {
            Display::Flex
        } else {
            Display::None
        };
    }
    let turret_mode = matches!(
        survival.custom_build_override,
        Some(BuildPieceKind::Turret | BuildPieceKind::ElectricTurret)
    );
    let gen_mode = matches!(
        survival.custom_build_override,
        Some(BuildPieceKind::Generator | BuildPieceKind::Battery)
    );
    let any_custom = survival.custom_build_override.is_some();
    for (slot, hovered, mut bg, mut border) in &mut hotbar_q {
        let selected = !any_custom && slot.0 == survival.active_slot;
        apply_slot_style(&mut bg, &mut border, selected, hovered.get());
    }
    if let Ok((hovered, mut bg, mut border)) = turret_q.single_mut() {
        apply_slot_style(&mut bg, &mut border, turret_mode, hovered.get());
    }
    if let Ok((hovered, mut bg, mut border)) = gen_q.single_mut() {
        apply_slot_style(&mut bg, &mut border, gen_mode, hovered.get());
    }
}

fn apply_slot_style(
    bg: &mut BackgroundColor,
    border: &mut BorderColor,
    selected: bool,
    hovered: bool,
) {
    *bg = BackgroundColor(if selected {
        Color::srgb(0.24, 0.42, 0.62)
    } else if hovered {
        Color::srgb(0.20, 0.28, 0.40)
    } else {
        Color::srgb(0.13, 0.18, 0.26)
    });
    *border = BorderColor::all(if selected {
        Color::srgb(0.95, 0.78, 0.30)
    } else if hovered {
        Color::srgb(0.60, 0.78, 0.96)
    } else {
        Color::srgb(0.36, 0.50, 0.72)
    });
}

fn update_battle_minimap_blips(
    client: Res<NetClientState>,
    mut blip_q: Query<(&MinimapBlip, &mut Node, &mut BackgroundColor)>,
) {
    let Some(local) = client.local_player_state() else {
        return;
    };
    let origin = Vec2::new(local.pos[0], local.pos[2]);
    let yaw = local.yaw;
    let fwd = Vec2::new(yaw.sin(), yaw.cos());
    let right = Vec2::new(-fwd.y, fwd.x);
    let half_px = MINIMAP_SIZE_PX * 0.5;
    let scale = (half_px - 6.0) / MINIMAP_RANGE_M;

    let mut blips: Vec<(f32, f32, f32, Color)> = Vec::with_capacity(MINIMAP_BLIP_POOL);
    let mut push_world = |w: Vec3, sz: f32, col: Color| {
        if blips.len() >= MINIMAP_BLIP_POOL {
            return;
        }
        let rel = Vec2::new(w.x, w.z) - origin;
        let rx = (rel.dot(right) * scale).clamp(-half_px + 4.0, half_px - 4.0);
        let dy = (-rel.dot(fwd) * scale).clamp(-half_px + 4.0, half_px - 4.0);
        blips.push((half_px + rx - sz * 0.5, half_px + dy - sz * 0.5, sz, col));
    };

    for e in client.enemies.values() {
        if e.hp == 0 {
            continue;
        }
        if e.kind == EnemyKind::PortBossMech {
            push_world(e.pos, 9.0, Color::srgb(1.0, 0.18, 0.22));
        } else {
            push_world(e.pos, 6.0, Color::srgb(0.95, 0.32, 0.30));
        }
    }
    for v in client.vehicles.values() {
        push_world(v.pos, 7.0, Color::srgb(0.96, 0.98, 1.0));
    }
    for d in client.loot_drops.values() {
        push_world(d.pos, 6.0, Color::srgb(0.28, 0.90, 0.95));
    }
    for s in client.structures.values() {
        let col = match s.kind {
            BuildPieceKind::Turret => Color::srgb(0.35, 0.88, 0.55),
            BuildPieceKind::ElectricTurret => Color::srgb(0.25, 0.85, 1.0),
            BuildPieceKind::Generator | BuildPieceKind::Battery => Color::srgb(0.98, 0.90, 0.25),
            _ => Color::srgb(0.96, 0.68, 0.24),
        };
        push_world(s.pos, 5.0, col);
    }

    for (tag, mut node, mut bg) in &mut blip_q {
        if let Some(&(bx, by, sz, col)) = blips.get(tag.0) {
            node.display = Display::Flex;
            node.left = Val::Px(bx);
            node.top = Val::Px(by);
            node.width = Val::Px(sz);
            node.height = Val::Px(sz);
            *bg = BackgroundColor(col);
        } else {
            node.display = Display::None;
        }
    }
}
