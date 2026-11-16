//! Pure-Bevy Login Lobby retaining APK `LoginPanel.json`, `LoginServerPanel.json`, and
//! `LanguagePanel.json` 1:1 with live `BoundUiPanel` bindings, `UiTextInput` callsign editor,
//! scrollable server & language modals, and offline/LAN/100P UDP session dispatch.

use std::collections::{HashMap, HashSet};

use bevy::picking::Pickable;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource};
use bevy::ui_widgets::{Activate, Button, observe};

use crate::auth::{AuthMgr, LoginOutcome, PlatformMgr};
use crate::ui::main_lobby::MainLobbyUiState;
use crate::config::{GameConfigs, LocalizationDb, PlayerProfileSave, PlayerSessionPrefs, ServerStatus};
use crate::net::protocol::MAX_PLAYERS;
use crate::net::transport::NetClientState;
use crate::states::{AppState, LoginUiRoot};
use crate::ui::panel_loader::{
    BoundUiClick, UiPanelFrame, UiPaletteButton, UiRetainedOptions, compute_panel_fit_scale,
    find_panel_node_dto, find_panel_node_texture, load_ui_panel_doc, spawn_chrome_backdrop,
    spawn_fullscreen_backdrop, spawn_retained_panel_into,
};
use crate::ui::widgets::{UiTextInput, UiTextInputChanged, attach_text_input_observers};

const PRESET_CALLSIGNS: &[&str] = &["Survivor_01", "Andlua", "AlphaScout", "Builder_UA"];

#[derive(Resource, Default)]
pub(crate) struct LoginUiState {
    pub(crate) dirty: bool,
    status_banner: String,
    announce_open: bool,
    pub(crate) show_read_effect: bool,
    pub(crate) loading_active: bool,
    pub(crate) cache_dropdown_open: bool,
}

#[derive(Component, Clone, Copy)]
enum LoginAction {
    CycleAccount,
    OpenAccountModal,
    ConfirmAccountModal,
    ClearAccountInputs,
    ToggleCacheNamesDropdown,
    SelectCachedAccount(usize),
    ToggleAgreement,
    ToggleServerModal,
    SelectRegion(usize),
    SelectServer(usize),
    ToggleLanguageModal,
    SelectLanguage(i32),
    CloseAnnounce,
    StartGame,
    EnterSandbox,
}

/// Registers the interactive Login, Server-Select, and Language UI systems.
pub struct LoginUiPlugin;

impl Plugin for LoginUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LoginUiState>()
            .init_resource::<AuthMgr>()
            .init_resource::<PlatformMgr>()
            .add_systems(OnEnter(AppState::LoginLobby), mark_login_dirty)
            .add_systems(
                Update,
                (
                    handle_login_shortcuts,
                    handle_login_bound_clicks,
                    handle_login_text_input,
                    rebuild_login_ui_if_dirty,
                )
                    .chain()
                    .run_if(in_state(AppState::LoginLobby)),
            );
    }
}

fn mark_login_dirty(
    mut ui_state: ResMut<LoginUiState>,
    mut client: ResMut<NetClientState>,
    mut auth: ResMut<AuthMgr>,
) {
    client.disconnect();
    auth.cancel_auth();
    ui_state.dirty = true;
    ui_state.status_banner.clear();
    ui_state.announce_open = false;
    ui_state.show_read_effect = false;
    ui_state.loading_active = false;
    ui_state.cache_dropdown_open = false;
}

fn handle_login_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut next_state: ResMut<NextState<AppState>>,
    mut prefs: ResMut<PlayerSessionPrefs>,
    mut ui_state: ResMut<LoginUiState>,
) {
    if keys.just_pressed(KeyCode::F12) {
        next_state.set_if_different(AppState::DevSandbox);
    }
    if keys.just_pressed(KeyCode::Escape)
        && (prefs.language_modal_open
            || prefs.server_modal_open
            || prefs.account_modal_open
            || ui_state.announce_open)
    {
        prefs.language_modal_open = false;
        prefs.server_modal_open = false;
        prefs.account_modal_open = false;
        ui_state.announce_open = false;
        ui_state.cache_dropdown_open = false;
        ui_state.dirty = true;
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct LoginActionCtx<'w> {
    configs: Res<'w, GameConfigs>,
    prefs: ResMut<'w, PlayerSessionPrefs>,
    profile: ResMut<'w, PlayerProfileSave>,
    l10n: ResMut<'w, LocalizationDb>,
    ui_state: ResMut<'w, LoginUiState>,
    next_state: ResMut<'w, NextState<AppState>>,
    auth: ResMut<'w, AuthMgr>,
    platform: ResMut<'w, PlatformMgr>,
    lobby: ResMut<'w, MainLobbyUiState>,
}

fn handle_login_bound_clicks(
    mut clicks: MessageReader<BoundUiClick>,
    mut ctx: LoginActionCtx,
) {
    for ev in clicks.read() {
        match (ev.panel_name.as_str(), ev.node_name.as_str()) {
            ("LoginPanel", "btn_start") => execute_login_action(LoginAction::StartGame, &mut ctx),
            ("LoginPanel", "btn_choose_server") => {
                execute_login_action(LoginAction::ToggleServerModal, &mut ctx)
            }
            ("LoginPanel", "ckb_read") => {
                execute_login_action(LoginAction::ToggleAgreement, &mut ctx)
            }
            ("LoginPanel", "btn_repair") => {
                ctx.ui_state.status_banner =
                    "Integrity OK: retained uGUI panels + GLBs verified.".to_owned();
                ctx.ui_state.dirty = true;
            }
            ("LoginPanel", "btn_announce") => {
                ctx.ui_state.announce_open = !ctx.ui_state.announce_open;
                ctx.ui_state.dirty = true;
            }
            ("LoginPanel", "btn_customer_service") => {
                execute_login_action(LoginAction::ToggleLanguageModal, &mut ctx)
            }
            ("LoginPanel", "btn_shequn") => execute_login_action(LoginAction::EnterSandbox, &mut ctx),
            ("LoginPanel", "btn_appeal") => {
                execute_login_action(LoginAction::OpenAccountModal, &mut ctx)
            }
            ("LoginPanel", "btn_upload") => {
                ctx.ui_state.status_banner =
                    "Diagnostics ready: Pure Bevy 0.20 100P UDP client.".to_owned();
                ctx.ui_state.dirty = true;
            }
            ("LoginAccoutPanel", "btn_ok") => {
                execute_login_action(LoginAction::ConfirmAccountModal, &mut ctx)
            }
            ("LoginAccoutPanel", "btn_clear") => {
                execute_login_action(LoginAction::ClearAccountInputs, &mut ctx)
            }
            ("LoginAccoutPanel", "m_cacheNames") => {
                execute_login_action(LoginAction::ToggleCacheNamesDropdown, &mut ctx)
            }
            ("LoginServerPanel", "btn_close") => {
                ctx.prefs.server_modal_open = false;
                ctx.ui_state.dirty = true;
            }
            ("LanguagePanel", "btn_close_rec") => {
                ctx.prefs.language_modal_open = false;
                ctx.ui_state.dirty = true;
            }
            _ => {}
        }
    }
}

fn handle_login_text_input(
    mut edits: MessageReader<UiTextInputChanged>,
    mut prefs: ResMut<PlayerSessionPrefs>,
    mut profile: ResMut<PlayerProfileSave>,
) {
    for ev in edits.read() {
        match ev.tag.as_str() {
            "login_callsign" | "account_username" => {
                let trimmed = ev.value.trim();
                prefs.account_name = trimmed.to_owned();
                if !trimmed.is_empty() {
                    profile.account_name = trimmed.to_owned();
                }
            }
            "account_password" => {
                prefs.session_key = ev.value.clone();
                profile.session_key = ev.value.clone();
            }
            _ => {}
        }
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct LoginUiReadCtx<'w> {
    asset_server: Res<'w, AssetServer>,
    configs: Res<'w, GameConfigs>,
    l10n: Res<'w, LocalizationDb>,
    prefs: Res<'w, PlayerSessionPrefs>,
    profile: Res<'w, PlayerProfileSave>,
}

fn rebuild_login_ui_if_dirty(
    mut commands: Commands,
    mut ui_state: ResMut<LoginUiState>,
    existing: Query<Entity, (With<LoginUiRoot>, Without<ChildOf>)>,
    windows: Query<&Window>,
    ctx: LoginUiReadCtx,
) {
    if !ui_state.dirty {
        return;
    }
    ui_state.dirty = false;

    for e in &existing {
        commands.entity(e).try_despawn();
    }

    let font_source = FontSource::sans_serif();
    let win_size = windows
        .iter()
        .next()
        .map(|w| Vec2::new(w.width(), w.height()))
        .unwrap_or(Vec2::new(1280.0, 720.0));

    let active_server = ctx.configs.active_server(ctx.prefs.selected_server_idx);
    let server_label = active_server
        .map(|s| s.name.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| "Localhost 100P".to_owned());
    let status_text = active_server
        .map(|s| ctx.l10n.tr(s.status.cn_key()).to_owned())
        .unwrap_or_else(|| "Rec".to_owned());

    let active_lang_label = ctx
        .l10n
        .active_language()
        .map(LocalizationDb::display_label_for)
        .unwrap_or_else(|| "en-US".to_owned());

    let login_doc = load_ui_panel_doc("ui_panels/LoginPanel.json").ok();
    let ref_w = login_doc
        .as_ref()
        .map(|d| d.reference_width.max(64.0))
        .unwrap_or(1920.0);
    let ref_h = login_doc
        .as_ref()
        .map(|d| d.reference_height.max(64.0))
        .unwrap_or(1080.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size, true);

    let mut text_overrides = HashMap::new();
    text_overrides.insert(
        "txt_server_name".to_owned(),
        format!("[{status_text}] {server_label}"),
    );
    text_overrides.insert("m_proto".to_owned(), "Terms".to_owned());
    text_overrides.insert("m_private".to_owned(), "Privacy".to_owned());

    let mut hidden_nodes = HashSet::from([
        "m_logo".to_owned(),
        "m_over_Korea_logo".to_owned(),
        "btn_Korea_customercenter".to_owned(),
        "btn_cj".to_owned(),
        "m_healthy".to_owned(),
        "btn_cadpa".to_owned(),
        "m_children".to_owned(),
        "m_channel".to_owned(),
    ]);
    if !ui_state.loading_active {
        hidden_nodes.insert("m_loading".to_owned());
    }
    if !ctx.prefs.notice_acknowledged && !ui_state.show_read_effect {
        hidden_nodes.insert("m_EF_dagou_01".to_owned());
    }

    let interactive_whitelist = Some(HashSet::from([
        "btn_start".to_owned(),
        "btn_choose_server".to_owned(),
        "ckb_read".to_owned(),
        "btn_repair".to_owned(),
        "btn_announce".to_owned(),
        "btn_customer_service".to_owned(),
        "btn_shequn".to_owned(),
        "btn_appeal".to_owned(),
        "btn_upload".to_owned(),
    ]));

    let opts = UiRetainedOptions {
        fullscreen: true,
        fit_scale,
        localize_text: true,
        preview_mode: false,
        text_overrides,
        hidden_nodes,
        passthrough_nodes: HashSet::from(["RawImage (1)".to_owned()]),
        interactive_whitelist,
        ..default()
    };

    let backdrop_tex = login_doc
        .as_ref()
        .and_then(|d| find_panel_node_texture(d, &["RawImage (1)", "m_bg"]));

    let mut bound_login = None;
    let root_e = commands
        .spawn((
            LoginUiRoot,
            DespawnOnExit(AppState::LoginLobby),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgb(0.05, 0.08, 0.14)),
        ))
        .with_children(|root| {
            if let Some(ref bg) = backdrop_tex {
                spawn_fullscreen_backdrop(
                    root,
                    &ctx.asset_server,
                    Some(bg.as_str()),
                    Color::srgb(0.05, 0.08, 0.14),
                    0.0,
                );
            }
            root.spawn((
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
                    overflow: Overflow::clip(),
                    ..default()
                },
            ))
            .with_children(|stage| {
                if let Some(ref doc) = login_doc {
                    bound_login = Some(spawn_retained_panel_into(
                        stage,
                        doc,
                        &ctx.asset_server,
                        Some(&ctx.l10n),
                        &font_source,
                        &opts,
                    ));
                } else {
                    spawn_fullscreen_backdrop(
                        stage,
                        &ctx.asset_server,
                        Some("ui/Loading_bg_2.png"),
                        Color::srgb(0.05, 0.08, 0.14),
                        0.35,
                    );
                }

                // Clean overlay bar inside 16:9 stage: Callsign TextInput + Language + Sandbox.
                stage
                    .spawn((
                        ZIndex(12),
                        Pickable::IGNORE,
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Percent(1.4),
                            bottom: Val::Percent(1.6),
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(8.0),
                            padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(8.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.05, 0.08, 0.14, 0.88)),
                        BorderColor::all(Color::srgba(0.36, 0.52, 0.76, 0.8)),
                    ))
                    .with_children(|strip| {
                        strip.spawn((
                            Text::new(format!(
                                "v{} | {}P | ID:",
                                ctx.configs.version.str_v, MAX_PLAYERS
                            )),
                            TextFont {
                                font: font_source.clone(),
                                font_size: FontSize::Px(13.0),
                                ..default()
                            },
                            TextColor(Color::srgb(0.84, 0.90, 0.96)),
                        ));
                        spawn_callsign_input(strip, &ctx.prefs.account_name, &font_source);
                        spawn_login_btn(
                            strip,
                            "Account",
                            LoginAction::OpenAccountModal,
                            UiPaletteButton::default(),
                            &font_source,
                        );
                        spawn_login_btn(
                            strip,
                            "Cycle",
                            LoginAction::CycleAccount,
                            UiPaletteButton::default(),
                            &font_source,
                        );
                        spawn_login_btn(
                            strip,
                            &format!("{}: {active_lang_label}", ctx.l10n.tr("选择语言")),
                            LoginAction::ToggleLanguageModal,
                            UiPaletteButton::default(),
                            &font_source,
                        );
                        spawn_login_btn(
                            strip,
                            "3D Sandbox [F12]",
                            LoginAction::EnterSandbox,
                            UiPaletteButton {
                                normal: Color::srgb(0.18, 0.25, 0.36),
                                hovered: Color::srgb(0.28, 0.38, 0.54),
                                pressed: Color::srgb(0.35, 0.55, 0.85),
                            },
                            &font_source,
                        );
                    });

                if !ui_state.status_banner.is_empty() {
                    stage
                        .spawn((
                            ZIndex(14),
                            Pickable::IGNORE,
                            Node {
                                position_type: PositionType::Absolute,
                                top: Val::Percent(2.0),
                                left: Val::Percent(25.0),
                                width: Val::Percent(50.0),
                                justify_content: JustifyContent::Center,
                                padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                border_radius: BorderRadius::all(Val::Px(6.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.55, 0.14, 0.14, 0.92)),
                            BorderColor::all(Color::srgb(0.95, 0.45, 0.45)),
                        ))
                        .with_children(|b| {
                            b.spawn((
                                Text::new(ui_state.status_banner.clone()),
                                TextFont {
                                    font: font_source.clone(),
                                    font_size: FontSize::Px(14.0),
                                    ..default()
                                },
                                TextColor(Color::WHITE),
                            ));
                        });
                }

                if ctx.prefs.account_modal_open {
                    spawn_apk_account_modal(
                        stage,
                        &ctx.asset_server,
                        &ctx.l10n,
                        &ctx.prefs,
                        &ctx.profile,
                        ui_state.cache_dropdown_open,
                        fit_scale,
                        &font_source,
                    );
                } else if ctx.prefs.server_modal_open {
                    spawn_apk_server_modal(
                        stage,
                        &ctx.asset_server,
                        &ctx.configs,
                        &ctx.l10n,
                        &ctx.prefs,
                        fit_scale,
                        &font_source,
                    );
                } else if ctx.prefs.language_modal_open {
                    spawn_apk_language_modal(
                        stage,
                        &ctx.asset_server,
                        &ctx.l10n,
                        fit_scale,
                        &font_source,
                    );
                } else if ui_state.announce_open {
                    spawn_announce_modal(stage, &ctx.asset_server, &ctx.l10n, &font_source);
                }
            });
        })
        .id();

    if let Some(bound) = bound_login {
        commands.entity(root_e).insert(bound);
    }
}

fn spawn_callsign_input(
    parent: &mut ChildSpawnerCommands,
    current: &str,
    font: &FontSource,
) {
    let mut text_e = None;
    let mut ec = parent.spawn((
        Node {
            width: Val::Px(145.0),
            height: Val::Px(30.0),
            padding: UiRect::horizontal(Val::Px(8.0)),
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(5.0)),
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(Color::srgba(0.10, 0.14, 0.21, 0.95)),
        BorderColor::all(Color::srgba(0.45, 0.56, 0.72, 0.85)),
    ));
    ec.with_children(|box_sp| {
        text_e = Some(
            box_sp
                .spawn((
                    Pickable::IGNORE,
                    Text::new(current.to_owned()),
                    TextFont {
                        font: font.clone(),
                        font_size: FontSize::Px(13.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ))
                .id(),
        );
    });
    ec.insert(UiTextInput {
        tag: "login_callsign".to_owned(),
        value: current.to_owned(),
        placeholder: "Callsign...".to_owned(),
        max_len: 20,
        text_entity: text_e,
        ..default()
    });
    attach_text_input_observers(&mut ec);
}

#[allow(clippy::too_many_arguments)]
fn spawn_apk_account_modal(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    l10n: &LocalizationDb,
    prefs: &PlayerSessionPrefs,
    profile: &PlayerProfileSave,
    dropdown_open: bool,
    fit_scale: f32,
    font: &FontSource,
) {
    let modal_doc = load_ui_panel_doc("ui_panels/LoginAccoutPanel.json").ok();
    let ref_w = modal_doc
        .as_ref()
        .map(|d| d.reference_width.max(64.0))
        .unwrap_or(1920.0);
    let ref_h = modal_doc
        .as_ref()
        .map(|d| d.reference_height.max(64.0))
        .unwrap_or(1080.0);

    let rect_pct = |name: &str, fallback: [f32; 4]| -> [f32; 4] {
        modal_doc
            .as_ref()
            .and_then(|d| find_panel_node_dto(&d.root, name))
            .map(|n| {
                [
                    (n.canvas_rect[0] / ref_w) * 100.0,
                    (n.canvas_rect[1] / ref_h) * 100.0,
                    (n.canvas_rect[2] / ref_w) * 100.0,
                    (n.canvas_rect[3] / ref_h) * 100.0,
                ]
            })
            .unwrap_or(fallback)
    };
    let user_r = rect_pct("inp_usename", [36.0, 40.0, 28.0, 5.2]);
    let pass_r = rect_pct("inp_password", [36.0, 48.0, 28.0, 5.2]);
    let cache_r = rect_pct("m_cacheNames", [64.5, 40.0, 12.0, 5.2]);

    parent
        .spawn((
            ZIndex(20),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.76)),
        ))
        .with_children(|overlay| {
            if let Some(ref doc) = modal_doc {
                let opts = UiRetainedOptions {
                    fullscreen: true,
                    fit_scale,
                    localize_text: true,
                    preview_mode: false,
                    hidden_nodes: HashSet::from([
                        "Template".to_owned(),
                        "Placeholder".to_owned(),
                    ]),
                    interactive_whitelist: Some(HashSet::from([
                        "btn_ok".to_owned(),
                        "btn_clear".to_owned(),
                        "m_cacheNames".to_owned(),
                    ])),
                    ..default()
                };
                let _ = spawn_retained_panel_into(
                    overlay,
                    doc,
                    asset_server,
                    Some(l10n),
                    font,
                    &opts,
                );
            }

            // Username input inside `inp_usename` (shifted right of `账号:` label)
            overlay
                .spawn((
                    ZIndex(23),
                    Pickable::IGNORE,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(user_r[0] + 4.8),
                        top: Val::Percent(user_r[1]),
                        width: Val::Percent((user_r[2] - 4.8).max(16.0)),
                        height: Val::Percent(user_r[3].max(4.2)),
                        ..default()
                    },
                ))
                .with_children(|holder| {
                    spawn_tagged_input(
                        holder,
                        "account_username",
                        &prefs.account_name,
                        "Account...",
                        false,
                        font,
                    );
                });

            // Password / SessionKey input inside `inp_password` (shifted right of `密码:` label)
            overlay
                .spawn((
                    ZIndex(23),
                    Pickable::IGNORE,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(pass_r[0] + 4.8),
                        top: Val::Percent(pass_r[1]),
                        width: Val::Percent((pass_r[2] - 4.8).max(16.0)),
                        height: Val::Percent(pass_r[3].max(4.2)),
                        ..default()
                    },
                ))
                .with_children(|holder| {
                    spawn_tagged_input(
                        holder,
                        "account_password",
                        &prefs.session_key,
                        "SessionKey / Password...",
                        true,
                        font,
                    );
                });

            // `m_cacheNames` dropdown toggle + cached `dev_Accounts` list
            overlay
                .spawn((
                    ZIndex(24),
                    Pickable::IGNORE,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(cache_r[0]),
                        top: Val::Percent(cache_r[1]),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(4.0),
                        ..default()
                    },
                ))
                .with_children(|dd| {
                    spawn_login_btn(
                        dd,
                        &format!("dev_Accounts ({}) v", profile.dev_accounts.len()),
                        LoginAction::ToggleCacheNamesDropdown,
                        UiPaletteButton::default(),
                        font,
                    );
                    if dropdown_open {
                        dd.spawn((
                            Node {
                                flex_direction: FlexDirection::Column,
                                padding: UiRect::all(Val::Px(6.0)),
                                row_gap: Val::Px(4.0),
                                border: UiRect::all(Val::Px(1.0)),
                                border_radius: BorderRadius::all(Val::Px(6.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.06, 0.09, 0.15, 0.96)),
                            BorderColor::all(Color::srgb(0.42, 0.64, 0.88)),
                        ))
                        .with_children(|menu| {
                            for (i, acc) in profile.dev_accounts.iter().enumerate() {
                                spawn_login_btn(
                                    menu,
                                    acc,
                                    LoginAction::SelectCachedAccount(i),
                                    UiPaletteButton::default(),
                                    font,
                                );
                            }
                        });
                    }
                });
        });
}

fn spawn_tagged_input(
    parent: &mut ChildSpawnerCommands,
    tag: &str,
    current: &str,
    placeholder: &str,
    is_password: bool,
    font: &FontSource,
) {
    let display_str = if current.is_empty() {
        placeholder.to_owned()
    } else if is_password {
        "*".repeat(current.chars().count())
    } else {
        current.to_owned()
    };
    let mut text_e = None;
    let mut ec = parent.spawn((
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            padding: UiRect::horizontal(Val::Px(10.0)),
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(5.0)),
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(Color::srgba(0.09, 0.13, 0.20, 0.94)),
        BorderColor::all(Color::srgba(0.46, 0.62, 0.84, 0.90)),
    ));
    ec.with_children(|box_sp| {
        text_e = Some(
            box_sp
                .spawn((
                    Pickable::IGNORE,
                    Text::new(display_str),
                    TextFont {
                        font: font.clone(),
                        font_size: FontSize::Px(13.5),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ))
                .id(),
        );
    });
    ec.insert(UiTextInput {
        tag: tag.to_owned(),
        value: current.to_owned(),
        placeholder: placeholder.to_owned(),
        max_len: 24,
        is_password,
        text_entity: text_e,
        ..default()
    });
    attach_text_input_observers(&mut ec);
}

fn spawn_apk_server_modal(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    configs: &GameConfigs,
    l10n: &LocalizationDb,
    prefs: &PlayerSessionPrefs,
    fit_scale: f32,
    font: &FontSource,
) {
    let modal_doc = load_ui_panel_doc("ui_panels/LoginServerPanel.json").ok();
    parent
        .spawn((
            ZIndex(20),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.72)),
        ))
        .with_children(|overlay| {
            let mut bound_opt = None;
            if let Some(ref doc) = modal_doc {
                let mut text_overrides = HashMap::new();
                text_overrides.insert(
                    "txt_title".to_owned(),
                    format!("{} (serverList.xml + Localhost UDP)", l10n.tr("服务器")),
                );
                let opts = UiRetainedOptions {
                    fullscreen: true,
                    fit_scale,
                    localize_text: true,
                    preview_mode: false,
                    text_overrides,
                    hidden_nodes: HashSet::from(["m_cell".to_owned(), "m_cell_0".to_owned()]),
                    interactive_whitelist: Some(HashSet::from(["btn_close".to_owned()])),
                    ..default()
                };
                bound_opt = Some(spawn_retained_panel_into(
                    overlay,
                    doc,
                    asset_server,
                    Some(l10n),
                    font,
                    &opts,
                ));
            }

            // Region tabs (`scp_sever_btns`) on the left + scrollable server list on the right.
            if !configs.server_list.regions.is_empty() {
                overlay
                    .spawn((
                        ZIndex(22),
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Percent(11.5),
                            top: Val::Percent(24.0),
                            width: Val::Percent(12.5),
                            row_gap: Val::Px(6.0),
                            flex_direction: FlexDirection::Column,
                            ..default()
                        },
                    ))
                    .with_children(|reg_col| {
                        for (r_idx, reg) in configs.server_list.regions.iter().enumerate() {
                            let active = r_idx == prefs.selected_region_idx;
                            spawn_login_btn(
                                reg_col,
                                &reg.name,
                                LoginAction::SelectRegion(r_idx),
                                UiPaletteButton {
                                    normal: if active {
                                        Color::srgb(0.24, 0.42, 0.66)
                                    } else {
                                        Color::srgb(0.11, 0.16, 0.24)
                                    },
                                    hovered: Color::srgb(0.28, 0.48, 0.72),
                                    pressed: Color::srgb(0.35, 0.58, 0.88),
                                },
                                font,
                            );
                        }
                    });
            }

            // Populate interactive scrollable server card list positioned inside APK server frame.
            let target_box = overlay
                .spawn((
                    ZIndex(22),
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(25.0),
                        top: Val::Percent(24.0),
                        width: Val::Percent(52.0),
                        height: Val::Percent(56.0),
                        padding: UiRect::all(Val::Px(14.0)),
                        row_gap: Val::Px(8.0),
                        flex_direction: FlexDirection::Column,
                        overflow: Overflow::scroll_y(),
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.06, 0.09, 0.14, 0.92)),
                    BorderColor::all(Color::srgba(0.42, 0.62, 0.86, 0.75)),
                    crate::ui::widgets::UiScrollArea { speed_px: 1.0 },
                    Hovered::default(),
                    ScrollPosition::default(),
                ))
                .with_children(|list| {
                    for (idx, srv) in configs.server_list.servers.iter().enumerate() {
                        let active = idx == prefs.selected_server_idx;
                        let tag = l10n.tr(srv.status.cn_key());
                        let dot_color = match srv.status {
                            ServerStatus::Recommended | ServerStatus::Smooth => {
                                Color::srgb(0.28, 0.88, 0.48)
                            }
                            ServerStatus::Full => Color::srgb(0.95, 0.42, 0.25),
                            ServerStatus::Maintenance => Color::srgb(0.65, 0.68, 0.72),
                        };
                        let label = format!(
                            "{} [{}] {} ({}:{}) | {} | {}",
                            if active { "*" } else { " " },
                            srv.id,
                            srv.name,
                            srv.gate_url,
                            srv.port,
                            srv.mode.label(),
                            tag
                        );
                        list.spawn((
                            Button,
                            Hovered::default(),
                            LoginAction::SelectServer(idx),
                            UiPaletteButton {
                                normal: if active {
                                    Color::srgba(0.20, 0.36, 0.56, 0.92)
                                } else {
                                    Color::srgba(0.11, 0.16, 0.24, 0.90)
                                },
                                hovered: Color::srgb(0.26, 0.42, 0.64),
                                pressed: Color::srgb(0.32, 0.58, 0.88),
                            },
                            Node {
                                width: Val::Percent(100.0),
                                padding: UiRect::axes(Val::Px(12.0), Val::Px(10.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                border_radius: BorderRadius::all(Val::Px(6.0)),
                                flex_direction: FlexDirection::Row,
                                align_items: AlignItems::Center,
                                column_gap: Val::Px(10.0),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.11, 0.16, 0.24, 0.90)),
                            BorderColor::all(Color::srgba(0.38, 0.54, 0.76, 0.8)),
                            observe(on_login_action),
                        ))
                        .with_children(|row| {
                            row.spawn((
                                Pickable::IGNORE,
                                Node {
                                    width: Val::Px(10.0),
                                    height: Val::Px(10.0),
                                    border_radius: BorderRadius::all(Val::Px(5.0)),
                                    ..default()
                                },
                                BackgroundColor(dot_color),
                            ));
                            row.spawn((
                                Pickable::IGNORE,
                                Text::new(label),
                                TextFont {
                                    font: font.clone(),
                                    font_size: FontSize::Px(14.0),
                                    ..default()
                                },
                                TextColor(Color::WHITE),
                            ));
                        });
                    }
                    if bound_opt.is_none() {
                        spawn_login_btn(
                            list,
                            l10n.tr("取消"),
                            LoginAction::ToggleServerModal,
                            UiPaletteButton::default(),
                            font,
                        );
                    }
                })
                .id();
            let _ = target_box;
        });
}

fn spawn_apk_language_modal(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    l10n: &LocalizationDb,
    fit_scale: f32,
    font: &FontSource,
) {
    let modal_doc = load_ui_panel_doc("ui_panels/LanguagePanel.json").ok();
    parent
        .spawn((
            ZIndex(20),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.72)),
        ))
        .with_children(|overlay| {
            let mut bound_ok = false;
            if let Some(ref doc) = modal_doc {
                let opts = UiRetainedOptions {
                    fullscreen: true,
                    fit_scale,
                    localize_text: true,
                    preview_mode: false,
                    hidden_nodes: HashSet::from(["m_cell".to_owned()]),
                    interactive_whitelist: Some(HashSet::from(["btn_close_rec".to_owned()])),
                    ..default()
                };
                let _ = spawn_retained_panel_into(overlay, doc, asset_server, Some(l10n), font, &opts);
                bound_ok = true;
            }

            overlay
                .spawn((
                    ZIndex(22),
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(22.0),
                        top: Val::Percent(24.0),
                        width: Val::Percent(56.0),
                        height: Val::Percent(52.0),
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
                    BackgroundColor(Color::srgba(0.07, 0.10, 0.16, 0.92)),
                    BorderColor::all(Color::srgba(0.42, 0.62, 0.86, 0.75)),
                    crate::ui::widgets::UiScrollArea { speed_px: 1.0 },
                    Hovered::default(),
                    ScrollPosition::default(),
                ))
                .with_children(|grid| {
                    for entry in &l10n.index_entries {
                        let active = entry.index == l10n.selected_lang_index;
                        let lbl = LocalizationDb::display_label_for(entry);
                        spawn_login_btn(
                            grid,
                            &if active { format!("* {lbl}") } else { lbl },
                            LoginAction::SelectLanguage(entry.index),
                            UiPaletteButton {
                                normal: if active {
                                    Color::srgb(0.20, 0.36, 0.56)
                                } else {
                                    Color::srgb(0.13, 0.18, 0.26)
                                },
                                hovered: Color::srgb(0.26, 0.42, 0.64),
                                pressed: Color::srgb(0.32, 0.58, 0.88),
                            },
                            font,
                        );
                    }
                    if !bound_ok {
                        spawn_login_btn(
                            grid,
                            l10n.tr("确定"),
                            LoginAction::ToggleLanguageModal,
                            UiPaletteButton::default(),
                            font,
                        );
                    }
                });
        });
}

fn spawn_announce_modal(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    l10n: &LocalizationDb,
    font: &FontSource,
) {
    parent
        .spawn((
            ZIndex(20),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.70)),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        width: Val::Px(560.0),
                        padding: UiRect::all(Val::Px(18.0)),
                        row_gap: Val::Px(10.0),
                        flex_direction: FlexDirection::Column,
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(10.0)),
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.07, 0.10, 0.16, 0.96)),
                    BorderColor::all(Color::srgb(0.45, 0.64, 0.88)),
                ))
                .with_children(|card| {
                    spawn_chrome_backdrop(
                        card,
                        asset_server,
                        "ui/LoginServerPanel_ditu.png",
                        Color::srgba(0.75, 0.88, 1.0, 0.22),
                    );
                    card.spawn((
                        Text::new(format!("{} — Phase 6 Pure Bevy 0.20", l10n.tr("公告"))),
                        TextFont {
                            font: font.clone(),
                            font_size: FontSize::Px(17.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                    card.spawn((
                        Text::new("• 1:1 Retained uGUI panels exported natively via devx-exporter\n• 100P authoritative UDP protocol (< 1100B MTU)\n• Editable callsign, 11 languages (incl. uk-UA), full 3D Sandbox [F12]"),
                        TextFont {
                            font: font.clone(),
                            font_size: FontSize::Px(14.0),
                            ..default()
                        },
                        TextColor(Color::srgb(0.84, 0.90, 0.97)),
                    ));
                    spawn_login_btn(
                        card,
                        l10n.tr("确定"),
                        LoginAction::CloseAnnounce,
                        UiPaletteButton::default(),
                        font,
                    );
                });
        });
}

fn spawn_login_btn(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    action: LoginAction,
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
                padding: UiRect::axes(Val::Px(12.0), Val::Px(7.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(palette.normal),
            BorderColor::all(Color::srgb(0.38, 0.52, 0.74)),
            observe(on_login_action),
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

fn execute_login_action(act: LoginAction, ctx: &mut LoginActionCtx) {
    match act {
        LoginAction::CycleAccount => {
            let pool = if ctx.profile.dev_accounts.is_empty() {
                PRESET_CALLSIGNS
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>()
            } else {
                ctx.profile.dev_accounts.clone()
            };
            let cur_pos = pool
                .iter()
                .position(|s| s == &ctx.prefs.account_name)
                .unwrap_or(0);
            ctx.prefs.account_name = pool[(cur_pos + 1) % pool.len()].clone();
            ctx.profile.account_name = ctx.prefs.account_name.clone();
            ctx.ui_state.dirty = true;
        }
        LoginAction::OpenAccountModal => {
            ctx.prefs.account_modal_open = !ctx.prefs.account_modal_open;
            ctx.prefs.server_modal_open = false;
            ctx.prefs.language_modal_open = false;
            ctx.ui_state.announce_open = false;
            ctx.ui_state.cache_dropdown_open = false;
            ctx.ui_state.dirty = true;
        }
        LoginAction::ConfirmAccountModal => {
            let trimmed = ctx.prefs.account_name.trim().to_owned();
            if trimmed.is_empty() {
                ctx.ui_state.status_banner = "Account cannot be empty.".to_owned();
                ctx.ui_state.dirty = true;
                return;
            }
            AuthMgr::record_dev_account(&mut ctx.profile.dev_accounts, &trimmed);
            ctx.prefs.account_name = trimmed.clone();
            ctx.profile.account_name = trimmed.clone();
            ctx.profile.session_key = ctx.prefs.session_key.clone();
            ctx.auth.set_auth(&trimmed, &ctx.prefs.session_key);
            ctx.prefs.account_modal_open = false;
            ctx.ui_state.cache_dropdown_open = false;
            ctx.ui_state.status_banner.clear();
            ctx.ui_state.dirty = true;
        }
        LoginAction::ClearAccountInputs => {
            ctx.prefs.account_name.clear();
            ctx.prefs.session_key.clear();
            ctx.profile.session_key.clear();
            ctx.ui_state.dirty = true;
        }
        LoginAction::ToggleCacheNamesDropdown => {
            ctx.ui_state.cache_dropdown_open = !ctx.ui_state.cache_dropdown_open;
            ctx.ui_state.dirty = true;
        }
        LoginAction::SelectCachedAccount(idx) => {
            if let Some(acc) = ctx.profile.dev_accounts.get(idx).cloned() {
                ctx.prefs.account_name = acc.clone();
                ctx.profile.account_name = acc;
            }
            ctx.ui_state.cache_dropdown_open = false;
            ctx.ui_state.dirty = true;
        }
        LoginAction::ToggleAgreement => {
            ctx.prefs.notice_acknowledged = !ctx.prefs.notice_acknowledged;
            ctx.ui_state.show_read_effect = ctx.prefs.notice_acknowledged;
            ctx.ui_state.dirty = true;
        }
        LoginAction::ToggleServerModal => {
            ctx.prefs.server_modal_open = !ctx.prefs.server_modal_open;
            ctx.prefs.language_modal_open = false;
            ctx.prefs.account_modal_open = false;
            ctx.ui_state.announce_open = false;
            ctx.ui_state.dirty = true;
        }
        LoginAction::SelectRegion(r_idx) => {
            ctx.prefs.selected_region_idx = r_idx;
            ctx.ui_state.dirty = true;
        }
        LoginAction::SelectServer(idx) => {
            ctx.prefs.selected_server_idx = idx;
            ctx.prefs.server_modal_open = false;
            ctx.ui_state.dirty = true;
        }
        LoginAction::ToggleLanguageModal => {
            ctx.prefs.language_modal_open = !ctx.prefs.language_modal_open;
            ctx.prefs.server_modal_open = false;
            ctx.prefs.account_modal_open = false;
            ctx.ui_state.announce_open = false;
            ctx.ui_state.dirty = true;
        }
        LoginAction::SelectLanguage(lang_idx) => {
            ctx.l10n.selected_lang_index = lang_idx;
            ctx.profile.lang_index = lang_idx;
            ctx.ui_state.dirty = true;
        }
        LoginAction::CloseAnnounce => {
            ctx.ui_state.announce_open = false;
            ctx.ui_state.dirty = true;
        }
        LoginAction::EnterSandbox => {
            ctx.next_state.set_if_different(AppState::DevSandbox);
        }
        LoginAction::StartGame => {
            if !ctx.prefs.notice_acknowledged {
                ctx.ui_state.show_read_effect = true;
                let msg = ctx.configs.get_string(&ctx.l10n, 1289, &[]);
                ctx.ui_state.status_banner = if msg.starts_with("String#") {
                    "Please check the user agreement box first (#1289).".to_owned()
                } else {
                    msg
                };
                ctx.ui_state.dirty = true;
                return;
            }
            let account = ctx.prefs.account_name.trim().to_owned();
            let session = ctx.prefs.session_key.clone();
            let roles = ctx.profile.created_roles.clone();
            match ctx.platform.login(&mut ctx.auth, &account, &session, &roles) {
                LoginOutcome::Rejected(msg) => {
                    ctx.ui_state.status_banner = msg;
                    ctx.ui_state.dirty = true;
                    return;
                }
                LoginOutcome::NoCreateRole => ctx.lobby.force_create = true,
                LoginOutcome::LoggedIn => ctx.lobby.force_create = false,
            }
            AuthMgr::record_dev_account(&mut ctx.profile.dev_accounts, &account);
            ctx.ui_state.show_read_effect = false;
            ctx.ui_state.loading_active = true;
            ctx.profile.account_name = account;
            ctx.profile.session_key = session;
            ctx.profile.lang_index = ctx.l10n.selected_lang_index;
            ctx.next_state.set_if_different(AppState::MainLobby);
        }
    }
}

fn on_login_action(
    activate: On<Activate>,
    actions: Query<&LoginAction>,
    mut ctx: LoginActionCtx,
) {
    let Ok(&act) = actions.get(activate.entity) else {
        return;
    };
    execute_login_action(act, &mut ctx);
}
