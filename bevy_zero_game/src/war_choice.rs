//! Native WarChoicePanel selection state and battle startup coordination.

use bevy::prelude::*;
use bevy::text::FontSource;
use std::collections::HashMap;

use crate::config::{GameConfigs, LocalizationDb, PlayerSessionPrefs, ZhanJuCfgEntry};
use crate::net::transport::NetClientState;
use crate::states::AppState;
use crate::ui::panel_loader::{
    BoundUiClick, BoundUiPanel, UiPanelDoc, UiPanelGraphicDto, UiPanelNodeDto,
    UiRetainedOptions, compute_panel_fit_scale, load_ui_panel_doc, spawn_retained_panel_stage,
};
use crate::ui::widgets::{
    UiTextInput, UiTextInputChanged, UiTextInputPlaceholder,
    attach_text_input_observers, handle_text_input_typing,
};

/// `cfg.MapCfg` ID 1 uses `battle_map1` and `gameconfig/scenecell.obj`, the
/// terrain assets currently installed for the local battle directory.
pub(crate) const LOCAL_BATTLE_MAP_ID: i32 = 1;

#[derive(Resource, Debug)]
pub struct WarChoiceState {
    pub requested_mode: u8,
    pub game_mode: u8,
    pub region_id: i32,
    pub war_type: i32,
    pub page: usize,
    pub search: String,
    pub competition: bool,
    pub selected_battle: Option<BattleSelection>,
    pub history: Vec<BattleSelection>,
    pub status: String,
}

impl Default for WarChoiceState {
    fn default() -> Self {
        Self {
            requested_mode: 0,
            game_mode: 7,
            region_id: 0,
            war_type: 1,
            page: 1,
            search: String::new(),
            competition: false,
            selected_battle: None,
            history: Vec::new(),
            status: String::new(),
        }
    }
}

/// The replacement battle directory supplies the map explicitly, like WantBsInfo.
/// Login gateways are never treated as native battle-directory responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleSelection {
    pub server_id: u32,
    pub region_id: i32,
    pub game_mode: u8,
    pub map_id: i32,
}

impl WarChoiceState {
    pub fn visible_modes<'a>(&self, configs: &'a GameConfigs) -> Vec<&'a ZhanJuCfgEntry> {
        let mut modes: Vec<_> = configs.zhanju_cfgs.iter().filter(|mode| {
            if self.game_mode == 17 || self.game_mode == 18 {
                mode.id == self.game_mode
            } else if self.competition {
                mode.show_in_competition_selection_panel
            } else {
                mode.need_show && !matches!(mode.id, 17 | 18)
            }
        }).collect();
        modes.sort_by_key(|mode| (mode.sort_id, mode.id));
        modes
    }

    fn reset_page(&mut self) {
        self.page = 1;
        self.search.clear();
    }
}

#[derive(Component)]
struct WarChoiceRoot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WarAction {
    Close,
    ToggleModes,
    ToggleRegions,
    SetMode(u8),
    SetRegion(i32),
    Tab(i32),
    Competition,
    Official,
    Community,
    NextPage,
    PreviousPage,
    Search,
    Join,
    Preview,
    Help,
}

#[derive(Component)]
struct WarChoiceBindings {
    bound: BoundUiPanel,
    actions: HashMap<Entity, WarAction>,
}

const ITEMS: &str = "GameObject/m_recording/GANme/Image/scp_items/content";
const MODES: &str = "GameObject/m_recording/GANme/Image/m_moshi/m_choose/scp_model/content";
const REGIONS: &str = "GameObject/m_recording/GANme/Image/m_area/buyoing/m_scpArea/scp_area/content";

fn node_at<'a>(root: &'a UiPanelNodeDto, path: &str) -> Option<&'a UiPanelNodeDto> {
    let mut node = root;
    for name in path.split('/').filter(|part| !part.is_empty()) {
        node = node.children.iter().find(|child| child.name == name)?;
    }
    Some(node)
}

fn node_at_mut<'a>(root: &'a mut UiPanelNodeDto, path: &str) -> Option<&'a mut UiPanelNodeDto> {
    let mut node = root;
    for name in path.split('/').filter(|part| !part.is_empty()) {
        node = node.children.iter_mut().find(|child| child.name == name)?;
    }
    Some(node)
}

fn edit_named(node: &mut UiPanelNodeDto, name: &str, text: Option<&str>, active: Option<bool>) {
    if node.name == name {
        if let Some(value) = text
            && let Some(UiPanelGraphicDto::Text { text, .. }) = node.graphic.as_mut()
        {
            *text = value.to_owned();
        }
        if let Some(value) = active {
            node.active_self = value;
        }
    }
    for child in &mut node.children {
        edit_named(child, name, text, active);
    }
}

fn translate_clone(node: &mut UiPanelNodeDto, dy: f32) {
    node.go_ref = None;
    node.canvas_rect[1] += dy;
    if let Some(rect) = node.canvas_render_rect.as_mut() {
        rect[1] += dy;
    }
    for child in &mut node.children {
        translate_clone(child, dy);
    }
}

fn replace_rows(doc: &mut UiPanelDoc, path: &str, rows: Vec<UiPanelNodeDto>, stride: f32) {
    let Some(content) = node_at_mut(&mut doc.root, path) else { return };
    let height = content.canvas_rect[3].max(rows.len() as f32 * stride);
    content.canvas_rect[3] = height;
    if let Some(rect) = content.canvas_render_rect.as_mut() { rect[3] = height; }
    content.local_rect[3] = height;
    content.children = rows;
}

fn bind_path(bindings: &mut WarChoiceBindings, path: &str, action: WarAction) {
    let prefix = format!("{path}/");
    for (candidate, &entity) in &bindings.bound.by_path {
        if candidate == path || candidate.starts_with(&prefix) {
            bindings.actions.insert(entity, action);
        }
    }
}

fn bind_slot(bindings: &mut WarChoiceBindings, slot: usize, action: WarAction) {
    let Some(entity) = bindings.bound.game_object_slot(slot) else { return };
    let Some(path) = bindings.bound.by_path.iter().find_map(|(path, &target)| {
        (target == entity).then_some(path.clone())
    }) else { return };
    bind_path(bindings, &path, action);
}

pub struct WarChoicePlugin;

impl Plugin for WarChoicePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WarChoiceState>()
            .add_systems(OnEnter(AppState::WarChoice), enter_war_choice)
            .add_systems(Update, handle_war_choice
                .after(handle_text_input_typing)
                .run_if(in_state(AppState::WarChoice)));
    }
}

fn enter_war_choice(
    mut commands: Commands,
    configs: Res<GameConfigs>,
    mut state: ResMut<WarChoiceState>,
    asset_server: Res<AssetServer>,
    l10n: Res<LocalizationDb>,
    prefs: Res<PlayerSessionPrefs>,
    windows: Query<&Window>,
) {
    state.competition = false;
    state.war_type = 1;
    state.reset_page();
    if state.requested_mode == 1 {
        state.game_mode = 18;
    } else if matches!(state.game_mode, 8 | 16 | 18) {
        state.game_mode = 7;
    }
    if state.region_id == 0 {
        state.region_id = configs.zhanju_regions.iter()
            .find(|region| region.oversea && region.is_hero_server && region.region_type == 0)
            .or_else(|| configs.zhanju_regions.first())
            .map_or(0, |region| region.id);
    }
    spawn_war_choice(&mut commands, &asset_server, &configs, &l10n, &prefs, &state, &windows);
}

#[allow(clippy::too_many_arguments)]
fn spawn_war_choice(
    commands: &mut Commands,
    asset_server: &AssetServer,
    configs: &GameConfigs,
    l10n: &LocalizationDb,
    prefs: &PlayerSessionPrefs,
    state: &WarChoiceState,
    windows: &Query<&Window>,
) {
    let Ok(mut doc) = load_ui_panel_doc("ui_panels/WarChoicePanel.json") else {
        error!("Native WarChoicePanel is missing");
        return;
    };
    let chosen_mode = configs.zhanju_cfgs.iter().find(|mode| mode.id == state.game_mode);
    let mode_name = chosen_mode.map_or("", |mode| l10n.tr(&mode.name));
    edit_named(&mut doc.root, "txt_choosedModel", Some(mode_name), None);
    if let Some(region) = configs.zhanju_regions.iter().find(|region| region.id == state.region_id) {
        edit_named(&mut doc.root, "txt_btnChooesArea", Some(&region.name_en), None);
    }
    for name in ["scp_friends", "m_scpArea", "m_choose", "m_downloadContent", "m_countDown",
        "btn_buildLocked", "btn_gameLocked", "btn_download"] {
        edit_named(&mut doc.root, name, None, Some(false));
    }
    for (name, id) in [("btn_office", 1), ("btn_history", 2), ("btn_friend", 3), ("btn_team", 4)] {
        if let Some(node) = find_named_mut(&mut doc.root, name) {
            edit_named(node, "On", None, Some(state.war_type == id && !state.competition));
            edit_named(node, "Off", None, Some(state.war_type != id || state.competition));
        }
    }
    if let Some(node) = find_named_mut(&mut doc.root, "btn_competition") {
        edit_named(node, "On", None, Some(state.competition));
        edit_named(node, "Off", None, Some(!state.competition));
    }
    let mode_template = node_at(&doc.root, &format!("{MODES}/m_cell")).cloned();
    let modes = state.visible_modes(configs);
    let mode_rows = mode_template.into_iter().flat_map(|template| {
        modes.iter().enumerate().map(move |(index, mode)| {
            let mut row = template.clone();
            translate_clone(&mut row, index as f32 * 60.0);
            row.name = format!("mode_{}", mode.id);
            edit_named(&mut row, "txt_info", Some(l10n.tr(&mode.name)), None);
            edit_named(&mut row, "m_checkmark", None, Some(mode.id == state.game_mode));
            row
        })
    }).collect();
    replace_rows(&mut doc, MODES, mode_rows, 60.0);
    let region_template = node_at(&doc.root, &format!("{REGIONS}/m_cell")).cloned();
    let region_rows = region_template.into_iter().flat_map(|template| {
        configs.zhanju_regions.iter().enumerate().map(move |(index, region)| {
            let mut row = template.clone();
            translate_clone(&mut row, index as f32 * 48.0);
            row.name = format!("region_{}", region.id);
            row.is_interactive = true;
            row.widget_kind = Some("EmptyButton".to_owned());
            edit_named(&mut row, "txt_area", Some(&region.name_en), None);
            edit_named(&mut row, "txt_choosedArea", Some(&region.name_en), None);
            edit_named(&mut row, "m_choose", None, Some(region.id == state.region_id));
            row
        })
    }).collect();
    replace_rows(&mut doc, REGIONS, region_rows, 48.0);
    let server = configs.active_server(prefs.selected_server_idx);
    let show_row = server.is_some_and(|server| {
        server.is_local_loopback && state.war_type < 3 && !state.competition
            && state.game_mode != 8
            && (state.war_type == 1 || state.history.iter().any(|battle| battle.server_id == server.id))
            && (state.search.is_empty() || server.name.to_lowercase().contains(&state.search.to_lowercase()))
    });
    let mut rows = Vec::new();
    if show_row && let Some(server) = server
        && let Some(mut row) = node_at(&doc.root, &format!("{ITEMS}/m_cell")).cloned()
    {
        translate_clone(&mut row, 0.0);
        row.name = "local_battle".to_owned();
        row.is_interactive = true;
        row.widget_kind = Some("EmptyButton".to_owned());
        edit_named(&mut row, "txt_name", Some(&server.name), None);
        edit_named(&mut row, "txt_model", Some(mode_name), None);
        let map_name = configs.battle_maps.iter().find(|map| map.id == LOCAL_BATTLE_MAP_ID)
            .map_or("", |map| l10n.tr(&map.name));
        edit_named(&mut row, "txt_map", Some(map_name), None);
        for label in ["txt_num", "txt_time", "txt_ping"] { edit_named(&mut row, label, Some("—"), None); }
        for label in ["m_baoman", "m_huobao", "m_recommend", "m_fanzuobi", "m_password", "m_ResDownLoad"] {
            edit_named(&mut row, label, None, Some(false));
        }
        rows.push(row);
    }
    replace_rows(&mut doc, ITEMS, rows, 107.0);
    edit_named(&mut doc.root, "txt_page", Some("1/1"), None);
    let win_size = windows.iter().next().map(|window| Vec2::new(window.width(), window.height()))
        .unwrap_or(Vec2::new(1920.0, 1080.0));
    let scale = compute_panel_fit_scale(doc.reference_width, doc.reference_height, win_size, true);
    let mut result = None;
    commands.spawn((WarChoiceRoot, DespawnOnExit(AppState::WarChoice), Node {
        width: Val::Percent(100.0), height: Val::Percent(100.0),
        align_items: AlignItems::Center, justify_content: JustifyContent::Center,
        ..default()
    })).with_children(|parent| {
        let (_, bound) = spawn_retained_panel_stage(parent, &doc, asset_server, Some(l10n),
            &FontSource::sans_serif(), scale, &UiRetainedOptions { fullscreen: true,
                viewport_size: Some(win_size), fit_scale: scale, ..default() });
        result = Some(bound);
        if !state.status.is_empty() {
            parent.spawn((Node { position_type: PositionType::Absolute, left: Val::Percent(15.0),
                bottom: Val::Px(20.0), ..default() }, Text::new(&state.status), TextColor(Color::WHITE)));
        }
    });
    let Some(bound) = result else { return };
    let mut bindings = WarChoiceBindings { bound, actions: HashMap::new() };
    for (slot, action) in [(4, WarAction::Close), (21, WarAction::ToggleModes), (9, WarAction::ToggleRegions),
        (24, WarAction::Official), (25, WarAction::Community), (26, WarAction::Tab(2)),
        (27, WarAction::Tab(3)), (28, WarAction::Tab(4)), (32, WarAction::Competition),
        (5, WarAction::PreviousPage), (6, WarAction::NextPage), (19, WarAction::Search), (20, WarAction::Help)] {
        bind_slot(&mut bindings, slot, action);
    }
    bind_path(&mut bindings, &format!("{ITEMS}/local_battle"), WarAction::Join);
    bind_path(&mut bindings, &format!("{ITEMS}/local_battle/Content/btn_look"), WarAction::Preview);
    for mode in modes { bind_path(&mut bindings, &format!("{MODES}/mode_{}", mode.id), WarAction::SetMode(mode.id)); }
    for region in &configs.zhanju_regions { bind_path(&mut bindings, &format!("{REGIONS}/region_{}", region.id), WarAction::SetRegion(region.id)); }
    if let Some(input) = bindings.bound.game_object_slot(11) {
        let path = bindings.bound.by_path.iter().find_map(|(path, &entity)| (entity == input).then_some(path.clone())).unwrap_or_default();
        let text_entity = bindings.bound.by_canvas_path(&format!("{path}/Text"));
        let mut entity = commands.entity(input);
        entity.insert(UiTextInput { tag: "war_search".to_owned(), value: state.search.clone(),
            max_len: 0, count_utf16_units: true, text_entity, ..default() });
        if let Some(placeholder) = bindings.bound.by_canvas_path(&format!("{path}/Placeholder")) {
            entity.insert(UiTextInputPlaceholder(placeholder));
        }
        attach_text_input_observers(&mut entity);
    }
    if let Some(root) = bindings.bound.by_canvas_path("") { commands.entity(root).insert(bindings); }
}

fn find_named_mut<'a>(node: &'a mut UiPanelNodeDto, name: &str) -> Option<&'a mut UiPanelNodeDto> {
    if node.name == name { return Some(node) }
    node.children.iter_mut().find_map(|child| find_named_mut(child, name))
}

#[allow(clippy::too_many_arguments)]
fn handle_war_choice(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut clicks: MessageReader<BoundUiClick>,
    mut edits: MessageReader<UiTextInputChanged>,
    bindings: Query<&WarChoiceBindings>,
    roots: Query<Entity, With<WarChoiceRoot>>,
    mut state: ResMut<WarChoiceState>,
    configs: Res<GameConfigs>,
    prefs: Res<PlayerSessionPrefs>,
    l10n: Res<LocalizationDb>,
    asset_server: Res<AssetServer>,
    windows: Query<&Window>,
    mut next_state: ResMut<NextState<AppState>>,
    mut client: ResMut<NetClientState>,
    mut visibility: Query<&mut Visibility>,
    mut nodes: Query<&mut Node>,
) {
    let mut actions = Vec::new();
    for edit in edits.read().filter(|edit| edit.tag == "war_search") {
        state.search = edit.value.clone();
        if edit.submitted { actions.push(WarAction::Search); }
    }
    if keys.just_pressed(KeyCode::Escape) { actions.push(WarAction::Close); }
    for click in clicks.read().filter(|click| click.panel_name == "WarChoicePanel") {
        for binding in &bindings {
            if let Some(&action) = binding.actions.get(&click.entity) { actions.push(action); }
        }
    }
    let mut rebuild = false;
    for action in actions {
        match action {
            WarAction::Close => { next_state.set(AppState::MainLobby); return; }
            WarAction::ToggleModes | WarAction::ToggleRegions => {
                let slot = if action == WarAction::ToggleModes { 22 } else { 13 };
                for binding in &bindings {
                    if let Some(entity) = binding.bound.game_object_slot(slot) {
                        let show = nodes.get(entity).is_ok_and(|node| node.display == Display::None);
                        BoundUiPanel::set_entity_active(entity, show, &mut visibility, &mut nodes);
                    }
                }
            }
            WarAction::SetMode(mode) => { state.game_mode = mode; state.reset_page(); rebuild = true; }
            WarAction::SetRegion(region) => { state.region_id = region; state.reset_page(); rebuild = true; }
            WarAction::Tab(tab) => {
                state.war_type = tab; state.reset_page();
                state.status = if tab >= 3 { "No battle sessions have been received for this list.".to_owned() } else { String::new() };
                rebuild = true;
            }
            WarAction::Official => {
                state.competition = false; state.game_mode = 7; state.war_type = 1;
                state.reset_page(); state.status.clear(); rebuild = true;
            }
            WarAction::Competition => {
                state.competition = true;
                if let Some(mode) = configs.zhanju_cfgs.iter().find(|mode| mode.show_in_competition_selection_panel) { state.game_mode = mode.id; }
                state.war_type = 1; state.reset_page();
                state.status = "No competition sessions have been received.".to_owned(); rebuild = true;
            }
            WarAction::Community => {
                state.game_mode = 8; state.war_type = 1; state.reset_page();
                state.status = "No community sessions have been received.".to_owned(); rebuild = true;
            }
            WarAction::NextPage | WarAction::PreviousPage => {}
            WarAction::Search => {
                let minimum = configs.native_const_u64("WAR_SEARCH_MINLENGTH").unwrap_or(3) as usize;
                if state.search.encode_utf16().count() < minimum {
                    state.status = configs.get_string(&l10n, 956, &[]);
                } else { state.page = 1; state.status.clear(); }
                rebuild = true;
            }
            WarAction::Preview => {
                state.status = "This local session does not provide spectator slots.".to_owned(); rebuild = true;
            }
            WarAction::Help => {
                state.status = configs.zhanju_cfgs.iter().find(|mode| mode.id == state.game_mode)
                    .map(|mode| crate::ui::panel_loader::strip_rich_text_tags(l10n.tr(&mode.desc))).unwrap_or_default();
                rebuild = true;
            }
            WarAction::Join => {
                let Some(server) = configs.active_server(prefs.selected_server_idx) else { continue };
                let battle = BattleSelection { server_id: server.id, region_id: state.region_id,
                    game_mode: state.game_mode, map_id: LOCAL_BATTLE_MAP_ID };
                let version = if configs.version.str_v.is_empty() { "3.14.0.0" } else { &configs.version.str_v };
                match client.connect_to_server_entry(&mut commands, server, version, &prefs.account_name, l10n.selected_lang_index) {
                    Ok(()) => {
                        state.selected_battle = Some(battle.clone());
                        if !state.history.contains(&battle) { state.history.push(battle); }
                        next_state.set(AppState::LoadingScene); return;
                    }
                    Err(error) => { state.status = error; rebuild = true; }
                }
            }
        }
    }
    if rebuild {
        for root in &roots { commands.entity(root).try_despawn(); }
        spawn_war_choice(&mut commands, &asset_server, &configs, &l10n, &prefs, &state, &windows);
    }
}
