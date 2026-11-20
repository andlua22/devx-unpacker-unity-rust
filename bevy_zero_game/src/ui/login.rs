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
use crate::config::{
    GameConfigs, LocalizationDb, PlayerProfileSave, PlayerProfileStorage, PlayerSessionPrefs,
    ServerStatus,
};
use crate::net::protocol::MAX_PLAYERS;
use crate::net::transport::NetClientState;
use crate::states::{AppState, LoginUiRoot};
use crate::ui::main_lobby::MainLobbyUiState;
use crate::ui::panel_loader::{
    BoundUiClick, BoundUiPanel, UiPaletteButton, UiPanelDoc, UiPanelFrame, UiPanelGraphicDto,
    UiPanelNodeDto, UiRetainedOptions, compute_panel_fit_scale, find_panel_node_texture,
    load_ui_panel_doc, spawn_chrome_backdrop, spawn_fullscreen_backdrop, spawn_retained_panel_into,
};
use crate::ui::widgets::{
    UiSelectableColors, UiTextInput, UiTextInputChanged, UiTextInputPlaceholder,
    attach_text_input_observers,
};

const PRESET_CALLSIGNS: &[&str] = &["Survivor_01", "Andlua", "AlphaScout", "Builder_UA"];

#[derive(Resource, Default)]
pub(crate) struct LoginUiState {
    pub(crate) dirty: bool,
    status_banner: String,
    announce_open: bool,
    pub(crate) show_read_effect: bool,
    pub(crate) loading_active: bool,
    account_confirm_timer: Option<Timer>,
}

#[derive(Component, Clone, Copy)]
enum LoginAction {
    CycleAccount,
    OpenAccountModal,
    ConfirmAccountModal,
    ClearAccountInputs,
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
                    finish_account_confirmation,
                    handle_login_text_input,
                    handle_login_bound_clicks,
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
    ui_state.account_confirm_timer = None;
}

fn finish_account_confirmation(
    time: Res<Time>,
    mut prefs: ResMut<PlayerSessionPrefs>,
    profile: Res<PlayerProfileSave>,
    mut ui_state: ResMut<LoginUiState>,
) {
    let Some(timer) = ui_state.account_confirm_timer.as_mut() else {
        return;
    };
    if timer.tick(time.delta()).is_finished() {
        ui_state.account_confirm_timer = None;
        prefs.account_name = profile.account_name.clone();
        prefs.session_key = profile.session_key.clone();
        prefs.account_modal_open = false;
        ui_state.dirty = true;
    }
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
        ui_state.dirty = true;
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct LoginActionCtx<'w> {
    configs: Res<'w, GameConfigs>,
    prefs: ResMut<'w, PlayerSessionPrefs>,
    profile: ResMut<'w, PlayerProfileSave>,
    profile_storage: Res<'w, PlayerProfileStorage>,
    l10n: ResMut<'w, LocalizationDb>,
    ui_state: ResMut<'w, LoginUiState>,
    next_state: ResMut<'w, NextState<AppState>>,
    auth: ResMut<'w, AuthMgr>,
    platform: ResMut<'w, PlatformMgr>,
    lobby: ResMut<'w, MainLobbyUiState>,
}

fn handle_login_bound_clicks(mut clicks: MessageReader<BoundUiClick>, mut ctx: LoginActionCtx) {
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
            ("LoginPanel", "btn_shequn") => {
                execute_login_action(LoginAction::EnterSandbox, &mut ctx)
            }
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
) {
    for ev in edits.read() {
        match ev.tag.as_str() {
            "login_callsign" | "account_username" => {
                prefs.account_name = ev.value.clone();
            }
            "account_password" => {
                prefs.session_key = ev.value.clone();
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
        .map(|s| {
            s.name
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
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

fn spawn_callsign_input(parent: &mut ChildSpawnerCommands, current: &str, font: &FontSource) {
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

fn spawn_apk_account_modal(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    l10n: &LocalizationDb,
    prefs: &PlayerSessionPrefs,
    fit_scale: f32,
    font: &FontSource,
) {
    let Ok(doc) = load_ui_panel_doc("ui_panels/LoginAccoutPanel.json") else {
        return;
    };
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
            let opts = UiRetainedOptions {
                fullscreen: true,
                fit_scale,
                localize_text: true,
                preview_mode: false,
                hidden_nodes: HashSet::from(["Template".to_owned()]),
                interactive_whitelist: Some(HashSet::from([
                    "btn_ok".to_owned(),
                    "btn_clear".to_owned(),
                ])),
                ..default()
            };
            let bound =
                spawn_retained_panel_into(overlay, &doc, asset_server, Some(l10n), font, &opts);
            bind_native_account_inputs(&bound, &doc, prefs, &mut overlay.commands());
        });
}

fn account_node_dto<'a>(
    node: &'a UiPanelNodeDto,
    bound: &BoundUiPanel,
    entity: Entity,
) -> Option<&'a UiPanelNodeDto> {
    if node
        .go_ref
        .and_then(|reference| bound.by_go_ref.get(&reference))
        .copied()
        == Some(entity)
    {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| account_node_dto(child, bound, entity))
}

fn bind_native_account_inputs(
    bound: &BoundUiPanel,
    doc: &UiPanelDoc,
    prefs: &PlayerSessionPrefs,
    commands: &mut Commands,
) {
    // LoginAccoutPanel.Awake (0x17CE920) binds username/session inputs to root slots 2/3.
    for (slot, tag, current) in [
        (2, "account_username", prefs.account_name.as_str()),
        (3, "account_password", prefs.session_key.as_str()),
    ] {
        let Some(input_entity) = bound.game_object_slot(slot) else {
            continue;
        };
        let Some(input_dto) = account_node_dto(&doc.root, bound, input_entity) else {
            continue;
        };
        let text_entity = input_dto
            .children
            .iter()
            .find(|child| child.name == "Text")
            .and_then(|child| child.go_ref)
            .and_then(|reference| bound.by_go_ref.get(&reference))
            .and_then(|entity| bound.node_text_child.get(entity))
            .copied();
        let Some(text_entity) = text_entity else {
            continue;
        };
        let props = input_dto.widget_props.clone().unwrap_or_default();
        let initial = if props.is_password {
            "*".repeat(current.chars().count())
        } else {
            current.to_owned()
        };
        commands
            .entity(text_entity)
            .entry::<Text>()
            .and_modify(move |mut text| {
                text.0 = initial;
            });
        let mut input = commands.entity(input_entity);
        input.insert((
            Pickable::default(),
            UiTextInput {
                tag: tag.to_owned(),
                value: current.to_owned(),
                // Keep the original Placeholder entity, font, color and solved artwork box.
                placeholder: String::new(),
                max_len: props
                    .char_limit
                    .and_then(|limit| usize::try_from(limit).ok())
                    .filter(|limit| *limit > 0)
                    .unwrap_or(usize::MAX),
                count_utf16_units: true,
                is_password: props.is_password,
                text_entity: Some(text_entity),
                ..default()
            },
        ));
        if let Some(placeholder) = input_dto
            .children
            .iter()
            .find(|child| child.name == "Placeholder")
            .and_then(|child| child.go_ref)
            .and_then(|reference| bound.by_go_ref.get(&reference))
            .copied()
        {
            input.insert(UiTextInputPlaceholder(placeholder));
        }
        if let Some(colors) = props.colors
            && let Some(UiPanelGraphicDto::Image { color, .. }) = &input_dto.graphic
        {
            input.insert(UiSelectableColors::from_unity_block(
                colors,
                Color::srgba(
                    color[0],
                    color[1],
                    color[2],
                    color[3] * input_dto.effective_alpha,
                ),
            ));
        }
        attach_text_input_observers(&mut input);
    }
    // Native onShow always hides its development account dropdown.
    if let Some(cache) = bound.game_object_slot(4) {
        commands.entity(cache).insert(Visibility::Hidden);
        commands
            .entity(cache)
            .entry::<Node>()
            .and_modify(|mut node| {
                node.display = Display::None;
            });
    }
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
                let _ =
                    spawn_retained_panel_into(overlay, doc, asset_server, Some(l10n), font, &opts);
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
            ctx.ui_state.dirty = true;
        }
        LoginAction::OpenAccountModal => {
            ctx.prefs.account_modal_open = !ctx.prefs.account_modal_open;
            ctx.prefs.server_modal_open = false;
            ctx.prefs.language_modal_open = false;
            ctx.ui_state.announce_open = false;
            ctx.ui_state.dirty = true;
        }
        LoginAction::ConfirmAccountModal => {
            if ctx.ui_state.account_confirm_timer.is_some() {
                return;
            }
            // Native confirm tests raw IsNullOrEmpty, then trims both stored fields.
            if ctx.prefs.account_name.is_empty() {
                return;
            }
            let trimmed = ctx.prefs.account_name.trim().to_owned();
            ctx.prefs.account_name = trimmed.clone();
            ctx.profile.account_name = trimmed.clone();
            ctx.prefs.session_key = ctx.prefs.session_key.trim().to_owned();
            ctx.profile.session_key = ctx.prefs.session_key.clone();
            if let Err(err) = ctx.profile.save_to_disk(&ctx.profile_storage.0) {
                ctx.ui_state.status_banner = err;
                ctx.ui_state.dirty = true;
                return;
            }
            // LoginAccoutPanel's native TweenTime callback opens LoginPanel after 5 s.
            ctx.ui_state.account_confirm_timer = Some(Timer::from_seconds(5.0, TimerMode::Once));
            ctx.ui_state.status_banner.clear();
            ctx.ui_state.dirty = true;
        }
        LoginAction::ClearAccountInputs => {
            if ctx.ui_state.account_confirm_timer.is_some() {
                return;
            }
            ctx.prefs.account_name.clear();
            ctx.prefs.session_key.clear();
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
            let Some(server) = ctx.configs.active_server(ctx.prefs.selected_server_idx) else {
                ctx.ui_state.status_banner = "No server configured.".to_owned();
                ctx.ui_state.dirty = true;
                return;
            };
            let server_id = server.id;
            let has_role = ctx.profile.has_saved_role(&account, server_id);
            match ctx
                .platform
                .login(&mut ctx.auth, &account, &session, server_id, has_role)
            {
                LoginOutcome::Rejected(msg) => {
                    ctx.ui_state.status_banner = msg;
                    ctx.ui_state.dirty = true;
                    return;
                }
                LoginOutcome::NoCreateRole => ctx.lobby.force_create = true,
                LoginOutcome::LoggedIn => ctx.lobby.force_create = false,
            }
            ctx.profile.activate_role(&account, server_id);
            ctx.ui_state.show_read_effect = false;
            ctx.ui_state.loading_active = true;
            ctx.profile.account_name = account;
            ctx.profile.session_key = session;
            ctx.profile.lang_index = ctx.l10n.selected_lang_index;
            ctx.next_state.set_if_different(AppState::MainLobby);
        }
    }
}

fn on_login_action(activate: On<Activate>, actions: Query<&LoginAction>, mut ctx: LoginActionCtx) {
    let Ok(&act) = actions.get(activate.entity) else {
        return;
    };
    execute_login_action(act, &mut ctx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::ButtonState;
    use bevy::input::keyboard::{Key, KeyboardInput};
    use bevy::input::mouse::MouseWheel;
    use bevy::input_focus::{FocusCause, InputFocus};

    #[derive(Resource)]
    struct NativeAccountFixture {
        bound: BoundUiPanel,
        doc: UiPanelDoc,
    }

    fn native_account_app() -> App {
        let mut doc =
            load_ui_panel_doc("ui_panels/LoginAccoutPanel.json").expect("native account panel");
        doc.textures.clear();
        fn rename_wrappers(node: &mut UiPanelNodeDto) {
            if node.name != "Text" && node.name != "Placeholder" {
                node.name = format!("reexported_{}", node.name);
            }
            for child in &mut node.children {
                rename_wrappers(child);
            }
        }
        rename_wrappers(&mut doc.root);
        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_resource::<InputFocus>()
            .add_message::<KeyboardInput>()
            .add_message::<MouseWheel>()
            .add_plugins(crate::ui::widgets::UiWidgetsRuntimePlugin)
            .insert_resource(PlayerSessionPrefs {
                account_name: "AccountNameLongerThanTwentyFourCharacters".to_owned(),
                session_key: String::new(),
                ..Default::default()
            })
            .add_systems(
                Startup,
                move |mut commands: Commands,
                      assets: Res<AssetServer>,
                      prefs: Res<PlayerSessionPrefs>| {
                    commands.spawn(Node::default()).with_children(|root| {
                        let bound = spawn_retained_panel_into(
                            root,
                            &doc,
                            &assets,
                            None,
                            &FontSource::sans_serif(),
                            &UiRetainedOptions {
                                animate: false,
                                fit_scale: 0.5,
                                interactive_whitelist: Some(HashSet::new()),
                                ..default()
                            },
                        );
                        bind_native_account_inputs(&bound, &doc, &prefs, &mut root.commands());
                        root.commands().insert_resource(NativeAccountFixture {
                            bound,
                            doc: doc.clone(),
                        });
                    });
                },
            );
        app
    }

    fn assert_percent(value: Val, expected: f32) {
        let Val::Percent(actual) = value else {
            panic!("expected retained parent-relative rectangle, got {value:?}");
        };
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }

    #[test]
    fn native_account_inputs_keep_bound_text_geometry_limits_and_hidden_cache() {
        let mut app = native_account_app();
        app.update();
        let fixture = app.world().resource::<NativeAccountFixture>();
        for (slot, tag) in [(2, "account_username"), (3, "account_password")] {
            let entity = fixture
                .bound
                .game_object_slot(slot)
                .expect("native input slot");
            let dto = account_node_dto(&fixture.doc.root, &fixture.bound, entity)
                .expect("input object identity");
            let input = app
                .world()
                .get::<UiTextInput>(entity)
                .expect("native input component");
            assert_eq!(input.tag, tag);
            assert_eq!(input.max_len, usize::MAX);
            assert!(input.count_utf16_units);
            assert!(!input.is_password);
            let parent = app
                .world()
                .get::<ChildOf>(entity)
                .expect("native parent wrapper")
                .parent();
            let parent_rect = account_node_dto(&fixture.doc.root, &fixture.bound, parent)
                .expect("native parent identity")
                .canvas_rect;
            let node = app.world().get::<Node>(entity).expect("input artwork node");
            // Retained layout expresses the APK canvas geometry relative to its actual
            // parent wrapper; fit_scale affects the frame and font, not these percentages.
            assert_percent(
                node.left,
                (dto.canvas_rect[0] - parent_rect[0]) / parent_rect[2] * 100.0,
            );
            assert_percent(
                node.top,
                (dto.canvas_rect[1] - parent_rect[1]) / parent_rect[3] * 100.0,
            );
            assert_percent(node.width, dto.canvas_rect[2] / parent_rect[2] * 100.0);
            assert_percent(node.height, dto.canvas_rect[3] / parent_rect[3] * 100.0);
            let text_dto = dto
                .children
                .iter()
                .find(|child| child.name == "Text")
                .expect("native Text");
            let text_node = fixture.bound.by_go_ref[&text_dto.go_ref.expect("Text identity")];
            assert_eq!(
                input.text_entity,
                fixture.bound.node_text_child.get(&text_node).copied()
            );
            let text_rect = text_dto.canvas_render_rect.unwrap_or(text_dto.canvas_rect);
            let node = app
                .world()
                .get::<Node>(text_node)
                .expect("native text artwork box");
            assert_percent(
                node.left,
                (text_rect[0] - dto.canvas_rect[0]) / dto.canvas_rect[2] * 100.0,
            );
            assert_percent(
                node.top,
                (text_rect[1] - dto.canvas_rect[1]) / dto.canvas_rect[3] * 100.0,
            );
            assert_percent(node.width, text_rect[2] / dto.canvas_rect[2] * 100.0);
            assert_percent(node.height, text_rect[3] / dto.canvas_rect[3] * 100.0);
            assert_eq!(
                app.world()
                    .get::<TextFont>(input.text_entity.unwrap())
                    .unwrap()
                    .font_size,
                FontSize::Px(15.0)
            );
        }
        let cache = fixture
            .bound
            .game_object_slot(4)
            .expect("native cache slot");
        assert_eq!(
            app.world().get::<Node>(cache).unwrap().display,
            Display::None
        );
        assert_eq!(
            app.world().get::<Visibility>(cache).unwrap(),
            &Visibility::Hidden
        );
        let mut inputs = app.world_mut().query::<&UiTextInput>();
        assert_eq!(inputs.iter(app.world()).count(), 2);
    }

    #[test]
    fn native_account_session_accepts_long_visible_text_and_preserves_placeholder() {
        let mut app = native_account_app();
        app.update();
        let session = app
            .world()
            .resource::<NativeAccountFixture>()
            .bound
            .game_object_slot(3)
            .unwrap();
        let placeholder = app
            .world()
            .get::<UiTextInputPlaceholder>(session)
            .unwrap()
            .0;
        assert_eq!(
            app.world().get::<Node>(placeholder).unwrap().display,
            Display::Flex
        );
        app.world_mut()
            .resource_mut::<InputFocus>()
            .set(session, FocusCause::Pressed);
        let entered = "A".repeat(40);
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::KeyA,
            logical_key: Key::Character(entered.clone().into()),
            state: ButtonState::Pressed,
            text: Some(entered.clone().into()),
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        app.update();
        let input = app.world().get::<UiTextInput>(session).unwrap();
        assert_eq!(input.value, entered);
        let text = app.world().get::<Text>(input.text_entity.unwrap()).unwrap();
        assert_eq!(text.0, format!("{entered}|"));
        assert_eq!(
            app.world().get::<Node>(placeholder).unwrap().display,
            Display::None
        );
        app.world_mut()
            .get_mut::<UiTextInput>(session)
            .unwrap()
            .value
            .clear();
        app.update();
        app.update();
        assert_eq!(
            app.world().get::<Node>(placeholder).unwrap().display,
            Display::Flex
        );
        let fixture = app.world().resource::<NativeAccountFixture>();
        let text = fixture.bound.node_text_child[&placeholder];
        assert_eq!(app.world().get::<Text>(text).unwrap().0, "点击输入密码");
        let actual = app.world().get::<TextColor>(text).unwrap().0.to_srgba();
        let expected = Color::WHITE.to_srgba();
        for (actual, expected) in [actual.red, actual.green, actual.blue, actual.alpha]
            .into_iter()
            .zip([expected.red, expected.green, expected.blue, expected.alpha])
        {
            assert!((actual - expected).abs() < 1.0e-6);
        }
    }
    use crate::config::{ServerEntry, ServerModeType};
    use bevy::ecs::system::RunSystemOnce;

    fn action_app(save_path: std::path::PathBuf) -> App {
        let mut app = App::new();
        app.init_resource::<GameConfigs>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<PlayerProfileSave>()
            .insert_resource(PlayerProfileStorage(save_path))
            .init_resource::<LocalizationDb>()
            .init_resource::<LoginUiState>()
            .init_resource::<NextState<AppState>>()
            .init_resource::<AuthMgr>()
            .init_resource::<PlatformMgr>()
            .init_resource::<MainLobbyUiState>();
        app.world_mut()
            .resource_mut::<GameConfigs>()
            .server_list
            .servers
            .push(ServerEntry {
                id: 17,
                mode: ServerModeType::BattleRoyale100,
                name: "Test".to_owned(),
                gate_url: "127.0.0.1".to_owned(),
                port: 0,
                status: ServerStatus::Smooth,
                is_local_loopback: true,
            });
        app
    }

    fn act(app: &mut App, action: LoginAction) {
        app.world_mut()
            .run_system_once(move |mut ctx: LoginActionCtx| {
                execute_login_action(action, &mut ctx);
            })
            .expect("login action");
    }

    #[test]
    fn account_edits_and_clear_preserve_stored_credentials_and_nickname() {
        let mut app = action_app(std::env::temp_dir().join("login_draft_unused.json"));
        {
            let mut profile = app.world_mut().resource_mut::<PlayerProfileSave>();
            profile.account_name = "StoredAccount".to_owned();
            profile.session_key = "StoredSession".to_owned();
            profile.role_name = "RoleNickname".to_owned();
        }
        app.add_message::<UiTextInputChanged>()
            .add_systems(Update, handle_login_text_input);
        app.world_mut().write_message(UiTextInputChanged {
            entity: Entity::PLACEHOLDER,
            tag: "account_username".to_owned(),
            value: " DraftAccount ".to_owned(),
            submitted: false,
        });
        app.world_mut().write_message(UiTextInputChanged {
            entity: Entity::PLACEHOLDER,
            tag: "account_password".to_owned(),
            value: " DraftSession ".to_owned(),
            submitted: false,
        });
        app.update();
        assert_eq!(
            app.world().resource::<PlayerSessionPrefs>().account_name,
            " DraftAccount "
        );
        act(&mut app, LoginAction::ClearAccountInputs);
        let prefs = app.world().resource::<PlayerSessionPrefs>();
        assert!(prefs.account_name.is_empty());
        assert!(prefs.session_key.is_empty());
        let profile = app.world().resource::<PlayerProfileSave>();
        assert_eq!(profile.account_name, "StoredAccount");
        assert_eq!(profile.session_key, "StoredSession");
        assert_eq!(profile.role_name, "RoleNickname");
    }

    #[test]
    fn account_confirmation_persists_trimmed_fields_and_waits_native_five_seconds() {
        let path = std::env::temp_dir().join("bevy_zero_game_account_confirm_test.json");
        let mut app = action_app(path.clone());
        app.init_resource::<Time>()
            .add_systems(Update, finish_account_confirmation);
        {
            let mut prefs = app.world_mut().resource_mut::<PlayerSessionPrefs>();
            prefs.account_name = " Account_A ".to_owned();
            prefs.session_key = " session ".to_owned();
            prefs.account_modal_open = true;
        }
        act(&mut app, LoginAction::ConfirmAccountModal);
        let saved = PlayerProfileSave::load_from_disk_or_default(&path);
        assert_eq!(saved.account_name, "Account_A");
        assert_eq!(saved.session_key, "session");
        assert_eq!(saved.role_name, "Survivor_01");
        assert!(app.world().resource::<AuthMgr>().account.is_empty());
        app.world_mut()
            .resource_mut::<PlayerSessionPrefs>()
            .account_name = "LaterDraft".to_owned();
        assert!(
            app.world()
                .resource::<PlayerSessionPrefs>()
                .account_modal_open
        );
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(4999));
        app.update();
        assert!(
            app.world()
                .resource::<PlayerSessionPrefs>()
                .account_modal_open
        );
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(1));
        app.update();
        assert!(
            !app.world()
                .resource::<PlayerSessionPrefs>()
                .account_modal_open
        );
        assert_eq!(
            app.world().resource::<PlayerSessionPrefs>().account_name,
            "Account_A"
        );
        assert_eq!(
            app.world().resource::<PlayerSessionPrefs>().session_key,
            "session"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn login_routes_and_restores_only_the_selected_accounts_role() {
        let mut app = action_app(std::env::temp_dir().join("login_route_unused.json"));
        {
            let mut profile = app.world_mut().resource_mut::<PlayerProfileSave>();
            profile.activate_role("Account_A", 17);
            profile.role_name = "Role_A".to_owned();
            profile.complete_role_creation("Account_A", 17);
        }
        app.world_mut()
            .resource_mut::<PlayerSessionPrefs>()
            .account_name = "Account_B".to_owned();
        act(&mut app, LoginAction::StartGame);
        assert!(app.world().resource::<MainLobbyUiState>().force_create);
        assert!(
            app.world()
                .resource::<PlayerProfileSave>()
                .role_name
                .is_empty()
        );
        assert_eq!(app.world().resource::<AuthMgr>().account, "Account_B");
        app.world_mut()
            .resource_mut::<PlayerSessionPrefs>()
            .account_name = "Account_A".to_owned();
        act(&mut app, LoginAction::StartGame);
        assert!(!app.world().resource::<MainLobbyUiState>().force_create);
        assert_eq!(
            app.world().resource::<PlayerProfileSave>().role_name,
            "Role_A"
        );
        assert_eq!(
            app.world().resource::<PlayerProfileSave>().account_name,
            "Account_A"
        );
    }
}
