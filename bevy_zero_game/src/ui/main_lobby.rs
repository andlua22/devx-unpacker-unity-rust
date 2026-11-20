//! Pure-Bevy `AppState::MainLobby` UI retaining APK `MainPanel.json`, `EstablishPanel.json`,
//! `BagAndBuildPanel.json`, and `SettingPanel.json` 1:1 with off-screen 3D `RenderTarget`
//! inside `m_hero_tex`, native `UiTextInput`/`UiSlider`/`UiToggle` widgets, local JSON profile
//! saving, and 100-player UDP match deployment.

use std::collections::{HashMap, HashSet};

use bevy::picking::Pickable;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource};
use bevy::ui::ScrollPosition;
use bevy::ui_widgets::{Activate, Button, observe};

use crate::actor_rig::{select_weapon_prop, selectable_weapon_prop_indices};
use crate::auth::{AuthMgr, NAME_ERR_EMPTY, NAME_ERR_SPECIAL, PlatformMgr, check_name};
use crate::config::{
    GameConfigs, KeybindProfile, LocalizationDb, PlayerProfileSave, PlayerProfileStorage,
    PlayerSessionPrefs,
};
use crate::lobby::{LobbyHeroPortraitRt, MainLobbyStageState};
use crate::net::transport::NetClientState;
use crate::states::AppState;
use crate::ui::panel_loader::{
    BoundUiClick, BoundUiPanel, UiPaletteButton, UiPanelAnimator, UiPanelDoc, UiPanelFrame,
    UiPanelGraphicDto, UiPanelNodeDto, UiRetainedOptions, UiTextureDto,
    compute_panel_canvas_metrics, compute_panel_fit_scale, find_panel_node_dto, load_ui_panel_doc,
    spawn_retained_panel_into,
};
use crate::ui::widgets::{
    UiScrollArea, UiSliderChanged, UiTextInput, UiTextInputChanged, UiTextInputPlaceholder,
    UiToggleChanged, attach_text_input_observers,
};

#[path = "lobby_panels.rs"]
mod lobby_panels;
use lobby_panels::{NativeLobbyBindings, NativeLobbyPanel};

#[derive(Component)]
struct LobbyUiRoot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LobbyTab {
    #[default]
    Overview,
    Establish,
    Armory,
    Settings,
    Native(NativeLobbyPanel),
}

#[derive(Resource, Default)]
pub(crate) struct MainLobbyUiState {
    pub(crate) active_tab: LobbyTab,
    /// `EstablishPanel` opened with `type = 1` (mandatory post-login character creation).
    pub(crate) force_create: bool,
    pub(crate) dirty: bool,
    status_toast: String,
    settings_subtab: u8,
    native_panel_page: i32,
    store_selected_instance: Option<i32>,
    editing_native_key: Option<(i32, usize)>,
    native_key_capture_frame: bool,
    footer_pose: MainPanelFooterPose,
    /// Native EstablishPanel caches skin/hair color choices by creation preset.
    role_color_cache: HashMap<i32, [i32; 2]>,
    name_recommendation_index: usize,
    /// Native `_currentSkinSet` is a preview until create/remodel confirmation.
    establish_preview: Option<EstablishProfilePreview>,
}

#[derive(Debug, Clone)]
struct EstablishProfilePreview {
    committed: PlayerProfileSave,
}

impl EstablishProfilePreview {
    fn owns(&self, profile: &PlayerProfileSave) -> bool {
        self.committed.account_name == profile.account_name
            && self.committed.active_role_key == profile.active_role_key
    }

    fn restore(&self, profile: &mut PlayerProfileSave) -> bool {
        if !self.owns(profile) {
            return false;
        }
        let saved = &self.committed;
        profile.role_name.clone_from(&saved.role_name);
        profile.role_preset_id = saved.role_preset_id;
        profile.sex_male = saved.sex_male;
        profile.body_variant_idx = saved.body_variant_idx;
        profile.head_variant_idx = saved.head_variant_idx;
        profile.hair_variant_idx = saved.hair_variant_idx;
        profile.skin_color_idx = saved.skin_color_idx;
        profile.skin_color_id = saved.skin_color_id;
        profile.hair_color_id = saved.hair_color_id;
        profile.head_item_id = saved.head_item_id;
        profile.face_item_id = saved.face_item_id;
        profile.gold = saved.gold;
        profile.diamond = saved.diamond;
        profile.coupons = saved.coupons;
        true
    }
}

impl MainLobbyUiState {
    fn begin_establish_preview(&mut self, profile: &PlayerProfileSave) {
        if self.active_tab != LobbyTab::Establish {
            return;
        }
        if self
            .establish_preview
            .as_ref()
            .is_some_and(|preview| !preview.owns(profile))
        {
            self.establish_preview = None;
        }
        if self.establish_preview.is_none() {
            self.establish_preview = Some(EstablishProfilePreview {
                committed: profile.clone(),
            });
        }
    }

    fn cancel_establish_preview(&mut self, profile: &mut PlayerProfileSave) -> bool {
        self.role_color_cache.clear();
        self.establish_preview
            .take()
            .is_some_and(|preview| preview.restore(profile))
    }
}

/// `SettingMgr.MainBottomShowMode` defaults to zero in the shipped ARM64 binary.
/// `InitMainBottom` (0x17EF4D4) therefore hides the second row on first display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum MainPanelFooterPose {
    #[default]
    PrefabCollapsed,
    Expanded,
    Collapsed,
}

const MAIN_PANEL_FOOTER_WAIT_SECS: f32 = 0.08;
const MAIN_PANEL_FOOTER_OPEN_CLIPS: [&str; 2] = ["DowntoUp", "Mainpanel_chat_downtoup"];
const MAIN_PANEL_FOOTER_CLOSE_CLIPS: [&str; 2] = ["UptoDown", "Mainpanel_chat_uptodown"];

fn main_panel_footer_clips(animator: &UiPanelAnimator, expanded: bool) -> &'static [&'static str] {
    let names = if expanded {
        &MAIN_PANEL_FOOTER_OPEN_CLIPS
    } else {
        &MAIN_PANEL_FOOTER_CLOSE_CLIPS
    };
    // The shipped m_middle_btns Animator has a null controller. Native Animator.Play
    // therefore leaves this row at its prefab pose while the real chat clip still runs.
    // An InjectFix/runtime controller assignment elsewhere remains unverified.
    if animator.clips.iter().any(|clip| clip.name == names[0]) {
        names
    } else {
        &names[1..]
    }
}

#[derive(Debug, Clone, Copy)]
struct MainPanelFooterTransition {
    expanded: bool,
    elapsed_secs: f32,
}

/// Binds `Awake` GameObjectData slots 92/94/116 to the original footer controller.
#[derive(Component)]
struct MainPanelFooter {
    show_button: Entity,
    hide_button: Entity,
    show_hitboxes: Vec<Entity>,
    hide_hitboxes: Vec<Entity>,
    holder: Entity,
    animator: Option<Entity>,
    pose: MainPanelFooterPose,
    initialized: bool,
    transitions: Vec<MainPanelFooterTransition>,
}

impl MainPanelFooter {
    fn from_panel(bound: &BoundUiPanel, pose: MainPanelFooterPose) -> Option<Self> {
        let show_button = bound.game_object_slot(92)?;
        let hide_button = bound.game_object_slot(94)?;
        // Retained child artwork forwards its parent's button name. Resolve those hitboxes
        // under the exact GameObjectData entity rather than accepting same-named siblings.
        let hitboxes = |button| {
            let path = bound
                .by_path
                .iter()
                .find_map(|(path, &entity)| (entity == button).then_some(path.as_str()));
            let Some(path) = path else {
                return vec![button];
            };
            let prefix = format!("{path}/");
            bound
                .by_path
                .iter()
                .filter_map(|(candidate, &entity)| {
                    (candidate == path || candidate.starts_with(&prefix)).then_some(entity)
                })
                .collect()
        };
        Some(Self {
            show_button,
            hide_button,
            show_hitboxes: hitboxes(show_button),
            hide_hitboxes: hitboxes(hide_button),
            holder: bound.game_object_slot(116)?,
            animator: bound.animator_entity,
            pose,
            initialized: false,
            transitions: Vec::new(),
        })
    }

    fn clicked_expanded_state(&self, click: &BoundUiClick) -> Option<bool> {
        if click.panel_name != "MainPanel" {
            return None;
        }
        match (click.node_name.as_str(), click.entity) {
            ("btn_show", entity) if self.show_hitboxes.contains(&entity) => Some(true),
            ("btn_hide", entity) if self.hide_hitboxes.contains(&entity) => Some(false),
            _ => None,
        }
    }
}

#[allow(dead_code)]
#[derive(Component, Clone, Copy)]
enum LobbyAction {
    SwitchTab(LobbyTab),
    OpenNativePanel(NativeLobbyPanel),
    OpenWarChoice(u8),
    NativeServiceUnavailable(&'static str),
    SetNativeSetting(&'static str, f32),
    AdjustNativeSlider(&'static str, i8),
    SetNativePanelPage(i32),
    SelectStoreInstance(i32),
    UseStoreItem,
    CloseStoreDescription,
    BeginNativeKeyEdit(i32, usize),
    ResetNativeKeys,
    SetSettingsSubtab(u8),
    RandomCallsign,
    SetGenderMale(bool),
    SelectEstablishRole(i32),
    SelectEstablishHair(usize, i32),
    SelectSkinColorId(i32),
    SelectHairColorId(i32),
    ResetEstablishRole,
    CloseEstablishPanel,
    CycleBodyVariant,
    CycleHeadVariant,
    CycleHairVariant,
    CycleEstablishPreset,
    CycleSkinColor,
    SelectBg(usize),
    ClaimSupplyReward,
    EquipWeapon(usize),
    CycleFpsTier,
    CycleQualityTier,
    CycleLanguage,
    CycleKeybindPreset,
    CycleInteractKey,
    CycleServer,
    ToggleAutoRotate,
    SaveProfileNow,
    ConfirmCreateRole,
    ResetDefaults,
    DeployMatch,
    EnterSandbox,
    BackToLogin,
}

/// Registers the retained Main Lobby, Character Studio, Armory, and Settings UI systems.
pub struct MainLobbyUiPlugin;

impl Plugin for MainLobbyUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MainLobbyUiState>()
            .add_systems(OnEnter(AppState::MainLobby), mark_main_lobby_dirty)
            .add_systems(OnExit(AppState::MainLobby), cancel_establish_on_exit)
            .add_systems(
                Update,
                (
                    sync_establish_preview,
                    handle_lobby_widget_events,
                    lobby_panels::capture_native_keys,
                    handle_main_lobby_shortcuts,
                    handle_lobby_bound_clicks,
                    lobby_panels::sync_native_settings,
                    lobby_panels::sync_native_store,
                    update_main_panel_footer,
                    handle_main_panel_footer_clicks,
                    rebuild_main_lobby_ui_if_dirty,
                )
                    .chain()
                    .run_if(in_state(AppState::MainLobby)),
            );
    }
}

fn mark_main_lobby_dirty(
    mut ui_state: ResMut<MainLobbyUiState>,
    mut client: ResMut<NetClientState>,
    mut profile: ResMut<PlayerProfileSave>,
    mut stage: ResMut<MainLobbyStageState>,
) {
    client.disconnect();
    stage.hero_dirty |= ui_state.cancel_establish_preview(&mut profile);
    ui_state.active_tab = if ui_state.force_create {
        LobbyTab::Establish
    } else {
        LobbyTab::Overview
    };
    ui_state.begin_establish_preview(&profile);
    ui_state.dirty = true;
}

fn sync_establish_preview(
    mut ui_state: ResMut<MainLobbyUiState>,
    mut profile: ResMut<PlayerProfileSave>,
    mut stage: ResMut<MainLobbyStageState>,
) {
    if ui_state.active_tab == LobbyTab::Establish {
        ui_state.begin_establish_preview(&profile);
    } else if ui_state.establish_preview.is_some() {
        stage.hero_dirty |= ui_state.cancel_establish_preview(&mut profile);
        ui_state.dirty = true;
    }
}

fn cancel_establish_on_exit(
    mut ui_state: ResMut<MainLobbyUiState>,
    mut profile: ResMut<PlayerProfileSave>,
    mut stage: ResMut<MainLobbyStageState>,
) {
    stage.hero_dirty |= ui_state.cancel_establish_preview(&mut profile);
}

fn handle_main_lobby_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut commands: Commands,
    mut ctx: LobbyActionCtx,
) {
    if ctx.ui_state.editing_native_key.is_some() || ctx.ui_state.native_key_capture_frame {
        return;
    }
    if keys.just_pressed(KeyCode::F12) {
        execute_lobby_action(LobbyAction::EnterSandbox, &mut commands, &mut ctx);
    } else if keys.just_pressed(KeyCode::Escape) {
        execute_lobby_action(LobbyAction::BackToLogin, &mut commands, &mut ctx);
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct LobbyActionCtx<'w> {
    configs: Res<'w, GameConfigs>,
    l10n: ResMut<'w, LocalizationDb>,
    prefs: ResMut<'w, PlayerSessionPrefs>,
    profile: ResMut<'w, PlayerProfileSave>,
    profile_storage: Res<'w, PlayerProfileStorage>,
    ui_state: ResMut<'w, MainLobbyUiState>,
    stage_state: ResMut<'w, MainLobbyStageState>,
    client: ResMut<'w, NetClientState>,
    next_state: ResMut<'w, NextState<AppState>>,
    auth: ResMut<'w, AuthMgr>,
    platform: ResMut<'w, PlatformMgr>,
    war_choice: ResMut<'w, crate::war_choice::WarChoiceState>,
}

fn handle_lobby_bound_clicks(
    mut clicks: MessageReader<BoundUiClick>,
    native_bindings: Query<&NativeLobbyBindings>,
    mut commands: Commands,
    mut ctx: LobbyActionCtx,
) {
    for ev in clicks.read() {
        if let Some(action) = native_bindings
            .iter()
            .find_map(|bindings| bindings.action(ev))
        {
            execute_lobby_action(action, &mut commands, &mut ctx);
            continue;
        }
        let act = match (ev.panel_name.as_str(), ev.node_name.as_str()) {
            ("EstablishPanel", "btn_close") => Some(LobbyAction::CloseEstablishPanel),
            ("EstablishPanel", "btn_creat") => Some(LobbyAction::ConfirmCreateRole),
            ("EstablishPanel", "btn_name") => Some(LobbyAction::RandomCallsign),
            ("EstablishPanel", "btn_male") => Some(LobbyAction::SetGenderMale(true)),
            ("EstablishPanel", "btn_female") => Some(LobbyAction::SetGenderMale(false)),
            ("EstablishPanel", "btn_reset_role") => Some(LobbyAction::ResetEstablishRole),
            ("BagAndBuildPanel", "btn_back") => Some(LobbyAction::SwitchTab(LobbyTab::Overview)),
            _ => None,
        };
        if let Some(a) = act {
            execute_lobby_action(a, &mut commands, &mut ctx);
        }
    }
}

fn handle_main_panel_footer_clicks(
    mut clicks: MessageReader<BoundUiClick>,
    mut footers: Query<&mut MainPanelFooter>,
    mut animators: Query<&mut UiPanelAnimator>,
    mut visibility: Query<&mut Visibility>,
    mut nodes: Query<&mut Node>,
) {
    for click in clicks.read() {
        for mut footer in &mut footers {
            let Some(expanded) = footer.clicked_expanded_state(click) else {
                continue;
            };
            let Some(animator_entity) = footer.animator else {
                warn!(
                    "MainPanel footer has no bound Animator; required APK clips were not exported"
                );
                continue;
            };
            let Ok(mut animator) = animators.get_mut(animator_entity) else {
                continue;
            };
            let clips = main_panel_footer_clips(&animator, expanded);
            if !animator.play_clips(clips, false) {
                warn!("MainPanel footer is missing required APK chat transition {clips:?}");
                continue;
            }
            if expanded {
                // `<ShowBottomBtns>d__19.MoveNext` (0x1812640), before yielding.
                BoundUiPanel::set_entity_active(footer.holder, true, &mut visibility, &mut nodes);
                BoundUiPanel::set_entity_active(
                    footer.hide_button,
                    true,
                    &mut visibility,
                    &mut nodes,
                );
            }
            footer.transitions.push(MainPanelFooterTransition {
                expanded,
                elapsed_secs: 0.0,
            });
        }
    }
}

fn update_main_panel_footer(
    time: Res<Time>,
    mut ui_state: ResMut<MainLobbyUiState>,
    mut footers: Query<&mut MainPanelFooter>,
    mut animators: Query<&mut UiPanelAnimator>,
    mut visibility: Query<&mut Visibility>,
    mut nodes: Query<&mut Node>,
) {
    for mut footer in &mut footers {
        if !footer.initialized {
            footer.initialized = true;
            let expanded = footer.pose == MainPanelFooterPose::Expanded;
            BoundUiPanel::set_entity_active(footer.holder, expanded, &mut visibility, &mut nodes);
            BoundUiPanel::set_entity_active(
                footer.hide_button,
                expanded,
                &mut visibility,
                &mut nodes,
            );
            BoundUiPanel::set_entity_active(
                footer.show_button,
                !expanded,
                &mut visibility,
                &mut nodes,
            );
            // Initial collapsed mode uses the prefab chat pose, exactly as InitMainBottom.
            // A rebuilt panel restores the final sampled pose of the actual prior transition.
            let clips = match footer.pose {
                MainPanelFooterPose::PrefabCollapsed => None,
                MainPanelFooterPose::Expanded => Some(true),
                MainPanelFooterPose::Collapsed => Some(false),
            };
            if let (Some(expanded), Some(entity)) = (clips, footer.animator)
                && let Ok(mut animator) = animators.get_mut(entity)
            {
                let clips = main_panel_footer_clips(&animator, expanded);
                if !animator.seek_clips(clips, f32::MAX, false) {
                    warn!("MainPanel footer cannot restore missing APK chat transition {clips:?}");
                }
            }
        }

        let holder = footer.holder;
        let hide_button = footer.hide_button;
        let show_button = footer.show_button;
        let mut completed_state = None;
        // Native callbacks start a coroutine on every click, including a reverse click
        // before the prior wait completes. Each wait retains its own visibility change.
        footer.transitions.retain_mut(|transition| {
            transition.elapsed_secs += time.delta_secs().max(0.0);
            if transition.elapsed_secs + 1e-6 < MAIN_PANEL_FOOTER_WAIT_SECS {
                return true;
            }
            if !transition.expanded {
                BoundUiPanel::set_entity_active(holder, false, &mut visibility, &mut nodes);
                BoundUiPanel::set_entity_active(hide_button, false, &mut visibility, &mut nodes);
            }
            BoundUiPanel::set_entity_active(
                show_button,
                !transition.expanded,
                &mut visibility,
                &mut nodes,
            );
            completed_state = Some(transition.expanded);
            false
        });
        if let Some(expanded) = completed_state {
            footer.pose = if expanded {
                MainPanelFooterPose::Expanded
            } else {
                MainPanelFooterPose::Collapsed
            };
            ui_state.footer_pose = footer.pose;
        }
    }
}

fn handle_lobby_widget_events(
    mut text_edits: MessageReader<UiTextInputChanged>,
    mut sliders: MessageReader<UiSliderChanged>,
    mut toggles: MessageReader<UiToggleChanged>,
    mut profile: ResMut<PlayerProfileSave>,
    profile_storage: Res<PlayerProfileStorage>,
    mut ui_state: ResMut<MainLobbyUiState>,
) {
    for ev in text_edits.read() {
        if ev.tag == "establish_callsign"
            && ui_state.active_tab == LobbyTab::Establish
            && ui_state.force_create
        {
            ui_state.begin_establish_preview(&profile);
            // Only creation exposes a nickname field; remodel leaves the owned name intact.
            profile.role_name = ev.value.clone();
        }
    }
    let mut settings_changed = false;
    for ev in sliders.read() {
        match ev.tag.as_str() {
            "music_vol" => {
                profile.music_volume = ev.value.clamp(0.0, 1.0);
                settings_changed = true;
            }
            "sfx_vol" => {
                profile.sfx_volume = ev.value.clamp(0.0, 1.0);
                settings_changed = true;
            }
            key if key.starts_with("native_setting:") => {
                profile.native_settings.insert(
                    key.trim_start_matches("native_setting:").to_owned(),
                    ev.value,
                );
                settings_changed = true;
            }
            _ => {}
        }
    }
    if settings_changed && let Err(error) = profile.save_to_disk(&profile_storage.0) {
        ui_state.status_toast = format!("Settings could not be saved: {error}");
        ui_state.dirty = true;
    }
    for ev in toggles.read() {
        match ev.tag.as_str() {
            "a11y_high_contrast" => profile.high_contrast_hud = ev.active,
            "a11y_control_hints" => profile.show_control_hints = ev.active,
            _ => {}
        }
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct LobbyUiReadCtx<'w> {
    asset_server: Res<'w, AssetServer>,
    configs: Res<'w, GameConfigs>,
    l10n: Res<'w, LocalizationDb>,
    prefs: Res<'w, PlayerSessionPrefs>,
    profile: Res<'w, PlayerProfileSave>,
    manifest: Res<'w, crate::config::AssetManifest>,
    hero_rt: Option<Res<'w, LobbyHeroPortraitRt>>,
}

fn rebuild_main_lobby_ui_if_dirty(
    mut commands: Commands,
    mut ui_state: ResMut<MainLobbyUiState>,
    existing_q: Query<Entity, (With<LobbyUiRoot>, Without<ChildOf>)>,
    windows: Query<&Window>,
    ctx: LobbyUiReadCtx,
) {
    if !ui_state.dirty {
        return;
    }
    ui_state.dirty = false;

    for e in &existing_q {
        commands.entity(e).try_despawn();
    }

    let font = FontSource::sans_serif();
    let win_size = windows
        .iter()
        .next()
        .map(|w| Vec2::new(w.width(), w.height()))
        .unwrap_or(Vec2::new(1280.0, 720.0));

    let fit_scale = compute_panel_fit_scale(1920.0, 1080.0, win_size, true);
    let root_bg = if ui_state.active_tab == LobbyTab::Overview {
        Color::NONE
    } else {
        Color::srgb(0.03, 0.04, 0.06)
    };
    let root_e = commands
        .spawn((
            LobbyUiRoot,
            DespawnOnExit(AppState::MainLobby),
            Pickable::IGNORE,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(root_bg),
        ))
        .with_children(|root| {
            root.spawn((
                UiPanelFrame {
                    ref_w: 1920.0,
                    ref_h: 1080.0,
                    fullscreen: true,
                },
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Relative,
                    width: Val::Px(1920.0 * fit_scale),
                    height: Val::Px(1080.0 * fit_scale),
                    overflow: Overflow::visible(),
                    ..default()
                },
            ))
            .with_children(|stage| {
                match ui_state.active_tab {
                    LobbyTab::Overview => {
                        spawn_overview_main_panel(stage, &ui_state, &ctx, win_size, &font)
                    }
                    LobbyTab::Establish => {
                        spawn_establish_tab_panel(stage, &ui_state, &ctx, win_size, &font)
                    }
                    LobbyTab::Armory => spawn_armory_tab_panel(stage, &ctx, win_size, &font),
                    LobbyTab::Settings => {
                        spawn_settings_tab_panel(stage, &ui_state, &ctx, win_size, &font)
                    }
                    LobbyTab::Native(panel) => lobby_panels::spawn_native_lobby_panel(
                        stage, panel, &ui_state, &ctx, win_size, &font,
                    ),
                }

                if !ui_state.status_toast.is_empty() {
                    stage
                        .spawn((
                            ZIndex(30),
                            Pickable::IGNORE,
                            Node {
                                position_type: PositionType::Absolute,
                                top: Val::Percent(5.8),
                                right: Val::Percent(1.4),
                                padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                border_radius: BorderRadius::all(Val::Px(6.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.10, 0.24, 0.16, 0.92)),
                            BorderColor::all(Color::srgb(0.42, 0.85, 0.56)),
                        ))
                        .with_children(|t| {
                            t.spawn((
                                Text::new(ui_state.status_toast.clone()),
                                TextFont {
                                    font: font.clone(),
                                    font_size: FontSize::Px(13.0),
                                    ..default()
                                },
                                TextColor(Color::WHITE),
                            ));
                        });
                }
            });
        })
        .id();
    let _ = root_e;
}

fn spawn_overview_main_panel(
    root: &mut ChildSpawnerCommands,
    ui_state: &MainLobbyUiState,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    let doc_opt = load_ui_panel_doc("ui_panels/MainPanel.json").ok();
    let ref_w = doc_opt
        .as_ref()
        .map(|d| d.reference_width.max(64.0))
        .unwrap_or(1920.0);
    let ref_h = doc_opt
        .as_ref()
        .map(|d| d.reference_height.max(64.0))
        .unwrap_or(1080.0);
    let fit_scale = doc_opt
        .as_ref()
        .map(|doc| compute_panel_canvas_metrics(doc, win_size, true).scale)
        .unwrap_or_else(|| compute_panel_fit_scale(ref_w, ref_h, win_size, true));

    let srv_name = ctx
        .configs
        .active_server(ctx.prefs.selected_server_idx)
        .map(|s| {
            s.name
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "Localhost 100P".to_owned());

    let mut text_overrides = HashMap::new();
    text_overrides.insert("txt_player_name".to_owned(), ctx.profile.role_name.clone());
    text_overrides.insert("ID".to_owned(), "ID".to_owned());
    text_overrides.insert("txt_id".to_owned(), ctx.profile.role_preset_id.to_string());
    text_overrides.insert("txt_gold".to_owned(), ctx.profile.gold.to_string());
    text_overrides.insert("txt_coupons".to_owned(), ctx.profile.coupons.to_string());
    text_overrides.insert("txt_myq".to_owned(), ctx.profile.diamond.to_string());
    text_overrides.insert("txt_mail_count".to_owned(), "1".to_owned());
    text_overrides.insert("txt_current_server".to_owned(), srv_name);
    text_overrides.insert(
        "txt_chat".to_owned(),
        "100P UDP Ready | Drag Hero to Orbit".to_owned(),
    );

    // Keep authentic 1:1 MainPanel chrome (`m_ding`, `m_zuobian`, `m_zuoxia`, `m_youshangjiao`,
    // `m_youxiajiao`, `down`); hide only inactive popups, red-dot badges, and GM test widgets.
    let mut hidden_nodes = HashSet::from([
        "m_posture_tip".to_owned(),
        "btn_Gm".to_owned(),
        "btn_Back_to_game".to_owned(),
        "m_action".to_owned(),
        "scp_action".to_owned(),
        "btn_pay_activity".to_owned(),
        "m_pay_activity".to_owned(),
        "m_lucky_activities".to_owned(),
        "m_Nostalgia".to_owned(),
        "btn_openserver".to_owned(),
        "txt_epithet".to_owned(),
        "btn_worldcup2022".to_owned(),
        "btn_vivoright".to_owned(),
        "btn_Shoujihyz".to_owned(),
        "btn_verifyCode".to_owned(),
        "btn_QQGame".to_owned(),
        "btn_huya".to_owned(),
        "btn_oldUser".to_owned(),
        "btn_panelJump".to_owned(),
        "btn_qitianle".to_owned(),
        "btn_actMgrStore".to_owned(),
        "btn_cj".to_owned(),
        "m_skin_red".to_owned(),
        "m_email_red".to_owned(),
        "m_community_red".to_owned(),
        "m_teQuanDingYue_red".to_owned(),
        "m_yueka_red".to_owned(),
        "m_vip_red".to_owned(),
        "m_pass_red".to_owned(),
        "m_mainstore_red".to_owned(),
        "m_shop_red".to_owned(),
        "m_rank_red".to_owned(),
        "m_talent_red".to_owned(),
        "m_lantu_red".to_owned(),
        "m_cheku_red".to_owned(),
        "m_friend_red".to_owned(),
        "m_union_red".to_owned(),
        "m_mail_num".to_owned(),
    ]);
    if ui_state.footer_pose == MainPanelFooterPose::Expanded {
        hidden_nodes.insert("btn_show".to_owned());
    } else {
        hidden_nodes.insert("m_btns_holder".to_owned());
        hidden_nodes.insert("btn_hide".to_owned());
    }

    let interactive_whitelist = Some(HashSet::from([
        "btn_start_game".to_owned(),
        "btn_setting".to_owned(),
        "btn_shop".to_owned(),
        "btn_mainstore".to_owned(),
        "btn_lantu".to_owned(),
        "btn_cheku".to_owned(),
        "btn_skinupgrade".to_owned(),
        "btn_show".to_owned(),
        "btn_hide".to_owned(),
        "btn_face".to_owned(),
        "m_head_icon".to_owned(),
        "m_touxiangk".to_owned(),
        "btn_talent".to_owned(),
        "btn_server".to_owned(),
        "btn_rank".to_owned(),
        "btn_competition".to_owned(),
        "btn_firepower".to_owned(),
        "btn_communityServer".to_owned(),
        "btn_huaijiufu".to_owned(),
        "btn_email".to_owned(),
        "btn_gold".to_owned(),
        "btn_coupons".to_owned(),
        "btn_myq".to_owned(),
        "btn_pass_check".to_owned(),
        "btn_firstpunch".to_owned(),
        "btn_Vip".to_owned(),
        "btn_yuekan".to_owned(),
        "btn_teQuanDingYue".to_owned(),
        "btn_help".to_owned(),
        "btn_community".to_owned(),
        "btn_friend".to_owned(),
        "btn_union".to_owned(),
        "btn_bluediamond".to_owned(),
        "btn_dailygift".to_owned(),
        "btn_laxin".to_owned(),
        "btn_active".to_owned(),
    ]));

    if let Some(ref doc) = doc_opt {
        let mut opts = UiRetainedOptions {
            fullscreen: true,
            viewport_size: Some(win_size),
            fit_scale,
            localize_text: true,
            preview_mode: false,
            text_overrides,
            hidden_nodes,
            passthrough_nodes: HashSet::from(["m_bg".to_owned(), "m_hero_space".to_owned()]),
            interactive_whitelist,
            ..default()
        };
        opts.hide_offscreen_nodes = true;
        let bound =
            spawn_retained_panel_into(root, doc, &ctx.asset_server, Some(&ctx.l10n), font, &opts);
        lobby_panels::bind_main_panel(&bound, root.commands());
        if let Some(entity) = bound.by_canvas_path("")
            && let Some(footer) = MainPanelFooter::from_panel(&bound, ui_state.footer_pose)
        {
            root.commands().entity(entity).insert(footer);
        } else {
            warn!("MainPanel footer GameObjectData slots 92, 94 or 116 are missing");
        }

        // Sync top-left avatar (`m_head_icon`) to the player's selected `EstablishRole` portrait.
        let role_opt = ctx
            .configs
            .establish_roles
            .iter()
            .find(|r| r.id == ctx.profile.role_preset_id)
            .or_else(|| {
                ctx.configs
                    .establish_roles
                    .iter()
                    .find(|r| r.sex == ctx.profile.sex_male)
            });
        if let Some(role) = role_opt
            && let Some(head_e) = bound.first("m_head_icon")
        {
            let icon_path = ctx.configs.starter_icon_asset_path(&role.icon);
            let mut img = ImageNode::new(ctx.asset_server.load(icon_path));
            img.image_mode = NodeImageMode::Stretch;
            root.commands().entity(head_e).insert(img);
        }
    }
}

fn spawn_establish_tab_panel(
    root: &mut ChildSpawnerCommands,
    ui_state: &MainLobbyUiState,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    let is_create_mode = ui_state.force_create;
    let mut doc_opt = load_ui_panel_doc("ui_panels/EstablishPanel.json").ok();
    let choices = doc_opt
        .as_mut()
        .map(|doc| instantiate_establish_choices(doc, &ctx.configs, &ctx.profile))
        .unwrap_or_default();
    let ref_w = doc_opt
        .as_ref()
        .map(|d| d.reference_width.max(64.0))
        .unwrap_or(1920.0);
    let ref_h = doc_opt
        .as_ref()
        .map(|d| d.reference_height.max(64.0))
        .unwrap_or(1080.0);
    let fit_scale = doc_opt
        .as_ref()
        .map(|doc| compute_panel_canvas_metrics(doc, win_size, true).scale)
        .unwrap_or_else(|| compute_panel_fit_scale(ref_w, ref_h, win_size, true));

    let mut render_targets = HashMap::new();
    if let Some(ref rt) = ctx.hero_rt {
        render_targets.insert("m_hero_tex".to_owned(), rt.0.clone());
    }

    let mut text_overrides = HashMap::new();
    text_overrides.insert(
        "txt_consum".to_owned(),
        ctx.configs.recreate_role_money().to_string(),
    );

    let interactive_whitelist = Some(HashSet::from([
        "btn_close".to_owned(),
        "btn_creat".to_owned(),
        "btn_name".to_owned(),
        "btn_male".to_owned(),
        "btn_female".to_owned(),
        "btn_reset_role".to_owned(),
    ]));

    let mut hidden_nodes = HashSet::from(["m_cell".to_owned(), "m_cell_1".to_owned()]);
    // Mirror `EstablishPanel.onShow(type)`: type 1 = mandatory creation (`btn_creat`),
    // type 2 = lobby remodel (`btn_reset_role` + `btn_close` + 3000 gold cost).
    if is_create_mode {
        hidden_nodes.insert("btn_reset_role".to_owned());
        hidden_nodes.insert("btn_close".to_owned());
    } else {
        hidden_nodes.insert("btn_creat".to_owned());
        hidden_nodes.insert("inp_inputName".to_owned());
        hidden_nodes.insert("btn_name".to_owned());
    }

    if let Some(ref doc) = doc_opt {
        let mut opts = UiRetainedOptions {
            fullscreen: true,
            viewport_size: Some(win_size),
            fit_scale,
            localize_text: true,
            preview_mode: false,
            text_overrides,
            hidden_nodes,
            passthrough_nodes: HashSet::from(["RawImage".to_owned(), "m_hero_tex".to_owned()]),
            interactive_whitelist,
            render_targets,
            ..default()
        };
        opts.hide_offscreen_nodes = true;
        let bound =
            spawn_retained_panel_into(root, doc, &ctx.asset_server, Some(&ctx.l10n), font, &opts);

        prepare_establish_panel_widgets(&bound, ctx.profile.sex_male, &mut root.commands());
        bind_establish_name_input(
            &bound,
            doc,
            &ctx.profile.role_name,
            &ctx.l10n,
            &ctx.configs,
            &mut root.commands(),
        );
        for (name, action) in choices {
            if let Some(entity) = bound.first(&name) {
                root.commands().entity(entity).insert((
                    Pickable::default(),
                    Button,
                    Hovered::default(),
                    action,
                    observe(on_lobby_action),
                ));
            }
        }
    }
}

fn find_panel_node_mut<'a>(
    node: &'a mut UiPanelNodeDto,
    name: &str,
) -> Option<&'a mut UiPanelNodeDto> {
    if node.name == name {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_panel_node_mut(child, name))
}

// Clone the APK cell hierarchy rather than rebuilding its art, padding and selection overlays.
fn instantiate_establish_choices(
    doc: &mut UiPanelDoc,
    configs: &GameConfigs,
    profile: &PlayerProfileSave,
) -> Vec<(String, LobbyAction)> {
    let roles: Vec<_> = configs
        .establish_roles
        .iter()
        .filter(|r| r.sex == profile.sex_male)
        .collect();
    let active = roles
        .iter()
        .copied()
        .find(|r| r.id == profile.role_preset_id)
        .or_else(|| roles.first().copied());
    let role_choices: Vec<_> = roles
        .iter()
        .map(|role| {
            (
                format!("role_choice_{}", role.id),
                configs.starter_icon_asset_path(&role.icon),
                active.is_some_and(|a| a.id == role.id),
                LobbyAction::SelectEstablishRole(role.id),
            )
        })
        .collect();
    let mut actions =
        instantiate_establish_strip(doc, "scp_role", "m_cell", "m_role_face_icon", role_choices);
    if let Some(role) = active {
        let hair_idx = role
            .head
            .iter()
            .position(|&id| id == profile.head_item_id)
            .unwrap_or_else(|| profile.hair_variant_idx % role.head.len().max(1));
        let choices = role
            .head
            .iter()
            .enumerate()
            .map(|(idx, &head)| {
                (
                    format!("hair_choice_{idx}"),
                    configs.item_icon_asset_path(head).unwrap_or_default(),
                    idx == hair_idx,
                    LobbyAction::SelectEstablishHair(idx, head),
                )
            })
            .collect();
        actions.extend(instantiate_establish_strip(
            doc,
            "scp_hair",
            "m_cell_1",
            "m_hair_icon",
            choices,
        ));
        actions.extend(instantiate_establish_colors(
            doc,
            configs,
            "m_skin_colors",
            &role.skin_color,
            profile.skin_color_id,
            LobbyAction::SelectSkinColorId,
        ));
        if let Some(group) = role
            .hair_color
            .get(hair_idx)
            .or_else(|| role.hair_color.first())
        {
            actions.extend(instantiate_establish_colors(
                doc,
                configs,
                "m_hair_colors",
                &group.colorlist,
                profile.hair_color_id,
                LobbyAction::SelectHairColorId,
            ));
        }
    }
    actions
}

fn instantiate_establish_colors(
    doc: &mut UiPanelDoc,
    configs: &GameConfigs,
    name: &str,
    ids: &[i32],
    selected: i32,
    action: fn(i32) -> LobbyAction,
) -> Vec<(String, LobbyAction)> {
    let Some(row) = find_panel_node_mut(&mut doc.root, name) else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    for (idx, toggle) in row.children.iter_mut().enumerate() {
        let Some(&id) = ids.get(idx) else {
            toggle.active_self = false;
            continue;
        };
        toggle.active_self = true;
        if let Some(check) = find_panel_node_mut(toggle, "Checkmark") {
            check.active_self = id == selected;
        }
        if let Some(color) = find_panel_node_mut(toggle, "m_color") {
            let key = format!("{name}_choice_{id}");
            color.name = key.clone();
            if let Some(UiPanelGraphicDto::Image { color, .. }) = &mut color.graphic
                && let Some(config) = configs.skin_color_by_id(id)
            {
                *color = Srgba::from(parse_hex_rgb(if config.show_color.is_empty() {
                    &config.color1
                } else {
                    &config.show_color
                }))
                .to_f32_array();
            }
            actions.push((key, action(id)));
        }
    }
    actions
}

fn instantiate_establish_strip(
    doc: &mut UiPanelDoc,
    scroll_name: &str,
    template_name: &str,
    icon_name: &str,
    choices: Vec<(String, String, bool, LobbyAction)>,
) -> Vec<(String, LobbyAction)> {
    let Some(scroll) = find_panel_node_mut(&mut doc.root, scroll_name) else {
        return Vec::new();
    };
    let cell_size = scroll
        .widget_props
        .as_ref()
        .and_then(|props| props.scroll_cell_size);
    let Some(content) = find_panel_node_mut(scroll, "content") else {
        return Vec::new();
    };
    let Some(template) = find_panel_node_dto(content, template_name).cloned() else {
        return Vec::new();
    };
    let width = cell_size.map_or(template.canvas_rect[2], |size| size[0]);
    content.children.clear();
    content.canvas_rect[2] = width * choices.len() as f32;
    if let Some(rect) = &mut content.canvas_render_rect {
        rect[2] = content.canvas_rect[2];
    }
    let mut actions = Vec::new();
    for (idx, (name, path, selected, action)) in choices.into_iter().enumerate() {
        let mut cell = template.clone();
        // UIScrollPanel uses its serialized CellSize; artwork bounds can be wider.
        let delta = [
            content.canvas_rect[0] - template.canvas_rect[0] + idx as f32 * width,
            content.canvas_rect[1] - template.canvas_rect[1],
        ];
        fn move_clone(node: &mut UiPanelNodeDto, delta: [f32; 2]) {
            // A clone has a distinct runtime identity; never overwrite source PPtr bindings.
            node.go_ref = None;
            for axis in 0..2 {
                node.canvas_rect[axis] += delta[axis];
                if let Some(rect) = &mut node.canvas_render_rect {
                    rect[axis] += delta[axis];
                }
            }
            for child in &mut node.children {
                move_clone(child, delta);
            }
        }
        move_clone(&mut cell, delta);
        cell.name = name.clone();
        cell.active_self = true;
        if let Some(select) = find_panel_node_mut(&mut cell, "m_select") {
            select.active_self = selected;
        }
        if let Some(icon) = find_panel_node_mut(&mut cell, icon_name)
            && let Some(UiPanelGraphicDto::Image { texture_key, .. }) = &mut icon.graphic
        {
            *texture_key = Some(name.clone());
            doc.textures.push(UiTextureDto {
                key: name.clone(),
                asset_path: (!path.is_empty()).then_some(path),
                ..default()
            });
        }
        content.children.push(cell);
        actions.push((name, action));
    }
    actions
}

fn bind_establish_name_input(
    bound: &BoundUiPanel,
    doc: &UiPanelDoc,
    current: &str,
    _l10n: &LocalizationDb,
    configs: &GameConfigs,
    commands: &mut Commands,
) {
    let Some(input) = bound.game_object_slot(1) else {
        return;
    };
    let Some(dto) = find_panel_node_dto(&doc.root, "inp_inputName") else {
        return;
    };
    let text_entity = establish_slot_child(bound, 1, "Text")
        .and_then(|entity| bound.node_text_child.get(&entity).copied());
    let props = dto.widget_props.clone().unwrap_or_default();
    if let Some(text) = text_entity {
        let initial = current.to_owned();
        commands
            .entity(text)
            .entry::<Text>()
            .and_modify(move |mut text| text.0 = initial);
    }
    let mut entity = commands.entity(input);
    entity.insert((
        Pickable::default(),
        UiTextInput {
            tag: "establish_callsign".into(),
            value: current.to_owned(),
            placeholder: String::new(),
            // onInit overrides the serialized InputField limit with cfg.Consts.
            max_len: configs.role_name_limit(),
            count_utf16_units: true,
            is_password: props.is_password,
            text_entity,
            ..default()
        },
    ));
    if let Some(placeholder) = establish_slot_child(bound, 1, "Placeholder") {
        entity.insert(UiTextInputPlaceholder(placeholder));
    }
    attach_text_input_observers(&mut entity);
}

fn establish_slot_child(bound: &BoundUiPanel, slot: usize, child: &str) -> Option<Entity> {
    let parent = bound.game_object_slot(slot)?;
    let path = bound
        .by_path
        .iter()
        .find_map(|(path, &entity)| (entity == parent).then_some(path))?;
    bound.by_canvas_path(&format!("{path}/{child}"))
}

fn prepare_establish_panel_widgets(bound: &BoundUiPanel, male: bool, commands: &mut Commands) {
    // EstablishPanel.Awake binds input, female and male to root slots 1, 6 and 9.
    // Resolve descendants from those identities so re-exported wrapper names can change.
    for (slot, selected) in [(9, male), (6, !male)] {
        for (child, show) in [("On", selected), ("Off", !selected)] {
            if let Some(entity) = establish_slot_child(bound, slot, child) {
                commands.entity(entity).insert(if show {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                });
                commands
                    .entity(entity)
                    .entry::<Node>()
                    .and_modify(move |mut node| {
                        node.display = if show { Display::Flex } else { Display::None };
                    });
            }
        }
    }
}

fn spawn_armory_tab_panel(
    root: &mut ChildSpawnerCommands,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    let doc_opt = load_ui_panel_doc("ui_panels/BagAndBuildPanel.json").ok();
    let ref_w = doc_opt
        .as_ref()
        .map(|d| d.reference_width.max(64.0))
        .unwrap_or(1920.0);
    let ref_h = doc_opt
        .as_ref()
        .map(|d| d.reference_height.max(64.0))
        .unwrap_or(1080.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, true);

    if let Some(ref doc) = doc_opt {
        let opts = UiRetainedOptions {
            fullscreen: true,
            fit_scale,
            localize_text: true,
            preview_mode: false,
            hidden_nodes: HashSet::from([
                "m_cell".to_owned(),
                "m_produce_page".to_owned(),
                "m_weapon".to_owned(),
                "m_equip_desc".to_owned(),
                "m_desc".to_owned(),
                "m_cofferTitle".to_owned(),
                "scp_coffer".to_owned(),
                "m_shortcut_bar".to_owned(),
                "m_equipment".to_owned(),
                "m_bag_zone".to_owned(),
                "m_role_bg".to_owned(),
                "m_bagTitle".to_owned(),
                "txt_bag_num".to_owned(),
            ]),
            interactive_whitelist: Some(HashSet::from(["btn_back".to_owned()])),
            ..default()
        };
        let _ =
            spawn_retained_panel_into(root, doc, &ctx.asset_server, Some(&ctx.l10n), font, &opts);
    }

    root.spawn((
        ZIndex(22),
        Pickable::IGNORE,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(1.2),
            left: Val::Percent(20.0),
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(8.0),
            ..default()
        },
    ))
    .with_children(|nav| {
        spawn_tab_buttons(nav, LobbyTab::Armory, font);
        spawn_lobby_btn(
            nav,
            "Deploy Match",
            LobbyAction::DeployMatch,
            accent_palette(),
            font,
        );
    });

    // Scrollable Weapon Loadout Grid overlaid cleanly inside BagAndBuildPanel left frame.
    root.spawn((
        ZIndex(22),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(5.5),
            top: Val::Percent(15.5),
            width: Val::Percent(54.0),
            height: Val::Percent(76.0),
            padding: UiRect::all(Val::Px(14.0)),
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            align_content: AlignContent::FlexStart,
            column_gap: Val::Px(10.0),
            row_gap: Val::Px(10.0),
            overflow: Overflow::scroll_y(),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.06, 0.09, 0.14, 0.90)),
        BorderColor::all(Color::srgba(0.38, 0.56, 0.78, 0.78)),
        UiScrollArea { speed_px: 1.0 },
        Hovered::default(),
        ScrollPosition::default(),
    ))
    .with_children(|grid| {
        for prop_idx in selectable_weapon_prop_indices(&ctx.manifest) {
            let prop = &ctx.manifest.props[prop_idx];
            let equipped = prop_idx == ctx.profile.equipped_prop_idx;
            let meta = ctx
                .configs
                .weapon_catalog
                .get(prop_idx % ctx.configs.weapon_catalog.len().max(1));
            let id = meta.map(|w| w.item_id).unwrap_or(0);
            let dmg = meta.map(|w| w.base_damage).unwrap_or(14);
            let cat = meta.map(|w| ctx.l10n.tr(&w.category_cn)).unwrap_or("Rifle");
            let name_tag = meta.map(|w| ctx.l10n.tr(&w.name_cn)).unwrap_or(&prop.name);
            let sfx = meta.map(|w| w.sound_hint.as_str()).unwrap_or("gunshot.ab");
            let q = meta.map(|w| w.quality).unwrap_or(1);
            let lbl = format!(
                "{} #{} {} ({name_tag}) [{cat} Q{q}] | DMG {dmg} | {sfx} | {}t",
                if equipped { "[EQ]" } else { "    " },
                id,
                prop.name,
                prop.triangle_count
            );
            spawn_lobby_btn(
                grid,
                &lbl,
                LobbyAction::EquipWeapon(prop_idx),
                if equipped {
                    accent_palette()
                } else {
                    UiPaletteButton::default()
                },
                font,
            );
        }
    });

    // Right Equipped Weapon Telemetry card alongside Armory list.
    if let Some(eq_prop) = select_weapon_prop(&ctx.manifest, ctx.profile.equipped_prop_idx) {
        let meta = ctx
            .configs
            .weapon_catalog
            .get(ctx.profile.equipped_prop_idx % ctx.configs.weapon_catalog.len().max(1));
        let item_id = meta.map(|w| w.item_id).unwrap_or(0);
        let dmg = meta.map(|w| w.base_damage).unwrap_or(14);
        let q = meta.map(|w| w.quality).unwrap_or(1);
        let cat = meta.map(|w| ctx.l10n.tr(&w.category_cn)).unwrap_or("Rifle");
        let name_tag = meta
            .map(|w| ctx.l10n.tr(&w.name_cn))
            .unwrap_or(&eq_prop.name);
        let sfx = meta.map(|w| w.sound_hint.as_str()).unwrap_or("gunshot.ab");
        root.spawn((
            ZIndex(22),
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(61.5),
                top: Val::Percent(15.5),
                width: Val::Percent(32.5),
                padding: UiRect::all(Val::Px(16.0)),
                row_gap: Val::Px(10.0),
                flex_direction: FlexDirection::Column,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.09, 0.14, 0.92)),
            BorderColor::all(Color::srgba(0.42, 0.62, 0.85, 0.82)),
        ))
        .with_children(|card| {
            card.spawn((
                Text::new(format!(
                    "Equipped Loadout\n\nWeapon: {name_tag}\nAsset: {}\nCatalog ID: #{item_id} [{cat} | Q{q}]\nBase Damage: {dmg}\nAudio Bank: {sfx}\nMesh Triangles: {}",
                    eq_prop.name, eq_prop.triangle_count
                )),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(13.5),
                    ..default()
                },
                TextColor(Color::srgb(0.90, 0.94, 0.98)),
            ));
            spawn_lobby_btn(card, "Deploy With Loadout", LobbyAction::DeployMatch, accent_palette(), font);
        });
    }
}

fn spawn_settings_tab_panel(
    root: &mut ChildSpawnerCommands,
    ui_state: &MainLobbyUiState,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    lobby_panels::spawn_native_settings(root, ui_state, ctx, win_size, font);
}

fn spawn_tab_buttons(parent: &mut ChildSpawnerCommands, active: LobbyTab, font: &FontSource) {
    for (tab, lbl) in [
        (LobbyTab::Overview, "Main"),
        (LobbyTab::Establish, "Character"),
        (LobbyTab::Armory, "Armory"),
        (LobbyTab::Settings, "Settings"),
    ] {
        spawn_lobby_btn(
            parent,
            lbl,
            LobbyAction::SwitchTab(tab),
            if tab == active {
                accent_palette()
            } else {
                UiPaletteButton::default()
            },
            font,
        );
    }
}

fn spawn_lobby_btn(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    action: LobbyAction,
    palette: UiPaletteButton,
    font: &FontSource,
) {
    parent
        .spawn((
            Button,
            Hovered::default(),
            action,
            palette,
            Node {
                padding: UiRect::axes(Val::Px(11.0), Val::Px(5.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(palette.normal),
            BorderColor::all(Color::srgb(0.38, 0.52, 0.74)),
            observe(on_lobby_action),
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new(label.to_owned()),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(12.5),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn accent_palette() -> UiPaletteButton {
    UiPaletteButton {
        normal: Color::srgb(0.84, 0.39, 0.14),
        hovered: Color::srgb(0.95, 0.49, 0.20),
        pressed: Color::srgb(1.0, 0.62, 0.28),
    }
}

fn parse_hex_rgb(hex: &str) -> Color {
    let trimmed = hex.trim().trim_start_matches('#');
    if trimmed.len() >= 6
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&trimmed[0..2], 16),
            u8::from_str_radix(&trimmed[2..4], 16),
            u8::from_str_radix(&trimmed[4..6], 16),
        )
    {
        return Color::srgb_u8(r, g, b);
    }
    Color::srgb(0.95, 0.72, 0.58)
}

/// Local replacement ownership follows the actual selected endpoint. Native server responses
/// determine this relationship in the original game; provisional MainPanel routing remains open.
fn activate_selected_server_role(server_id: u32, ctx: &mut LobbyActionCtx) -> bool {
    ctx.stage_state.hero_dirty |= ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
    let account = ctx.auth.account.clone();
    let has_role = ctx.profile.activate_role(&account, server_id);
    ctx.auth.server_id = server_id;
    ctx.prefs.account_name = account;
    ctx.ui_state.force_create = !has_role;
    ctx.ui_state.active_tab = if has_role {
        LobbyTab::Overview
    } else {
        LobbyTab::Establish
    };
    ctx.ui_state.begin_establish_preview(&ctx.profile);
    ctx.stage_state.hero_dirty = true;
    ctx.stage_state.socket_synced = false;
    persist_profile_with_toast(
        &ctx.profile,
        &ctx.profile_storage.0,
        &mut ctx.ui_state,
        if has_role {
            "Server character selected."
        } else {
            "Create your character for this server."
        },
    );
    has_role
}

fn execute_lobby_action(act: LobbyAction, commands: &mut Commands, ctx: &mut LobbyActionCtx) {
    if ctx.ui_state.active_tab == LobbyTab::Establish {
        ctx.ui_state.begin_establish_preview(&ctx.profile);
    } else {
        ctx.stage_state.hero_dirty |= ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
    }
    if matches!(
        act,
        LobbyAction::RandomCallsign
            | LobbyAction::SetGenderMale(_)
            | LobbyAction::SelectEstablishRole(_)
            | LobbyAction::SelectEstablishHair(_, _)
            | LobbyAction::SelectSkinColorId(_)
            | LobbyAction::SelectHairColorId(_)
            | LobbyAction::ResetEstablishRole
            | LobbyAction::CloseEstablishPanel
            | LobbyAction::CycleBodyVariant
            | LobbyAction::CycleHeadVariant
            | LobbyAction::CycleHairVariant
            | LobbyAction::CycleEstablishPreset
            | LobbyAction::CycleSkinColor
            | LobbyAction::ConfirmCreateRole
    ) && ctx.ui_state.active_tab != LobbyTab::Establish
    {
        // A retained click can arrive after confirmation has already hidden its panel.
        return;
    }
    match act {
        LobbyAction::BeginNativeKeyEdit(id, slot) => {
            ctx.ui_state.editing_native_key = Some((id, slot));
            return;
        }
        LobbyAction::ResetNativeKeys => {
            ctx.profile.native_keybinds.clear();
            ctx.profile.keybinds = KeybindProfile::default();
            ctx.ui_state.editing_native_key = None;
            if let Err(error) = ctx.profile.save_to_disk(&ctx.profile_storage.0) {
                ctx.ui_state.status_toast = format!("Settings could not be saved: {error}");
                ctx.ui_state.dirty = true;
            }
            return;
        }
        LobbyAction::OpenWarChoice(mode) => {
            ctx.stage_state.hero_dirty |= ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
            ctx.war_choice.requested_mode = mode;
            ctx.next_state.set_if_different(AppState::WarChoice);
        }
        LobbyAction::SetNativeSetting(key, value) => {
            lobby_panels::set_native_setting(&mut ctx.profile, key, value);
            if key == "MainBottomShowMode" {
                ctx.ui_state.footer_pose = if value == 0.0 {
                    MainPanelFooterPose::Collapsed
                } else {
                    MainPanelFooterPose::Expanded
                };
            }
            if let Err(error) = ctx.profile.save_to_disk(&ctx.profile_storage.0) {
                ctx.ui_state.status_toast = format!("Settings could not be saved: {error}");
                ctx.ui_state.dirty = true;
            }
            return;
        }
        LobbyAction::AdjustNativeSlider(tag, step) => {
            let value = match tag {
                "music_vol" => &mut ctx.profile.music_volume,
                "sfx_vol" => &mut ctx.profile.sfx_volume,
                key => ctx
                    .profile
                    .native_settings
                    .entry(key.to_owned())
                    .or_insert(1.0),
            };
            *value = (*value + f32::from(step) * 0.01).clamp(0.0, 1.0);
            if let Err(error) = ctx.profile.save_to_disk(&ctx.profile_storage.0) {
                ctx.ui_state.status_toast = format!("Settings could not be saved: {error}");
                ctx.ui_state.dirty = true;
            }
            return;
        }
        LobbyAction::OpenNativePanel(panel) => {
            if load_ui_panel_doc(&format!("ui_panels/{}.json", panel.name())).is_err() {
                ctx.ui_state.status_toast = format!("{} is unavailable.", panel.label());
            } else {
                ctx.stage_state.hero_dirty |=
                    ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
                ctx.ui_state.active_tab = LobbyTab::Native(panel);
                ctx.ui_state.native_panel_page = panel.default_page();
                ctx.ui_state.store_selected_instance = None;
                ctx.ui_state.status_toast.clear();
            }
        }
        LobbyAction::SetNativePanelPage(page) => {
            ctx.ui_state.native_panel_page = page;
            ctx.ui_state.store_selected_instance = None;
        }
        LobbyAction::SelectStoreInstance(instance) => {
            if ctx
                .profile
                .lobby_store_items
                .iter()
                .any(|item| item.instance_id == instance && item.count > 0)
            {
                ctx.ui_state.store_selected_instance = Some(instance);
            }
        }
        LobbyAction::CloseStoreDescription => ctx.ui_state.store_selected_instance = None,
        LobbyAction::UseStoreItem => {
            if let Some(instance) = ctx.ui_state.store_selected_instance {
                if let Some(item) = ctx
                    .profile
                    .lobby_store_items
                    .iter()
                    .find(|item| item.instance_id == instance && item.count > 0)
                {
                    let item_id = item.item_id;
                    if ctx
                        .configs
                        .item_cfgs
                        .iter()
                        .any(|cfg| cfg.id == item_id && !cfg.model_path.is_empty())
                    {
                        ctx.profile.lobby_equipped_instance_id = Some(instance);
                        ctx.stage_state.socket_synced = false;
                        persist_profile_with_toast(
                            &ctx.profile,
                            &ctx.profile_storage.0,
                            &mut ctx.ui_state,
                            "Equipped.",
                        );
                    }
                }
            }
        }
        LobbyAction::NativeServiceUnavailable(service) => {
            ctx.ui_state.status_toast = format!("{service} is unavailable on this server.");
        }
        LobbyAction::SwitchTab(tab) => {
            if ctx.ui_state.force_create {
                ctx.ui_state.status_toast =
                    "Create your character first (EstablishPanel type 1).".to_owned();
            } else {
                if tab == LobbyTab::Establish && ctx.ui_state.active_tab != tab {
                    ctx.ui_state.role_color_cache.clear();
                } else if tab != LobbyTab::Establish {
                    ctx.stage_state.hero_dirty |=
                        ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
                }
                ctx.ui_state.active_tab = tab;
                if tab == LobbyTab::Settings {
                    ctx.ui_state.settings_subtab = 1;
                }
                ctx.ui_state.begin_establish_preview(&ctx.profile);
            }
        }
        LobbyAction::ConfirmCreateRole => {
            if !ctx.ui_state.force_create {
                return;
            } else if let Some(code) = check_name(&ctx.profile.role_name) {
                let msg = match code {
                    NAME_ERR_EMPTY => "Name is empty.",
                    NAME_ERR_SPECIAL => "Name contains special characters.",
                    _ => "Name is too short.",
                }
                .to_owned();
                ctx.ui_state.status_toast = msg;
            } else {
                if !ctx.platform.on_create_role(&ctx.auth) {
                    ctx.ui_state.status_toast = "Log in before creating a character.".into();
                    ctx.ui_state.dirty = true;
                    return;
                }
                ctx.ui_state.establish_preview = None;
                ctx.profile
                    .complete_role_creation(&ctx.auth.account, ctx.auth.server_id);
                ctx.ui_state.force_create = false;
                ctx.ui_state.active_tab = LobbyTab::Overview;
                ctx.stage_state.hero_dirty = true;
                persist_profile_with_toast(
                    &ctx.profile,
                    &ctx.profile_storage.0,
                    &mut ctx.ui_state,
                    "Character created.",
                );
            }
        }
        LobbyAction::SetSettingsSubtab(sub) => {
            if [1, 2, 5, 6, 7, 8, 9].contains(&sub) {
                ctx.ui_state.settings_subtab = sub;
            }
            return;
        }
        LobbyAction::RandomCallsign => {
            if !ctx.ui_state.force_create {
                return;
            }
            let seed = ctx.ui_state.name_recommendation_index;
            ctx.ui_state.name_recommendation_index = seed.wrapping_add(1);
            let generated = ctx
                .configs
                .generate_random_role_name(ctx.l10n.selected_lang_index, seed);
            if !generated.is_empty() {
                ctx.profile.role_name = generated;
            }
        }
        LobbyAction::SetGenderMale(male) => {
            ctx.profile.sex_male = male;
            if let Some(first_role) = ctx.configs.establish_roles.iter().find(|r| r.sex == male) {
                ctx.profile.role_preset_id = first_role.id;
                ctx.profile.face_item_id =
                    first_role
                        .face
                        .first()
                        .copied()
                        .unwrap_or(if male { 20550 } else { 23550 });
                ctx.profile.head_item_id =
                    first_role
                        .head
                        .first()
                        .copied()
                        .unwrap_or(if male { 20500 } else { 23500 });
                ctx.profile.skin_color_id = first_role
                    .skin_color
                    .first()
                    .copied()
                    .unwrap_or(if male { 1 } else { 10 });
                ctx.profile.hair_color_id = first_role
                    .hair_color
                    .first()
                    .and_then(|g| g.colorlist.first().copied())
                    .unwrap_or(if male { 150 } else { 101 });
            }
            ctx.profile.head_variant_idx = 0;
            ctx.profile.hair_variant_idx = 0;
            ctx.stage_state.hero_dirty = true;
        }
        LobbyAction::SelectEstablishRole(role_id) => {
            if role_id == ctx.profile.role_preset_id {
                return;
            }
            if let Some((idx, role)) = ctx
                .configs
                .establish_roles
                .iter()
                .filter(|r| r.sex == ctx.profile.sex_male)
                .enumerate()
                .find(|(_, r)| r.id == role_id)
            {
                ctx.profile.role_preset_id = role.id;
                ctx.profile.head_variant_idx = idx;
                if let Some(&face_id) = role.face.first() {
                    ctx.profile.face_item_id = face_id;
                }
                // Native role click rebuilds hair, skin and color choices from their first entry.
                ctx.profile.hair_variant_idx = 0;
                if let Some(&head) = role.head.first() {
                    ctx.profile.head_item_id = head;
                }
                let cached = ctx.ui_state.role_color_cache.entry(role.id).or_default();
                let skin = role
                    .skin_color
                    .iter()
                    .copied()
                    .find(|&id| id == cached[0])
                    .or_else(|| role.skin_color.first().copied())
                    .unwrap_or_default();
                let hair_colors = role
                    .hair_color
                    .first()
                    .map(|group| group.colorlist.as_slice())
                    .unwrap_or_default();
                let hair = hair_colors
                    .iter()
                    .copied()
                    .find(|&id| id == cached[1])
                    .or_else(|| hair_colors.first().copied())
                    .unwrap_or_default();
                *cached = [skin, hair];
                ctx.profile.skin_color_id = skin;
                ctx.profile.hair_color_id = hair;
                ctx.stage_state.hero_dirty = true;
            }
        }
        LobbyAction::SelectEstablishHair(idx, head_id) => {
            if head_id == ctx.profile.head_item_id {
                return;
            }
            ctx.profile.hair_variant_idx = idx;
            ctx.profile.head_item_id = head_id;
            if let Some(role) = ctx
                .configs
                .establish_roles
                .iter()
                .find(|r| r.id == ctx.profile.role_preset_id)
                && let Some(hc_group) = role.hair_color.get(idx)
            {
                let cached = ctx
                    .ui_state
                    .role_color_cache
                    .get(&role.id)
                    .map_or(0, |pair| pair[1]);
                ctx.profile.hair_color_id = hc_group
                    .colorlist
                    .iter()
                    .copied()
                    .find(|&id| id == cached)
                    .or_else(|| hc_group.colorlist.first().copied())
                    .unwrap_or_default();
                if let Some(pair) = ctx.ui_state.role_color_cache.get_mut(&role.id) {
                    pair[1] = ctx.profile.hair_color_id;
                }
            }
            ctx.stage_state.hero_dirty = true;
        }
        LobbyAction::SelectSkinColorId(sc_id) => {
            ctx.profile.skin_color_id = sc_id;
            if let Some(pair) = ctx
                .ui_state
                .role_color_cache
                .get_mut(&ctx.profile.role_preset_id)
            {
                pair[0] = sc_id;
            }
        }
        LobbyAction::SelectHairColorId(hc_id) => {
            ctx.profile.hair_color_id = hc_id;
            if let Some(pair) = ctx
                .ui_state
                .role_color_cache
                .get_mut(&ctx.profile.role_preset_id)
            {
                pair[1] = hc_id;
            }
        }
        LobbyAction::ResetEstablishRole => {
            if ctx.ui_state.force_create {
                return;
            }
            let cost = ctx.configs.recreate_role_money();
            if ctx.profile.gold < cost {
                ctx.ui_state.status_toast = ctx.configs.get_string(&ctx.l10n, 155, &[]);
            } else {
                if let Some(preview) = ctx.ui_state.establish_preview.take()
                    && preview.owns(&ctx.profile)
                {
                    // Native CReCreateRole carries appearance only, never a nickname.
                    ctx.profile.role_name = preview.committed.role_name;
                }
                ctx.profile.gold -= cost;
                if let Some(key) = ctx.profile.active_role_key.clone() {
                    ctx.profile
                        .complete_role_creation(&key.account_name, key.server_id);
                }
                ctx.stage_state.hero_dirty = true;
                ctx.ui_state.active_tab = LobbyTab::Overview;
                persist_profile_with_toast(
                    &ctx.profile,
                    &ctx.profile_storage.0,
                    &mut ctx.ui_state,
                    "Appearance remodeled & saved.",
                );
            }
        }
        LobbyAction::CloseEstablishPanel => {
            if !ctx.ui_state.force_create {
                ctx.stage_state.hero_dirty |=
                    ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
                ctx.ui_state.active_tab = LobbyTab::Overview;
            }
        }
        LobbyAction::CycleBodyVariant => {
            ctx.profile.body_variant_idx = ctx.profile.body_variant_idx.wrapping_add(1);
            ctx.stage_state.hero_dirty = true;
        }
        LobbyAction::CycleHeadVariant => {
            ctx.profile.head_variant_idx = ctx.profile.head_variant_idx.wrapping_add(1);
            ctx.stage_state.hero_dirty = true;
        }
        LobbyAction::CycleHairVariant => {
            ctx.profile.hair_variant_idx = ctx.profile.hair_variant_idx.wrapping_add(1);
            ctx.stage_state.hero_dirty = true;
        }
        LobbyAction::CycleEstablishPreset => {
            if !ctx.configs.establish_roles.is_empty() {
                let cur = ctx
                    .configs
                    .establish_roles
                    .iter()
                    .position(|r| r.id == ctx.profile.role_preset_id)
                    .unwrap_or(0);
                let next =
                    &ctx.configs.establish_roles[(cur + 1) % ctx.configs.establish_roles.len()];
                ctx.profile.role_preset_id = next.id;
                ctx.profile.sex_male = next.sex;
                ctx.profile.head_variant_idx = cur + 1;
                ctx.stage_state.hero_dirty = true;
            }
        }
        LobbyAction::CycleSkinColor => {
            let n = ctx.configs.skin_colors.len().max(1);
            ctx.profile.skin_color_idx = (ctx.profile.skin_color_idx + 1) % n;
        }
        LobbyAction::SelectBg(idx) => {
            ctx.profile.selected_bg_idx = idx;
            ctx.stage_state.hero_dirty = true;
        }
        LobbyAction::ClaimSupplyReward => {
            if ctx.ui_state.active_tab == LobbyTab::Establish {
                if ctx.ui_state.force_create {
                    return;
                }
                ctx.stage_state.hero_dirty |=
                    ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
                ctx.ui_state.active_tab = LobbyTab::Overview;
            }
            ctx.profile.gold = ctx.profile.gold.saturating_add(500);
            ctx.profile.diamond = ctx.profile.diamond.saturating_add(20);
            ctx.profile.exp = ctx.profile.exp.saturating_add(50);
            if ctx.profile.exp >= 200 {
                ctx.profile.exp -= 200;
                ctx.profile.level = ctx.profile.level.saturating_add(1);
            }
            persist_profile_with_toast(
                &ctx.profile,
                &ctx.profile_storage.0,
                &mut ctx.ui_state,
                "Claimed daily supplies!",
            );
        }
        LobbyAction::EquipWeapon(idx) => {
            ctx.profile.equipped_prop_idx = idx;
            ctx.stage_state.socket_synced = false;
        }
        LobbyAction::CycleFpsTier => ctx.profile.fps_tier = (ctx.profile.fps_tier + 1) % 4,
        LobbyAction::CycleQualityTier => {
            ctx.profile.quality_tier = (ctx.profile.quality_tier + 1) % 3
        }
        LobbyAction::CycleLanguage => {
            if !ctx.l10n.index_entries.is_empty() {
                let cur = ctx
                    .l10n
                    .index_entries
                    .iter()
                    .position(|e| e.index == ctx.l10n.selected_lang_index)
                    .unwrap_or(0);
                let next_idx =
                    ctx.l10n.index_entries[(cur + 1) % ctx.l10n.index_entries.len()].index;
                ctx.l10n.selected_lang_index = next_idx;
                ctx.profile.lang_index = next_idx;
            }
        }
        LobbyAction::CycleKeybindPreset => {
            let next_p = (ctx.profile.keybinds.preset_idx + 1) % 3;
            ctx.profile.keybinds = KeybindProfile::preset(next_p);
        }
        LobbyAction::CycleInteractKey => ctx.profile.keybinds.cycle_interact_key(),
        LobbyAction::CycleServer => {
            let n = ctx.configs.server_list.servers.len();
            if n > 0 {
                if ctx.auth.account.is_empty() {
                    ctx.ui_state.status_toast = "Log in before selecting a server.".to_owned();
                    ctx.ui_state.dirty = true;
                    ctx.next_state.set_if_different(AppState::LoginLobby);
                    return;
                }
                ctx.prefs.selected_server_idx = (ctx.prefs.selected_server_idx + 1) % n;
                let server_id = ctx.configs.server_list.servers[ctx.prefs.selected_server_idx].id;
                activate_selected_server_role(server_id, ctx);
            }
        }
        LobbyAction::ToggleAutoRotate => ctx.stage_state.auto_rotate = !ctx.stage_state.auto_rotate,
        LobbyAction::SaveProfileNow => {
            persist_profile_with_toast(
                &ctx.profile,
                &ctx.profile_storage.0,
                &mut ctx.ui_state,
                "Profile saved cleanly.",
            );
        }
        LobbyAction::ResetDefaults => {
            // Reset settings without erasing the authenticated account or its owned roles.
            let defaults = PlayerProfileSave::default();
            ctx.profile.music_volume = defaults.music_volume;
            ctx.profile.sfx_volume = defaults.sfx_volume;
            ctx.profile.fps_tier = defaults.fps_tier;
            ctx.profile.quality_tier = defaults.quality_tier;
            ctx.profile.high_contrast_hud = defaults.high_contrast_hud;
            ctx.profile.show_control_hints = defaults.show_control_hints;
            ctx.profile.keybinds = defaults.keybinds;
            ctx.profile.lang_index = defaults.lang_index;
            ctx.l10n.selected_lang_index = ctx.profile.lang_index;
            ctx.stage_state.hero_dirty = true;
            persist_profile_with_toast(
                &ctx.profile,
                &ctx.profile_storage.0,
                &mut ctx.ui_state,
                "Settings reset.",
            );
        }
        LobbyAction::DeployMatch => {
            let Some(srv) = ctx
                .configs
                .active_server(ctx.prefs.selected_server_idx)
                .cloned()
            else {
                ctx.ui_state.status_toast = "No server configured.".to_owned();
                ctx.ui_state.dirty = true;
                return;
            };
            if ctx.auth.account.is_empty() {
                ctx.ui_state.status_toast = "Log in before deploying.".to_owned();
                ctx.ui_state.dirty = true;
                ctx.next_state.set_if_different(AppState::LoginLobby);
                return;
            }
            let owns_selected_role =
                ctx.profile.active_role_key.as_ref().is_some_and(|key| {
                    key.account_name == ctx.auth.account && key.server_id == srv.id
                }) && ctx.profile.account_name == ctx.auth.account
                    && ctx.profile.has_saved_role(&ctx.auth.account, srv.id);
            let creating_selected_role = ctx.ui_state.force_create
                && ctx.profile.active_role_key.is_none()
                && ctx.profile.account_name == ctx.auth.account
                && ctx.auth.server_id == srv.id;
            if !owns_selected_role && !creating_selected_role {
                activate_selected_server_role(srv.id, ctx);
            } else {
                ctx.auth.server_id = srv.id;
            }
            if ctx.ui_state.force_create {
                ctx.ui_state.active_tab = LobbyTab::Establish;
                ctx.ui_state.begin_establish_preview(&ctx.profile);
                ctx.ui_state.status_toast = "Create your character for this server.".to_owned();
                ctx.ui_state.dirty = true;
                return;
            }
            ctx.stage_state.hero_dirty |= ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
            ctx.ui_state.active_tab = LobbyTab::Overview;
            let ver = if ctx.configs.version.str_v.is_empty() {
                "3.14.0.0"
            } else {
                ctx.configs.version.str_v.as_str()
            };
            ctx.prefs.account_name = ctx.profile.account_name.clone();
            if let Err(err) = ctx.client.connect_to_server_entry(
                commands,
                &srv,
                ver,
                &ctx.profile.account_name,
                ctx.l10n.selected_lang_index,
            ) {
                ctx.ui_state.status_toast = err;
                ctx.ui_state.dirty = true;
                return;
            }
            let _ = ctx.profile.save_to_disk(&ctx.profile_storage.0);
            ctx.next_state.set_if_different(AppState::LoadingScene);
        }
        LobbyAction::EnterSandbox => {
            ctx.stage_state.hero_dirty |= ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
            ctx.ui_state.active_tab = LobbyTab::Overview;
            ctx.next_state.set_if_different(AppState::DevSandbox);
        }
        LobbyAction::BackToLogin => {
            ctx.stage_state.hero_dirty |= ctx.ui_state.cancel_establish_preview(&mut ctx.profile);
            ctx.ui_state.active_tab = LobbyTab::Overview;
            ctx.next_state.set_if_different(AppState::LoginLobby);
        }
    }
    ctx.ui_state.dirty = true;
}

fn on_lobby_action(
    activate: On<Activate>,
    actions: Query<&LobbyAction>,
    mut commands: Commands,
    mut ctx: LobbyActionCtx,
) {
    let Ok(&act) = actions.get(activate.entity) else {
        return;
    };
    execute_lobby_action(act, &mut commands, &mut ctx);
}

fn persist_profile_with_toast(
    profile: &PlayerProfileSave,
    path: &std::path::Path,
    ui_state: &mut MainLobbyUiState,
    ok_msg: &str,
) {
    let result = if let Some(preview) = &ui_state.establish_preview {
        let mut committed = profile.clone();
        preview.restore(&mut committed);
        committed.save_to_disk(path)
    } else {
        profile.save_to_disk(path)
    };
    ui_state.status_toast = match result {
        Ok(()) => ok_msg.to_owned(),
        Err(e) => format!("Save warning: {e}"),
    };
}

#[cfg(test)]
mod establish_tests {
    use crate::config::{ServerEntry, ServerModeType, ServerStatus};
    use crate::ui::panel_loader::UiPanelNodeDto;
    use bevy::ecs::system::RunSystemOnce;

    use super::*;

    #[derive(Resource)]
    struct EstablishFixture(BoundUiPanel);

    fn character_action_app(path: std::path::PathBuf) -> App {
        let mut app = App::new();
        app.init_resource::<GameConfigs>()
            .init_resource::<LocalizationDb>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<PlayerProfileSave>()
            .insert_resource(PlayerProfileStorage(path))
            .init_resource::<MainLobbyUiState>()
            .init_resource::<MainLobbyStageState>()
            .init_resource::<NetClientState>()
            .init_resource::<crate::war_choice::WarChoiceState>()
            .init_resource::<NextState<AppState>>()
            .init_resource::<AuthMgr>()
            .init_resource::<PlatformMgr>()
            .add_message::<UiTextInputChanged>()
            .add_message::<UiSliderChanged>()
            .add_message::<UiToggleChanged>()
            .add_message::<BoundUiClick>()
            .add_systems(
                Update,
                (
                    sync_establish_preview,
                    handle_lobby_widget_events,
                    handle_lobby_bound_clicks,
                )
                    .chain(),
            );
        app
    }

    fn act(app: &mut App, action: LobbyAction) {
        app.world_mut()
            .run_system_once(move |mut commands: Commands, mut ctx: LobbyActionCtx| {
                execute_lobby_action(action, &mut commands, &mut ctx);
            })
            .expect("lobby action");
    }

    fn owned_remodel_app(path: std::path::PathBuf) -> (App, PlayerProfileSave) {
        let mut app = character_action_app(path.clone());
        {
            let mut profile = app.world_mut().resource_mut::<PlayerProfileSave>();
            profile.activate_role("OwnedAccount", 42);
            profile.role_name = "Hero".into();
            profile.gold = 6_000;
            profile.complete_role_creation("OwnedAccount", 42);
            profile.save_to_disk(&path).unwrap();
        }
        let committed = app.world().resource::<PlayerProfileSave>().clone();
        act(&mut app, LobbyAction::SwitchTab(LobbyTab::Establish));
        act(&mut app, LobbyAction::SetGenderMale(false));
        act(&mut app, LobbyAction::SelectEstablishHair(1, 23501));
        act(&mut app, LobbyAction::SelectSkinColorId(10));
        act(&mut app, LobbyAction::SelectHairColorId(101));
        (app, committed)
    }

    fn server_role_action_app(path: std::path::PathBuf) -> App {
        let mut app = character_action_app(path.clone());
        app.world_mut()
            .resource_mut::<GameConfigs>()
            .server_list
            .servers = [17, 42, 77]
            .into_iter()
            .map(|id| ServerEntry {
                id,
                mode: ServerModeType::BattleRoyale100,
                name: format!("Test server {id}"),
                gate_url: "127.0.0.1".into(),
                port: 0,
                status: ServerStatus::Smooth,
                is_local_loopback: false,
            })
            .collect();
        {
            let mut auth = app.world_mut().resource_mut::<AuthMgr>();
            auth.set_auth("LoginAccount", "Session");
            auth.server_id = 17;
        }
        {
            let mut profile = app.world_mut().resource_mut::<PlayerProfileSave>();
            profile.activate_role("LoginAccount", 17);
            profile.role_name = "Alice".into();
            profile.level = 10;
            profile.exp = 123;
            profile.gold = 9_000;
            profile.complete_role_creation("LoginAccount", 17);
            profile.activate_role("LoginAccount", 42);
            profile.role_name = "Bella".into();
            profile.level = 7;
            profile.exp = 456;
            profile.gold = 6_000;
            profile.sex_male = false;
            profile.head_item_id = 23501;
            profile.skin_color_id = 10;
            profile.hair_color_id = 101;
            profile.complete_role_creation("LoginAccount", 42);
            assert!(profile.activate_role("LoginAccount", 17));
            profile.save_to_disk(&path).unwrap();
        }
        app
    }

    #[test]
    fn cycling_servers_restores_separate_owned_roles_and_can_return_from_missing_role() {
        let path = std::env::temp_dir().join(format!(
            "devx-server-role-cycle-{}.json",
            std::process::id()
        ));
        let mut app = server_role_action_app(path.clone());
        {
            let mut profile = app.world_mut().resource_mut::<PlayerProfileSave>();
            profile.level = 11;
            profile.gold = 8_123;
            profile.head_item_id = 20501;
        }
        act(&mut app, LobbyAction::CycleServer);
        let profile = app.world().resource::<PlayerProfileSave>();
        assert_eq!(profile.account_name, "LoginAccount");
        assert_eq!(profile.role_name, "Bella");
        assert_eq!((profile.level, profile.exp, profile.gold), (7, 456, 6_000));
        assert!(!profile.sex_male);
        assert_eq!(
            (
                profile.head_item_id,
                profile.skin_color_id,
                profile.hair_color_id
            ),
            (23501, 10, 101)
        );
        assert_eq!(app.world().resource::<AuthMgr>().server_id, 42);
        assert!(!app.world().resource::<MainLobbyUiState>().force_create);
        let mut saved = PlayerProfileSave::load_from_disk_or_default(&path);
        assert!(saved.activate_role("LoginAccount", 17));
        assert_eq!(
            (
                saved.role_name.as_str(),
                saved.level,
                saved.gold,
                saved.head_item_id
            ),
            ("Alice", 11, 8_123, 20501)
        );

        act(&mut app, LobbyAction::CycleServer);
        let profile = app.world().resource::<PlayerProfileSave>();
        assert_eq!(profile.account_name, "LoginAccount");
        assert!(profile.role_name.is_empty());
        assert!(profile.active_role_key.is_none());
        assert!(!profile.has_saved_role("LoginAccount", 77));
        assert_eq!(app.world().resource::<AuthMgr>().server_id, 77);
        assert!(app.world().resource::<MainLobbyUiState>().force_create);
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().active_tab,
            LobbyTab::Establish
        );
        act(&mut app, LobbyAction::SetGenderMale(false));
        act(&mut app, LobbyAction::SelectEstablishHair(1, 23599));
        app.world_mut().write_message(UiTextInputChanged {
            entity: Entity::PLACEHOLDER,
            tag: "establish_callsign".into(),
            value: "Draft".into(),
            submitted: false,
        });
        app.update();
        act(&mut app, LobbyAction::CycleServer);
        let profile = app.world().resource::<PlayerProfileSave>();
        assert_eq!(
            (
                profile.role_name.as_str(),
                profile.level,
                profile.gold,
                profile.head_item_id
            ),
            ("Alice", 11, 8_123, 20501)
        );
        assert!(profile.sex_male);
        assert!(!profile.has_saved_role("LoginAccount", 77));
        assert_eq!(app.world().resource::<AuthMgr>().server_id, 17);
        assert_eq!(app.world().resource::<AuthMgr>().session_key, "Session");
        assert!(!app.world().resource::<MainLobbyUiState>().force_create);
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().active_tab,
            LobbyTab::Overview
        );
        let mut saved = PlayerProfileSave::load_from_disk_or_default(&path);
        assert!(saved.activate_role("LoginAccount", 42));
        assert_eq!(
            (
                saved.role_name.as_str(),
                saved.level,
                saved.gold,
                saved.head_item_id
            ),
            ("Bella", 7, 6_000, 23501)
        );
        assert!(!saved.has_saved_role("LoginAccount", 77));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn deploying_to_missing_server_role_preserves_creation_draft_and_creates_for_that_endpoint() {
        let path = std::env::temp_dir().join(format!(
            "devx-server-role-deploy-{}.json",
            std::process::id()
        ));
        let mut app = server_role_action_app(path.clone());
        // A selection changed outside CycleServer still cannot deploy the former endpoint's role.
        app.world_mut()
            .resource_mut::<PlayerSessionPrefs>()
            .selected_server_idx = 2;
        act(&mut app, LobbyAction::DeployMatch);
        assert!(app.world().resource::<MainLobbyUiState>().force_create);
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().active_tab,
            LobbyTab::Establish
        );
        assert_eq!(app.world().resource::<AuthMgr>().server_id, 77);
        assert!(
            app.world()
                .resource::<PlayerProfileSave>()
                .active_role_key
                .is_none()
        );
        assert_eq!(
            app.world().resource::<NetClientState>().phase,
            crate::net::transport::ClientLinkPhase::Disconnected
        );
        assert!(matches!(
            app.world().resource::<NextState<AppState>>(),
            NextState::Unchanged
        ));
        act(&mut app, LobbyAction::SetGenderMale(false));
        act(&mut app, LobbyAction::SelectEstablishHair(1, 23599));
        app.world_mut().write_message(UiTextInputChanged {
            entity: Entity::PLACEHOLDER,
            tag: "establish_callsign".into(),
            value: "NewHero".into(),
            submitted: false,
        });
        app.update();
        act(&mut app, LobbyAction::DeployMatch);
        assert_eq!(
            app.world().resource::<PlayerProfileSave>().role_name,
            "NewHero"
        );
        assert_eq!(
            app.world().resource::<PlayerProfileSave>().head_item_id,
            23599
        );
        assert_eq!(
            app.world().resource::<NetClientState>().phase,
            crate::net::transport::ClientLinkPhase::Disconnected
        );
        act(&mut app, LobbyAction::ConfirmCreateRole);
        let profile = app.world().resource::<PlayerProfileSave>();
        assert_eq!(profile.account_name, "LoginAccount");
        assert_eq!(profile.role_name, "NewHero");
        assert_eq!(profile.active_role_key.as_ref().unwrap().server_id, 77);
        assert!(profile.has_saved_role("LoginAccount", 17));
        assert!(profile.has_saved_role("LoginAccount", 42));
        assert!(profile.has_saved_role("LoginAccount", 77));
        assert!(!app.world().resource::<MainLobbyUiState>().force_create);
        let mut saved = PlayerProfileSave::load_from_disk_or_default(&path);
        assert!(saved.activate_role("LoginAccount", 77));
        assert_eq!(saved.role_name, "NewHero");
        assert!(!saved.sex_male);
        assert_eq!(saved.head_item_id, 23599);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cancelling_remodel_restores_owned_role_after_gender_hair_and_color_previews() {
        for (index, exit) in [
            LobbyAction::CloseEstablishPanel,
            LobbyAction::SwitchTab(LobbyTab::Settings),
            LobbyAction::BackToLogin,
            LobbyAction::EnterSandbox,
        ]
        .into_iter()
        .enumerate()
        {
            let path = std::env::temp_dir().join(format!(
                "devx-remodel-cancel-{}-{index}.json",
                std::process::id()
            ));
            let (mut app, committed) = owned_remodel_app(path.clone());
            let unchanged_save = std::fs::read(&path).unwrap();
            let preview = app.world().resource::<PlayerProfileSave>();
            assert!(!preview.sex_male);
            assert_eq!(preview.head_item_id, 23501);
            assert_eq!(preview.skin_color_id, 10);
            assert_eq!(preview.hair_color_id, 101);
            assert_eq!(preview.gold, committed.gold);
            assert_eq!(preview.local_roles, committed.local_roles);

            // A delayed event from the hidden creation input cannot rename a remodelled role.
            app.world_mut().write_message(UiTextInputChanged {
                entity: Entity::PLACEHOLDER,
                tag: "establish_callsign".into(),
                value: "Unpaid".into(),
                submitted: false,
            });
            app.update();
            act(&mut app, exit);
            assert_eq!(app.world().resource::<PlayerProfileSave>(), &committed);
            assert!(
                app.world()
                    .resource::<MainLobbyUiState>()
                    .establish_preview
                    .is_none()
            );
            assert!(app.world().resource::<MainLobbyStageState>().hero_dirty);
            assert_eq!(std::fs::read(&path).unwrap(), unchanged_save);
            let mut saved = PlayerProfileSave::load_from_disk_or_default(&path);
            assert!(saved.activate_role("OwnedAccount", 42));
            assert_eq!(saved, committed);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn saving_during_remodel_keeps_unaccepted_preview_out_of_the_owned_save() {
        let path = std::env::temp_dir().join(format!(
            "devx-remodel-unaccepted-save-{}.json",
            std::process::id()
        ));
        let (mut app, committed) = owned_remodel_app(path.clone());
        act(&mut app, LobbyAction::SaveProfileNow);
        assert!(!app.world().resource::<PlayerProfileSave>().sex_male);
        let mut saved = PlayerProfileSave::load_from_disk_or_default(&path);
        assert!(saved.activate_role("OwnedAccount", 42));
        assert_eq!(saved, committed);
        app.world_mut()
            .run_system_once(cancel_establish_on_exit)
            .unwrap();
        assert_eq!(app.world().resource::<PlayerProfileSave>(), &committed);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn accepted_remodel_keeps_preview_and_charges_native_cost_once_for_duplicate_clicks() {
        let path =
            std::env::temp_dir().join(format!("devx-remodel-accepted-{}.json", std::process::id()));
        let (mut app, committed) = owned_remodel_app(path.clone());
        for _ in 0..2 {
            app.world_mut().write_message(BoundUiClick {
                panel_name: "EstablishPanel".into(),
                node_name: "btn_reset_role".into(),
                entity: Entity::PLACEHOLDER,
            });
        }
        app.update();
        let profile = app.world().resource::<PlayerProfileSave>();
        assert_eq!(profile.gold, committed.gold - 3_000);
        assert_eq!(profile.role_name, committed.role_name);
        assert!(!profile.sex_male);
        assert_eq!(profile.head_item_id, 23501);
        assert_eq!(profile.skin_color_id, 10);
        assert_eq!(profile.hair_color_id, 101);
        let accepted = profile.clone();
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().active_tab,
            LobbyTab::Overview
        );
        assert!(
            app.world()
                .resource::<MainLobbyUiState>()
                .establish_preview
                .is_none()
        );
        act(&mut app, LobbyAction::CloseEstablishPanel);
        act(&mut app, LobbyAction::SwitchTab(LobbyTab::Establish));
        act(&mut app, LobbyAction::SetGenderMale(true));
        act(&mut app, LobbyAction::CloseEstablishPanel);
        assert_eq!(app.world().resource::<PlayerProfileSave>(), &accepted);
        let mut saved = PlayerProfileSave::load_from_disk_or_default(&path);
        assert!(saved.activate_role("OwnedAccount", 42));
        assert_eq!(saved.gold, 3_000);
        assert_eq!(saved.role_name, "Hero");
        assert!(!saved.sex_male);
        assert_eq!(saved.head_item_id, 23501);
        assert_eq!(saved.skin_color_id, 10);
        assert_eq!(saved.hair_color_id, 101);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn create_button_commits_current_nickname_to_the_authenticated_accounts_role() {
        let path =
            std::env::temp_dir().join(format!("devx-native-create-{}.json", std::process::id()));
        let mut app = character_action_app(path.clone());
        app.world_mut()
            .resource_mut::<MainLobbyUiState>()
            .force_create = true;
        app.world_mut()
            .resource_mut::<MainLobbyUiState>()
            .active_tab = LobbyTab::Establish;
        app.world_mut()
            .resource_mut::<AuthMgr>()
            .set_auth("LoginIdentity", "Session");
        app.world_mut().resource_mut::<AuthMgr>().server_id = 42;
        app.world_mut()
            .resource_mut::<PlayerProfileSave>()
            .activate_role("LoginIdentity", 42);
        act(&mut app, LobbyAction::SetGenderMale(false));
        app.world_mut().write_message(UiTextInputChanged {
            entity: Entity::PLACEHOLDER,
            tag: "establish_callsign".into(),
            value: "Hero".into(),
            submitted: false,
        });
        app.world_mut().write_message(BoundUiClick {
            panel_name: "EstablishPanel".into(),
            node_name: "btn_creat".into(),
            entity: Entity::PLACEHOLDER,
        });
        app.update();
        let profile = app.world().resource::<PlayerProfileSave>();
        assert_eq!(profile.account_name, "LoginIdentity");
        assert_eq!(profile.role_name, "Hero");
        assert!(!profile.sex_male);
        assert!(profile.has_saved_role("LoginIdentity", 42));
        assert!(!profile.has_saved_role("Hero", 42));
        assert!(!app.world().resource::<MainLobbyUiState>().force_create);
        let mut saved = PlayerProfileSave::load_from_disk_or_default(&path);
        assert!(saved.activate_role("LoginIdentity", 42));
        assert_eq!(saved.role_name, "Hero");
        assert!(!saved.sex_male);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn remodel_checks_native_cost_and_keeps_nickname_out_of_appearance_validation() {
        let path =
            std::env::temp_dir().join(format!("devx-native-remodel-{}.json", std::process::id()));
        let mut app = character_action_app(path.clone());
        app.world_mut().resource_mut::<PlayerProfileSave>().gold = 2_999;
        app.world_mut()
            .resource_mut::<PlayerProfileSave>()
            .role_name
            .clear();
        app.world_mut()
            .resource_mut::<MainLobbyUiState>()
            .active_tab = LobbyTab::Establish;
        app.world_mut().write_message(BoundUiClick {
            panel_name: "EstablishPanel".into(),
            node_name: "btn_reset_role".into(),
            entity: Entity::PLACEHOLDER,
        });
        app.update();
        assert_eq!(app.world().resource::<PlayerProfileSave>().gold, 2_999);
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().active_tab,
            LobbyTab::Establish
        );
        assert!(!path.exists());
        app.world_mut().resource_mut::<PlayerProfileSave>().gold = 3_000;
        app.world_mut().write_message(BoundUiClick {
            panel_name: "EstablishPanel".into(),
            node_name: "btn_reset_role".into(),
            entity: Entity::PLACEHOLDER,
        });
        app.update();
        assert_eq!(app.world().resource::<PlayerProfileSave>().gold, 0);
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().active_tab,
            LobbyTab::Overview
        );
        assert!(
            app.world()
                .resource::<PlayerProfileSave>()
                .role_name
                .is_empty()
        );
        assert!(path.exists());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn establish_visuals_follow_native_slots_after_wrapper_names_change() {
        for male in [true, false] {
            let mut doc = load_ui_panel_doc("ui_panels/EstablishPanel.json").unwrap();
            doc.textures.clear();
            fn clear_graphics(node: &mut UiPanelNodeDto) {
                node.graphic = None;
                for child in &mut node.children {
                    clear_graphics(child);
                }
            }
            clear_graphics(&mut doc.root);
            // PPtr-bound slots survive wrapper changes; literal old paths do not.
            for child in &mut doc.root.children {
                if child.name == "m_zuo" {
                    child.name = "reexported_controls".into();
                } else if child.name == "m_name" {
                    child.name = "reexported_name".into();
                }
            }

            let mut app = App::new();
            app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()))
                .add_systems(
                    Startup,
                    move |mut commands: Commands, assets: Res<AssetServer>| {
                        commands.spawn(Node::default()).with_children(|root| {
                            let bound = spawn_retained_panel_into(
                                root,
                                &doc,
                                &assets,
                                None,
                                &FontSource::sans_serif(),
                                &UiRetainedOptions {
                                    animate: false,
                                    interactive_whitelist: Some(HashSet::new()),
                                    ..default()
                                },
                            );
                            prepare_establish_panel_widgets(&bound, male, &mut root.commands());
                            root.commands().insert_resource(EstablishFixture(bound));
                        });
                    },
                );
            app.update();

            let bound = &app.world().resource::<EstablishFixture>().0;
            for (slot, button, selected) in [(9, "btn_male", male), (6, "btn_female", !male)] {
                assert_eq!(
                    bound.game_object_slot(slot),
                    bound.by_canvas_path(&format!("reexported_controls/sex/{button}"))
                );
                for (child, active) in [("On", selected), ("Off", !selected)] {
                    let entity = establish_slot_child(bound, slot, child).unwrap();
                    assert_eq!(
                        app.world().get::<Visibility>(entity).unwrap(),
                        &if active {
                            Visibility::Inherited
                        } else {
                            Visibility::Hidden
                        }
                    );
                    assert_eq!(
                        app.world().get::<Node>(entity).unwrap().display,
                        if active { Display::Flex } else { Display::None }
                    );
                }
            }
            let placeholder = establish_slot_child(bound, 1, "Placeholder").unwrap();
            assert_eq!(
                app.world().get::<Visibility>(placeholder).unwrap(),
                &Visibility::Inherited
            );
            assert_eq!(
                app.world().get::<Node>(placeholder).unwrap().display,
                Display::Flex
            );
            let text = establish_slot_child(bound, 1, "Text").unwrap();
            assert_eq!(
                app.world().get::<Node>(text).unwrap().display,
                Display::Flex
            );
        }
    }

    #[test]
    fn clearing_establish_nickname_clears_the_name_checked_by_creation() {
        let mut app = App::new();
        app.init_resource::<PlayerSessionPrefs>()
            .init_resource::<PlayerProfileSave>()
            .init_resource::<PlayerProfileStorage>()
            .insert_resource(MainLobbyUiState {
                active_tab: LobbyTab::Establish,
                force_create: true,
                ..default()
            })
            .add_message::<UiTextInputChanged>()
            .add_message::<UiSliderChanged>()
            .add_message::<UiToggleChanged>()
            .add_systems(Update, handle_lobby_widget_events);
        let account = app
            .world()
            .resource::<PlayerProfileSave>()
            .account_name
            .clone();
        let login_account = app
            .world()
            .resource::<PlayerSessionPrefs>()
            .account_name
            .clone();
        for value in ["Survivor", "", "   "] {
            app.world_mut().write_message(UiTextInputChanged {
                entity: Entity::PLACEHOLDER,
                tag: "establish_callsign".into(),
                value: value.into(),
                submitted: false,
            });
            app.update();
            let profile = app.world().resource::<PlayerProfileSave>();
            assert_eq!(profile.role_name, value);
            assert_eq!(profile.account_name, account);
            assert_eq!(
                app.world().resource::<PlayerSessionPrefs>().account_name,
                login_account
            );
        }
    }

    #[test]
    fn native_choices_keep_template_art_offsets_and_separate_runtime_identities() {
        let mut doc = load_ui_panel_doc("ui_panels/EstablishPanel.json").unwrap();
        let template = find_panel_node_dto(&doc.root, "m_cell").unwrap().clone();
        let hair = find_panel_node_dto(&doc.root, "m_cell_1").unwrap().clone();
        let original_image = find_panel_node_dto(&template, "Image").unwrap().clone();
        let stride = find_panel_node_dto(&doc.root, "scp_role")
            .unwrap()
            .widget_props
            .as_ref()
            .and_then(|props| props.scroll_cell_size)
            .unwrap();
        assert_eq!(stride, [110.0, 160.0]);
        let choices = instantiate_establish_strip(
            &mut doc,
            "scp_role",
            "m_cell",
            "m_role_face_icon",
            vec![
                (
                    "face_a".into(),
                    "ui/face_a.png".into(),
                    true,
                    LobbyAction::SelectEstablishRole(1),
                ),
                (
                    "face_b".into(),
                    "ui/face_b.png".into(),
                    false,
                    LobbyAction::SelectEstablishRole(2),
                ),
            ],
        );
        assert_eq!(choices.len(), 2);
        let first = find_panel_node_dto(&doc.root, "face_a").unwrap();
        let second = find_panel_node_dto(&doc.root, "face_b").unwrap();
        assert!(first.go_ref.is_none() && second.go_ref.is_none());
        assert_eq!(first.canvas_rect, template.canvas_rect);
        assert!((second.canvas_rect[0] - first.canvas_rect[0] - stride[0]).abs() < 0.001);
        let image = find_panel_node_dto(second, "Image").unwrap();
        assert_eq!(&image.canvas_rect[2..], &original_image.canvas_rect[2..]);
        assert!(
            (image.canvas_rect[0]
                - second.canvas_rect[0]
                - (original_image.canvas_rect[0] - template.canvas_rect[0]))
                .abs()
                < 0.001
        );
        assert!(!find_panel_node_dto(second, "m_select").unwrap().active_self);
        assert_eq!(
            find_panel_node_dto(&doc.root, "m_cell_1")
                .unwrap()
                .canvas_rect,
            hair.canvas_rect
        );
    }

    #[test]
    fn color_choices_keep_native_padding_and_hide_unused_slots() {
        let mut doc = load_ui_panel_doc("ui_panels/EstablishPanel.json").unwrap();
        let original = find_panel_node_dto(&doc.root, "m_skin_colors")
            .unwrap()
            .clone();
        let actions = instantiate_establish_colors(
            &mut doc,
            &GameConfigs::default(),
            "m_skin_colors",
            &[1, 2, 3],
            2,
            LobbyAction::SelectSkinColorId,
        );
        assert_eq!(actions.len(), 3);
        let row = find_panel_node_dto(&doc.root, "m_skin_colors").unwrap();
        for (idx, toggle) in row.children.iter().enumerate() {
            assert_eq!(toggle.active_self, idx < 3);
            if idx < 3 {
                let color =
                    find_panel_node_dto(toggle, &format!("m_skin_colors_choice_{}", idx + 1))
                        .unwrap();
                assert_eq!(
                    color.canvas_rect,
                    find_panel_node_dto(&original.children[idx], "m_color")
                        .unwrap()
                        .canvas_rect
                );
                assert_eq!(
                    find_panel_node_dto(toggle, "Checkmark")
                        .unwrap()
                        .active_self,
                    idx == 1
                );
            }
        }
    }

    #[test]
    fn nickname_input_uses_native_text_rect_and_character_limit() {
        let mut doc = load_ui_panel_doc("ui_panels/EstablishPanel.json").unwrap();
        doc.textures.clear();
        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()))
            .add_systems(
                Startup,
                move |mut commands: Commands, assets: Res<AssetServer>| {
                    commands.spawn(Node::default()).with_children(|root| {
                        let bound = spawn_retained_panel_into(
                            root,
                            &doc,
                            &assets,
                            None,
                            &FontSource::sans_serif(),
                            &UiRetainedOptions {
                                animate: false,
                                interactive_whitelist: Some(HashSet::new()),
                                ..default()
                            },
                        );
                        let constants: serde_json::Value = serde_json::from_slice(
                            &std::fs::read(
                                crate::config::resolve_assets_root().join("config/cfg.Consts.json"),
                            )
                            .unwrap(),
                        )
                        .unwrap();
                        let configs = GameConfigs {
                            native_consts: serde_json::from_value(constants["records"][0].clone())
                                .unwrap(),
                            ..default()
                        };
                        bind_establish_name_input(
                            &bound,
                            &doc,
                            "Player",
                            &LocalizationDb::default(),
                            &configs,
                            &mut root.commands(),
                        );
                        root.commands().insert_resource(EstablishFixture(bound));
                    });
                },
            );
        app.update();
        let bound = &app.world().resource::<EstablishFixture>().0;
        let native = bound.game_object_slot(1).unwrap();
        let input = app.world().get::<UiTextInput>(native).unwrap();
        assert_eq!(input.value, "Player");
        assert_eq!(input.max_len, 7);
        assert!(input.count_utf16_units);
        let text = establish_slot_child(bound, 1, "Text").unwrap();
        assert_eq!(input.text_entity, bound.node_text_child.get(&text).copied());
        assert_eq!(
            app.world().get::<UiTextInputPlaceholder>(native).unwrap().0,
            establish_slot_child(bound, 1, "Placeholder").unwrap()
        );
    }
}

#[cfg(test)]
mod footer_tests {
    use std::time::Duration;

    use crate::ui::panel_loader::{
        UiPanelGraphicDto, UiPanelNodeDto, UiPreviewInteractionState, drive_ui_panel_animations,
    };

    use super::*;

    #[derive(Resource)]
    struct FooterFixture {
        bound: BoundUiPanel,
        controller: Entity,
    }

    fn footer_app(pose: MainPanelFooterPose) -> App {
        let mut doc = load_ui_panel_doc("ui_panels/MainPanel.json").unwrap();
        for name in [
            MAIN_PANEL_FOOTER_OPEN_CLIPS[1],
            MAIN_PANEL_FOOTER_CLOSE_CLIPS[1],
        ] {
            assert!(
                doc.animations.iter().any(|clip| clip.name == name),
                "missing APK footer clip {name}"
            );
        }
        // Exercise the exported hierarchy, bindings and tracks without starting image I/O.
        doc.textures.clear();
        fn strip_text(node: &mut UiPanelNodeDto) {
            if matches!(node.graphic, Some(UiPanelGraphicDto::Text { .. })) {
                node.graphic = None;
            }
            for child in &mut node.children {
                strip_text(child);
            }
        }
        strip_text(&mut doc.root);

        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_resource::<Time>()
            .init_resource::<MainLobbyUiState>()
            .init_resource::<UiPreviewInteractionState>()
            .init_resource::<GameConfigs>()
            .init_resource::<LocalizationDb>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<PlayerProfileSave>()
            .init_resource::<PlayerProfileStorage>()
            .init_resource::<MainLobbyStageState>()
            .init_resource::<NetClientState>()
            .init_resource::<crate::war_choice::WarChoiceState>()
            .init_resource::<NextState<AppState>>()
            .init_resource::<AuthMgr>()
            .init_resource::<PlatformMgr>()
            .add_message::<BoundUiClick>()
            .add_systems(
                Startup,
                move |mut commands: Commands, assets: Res<AssetServer>| {
                    commands
                        .spawn((
                            Node {
                                width: Val::Px(1920.0),
                                height: Val::Px(1080.0),
                                ..default()
                            },
                            UiPanelFrame {
                                ref_w: 1920.0,
                                ref_h: 1080.0,
                                fullscreen: false,
                            },
                        ))
                        .with_children(|root| {
                            let bound = spawn_retained_panel_into(
                                root,
                                &doc,
                                &assets,
                                None,
                                &FontSource::sans_serif(),
                                &UiRetainedOptions {
                                    fit_scale: 1.0,
                                    interactive_whitelist: Some(HashSet::from([
                                        "btn_show".into(),
                                        "btn_hide".into(),
                                    ])),
                                    ..default()
                                },
                            );
                            let controller = bound.by_canvas_path("").unwrap();
                            let footer = MainPanelFooter::from_panel(&bound, pose).unwrap();
                            root.commands().entity(controller).insert(footer);
                            root.commands()
                                .insert_resource(FooterFixture { bound, controller });
                        });
                },
            )
            .add_systems(
                Update,
                (
                    handle_lobby_bound_clicks,
                    update_main_panel_footer,
                    handle_main_panel_footer_clicks,
                    drive_ui_panel_animations,
                )
                    .chain(),
            );
        step(&mut app, 0);
        app
    }

    fn step(app: &mut App, millis: u64) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_millis(millis));
        app.update();
    }

    fn assert_active(app: &App, entity: Entity, active: bool) {
        assert_eq!(
            app.world().get::<Visibility>(entity).unwrap(),
            &if active {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            }
        );
        assert_eq!(
            app.world().get::<Node>(entity).unwrap().display,
            if active { Display::Flex } else { Display::None }
        );
    }

    fn assert_clips(app: &App, names: &[&str]) {
        let animator_entity = app
            .world()
            .resource::<FooterFixture>()
            .bound
            .animator_entity
            .unwrap();
        let animator = app.world().get::<UiPanelAnimator>(animator_entity).unwrap();
        let actual = animator
            .active_clips
            .iter()
            .map(|&index| animator.clips[index].name.as_str())
            .collect::<Vec<_>>();
        let expected = names
            .iter()
            .copied()
            .filter(|name| animator.clips.iter().any(|clip| clip.name == *name))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn main_panel_footer_uses_native_slots_and_animated_arrow_cycle() {
        let mut app = footer_app(MainPanelFooterPose::PrefabCollapsed);
        let fixture = app.world().resource::<FooterFixture>();
        let controller = fixture.controller;
        let show = fixture.bound.game_object_slot(92).unwrap();
        let hide = fixture.bound.game_object_slot(94).unwrap();
        let holder = fixture.bound.game_object_slot(116).unwrap();
        assert_eq!(
            Some(show),
            fixture.bound.by_canvas_path("down/Image/btn_show")
        );
        assert_eq!(
            Some(hide),
            fixture
                .bound
                .by_canvas_path("down/m_btns_holder/Viewport/m_middle_btns/btn_hide")
        );
        assert_eq!(
            Some(holder),
            fixture.bound.by_canvas_path("down/m_btns_holder")
        );
        let show_artwork = fixture
            .bound
            .by_canvas_path("down/Image/btn_show/Image")
            .unwrap();
        let hide_artwork = fixture
            .bound
            .by_canvas_path("down/m_btns_holder/Viewport/m_middle_btns/btn_hide/Image")
            .unwrap();
        assert_active(&app, show, true);
        assert_active(&app, hide, false);
        assert_active(&app, holder, false);

        let animator_entity = app
            .world()
            .resource::<FooterFixture>()
            .bound
            .animator_entity
            .unwrap();
        let mut animator = app
            .world_mut()
            .get_mut::<UiPanelAnimator>(animator_entity)
            .unwrap();
        animator.active_clips.clear();
        animator
            .clips
            .retain(|clip| !["DowntoUp", "UptoDown"].contains(&clip.name.as_str()));
        app.world_mut().trigger(Activate {
            entity: show_artwork,
        });
        step(&mut app, 20);
        assert_clips(&app, &MAIN_PANEL_FOOTER_OPEN_CLIPS);
        assert_eq!(
            app.world()
                .get::<UiPanelAnimator>(animator_entity)
                .unwrap()
                .active_clips
                .len(),
            1,
            "a missing native row controller must leave the row at its prefab pose"
        );
        assert_active(&app, holder, true);
        assert_active(&app, hide, true);
        assert_active(&app, show, true);
        for _ in 0..3 {
            step(&mut app, 20);
        }
        assert_active(&app, show, true);
        step(&mut app, 20);
        assert_active(&app, show, false);
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().footer_pose,
            MainPanelFooterPose::Expanded
        );
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().active_tab,
            LobbyTab::Overview
        );
        assert!(
            !app.world().resource::<MainLobbyUiState>().dirty,
            "footer animation must not rebuild the panel"
        );
        assert!(
            app.world()
                .get::<UiPanelAnimator>(animator_entity)
                .unwrap()
                .playing,
            "the native visibility yield precedes the 0.133 second chat clip endpoint"
        );
        step(&mut app, 40);
        assert!(
            !app.world()
                .get::<UiPanelAnimator>(animator_entity)
                .unwrap()
                .playing,
            "the chat clip must finish at its exported endpoint"
        );

        app.world_mut().trigger(Activate {
            entity: hide_artwork,
        });
        step(&mut app, 20);
        assert_clips(&app, &MAIN_PANEL_FOOTER_CLOSE_CLIPS);
        assert_active(&app, holder, true);
        for _ in 0..3 {
            step(&mut app, 20);
        }
        assert_active(&app, holder, true);
        step(&mut app, 20);
        assert_active(&app, holder, false);
        assert_active(&app, hide, false);
        assert_active(&app, show, true);
        assert_eq!(
            app.world().get::<MainPanelFooter>(controller).unwrap().pose,
            MainPanelFooterPose::Collapsed
        );
        assert_eq!(
            app.world().resource::<MainLobbyUiState>().footer_pose,
            MainPanelFooterPose::Collapsed
        );
    }

    #[test]
    fn main_panel_footer_rejects_unbound_clicks_and_incomplete_native_clip_pairs() {
        let mut app = footer_app(MainPanelFooterPose::PrefabCollapsed);
        let fixture = app.world().resource::<FooterFixture>();
        let controller = fixture.controller;
        let holder = fixture.bound.game_object_slot(116).unwrap();
        let show = fixture.bound.game_object_slot(92).unwrap();
        let animator_entity = fixture.bound.animator_entity.unwrap();
        let unrelated = app.world_mut().spawn_empty().id();
        app.world_mut().write_message(BoundUiClick {
            panel_name: "MainPanel".into(),
            node_name: "btn_show".into(),
            entity: unrelated,
        });
        step(&mut app, 20);
        assert!(
            app.world()
                .get::<MainPanelFooter>(controller)
                .unwrap()
                .transitions
                .is_empty()
        );
        assert_active(&app, holder, false);

        let mut animator = app
            .world_mut()
            .get_mut::<UiPanelAnimator>(animator_entity)
            .unwrap();
        animator.active_clips.clear();
        animator.playing = false;
        animator
            .clips
            .retain(|clip| clip.name != "Mainpanel_chat_downtoup");
        app.world_mut().trigger(Activate { entity: show });
        step(&mut app, 20);
        assert!(
            app.world()
                .get::<MainPanelFooter>(controller)
                .unwrap()
                .transitions
                .is_empty()
        );
        assert_active(&app, holder, false);
        assert_active(&app, show, true);
        assert!(
            app.world()
                .get::<UiPanelAnimator>(animator_entity)
                .unwrap()
                .active_clips
                .is_empty()
        );
    }

    #[test]
    fn main_panel_footer_retains_native_waits_during_rapid_reverse_clicks() {
        let mut app = footer_app(MainPanelFooterPose::PrefabCollapsed);
        let fixture = app.world().resource::<FooterFixture>();
        let controller = fixture.controller;
        let show = fixture.bound.game_object_slot(92).unwrap();
        let hide = fixture.bound.game_object_slot(94).unwrap();
        let holder = fixture.bound.game_object_slot(116).unwrap();
        app.world_mut().trigger(Activate { entity: show });
        step(&mut app, 20);
        app.world_mut().trigger(Activate { entity: hide });
        step(&mut app, 20);
        assert_clips(&app, &MAIN_PANEL_FOOTER_CLOSE_CLIPS);
        assert_eq!(
            app.world()
                .get::<MainPanelFooter>(controller)
                .unwrap()
                .transitions
                .len(),
            2
        );
        for _ in 0..3 {
            step(&mut app, 20);
        }
        assert_active(&app, show, false);
        assert_active(&app, holder, true);
        assert_eq!(
            app.world()
                .get::<MainPanelFooter>(controller)
                .unwrap()
                .transitions
                .len(),
            1
        );
        step(&mut app, 20);
        assert_active(&app, show, true);
        assert_active(&app, hide, false);
        assert_active(&app, holder, false);
        assert!(
            app.world()
                .get::<MainPanelFooter>(controller)
                .unwrap()
                .transitions
                .is_empty()
        );
    }

    #[test]
    fn main_panel_footer_restores_expanded_pose_on_rebuild() {
        let app = footer_app(MainPanelFooterPose::Expanded);
        let fixture = app.world().resource::<FooterFixture>();
        assert_active(&app, fixture.bound.game_object_slot(92).unwrap(), false);
        assert_active(&app, fixture.bound.game_object_slot(94).unwrap(), true);
        assert_active(&app, fixture.bound.game_object_slot(116).unwrap(), true);
        assert_clips(&app, &MAIN_PANEL_FOOTER_OPEN_CLIPS);
        let animator = app
            .world()
            .get::<UiPanelAnimator>(fixture.bound.animator_entity.unwrap())
            .unwrap();
        assert!(
            !animator.playing,
            "restoring a panel must sample the final pose without replaying it"
        );
    }
}
