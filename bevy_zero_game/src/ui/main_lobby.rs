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

use crate::auth::{AuthMgr, NAME_ERR_EMPTY, NAME_ERR_SPECIAL, PlatformMgr, check_name};
use crate::actor_rig::{select_weapon_prop, selectable_weapon_prop_indices};
use crate::config::{
    GameConfigs, KeybindProfile, LocalizationDb, PlayerProfileSave, PlayerSessionPrefs,
};
use crate::lobby::{LobbyHeroPortraitRt, MainLobbyStageState};
use crate::net::transport::NetClientState;
use crate::states::AppState;
use crate::ui::panel_loader::{
    BoundUiClick, UiPanelFrame, UiPaletteButton, UiRetainedOptions, compute_panel_fit_scale,
    load_ui_panel_doc, spawn_retained_panel_into,
};
use crate::ui::widgets::{
    UiScrollArea, UiSliderChanged, UiTextInput, UiTextInputChanged, UiToggle, UiToggleChanged,
    attach_text_input_observers, attach_toggle_observer, spawn_clean_slider,
};

const LOBBY_CALLSIGNS: &[&str] = &[
    "Survivor_01",
    "Andlua",
    "AlphaScout",
    "Builder_UA",
    "DesertFox",
    "Nomad_99",
];

#[derive(Component)]
struct LobbyUiRoot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LobbyTab {
    #[default]
    Overview,
    Establish,
    Armory,
    Settings,
}

#[derive(Resource, Default)]
pub(crate) struct MainLobbyUiState {
    pub(crate) active_tab: LobbyTab,
    /// `EstablishPanel` opened with `type = 1` (mandatory post-login character creation).
    pub(crate) force_create: bool,
    pub(crate) dirty: bool,
    status_toast: String,
    settings_subtab: u8,
}

#[allow(dead_code)]
#[derive(Component, Clone, Copy)]
enum LobbyAction {
    SwitchTab(LobbyTab),
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
            .add_systems(
                Update,
                (
                    handle_main_lobby_shortcuts,
                    handle_lobby_bound_clicks,
                    handle_lobby_widget_events,
                    rebuild_main_lobby_ui_if_dirty,
                )
                    .chain()
                    .run_if(in_state(AppState::MainLobby)),
            );
    }
}

fn mark_main_lobby_dirty(mut ui_state: ResMut<MainLobbyUiState>, mut client: ResMut<NetClientState>) {
    client.disconnect();
    ui_state.active_tab = if ui_state.force_create {
        LobbyTab::Establish
    } else {
        LobbyTab::Overview
    };
    ui_state.dirty = true;
}

fn handle_main_lobby_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if keys.just_pressed(KeyCode::F12) {
        next_state.set_if_different(AppState::DevSandbox);
    } else if keys.just_pressed(KeyCode::Escape) {
        next_state.set_if_different(AppState::LoginLobby);
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct LobbyActionCtx<'w> {
    configs: Res<'w, GameConfigs>,
    l10n: ResMut<'w, LocalizationDb>,
    prefs: ResMut<'w, PlayerSessionPrefs>,
    profile: ResMut<'w, PlayerProfileSave>,
    ui_state: ResMut<'w, MainLobbyUiState>,
    stage_state: ResMut<'w, MainLobbyStageState>,
    client: ResMut<'w, NetClientState>,
    next_state: ResMut<'w, NextState<AppState>>,
    auth: Res<'w, AuthMgr>,
    platform: ResMut<'w, PlatformMgr>,
}

fn handle_lobby_bound_clicks(
    mut clicks: MessageReader<BoundUiClick>,
    mut commands: Commands,
    mut ctx: LobbyActionCtx,
) {
    for ev in clicks.read() {
        let act = match (ev.panel_name.as_str(), ev.node_name.as_str()) {
            ("MainPanel", "btn_start_game") => Some(LobbyAction::DeployMatch),
            ("MainPanel", "btn_setting") => Some(LobbyAction::SwitchTab(LobbyTab::Settings)),
            (
                "MainPanel",
                "btn_shop" | "btn_mainstore" | "btn_lantu" | "btn_cheku" | "btn_skinupgrade",
            ) => Some(LobbyAction::SwitchTab(LobbyTab::Armory)),
            (
                "MainPanel",
                "btn_show" | "btn_face" | "m_head_icon" | "m_touxiangk" | "btn_talent",
            ) => Some(LobbyAction::SwitchTab(LobbyTab::Establish)),
            (
                "MainPanel",
                "btn_server"
                | "btn_rank"
                | "btn_competition"
                | "btn_firepower"
                | "btn_communityServer"
                | "btn_huaijiufu",
            ) => Some(LobbyAction::CycleServer),
            (
                "MainPanel",
                "btn_email"
                | "btn_gold"
                | "btn_coupons"
                | "btn_myq"
                | "btn_pass_check"
                | "btn_firstpunch"
                | "btn_Vip"
                | "btn_yuekan"
                | "btn_teQuanDingYue",
            ) => Some(LobbyAction::ClaimSupplyReward),
            ("MainPanel", "btn_help" | "btn_community") => Some(LobbyAction::EnterSandbox),
            ("MainPanel", "btn_friend" | "btn_union") => Some(LobbyAction::SaveProfileNow),
            ("EstablishPanel", "btn_close") => Some(LobbyAction::CloseEstablishPanel),
            ("EstablishPanel", "btn_creat") => Some(LobbyAction::ConfirmCreateRole),
            ("EstablishPanel", "btn_name") => Some(LobbyAction::RandomCallsign),
            ("EstablishPanel", "btn_male") => Some(LobbyAction::SetGenderMale(true)),
            ("EstablishPanel", "btn_female") => Some(LobbyAction::SetGenderMale(false)),
            ("EstablishPanel", "btn_reset_role") => Some(LobbyAction::ResetEstablishRole),
            ("BagAndBuildPanel", "btn_back") => Some(LobbyAction::SwitchTab(LobbyTab::Overview)),
            ("SettingPanel", "btn_back") => Some(LobbyAction::SwitchTab(LobbyTab::Overview)),
            ("SettingPanel", "btn_normal") => Some(LobbyAction::SetSettingsSubtab(0)),
            ("SettingPanel", "btn_pic") => Some(LobbyAction::SetSettingsSubtab(1)),
            ("SettingPanel", "btn_operation") => Some(LobbyAction::SetSettingsSubtab(2)),
            ("SettingPanel", "btn_PCKeyCode") => Some(LobbyAction::SetSettingsSubtab(3)),
            _ => None,
        };
        if let Some(a) = act {
            execute_lobby_action(a, &mut commands, &mut ctx);
        }
    }
}

fn handle_lobby_widget_events(
    mut text_edits: MessageReader<UiTextInputChanged>,
    mut sliders: MessageReader<UiSliderChanged>,
    mut toggles: MessageReader<UiToggleChanged>,
    mut prefs: ResMut<PlayerSessionPrefs>,
    mut profile: ResMut<PlayerProfileSave>,
) {
    for ev in text_edits.read() {
        if ev.tag == "establish_callsign" {
            let trimmed = ev.value.trim();
            if !trimmed.is_empty() {
                profile.account_name = trimmed.to_owned();
                prefs.account_name = trimmed.to_owned();
            }
        }
    }
    for ev in sliders.read() {
        match ev.tag.as_str() {
            "music_vol" => profile.music_volume = ev.value.clamp(0.0, 1.0),
            "sfx_vol" => profile.sfx_volume = ev.value.clamp(0.0, 1.0),
            _ => {}
        }
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
    _ui_state: &MainLobbyUiState,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    let doc_opt = load_ui_panel_doc("ui_panels/MainPanel.json").ok();
    let ref_w = doc_opt.as_ref().map(|d| d.reference_width.max(64.0)).unwrap_or(1920.0);
    let ref_h = doc_opt.as_ref().map(|d| d.reference_height.max(64.0)).unwrap_or(1080.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, true);

    let srv_name = ctx
        .configs
        .active_server(ctx.prefs.selected_server_idx)
        .map(|s| s.name.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "Localhost 100P".to_owned());

    let mut text_overrides = HashMap::new();
    text_overrides.insert("txt_player_name".to_owned(), ctx.profile.account_name.clone());
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
    let hidden_nodes = HashSet::from([
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
        "btn_bluediamond".to_owned(),
        "btn_huya".to_owned(),
        "btn_oldUser".to_owned(),
        "btn_panelJump".to_owned(),
        "btn_qitianle".to_owned(),
        "btn_laxin".to_owned(),
        "btn_actMgrStore".to_owned(),
        "btn_cj".to_owned(),
        "btn_dailygift".to_owned(),
        "btn_active".to_owned(),
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

    let interactive_whitelist = Some(HashSet::from([
        "btn_start_game".to_owned(),
        "btn_setting".to_owned(),
        "btn_shop".to_owned(),
        "btn_mainstore".to_owned(),
        "btn_lantu".to_owned(),
        "btn_cheku".to_owned(),
        "btn_skinupgrade".to_owned(),
        "btn_show".to_owned(),
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
    ]));

    if let Some(ref doc) = doc_opt {
        let mut opts = UiRetainedOptions {
            fullscreen: true,
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
    let doc_opt = load_ui_panel_doc("ui_panels/EstablishPanel.json").ok();
    let ref_w = doc_opt.as_ref().map(|d| d.reference_width.max(64.0)).unwrap_or(1920.0);
    let ref_h = doc_opt.as_ref().map(|d| d.reference_height.max(64.0)).unwrap_or(1080.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, true);

    let mut render_targets = HashMap::new();
    if let Some(ref rt) = ctx.hero_rt {
        render_targets.insert("m_hero_tex".to_owned(), rt.0.clone());
    }

    let mut text_overrides = HashMap::new();
    text_overrides.insert("txt_consum".to_owned(), "3000".to_owned());

    let interactive_whitelist = Some(HashSet::from([
        "btn_close".to_owned(),
        "btn_creat".to_owned(),
        "btn_name".to_owned(),
        "btn_male".to_owned(),
        "btn_female".to_owned(),
        "btn_reset_role".to_owned(),
    ]));

    let mut hidden_nodes = HashSet::from([
        "m_cell".to_owned(),
        "m_cell_1".to_owned(),
        "m_skin_colors".to_owned(),
        "m_hair_colors".to_owned(),
        "scp_role".to_owned(),
        "scp_hair".to_owned(),
        "Placeholder".to_owned(),
    ]);
    // Mirror `EstablishPanel.onShow(type)`: type 1 = mandatory creation (`btn_creat`),
    // type 2 = lobby remodel (`btn_reset_role` + `btn_close` + 3000 gold cost).
    if is_create_mode {
        hidden_nodes.insert("btn_reset_role".to_owned());
        hidden_nodes.insert("btn_close".to_owned());
    } else {
        hidden_nodes.insert("btn_creat".to_owned());
    }

    if let Some(ref doc) = doc_opt {
        let mut opts = UiRetainedOptions {
            fullscreen: true,
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

        // Hide static `inp_inputName/Text` so our live `UiTextInput` renders cleanly inside `inp_inputName`.
        if let Some(&inp_txt_e) = bound.by_path.get("Image/inp_inputName/Text") {
            root.commands().entity(inp_txt_e).insert(Visibility::Hidden);
        }

        // Toggle `btn_male` / `btn_female` RadioButton `On` / `Off` state visuals 1:1.
        for (path, show) in [
            ("Image/GameObject/Sex/btn_male/On", ctx.profile.sex_male),
            ("Image/GameObject/Sex/btn_male/Off", !ctx.profile.sex_male),
            ("Image/GameObject/Sex/btn_female/On", !ctx.profile.sex_male),
            ("Image/GameObject/Sex/btn_female/Off", ctx.profile.sex_male),
        ] {
            if let Some(&e) = bound.by_path.get(path) {
                root.commands().entity(e).insert(if show {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                });
            }
        }
    }

    let sex_roles: Vec<_> = ctx
        .configs
        .establish_roles
        .iter()
        .filter(|r| r.sex == ctx.profile.sex_male)
        .collect();
    let active_role = sex_roles
        .iter()
        .copied()
        .find(|r| r.id == ctx.profile.role_preset_id)
        .or_else(|| sex_roles.first().copied());

    // 1. `scp_role` (`canvas_rect: [73.8, 303.53, 630.31, 255.0]`): horizontal scrollable strip of
    // tall `108.15 x 246` (`m_cell`) face portrait cards matching `EstablishPanel.InitRoles` 1:1.
    root.spawn((
        ZIndex(22),
        UiScrollArea { speed_px: 1.5 },
        Hovered::default(),
        ScrollPosition::default(),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent((73.8 / ref_w) * 100.0),
            top: Val::Percent((303.53 / ref_h) * 100.0),
            width: Val::Percent((630.31 / ref_w) * 100.0),
            height: Val::Percent((255.0 / ref_h) * 100.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(5.5 * fit_scale),
            overflow: Overflow::scroll_x(),
            ..default()
        },
    ))
    .with_children(|strip| {
        for role in &sex_roles {
            let selected = active_role.is_some_and(|a| a.id == role.id);
            let icon_path = ctx.configs.starter_icon_asset_path(&role.icon);
            spawn_establish_role_card(
                strip,
                &ctx.asset_server,
                &icon_path,
                selected,
                fit_scale,
                LobbyAction::SelectEstablishRole(role.id),
            );
        }
    });

    // 2. `m_skin_colors` (`y = 595.13, x = 60.6`): horizontal row of `44x44` skin tone swatches
    // (`ui/EstablishPanel_bai-1.png` tinted by `SkinColorCfg`, `61.5x61.5` selection frame + checkmark).
    if let Some(role) = active_role {
        root.spawn((
            ZIndex(22),
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent((60.6 / ref_w) * 100.0),
                top: Val::Percent((595.13 / ref_h) * 100.0),
                height: Val::Percent((44.0 / ref_h) * 100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(33.9 * fit_scale),
                ..default()
            },
        ))
        .with_children(|row| {
            for &sc_id in &role.skin_color {
                let color = ctx
                    .configs
                    .skin_color_by_id(sc_id)
                    .map(|s| {
                        parse_hex_rgb(if s.show_color.is_empty() {
                            &s.color1
                        } else {
                            &s.show_color
                        })
                    })
                    .unwrap_or(Color::srgb(0.92, 0.70, 0.56));
                let selected = ctx.profile.skin_color_id == sc_id;
                spawn_establish_swatch_btn(
                    row,
                    &ctx.asset_server,
                    color,
                    selected,
                    fit_scale,
                    LobbyAction::SelectSkinColorId(sc_id),
                );
            }
        });

        // 3. `scp_hair` (`canvas_rect: [73.0, 728.89, 490.0, 206.31]`): horizontal scrollable strip of
        // tall `83.56 x 203.10` (`m_cell_1`) hair cards matching `EstablishPanel.InitHair` 1:1.
        let hair_idx = role
            .head
            .iter()
            .position(|&id| id == ctx.profile.head_item_id)
            .unwrap_or_else(|| ctx.profile.hair_variant_idx % role.head.len().max(1));

        root.spawn((
            ZIndex(22),
            UiScrollArea { speed_px: 1.5 },
            Hovered::default(),
            ScrollPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent((73.0 / ref_w) * 100.0),
                top: Val::Percent((728.89 / ref_h) * 100.0),
                width: Val::Percent((490.0 / ref_w) * 100.0),
                height: Val::Percent((206.31 / ref_h) * 100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.7 * fit_scale),
                overflow: Overflow::scroll_x(),
                ..default()
            },
        ))
        .with_children(|strip| {
            for (idx, &head_id) in role.head.iter().enumerate() {
                let selected = idx == hair_idx;
                let icon_path = ctx
                    .configs
                    .item_icon_asset_path(head_id)
                    .unwrap_or_else(|| "ui/man_fa_2_1.png".to_owned());
                spawn_establish_hair_card(
                    strip,
                    &ctx.asset_server,
                    &icon_path,
                    selected,
                    fit_scale,
                    LobbyAction::SelectEstablishHair(idx, head_id),
                );
            }
        });

        // 4. `m_hair_colors` (`y = 963.49, x = 60.9`): horizontal row of `44x44` hair color swatches.
        if let Some(hc_group) = role.hair_color.get(hair_idx).or_else(|| role.hair_color.first()) {
            root.spawn((
                ZIndex(22),
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent((60.9 / ref_w) * 100.0),
                    top: Val::Percent((963.49 / ref_h) * 100.0),
                    height: Val::Percent((44.0 / ref_h) * 100.0),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(31.0 * fit_scale),
                    ..default()
                },
            ))
            .with_children(|row| {
                for &hc_id in &hc_group.colorlist {
                    let color = ctx
                        .configs
                        .skin_color_by_id(hc_id)
                        .map(|s| {
                            parse_hex_rgb(if s.show_color.is_empty() {
                                &s.color1
                            } else {
                                &s.show_color
                            })
                        })
                        .unwrap_or(Color::srgb(0.35, 0.25, 0.20));
                    let selected = ctx.profile.hair_color_id == hc_id;
                    spawn_establish_swatch_btn(
                        row,
                        &ctx.asset_server,
                        color,
                        selected,
                        fit_scale,
                        LobbyAction::SelectHairColorId(hc_id),
                    );
                }
            });
        }
    }

    // 5. Live `UiTextInput` transparently embedded inside `inp_inputName` (`[725.38, 977.43, 444.44, 47.81]`)
    // leaving the prefab's `EstablishPanel_xingmingdiban-1.png` background and `btn_name` dice intact.
    root.spawn((
        ZIndex(22),
        Pickable::IGNORE,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent((725.38 / ref_w) * 100.0),
            top: Val::Percent((977.43 / ref_h) * 100.0),
            width: Val::Percent((376.0 / ref_w) * 100.0),
            height: Val::Percent((47.81 / ref_h) * 100.0),
            ..default()
        },
    ))
    .with_children(|holder| {
        spawn_establish_callsign_input(holder, &ctx.profile.account_name, fit_scale, font);
    });
}

/// Spawns a 1:1 `EstablishPanel` `scp_role/m_cell` card (`108.15 x 246`, background
/// `ui/Establish_lianxing-weixuanzhong.png`, selection highlight `ui/Establish_lianxing-xuanzhong.png`,
/// and bottom-aligned `106 x 188` face portrait `m_role_face_icon`).
fn spawn_establish_role_card(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    icon_path: &str,
    selected: bool,
    fit_scale: f32,
    action: LobbyAction,
) {
    let mut bg_img = ImageNode::new(asset_server.load("ui/Establish_lianxing-weixuanzhong.png"));
    bg_img.image_mode = NodeImageMode::Stretch;

    parent
        .spawn((
            Button,
            Hovered::default(),
            action,
            bg_img,
            Node {
                width: Val::Px(108.15 * fit_scale),
                height: Val::Px(246.0 * fit_scale),
                flex_shrink: 0.0,
                justify_content: JustifyContent::FlexEnd,
                align_items: AlignItems::Center,
                overflow: Overflow::clip(),
                ..default()
            },
            observe(on_lobby_action),
        ))
        .with_children(|card| {
            if selected {
                let mut sel_img =
                    ImageNode::new(asset_server.load("ui/Establish_lianxing-xuanzhong.png"));
                sel_img.image_mode = NodeImageMode::Stretch;
                card.spawn((
                    Pickable::IGNORE,
                    sel_img,
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
            let mut face_img = ImageNode::new(asset_server.load(icon_path.to_owned()));
            face_img.image_mode = NodeImageMode::Stretch;
            card.spawn((
                Pickable::IGNORE,
                face_img,
                Node {
                    width: Val::Px(106.0 * fit_scale),
                    height: Val::Px(188.0 * fit_scale),
                    ..default()
                },
            ));
        });
}

/// Spawns a 1:1 `EstablishPanel` `scp_hair/m_cell_1` card (`83.56 x 203.10`, background
/// `ui/Establish_lianxing-weixuanzhong.png`, selection highlight `ui/Establish_faxing-xuanzhong.png`,
/// and bottom-aligned `82 x 150` hair icon `m_hair_icon`).
fn spawn_establish_hair_card(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    icon_path: &str,
    selected: bool,
    fit_scale: f32,
    action: LobbyAction,
) {
    let mut bg_img = ImageNode::new(asset_server.load("ui/Establish_lianxing-weixuanzhong.png"));
    bg_img.image_mode = NodeImageMode::Stretch;

    parent
        .spawn((
            Button,
            Hovered::default(),
            action,
            bg_img,
            Node {
                width: Val::Px(83.56 * fit_scale),
                height: Val::Px(203.10 * fit_scale),
                flex_shrink: 0.0,
                justify_content: JustifyContent::FlexEnd,
                align_items: AlignItems::Center,
                overflow: Overflow::clip(),
                ..default()
            },
            observe(on_lobby_action),
        ))
        .with_children(|card| {
            if selected {
                let mut sel_img =
                    ImageNode::new(asset_server.load("ui/Establish_faxing-xuanzhong.png"));
                sel_img.image_mode = NodeImageMode::Stretch;
                card.spawn((
                    Pickable::IGNORE,
                    sel_img,
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
            let mut hair_img = ImageNode::new(asset_server.load(icon_path.to_owned()));
            hair_img.image_mode = NodeImageMode::Stretch;
            card.spawn((
                Pickable::IGNORE,
                hair_img,
                Node {
                    width: Val::Px(82.0 * fit_scale),
                    height: Val::Px(150.0 * fit_scale),
                    ..default()
                },
            ));
        });
}

/// Spawns a 1:1 `EstablishPanel` skin/hair color `Toggle` swatch (`44x44` `ui/EstablishPanel_bai-1.png`
/// tinted with `swatch_color`, `61.5x61.5` 9-sliced `ui/EstablishPanel_xuanzhongkuang-1.png` frame,
/// and `20x20` `ui/EstablishPanel_xuanzhongkuang-2.png` bottom-right checkmark badge).
fn spawn_establish_swatch_btn(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    swatch_color: Color,
    selected: bool,
    fit_scale: f32,
    action: LobbyAction,
) {
    let mut swatch_img =
        ImageNode::new(asset_server.load("ui/EstablishPanel_bai-1.png")).with_color(swatch_color);
    swatch_img.image_mode = NodeImageMode::Stretch;

    parent
        .spawn((
            Button,
            Hovered::default(),
            action,
            swatch_img,
            Node {
                width: Val::Px(44.0 * fit_scale),
                height: Val::Px(44.0 * fit_scale),
                flex_shrink: 0.0,
                ..default()
            },
            observe(on_lobby_action),
        ))
        .with_children(|b| {
            if selected {
                let mut frame_img =
                    ImageNode::new(asset_server.load("ui/EstablishPanel_xuanzhongkuang-1.png"));
                frame_img.image_mode = NodeImageMode::Sliced(TextureSlicer {
                    border: BorderRect::all(8.0),
                    ..default()
                });
                b.spawn((
                    Pickable::IGNORE,
                    frame_img,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(-8.75 * fit_scale),
                        top: Val::Px(-8.75 * fit_scale),
                        width: Val::Px(61.5 * fit_scale),
                        height: Val::Px(61.5 * fit_scale),
                        ..default()
                    },
                ))
                .with_children(|chk| {
                    let mut badge_img =
                        ImageNode::new(asset_server.load("ui/EstablishPanel_xuanzhongkuang-2.png"));
                    badge_img.image_mode = NodeImageMode::Stretch;
                    chk.spawn((
                        Pickable::IGNORE,
                        badge_img,
                        Node {
                            position_type: PositionType::Absolute,
                            right: Val::Px(1.0 * fit_scale),
                            bottom: Val::Px(1.0 * fit_scale),
                            width: Val::Px(20.0 * fit_scale),
                            height: Val::Px(20.0 * fit_scale),
                            ..default()
                        },
                    ));
                });
            }
        });
}

fn spawn_establish_callsign_input(
    parent: &mut ChildSpawnerCommands,
    current: &str,
    fit_scale: f32,
    font: &FontSource,
) {
    let mut text_e = None;
    let mut ec = parent.spawn((
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            padding: UiRect::horizontal(Val::Px(14.0 * fit_scale)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(Color::NONE),
    ));
    ec.with_children(|b| {
        text_e = Some(
            b.spawn((
                Pickable::IGNORE,
                Text::new(current.to_owned()),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px((26.0 * fit_scale).clamp(12.0, 36.0)),
                    ..default()
                },
                TextColor(Color::srgb(0.85, 0.88, 0.94)),
            ))
            .id(),
        );
    });
    ec.insert(UiTextInput {
        tag: "establish_callsign".to_owned(),
        value: current.to_owned(),
        placeholder: "Nickname...".to_owned(),
        max_len: 20,
        text_entity: text_e,
        ..default()
    });
    attach_text_input_observers(&mut ec);
}

fn spawn_armory_tab_panel(
    root: &mut ChildSpawnerCommands,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    let doc_opt = load_ui_panel_doc("ui_panels/BagAndBuildPanel.json").ok();
    let ref_w = doc_opt.as_ref().map(|d| d.reference_width.max(64.0)).unwrap_or(1920.0);
    let ref_h = doc_opt.as_ref().map(|d| d.reference_height.max(64.0)).unwrap_or(1080.0);
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
        let _ = spawn_retained_panel_into(root, doc, &ctx.asset_server, Some(&ctx.l10n), font, &opts);
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
        spawn_lobby_btn(nav, "Deploy Match", LobbyAction::DeployMatch, accent_palette(), font);
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
            let cat = meta
                .map(|w| ctx.l10n.tr(&w.category_cn))
                .unwrap_or("Rifle");
            let name_tag = meta
                .map(|w| ctx.l10n.tr(&w.name_cn))
                .unwrap_or(&prop.name);
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
                if equipped { accent_palette() } else { UiPaletteButton::default() },
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
        let cat = meta
            .map(|w| ctx.l10n.tr(&w.category_cn))
            .unwrap_or("Rifle");
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
    let doc_opt = load_ui_panel_doc("ui_panels/SettingPanel.json").ok();
    let ref_w = doc_opt.as_ref().map(|d| d.reference_width.max(64.0)).unwrap_or(1920.0);
    let ref_h = doc_opt.as_ref().map(|d| d.reference_height.max(64.0)).unwrap_or(1080.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, true);

    // Clear overlapping APK subpages (including `m_sensitivity` and CN agreement links) while keeping
    // `SettingPanel`'s frame, title, left sidebar buttons, and bottom bar intact.
    let hidden_nodes = HashSet::from([
        "m_basic_page".to_owned(),
        "m_pic".to_owned(),
        "m_operation_page".to_owned(),
        "m_PCKeyCode".to_owned(),
        "m_sensitivity".to_owned(),
        "m_trusteeship".to_owned(),
        "m_DownloadPage".to_owned(),
        "m_help".to_owned(),
    ]);

    if let Some(ref doc) = doc_opt {
        let opts = UiRetainedOptions {
            fullscreen: true,
            fit_scale,
            localize_text: true,
            preview_mode: false,
            hidden_nodes,
            interactive_whitelist: Some(HashSet::from([
                "btn_back".to_owned(),
                "btn_normal".to_owned(),
                "btn_pic".to_owned(),
                "btn_operation".to_owned(),
                "btn_PCKeyCode".to_owned(),
            ])),
            ..default()
        };
        let _ = spawn_retained_panel_into(root, doc, &ctx.asset_server, Some(&ctx.l10n), font, &opts);
    }

    root.spawn((
        ZIndex(22),
        Pickable::IGNORE,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(1.2),
            left: Val::Percent(18.0),
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(8.0),
            ..default()
        },
    ))
    .with_children(|nav| {
        spawn_tab_buttons(nav, LobbyTab::Settings, font);
        spawn_lobby_btn(
            nav,
            "Audio/Perf",
            LobbyAction::SetSettingsSubtab(0),
            if ui_state.settings_subtab == 0 {
                accent_palette()
            } else {
                UiPaletteButton::default()
            },
            font,
        );
        spawn_lobby_btn(
            nav,
            "Keybinds",
            LobbyAction::SetSettingsSubtab(3),
            if ui_state.settings_subtab == 3 {
                accent_palette()
            } else {
                UiPaletteButton::default()
            },
            font,
        );
    });

    // Clean Interactive Settings Dock overlaid inside SettingPanel's viewport (`scp_j` area).
    let lang_label = ctx
        .l10n
        .active_language()
        .map(LocalizationDb::display_label_for)
        .unwrap_or_else(|| "en-US".to_owned());
    let fps_str = PlayerProfileSave::fps_tier_label(ctx.profile.fps_tier);
    let qual_str = PlayerProfileSave::quality_tier_label(ctx.profile.quality_tier);

    root.spawn((
        ZIndex(24),
        Pickable::IGNORE,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(16.0),
            top: Val::Percent(13.0),
            width: Val::Percent(80.5),
            padding: UiRect::all(Val::Px(22.0)),
            row_gap: Val::Px(14.0),
            flex_direction: FlexDirection::Column,
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.06, 0.09, 0.14, 0.92)),
        BorderColor::all(Color::srgba(0.40, 0.58, 0.82, 0.80)),
    ))
    .with_children(|card| {
        spawn_slider_row(
            card,
            "Music Volume:",
            "music_vol",
            ctx.profile.music_volume,
            Color::srgb(0.28, 0.68, 0.96),
            font,
        );
        spawn_slider_row(
            card,
            "SFX Volume:  ",
            "sfx_vol",
            ctx.profile.sfx_volume,
            Color::srgb(0.92, 0.56, 0.22),
            font,
        );

        card.spawn((
            Pickable::IGNORE,
            Node {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                column_gap: Val::Px(8.0),
                row_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .with_children(|row| {
            spawn_lobby_btn(row, &format!("FPS: {fps_str}"), LobbyAction::CycleFpsTier, UiPaletteButton::default(), font);
            spawn_lobby_btn(row, &format!("Quality: {qual_str}"), LobbyAction::CycleQualityTier, UiPaletteButton::default(), font);
            spawn_lobby_btn(row, &format!("Language: {lang_label}"), LobbyAction::CycleLanguage, UiPaletteButton::default(), font);
        });

        card.spawn((
            Pickable::IGNORE,
            Node {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                column_gap: Val::Px(12.0),
                row_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .with_children(|row| {
            spawn_clean_toggle(
                row,
                "a11y_high_contrast",
                "High-Contrast HUD (A11y)",
                ctx.profile.high_contrast_hud,
                font,
            );
            spawn_clean_toggle(
                row,
                "a11y_control_hints",
                "Control Hints Bar",
                ctx.profile.show_control_hints,
                font,
            );
        });

        card.spawn((
            Pickable::IGNORE,
            Node {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                column_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .with_children(|row| {
            spawn_lobby_btn(
                row,
                &format!(
                    "Preset: {} | Interact: [{}]",
                    KeybindProfile::preset_label(ctx.profile.keybinds.preset_idx),
                    KeybindProfile::short_label(&ctx.profile.keybinds.interact_key)
                ),
                LobbyAction::CycleKeybindPreset,
                UiPaletteButton::default(),
                font,
            );
            spawn_lobby_btn(row, "Cycle Interact Key", LobbyAction::CycleInteractKey, UiPaletteButton::default(), font);
        });

        card.spawn((
            Pickable::IGNORE,
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(10.0),
                ..default()
            },
        ))
        .with_children(|row| {
            spawn_lobby_btn(row, "Save Settings JSON", LobbyAction::SaveProfileNow, accent_palette(), font);
            spawn_lobby_btn(row, "Reset Defaults", LobbyAction::ResetDefaults, UiPaletteButton::default(), font);
        });
    });
}

fn spawn_slider_row(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    tag: &str,
    value: f32,
    fill_color: Color,
    font: &FontSource,
) {
    parent
        .spawn((
            Pickable::IGNORE,
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(12.0),
                ..default()
            },
        ))
        .with_children(|row| {
            row.spawn((
                Text::new(format!("{label} {:.0}%", value.clamp(0.0, 1.0) * 100.0)),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(14.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            let _ = spawn_clean_slider(
                row,
                tag,
                0.0,
                1.0,
                value,
                Val::Px(280.0),
                Val::Px(18.0),
                fill_color,
                None,
                None,
            );
        });
}

fn spawn_clean_toggle(
    parent: &mut ChildSpawnerCommands,
    tag: &str,
    label: &str,
    active: bool,
    font: &FontSource,
) {
    let mut on_e = None;
    let mut off_e = None;
    let mut ec = parent.spawn((
        Node {
            padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(6.0)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.11, 0.16, 0.24, 0.92)),
        BorderColor::all(Color::srgba(0.42, 0.62, 0.86, 0.8)),
    ));
    ec.with_children(|row| {
        row.spawn((
            Pickable::IGNORE,
            Node {
                width: Val::Px(24.0),
                height: Val::Px(16.0),
                position_type: PositionType::Relative,
                ..default()
            },
        ))
        .with_children(|ind| {
            on_e = Some(
                ind.spawn((
                    Pickable::IGNORE,
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        border_radius: BorderRadius::all(Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.22, 0.78, 0.42)),
                    if active {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    },
                ))
                .id(),
            );
            off_e = Some(
                ind.spawn((
                    Pickable::IGNORE,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        top: Val::Px(0.0),
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        border_radius: BorderRadius::all(Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.32, 0.36, 0.44)),
                    if active {
                        Visibility::Hidden
                    } else {
                        Visibility::Inherited
                    },
                ))
                .id(),
            );
        });
        row.spawn((
            Pickable::IGNORE,
            Text::new(label.to_owned()),
            TextFont {
                font: font.clone(),
                font_size: FontSize::Px(13.0),
                ..default()
            },
            TextColor(Color::WHITE),
        ));
    });
    ec.insert(UiToggle {
        tag: tag.to_owned(),
        active,
        on_entity: on_e,
        off_entity: off_e,
    });
    attach_toggle_observer(&mut ec);
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
            if tab == active { accent_palette() } else { UiPaletteButton::default() },
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

fn execute_lobby_action(
    act: LobbyAction,
    commands: &mut Commands,
    ctx: &mut LobbyActionCtx,
) {
    match act {
        LobbyAction::SwitchTab(tab) => {
            if ctx.ui_state.force_create {
                ctx.ui_state.status_toast =
                    "Create your character first (EstablishPanel type 1).".to_owned();
            } else {
                ctx.ui_state.active_tab = tab;
            }
        }
        LobbyAction::ConfirmCreateRole => {
            if !ctx.ui_state.force_create {
                persist_profile_with_toast(&ctx.profile, &mut ctx.ui_state, "Profile saved cleanly.");
            } else if let Some(code) = check_name(&ctx.profile.account_name) {
                let msg = match code {
                    NAME_ERR_EMPTY => "Name is empty.",
                    NAME_ERR_SPECIAL => "Name contains special characters.",
                    _ => "Name is too short.",
                }
                .to_owned();
                ctx.ui_state.status_toast = msg;
            } else {
                ctx.platform
                    .on_create_role(&ctx.auth, &mut ctx.profile.created_roles);
                ctx.ui_state.force_create = false;
                ctx.ui_state.active_tab = LobbyTab::Overview;
                ctx.stage_state.hero_dirty = true;
                persist_profile_with_toast(&ctx.profile, &mut ctx.ui_state, "Character created.");
            }
        }
        LobbyAction::SetSettingsSubtab(sub) => ctx.ui_state.settings_subtab = sub,
        LobbyAction::RandomCallsign => {
            let seed = (ctx.profile.exp as usize)
                .wrapping_add(ctx.profile.gold as usize)
                .wrapping_add(ctx.profile.account_name.len().wrapping_mul(13));
            let generated = ctx
                .configs
                .generate_random_role_name(ctx.l10n.selected_lang_index, seed);
            ctx.profile.account_name = if generated.is_empty() {
                LOBBY_CALLSIGNS[seed % LOBBY_CALLSIGNS.len()].to_owned()
            } else {
                generated
            };
            ctx.prefs.account_name = ctx.profile.account_name.clone();
            ctx.profile.exp = ctx.profile.exp.wrapping_add(17);
        }
        LobbyAction::SetGenderMale(male) => {
            ctx.profile.sex_male = male;
            if let Some(first_role) = ctx.configs.establish_roles.iter().find(|r| r.sex == male) {
                ctx.profile.role_preset_id = first_role.id;
                ctx.profile.face_item_id = first_role
                    .face
                    .first()
                    .copied()
                    .unwrap_or(if male { 20550 } else { 23550 });
                ctx.profile.head_item_id = first_role
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
                if !role.skin_color.contains(&ctx.profile.skin_color_id)
                    && let Some(&sc) = role.skin_color.first()
                {
                    ctx.profile.skin_color_id = sc;
                }
                ctx.stage_state.hero_dirty = true;
            }
        }
        LobbyAction::SelectEstablishHair(idx, head_id) => {
            ctx.profile.hair_variant_idx = idx;
            ctx.profile.head_item_id = head_id;
            if let Some(role) = ctx
                .configs
                .establish_roles
                .iter()
                .find(|r| r.id == ctx.profile.role_preset_id)
                && let Some(hc_group) = role.hair_color.get(idx)
                && let Some(&first_hc) = hc_group.colorlist.first()
            {
                ctx.profile.hair_color_id = first_hc;
            }
            ctx.stage_state.hero_dirty = true;
        }
        LobbyAction::SelectSkinColorId(sc_id) => {
            ctx.profile.skin_color_id = sc_id;
        }
        LobbyAction::SelectHairColorId(hc_id) => {
            ctx.profile.hair_color_id = hc_id;
        }
        LobbyAction::ResetEstablishRole => {
            if let Some(code) = check_name(&ctx.profile.account_name) {
                let msg = match code {
                    NAME_ERR_EMPTY => "Name is empty.",
                    NAME_ERR_SPECIAL => "Name contains special characters.",
                    _ => "Name is too short.",
                };
                ctx.ui_state.status_toast = msg.to_owned();
            } else {
                if ctx.profile.gold >= 3_000 {
                    ctx.profile.gold -= 3_000;
                }
                ctx.stage_state.hero_dirty = true;
                persist_profile_with_toast(
                    &ctx.profile,
                    &mut ctx.ui_state,
                    "Appearance remodeled & saved.",
                );
            }
        }
        LobbyAction::CloseEstablishPanel => {
            if !ctx.ui_state.force_create {
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
                let next = &ctx.configs.establish_roles[(cur + 1) % ctx.configs.establish_roles.len()];
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
            ctx.profile.gold = ctx.profile.gold.saturating_add(500);
            ctx.profile.diamond = ctx.profile.diamond.saturating_add(20);
            ctx.profile.exp = ctx.profile.exp.saturating_add(50);
            if ctx.profile.exp >= 200 {
                ctx.profile.exp -= 200;
                ctx.profile.level = ctx.profile.level.saturating_add(1);
            }
            persist_profile_with_toast(&ctx.profile, &mut ctx.ui_state, "Claimed daily supplies!");
        }
        LobbyAction::EquipWeapon(idx) => {
            ctx.profile.equipped_prop_idx = idx;
            ctx.stage_state.socket_synced = false;
        }
        LobbyAction::CycleFpsTier => ctx.profile.fps_tier = (ctx.profile.fps_tier + 1) % 4,
        LobbyAction::CycleQualityTier => ctx.profile.quality_tier = (ctx.profile.quality_tier + 1) % 3,
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
                ctx.prefs.selected_server_idx = (ctx.prefs.selected_server_idx + 1) % n;
            }
        }
        LobbyAction::ToggleAutoRotate => ctx.stage_state.auto_rotate = !ctx.stage_state.auto_rotate,
        LobbyAction::SaveProfileNow => {
            persist_profile_with_toast(&ctx.profile, &mut ctx.ui_state, "Profile saved cleanly.");
        }
        LobbyAction::ResetDefaults => {
            let name = ctx.profile.account_name.clone();
            *ctx.profile = PlayerProfileSave {
                account_name: name,
                ..Default::default()
            };
            ctx.l10n.selected_lang_index = ctx.profile.lang_index;
            ctx.stage_state.hero_dirty = true;
            persist_profile_with_toast(&ctx.profile, &mut ctx.ui_state, "Settings reset.");
        }
        LobbyAction::DeployMatch => {
            let Some(srv) = ctx.configs.active_server(ctx.prefs.selected_server_idx) else {
                ctx.ui_state.status_toast = "No server configured.".to_owned();
                ctx.ui_state.dirty = true;
                return;
            };
            let ver = if ctx.configs.version.str_v.is_empty() {
                "3.14.0.0"
            } else {
                ctx.configs.version.str_v.as_str()
            };
            ctx.prefs.account_name = ctx.profile.account_name.clone();
            if let Err(err) = ctx.client.connect_to_server_entry(
                commands,
                srv,
                ver,
                &ctx.profile.account_name,
                ctx.l10n.selected_lang_index,
            ) {
                ctx.ui_state.status_toast = err;
                ctx.ui_state.dirty = true;
                return;
            }
            let _ = ctx
                .profile
                .save_to_disk(&PlayerProfileSave::resolve_save_path());
            ctx.next_state.set_if_different(AppState::LoadingScene);
        }
        LobbyAction::EnterSandbox => ctx.next_state.set_if_different(AppState::DevSandbox),
        LobbyAction::BackToLogin => ctx.next_state.set_if_different(AppState::LoginLobby),
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
    ui_state: &mut MainLobbyUiState,
    ok_msg: &str,
) {
    let path = PlayerProfileSave::resolve_save_path();
    ui_state.status_toast = match profile.save_to_disk(&path) {
        Ok(()) => ok_msg.to_owned(),
        Err(e) => format!("Save warning: {e}"),
    };
}
