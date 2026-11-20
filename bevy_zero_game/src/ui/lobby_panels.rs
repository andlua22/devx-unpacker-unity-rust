//! Native lobby controller destinations and retained SettingPanel pages.

use super::*;
use crate::ui::panel_loader::UiAssetRefDto;
use crate::ui::widgets::{UiSlider, attach_slider_observers, sync_one_slider_visuals};

#[path = "native_store.rs"]
mod native_store;
pub(super) use native_store::sync_native_store;

#[path = "native_settings_input.rs"]
mod native_settings_input;
pub(super) use native_settings_input::capture_native_keys;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeLobbyPanel {
    Shop,
    Store,
    Mail,
    Friends,
    Leaderboard,
    UnionJoin,
    Chat,
    Face,
    Talent,
    Blueprints,
    WeaponSkins,
    Community,
    Vip,
    Magazine,
    Privilege,
    FirstPunch,
    LadderNotice,
    Achievement,
    Activity,
    DailyGift,
    LobbyMarket,
    BlueDiamond,
}

impl NativeLobbyPanel {
    pub(super) fn default_page(self) -> i32 {
        match self {
            Self::Store => 5,
            Self::Shop => 1,
            _ => 0,
        }
    }
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Shop => "ShopOutBattlePanel",
            Self::Store => "StorePanel",
            Self::Mail => "MailPanel",
            Self::Friends => "FriendPanel",
            Self::Leaderboard => "LeaderboardPanel",
            Self::UnionJoin => "JunTuanJoinPanel",
            Self::Chat => "ChatPanel",
            Self::Face => "FacePanel",
            Self::Talent => "TalentPanel1",
            Self::Blueprints => "BluePrintBuildPanel",
            Self::WeaponSkins => "WeaponsInstitutePanel",
            Self::Community => "CommunityEntrancePanel",
            Self::Vip => "VipPanel",
            Self::Magazine => "FashionMagazinePanel",
            Self::Privilege => "PrivilegePanel",
            Self::FirstPunch => "FirstPunchPanel",
            Self::LadderNotice => "LadderNoticePanel",
            Self::Achievement => "AchievementPanel",
            Self::Activity => "ActivityNewPanel",
            Self::DailyGift => "DailyGiftPanel",
            Self::LobbyMarket => "LobbyMarketPanel",
            Self::BlueDiamond => "TeQuanLanZuanPanel",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Shop => "Shop",
            Self::Store => "Storage",
            Self::Mail => "Mail",
            Self::Friends => "Friends",
            Self::Leaderboard => "Leaderboard",
            Self::UnionJoin => "Union",
            Self::Chat => "Chat",
            Self::Face => "Appearance",
            Self::Talent => "Talents",
            Self::Blueprints => "Blueprints",
            Self::WeaponSkins => "Weapon skins",
            Self::Community => "Community",
            Self::Vip => "VIP",
            Self::Magazine => "Fashion magazine",
            Self::Privilege => "Privileges",
            Self::FirstPunch => "First charge",
            Self::LadderNotice => "Ladder pass",
            Self::Achievement => "Achievements",
            Self::Activity => "Activities",
            Self::DailyGift => "Daily gift",
            Self::LobbyMarket => "Lobby market",
            Self::BlueDiamond => "Blue diamonds",
        }
    }
}

#[derive(Component)]
pub(super) struct NativeLobbyBindings {
    panel_name: String,
    actions: HashMap<Entity, LobbyAction>,
}

impl NativeLobbyBindings {
    fn new(bound: &BoundUiPanel) -> Self {
        Self {
            panel_name: bound.panel_name.clone(),
            actions: HashMap::new(),
        }
    }

    pub(super) fn action(&self, click: &BoundUiClick) -> Option<LobbyAction> {
        (click.panel_name == self.panel_name)
            .then(|| self.actions.get(&click.entity).copied())
            .flatten()
    }

    fn bind(&mut self, bound: &BoundUiPanel, entity: Entity, action: LobbyAction) {
        let Some(path) = entity_path(bound, entity) else {
            return;
        };
        let prefix = format!("{path}/");
        for (&target, _) in bound.by_path.iter().filter_map(|(candidate, target)| {
            (candidate == path || candidate.starts_with(&prefix)).then_some((target, candidate))
        }) {
            self.actions.insert(target, action);
        }
    }

    fn bind_root_slot(&mut self, bound: &BoundUiPanel, slot: usize, action: LobbyAction) {
        if let Some(entity) = bound.game_object_slot(slot) {
            self.bind(bound, entity, action);
        }
    }
}

fn entity_path(bound: &BoundUiPanel, entity: Entity) -> Option<&str> {
    bound
        .by_path
        .iter()
        .find_map(|(path, &target)| (target == entity).then_some(path.as_str()))
}

pub(super) fn bind_main_panel(bound: &BoundUiPanel, mut commands: Commands) {
    use NativeLobbyPanel as Panel;
    let mut bindings = NativeLobbyBindings::new(bound);
    // MainPanel.Awake 0x17F4C74 and onInit 0x17DD3D0; exact root GameObjects slots.
    for (slot, panel) in [
        (2, Panel::Shop),
        (4, Panel::Mail),
        (13, Panel::Community),
        (18, Panel::Chat),
        (40, Panel::Talent),
        (43, Panel::Friends),
        (44, Panel::UnionJoin),
        (47, Panel::Store),
        (53, Panel::Privilege),
        (76, Panel::Vip),
        (79, Panel::Blueprints),
        (85, Panel::Achievement),
        (88, Panel::Leaderboard),
        (90, Panel::WeaponSkins),
        (98, Panel::Face),
        (127, Panel::Magazine),
        (10, Panel::FirstPunch),
        (26, Panel::LadderNotice),
        (58, Panel::BlueDiamond),
        (87, Panel::LobbyMarket),
        (102, Panel::Activity),
        (144, Panel::DailyGift),
        (146, Panel::Activity),
    ] {
        bindings.bind_root_slot(bound, slot, LobbyAction::OpenNativePanel(panel));
    }
    bindings.bind_root_slot(bound, 3, LobbyAction::SwitchTab(LobbyTab::Settings));
    bindings.bind_root_slot(bound, 30, LobbyAction::OpenWarChoice(0));
    bindings.bind_root_slot(bound, 28, LobbyAction::OpenWarChoice(0));
    bindings.bind_root_slot(bound, 133, LobbyAction::OpenWarChoice(1));
    bindings.bind_root_slot(
        bound,
        130,
        LobbyAction::NativeServiceUnavailable("Customer support"),
    );
    // Currency controls open native acquisition flows; they never grant local rewards.
    for (slot, service) in [
        (7, "Coupon acquisition"),
        (9, "Gold acquisition"),
        (39, "Diamond acquisition"),
        (112, "Vehicle access"),
        (68, "Competition access"),
        (81, "Firepower access"),
        (122, "Community server access"),
        (21, "Player profile"),
        (84, "Blueprint acquisition"),
    ] {
        bindings.bind_root_slot(bound, slot, LobbyAction::NativeServiceUnavailable(service));
    }
    if let Some(root) = bound.by_canvas_path("") {
        commands.entity(root).insert(bindings);
    }
}

// SettingPanel.SetPage 0x19C7494: (SettingPage enum, root page slot, root radio slot).
const SETTING_PAGES: [(u8, usize, usize); 7] = [
    (1, 5, 6),
    (2, 12, 13),
    (5, 8, 7),
    (6, 11, 10),
    (7, 16, 15),
    (8, 18, 17),
    (9, 32, 29),
];

#[derive(Component)]
pub(super) struct NativeSettingsPanel {
    bound: BoundUiPanel,
    sliders: Vec<Entity>,
    choices: Vec<NativeSettingChoice>,
    keys: Vec<native_settings_input::NativeKeyRow>,
}

struct NativeSettingChoice {
    key: &'static str,
    value: f32,
    on: Option<Entity>,
    off: Option<Entity>,
}

fn bind_setting_choices(
    bound: &BoundUiPanel,
    doc: &UiPanelDoc,
    path_id: i64,
    choices: &[(usize, &'static str, f32)],
    bindings: &mut NativeLobbyBindings,
) -> Vec<NativeSettingChoice> {
    let Some(controller) = controller_ref(doc, path_id) else {
        return Vec::new();
    };
    choices
        .iter()
        .filter_map(|&(slot, key, value)| {
            let entity = bound.controller_slot(controller, "GameObjects", slot)?;
            bindings.bind(bound, entity, LobbyAction::SetNativeSetting(key, value));
            Some(NativeSettingChoice {
                key,
                value,
                on: child_entity(bound, entity, "Background/1/Checkmark")
                    .or_else(|| child_entity(bound, entity, "On")),
                off: child_entity(bound, entity, "Off"),
            })
        })
        .collect()
}

const PIC_CONTROLLER: i64 = -5_573_862_520_862_777_783;
const BASIC_CONTROLLER: i64 = -3_157_394_200_045_618_298;

// SettingGPicPage.OnInit/refresh methods: native SettingMgr keys and chosen values.
const GRAPHICS_CHOICES: [(usize, &str, f32); 32] = [
    (22, "QualityLevel", -1.0),
    (10, "QualityLevel", 0.0),
    (11, "QualityLevel", 1.0),
    (4, "QualityLevel", 2.0),
    (20, "QualityLevel", 4.0),
    (1, "FrameRateQuality", 0.0),
    (2, "FrameRateQuality", 1.0),
    (3, "FrameRateQuality", 2.0),
    (30, "FrameRateQuality", 3.0),
    (5, "RenderQuality", 1.0),
    (6, "RenderQuality", 2.0),
    (7, "RenderQuality", 3.0),
    (8, "RenderQuality", 4.0),
    (9, "RenderQuality", 5.0),
    (15, "ShadowQuality", 1.0),
    (16, "ShadowQuality", 2.0),
    (17, "ShadowQuality", 3.0),
    (18, "ShadowQuality", 4.0),
    (19, "ShadowQuality", 5.0),
    (12, "ResolutionKey", 0.0),
    (13, "ResolutionKey", 1.0),
    (14, "ResolutionKey", 2.0),
    (23, "ModelCutKey", 1.0),
    (24, "ModelCutKey", 0.0),
    (25, "ModelCutKey", 2.0),
    (26, "PasswordLockKey", 0.0),
    (27, "PasswordLockKey", 1.0),
    (28, "MainBottomShowMode", 0.0),
    (29, "MainBottomShowMode", 1.0),
    (31, "CameraSMAAKey", 0.0),
    (32, "CameraSMAAKey", 1.0),
    (0, "ResetGraphics", 0.0),
];

const BASIC_CHOICES: [(usize, &str, f32); 15] = [
    (2, "LeftShootMode", 0.0),
    (3, "LeftShootMode", 1.0),
    (4, "LeftShootMode", 2.0),
    (12, "HelpAim", 1.0),
    (11, "HelpAim", 0.0),
    (10, "HurtFrontKey", 1.0),
    (9, "HurtFrontKey", 0.0),
    (14, "OpenNameKey", 1.0),
    (13, "OpenNameKey", 0.0),
    (24, "GyroMode", 1.0),
    (25, "GyroMode", 0.0),
    (26, "GyroMode", 2.0),
    (36, "OptionsKaijingMode", 0.0),
    (37, "OptionsKaijingMode", 2.0),
    (38, "OptionsKaijingMode", 1.0),
];

pub(super) fn setting_value(profile: &PlayerProfileSave, key: &str) -> f32 {
    match key {
        "music_vol" => profile.music_volume,
        "sfx_vol" => profile.sfx_volume,
        "FrameRateQuality" => f32::from(profile.fps_tier),
        "QualityLevel" => profile
            .native_settings
            .get(key)
            .copied()
            .unwrap_or(f32::from(profile.quality_tier)),
        _ => profile
            .native_settings
            .get(key)
            .copied()
            .unwrap_or(match key {
                "ShadowQuality" | "RenderQuality" => 3.0,
                "LeftShootMode" | "OptionsKaijingMode" => 2.0,
                "micValume" | "speakerVlume" => 1.0,
                _ => 0.0,
            }),
    }
}

pub(super) fn set_native_setting(profile: &mut PlayerProfileSave, key: &str, value: f32) {
    match key {
        "ResetGraphics" => {
            for (_, key, _) in GRAPHICS_CHOICES {
                profile.native_settings.remove(key);
            }
            let defaults = PlayerProfileSave::default();
            profile.fps_tier = defaults.fps_tier;
            profile.quality_tier = defaults.quality_tier;
        }
        "FrameRateQuality" => {
            profile.fps_tier = value.clamp(0.0, 3.0) as u8;
            profile.native_settings.insert("QualityLevel".into(), 4.0);
        }
        "QualityLevel" => {
            profile.quality_tier = value.clamp(0.0, 2.0) as u8;
            profile.native_settings.insert(key.into(), value);
        }
        "GyroMode" => {
            profile
                .native_settings
                .insert("GyroClose".into(), f32::from(u8::from(value == 0.0)));
            profile
                .native_settings
                .insert("GyroOpen".into(), f32::from(u8::from(value == 1.0)));
            profile
                .native_settings
                .insert("GyroOpenJing".into(), f32::from(u8::from(value == 2.0)));
            profile.native_settings.insert(key.into(), value);
        }
        _ => {
            if ["ShadowQuality", "RenderQuality", "ResolutionKey"].contains(&key) {
                profile.native_settings.insert("QualityLevel".into(), 4.0);
            }
            profile.native_settings.insert(key.into(), value);
        }
    }
}

fn controller_ref(doc: &UiPanelDoc, path_id: i64) -> Option<UiAssetRefDto> {
    doc.script_bindings.iter().find_map(|binding| {
        binding
            .owner_component_ref
            .filter(|reference| reference.path_id == path_id)
    })
}

fn child_entity(bound: &BoundUiPanel, entity: Entity, child: &str) -> Option<Entity> {
    let path = entity_path(bound, entity)?;
    bound.by_canvas_path(&format!("{path}/{child}"))
}

fn set_active(commands: &mut Commands, entity: Entity, active: bool) {
    commands.entity(entity).insert(if active {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
    commands
        .entity(entity)
        .entry::<Node>()
        .and_modify(move |mut node| {
            node.display = if active { Display::Flex } else { Display::None };
        });
}

fn set_doc_active(node: &mut UiPanelNodeDto, reference: UiAssetRefDto, active: bool) -> bool {
    if node.go_ref == Some(reference) {
        node.active_self = active;
        return true;
    }
    node.children
        .iter_mut()
        .any(|child| set_doc_active(child, reference, active))
}

fn root_slot_ref(doc: &UiPanelDoc, slot: usize) -> Option<UiAssetRefDto> {
    doc.script_bindings.iter().find_map(|binding| {
        (binding.owner_path.as_deref() == Some("")
            && binding.field_name == "GameObjects"
            && binding.slot_index == Some(slot))
        .then_some(binding.target_go_ref)
        .flatten()
    })
}

pub(super) fn spawn_native_settings(
    root: &mut ChildSpawnerCommands,
    ui_state: &MainLobbyUiState,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    let Ok(mut doc) = load_ui_panel_doc("ui_panels/SettingPanel.json") else {
        return;
    };
    let page = if SETTING_PAGES
        .iter()
        .any(|&(page, _, _)| page == ui_state.settings_subtab)
    {
        ui_state.settings_subtab
    } else {
        1
    };
    for (_, page_slot, _) in SETTING_PAGES {
        if let Some(reference) = root_slot_ref(&doc, page_slot) {
            set_doc_active(&mut doc.root, reference, true);
        }
    }
    let fit_scale = compute_panel_canvas_metrics(&doc, win_size, true).scale;
    let opts = UiRetainedOptions {
        fullscreen: true,
        viewport_size: Some(win_size),
        fit_scale,
        localize_text: true,
        preview_mode: false,
        ..default()
    };
    let bound =
        spawn_retained_panel_into(root, &doc, &ctx.asset_server, Some(&ctx.l10n), font, &opts);
    let mut bindings = NativeLobbyBindings::new(&bound);
    bindings.bind_root_slot(&bound, 0, LobbyAction::SwitchTab(LobbyTab::Overview));
    for (id, _, button_slot) in SETTING_PAGES {
        bindings.bind_root_slot(&bound, button_slot, LobbyAction::SetSettingsSubtab(id));
        if let Some(button) = bound.game_object_slot(button_slot) {
            for (child, active) in [("On", page == id), ("Off", page != id)] {
                if let Some(entity) = child_entity(&bound, button, child) {
                    set_active(&mut root.commands(), entity, active);
                }
            }
        }
    }
    for (id, page_slot, _) in SETTING_PAGES {
        if let Some(entity) = bound.game_object_slot(page_slot) {
            set_active(&mut root.commands(), entity, id == page);
        }
    }
    let mut sliders = Vec::new();
    for (path_id, tag, value, maximum) in [
        (
            4_541_547_874_680_693_843,
            "music_vol",
            ctx.profile.music_volume,
            1.0,
        ),
        (
            -6_852_542_401_015_897_005,
            "sfx_vol",
            ctx.profile.sfx_volume,
            1.0,
        ),
        (
            -499_810_559_927_316_136,
            "micValume",
            setting_value(&ctx.profile, "micValume"),
            2.0,
        ),
        (
            -5_184_980_954_268_416_437,
            "speakerVlume",
            setting_value(&ctx.profile, "speakerVlume"),
            2.0,
        ),
    ] {
        let Some(controller) = controller_ref(&doc, path_id) else {
            continue;
        };
        let Some(slider) = bound.controller_slot(controller, "GameObjects", 2) else {
            continue;
        };
        let fill = child_entity(&bound, slider, "Fill Area/Fill");
        let handle = child_entity(&bound, slider, "Handle Slide Area/Handle");
        let label = bound
            .controller_slot(controller, "GameObjects", 3)
            .map(|label| bound.node_text_child.get(&label).copied().unwrap_or(label));
        let mut commands = root.commands();
        let mut entity_commands = commands.entity(slider);
        entity_commands.insert(UiSlider {
            tag: if ["music_vol", "sfx_vol"].contains(&tag) {
                tag.to_owned()
            } else {
                format!("native_setting:{tag}")
            },
            min: 0.0,
            max: maximum,
            value: value.clamp(0.0, maximum),
            fill_entity: fill,
            handle_entity: handle,
            label_entity: label,
        });
        attach_slider_observers(&mut entity_commands);
        for entity in [fill, handle].into_iter().flatten() {
            commands.entity(entity).insert(Pickable::IGNORE);
        }
        for (slot, step) in [(0, -1), (1, 1)] {
            if let Some(entity) = bound.controller_slot(controller, "GameObjects", slot) {
                bindings.bind(&bound, entity, LobbyAction::AdjustNativeSlider(tag, step));
            }
        }
        sliders.push(slider);
    }
    let mut choices = bind_setting_choices(
        &bound,
        &doc,
        PIC_CONTROLLER,
        &GRAPHICS_CHOICES,
        &mut bindings,
    );
    choices.extend(bind_setting_choices(
        &bound,
        &doc,
        BASIC_CONTROLLER,
        &BASIC_CHOICES,
        &mut bindings,
    ));
    if let Some(controller) = controller_ref(&doc, BASIC_CONTROLLER) {
        for (slot, action) in [
            (0, LobbyAction::CycleLanguage),
            (21, LobbyAction::BackToLogin),
            (17, LobbyAction::NativeServiceUnavailable("Player reports")),
            (
                18,
                LobbyAction::NativeServiceUnavailable("Customer support"),
            ),
            (28, LobbyAction::NativeServiceUnavailable("QR login")),
            (
                35,
                LobbyAction::NativeServiceUnavailable("Account deletion"),
            ),
        ] {
            if let Some(entity) = bound.controller_slot(controller, "GameObjects", slot) {
                bindings.bind(&bound, entity, action);
            }
        }
        // Native basic settings hide battle-only HUD statistics and suicide outside a battle.
        if let Some(entity) = bound.controller_slot(controller, "GameObjects", 8) {
            set_active(&mut root.commands(), entity, false);
        }
    }
    if let Some(root_entity) = bound.by_canvas_path("") {
        let keys =
            native_settings_input::bind_native_keys(&bound, &doc, &ctx.configs, &mut bindings);
        root.commands().entity(root_entity).insert((
            bindings,
            NativeSettingsPanel {
                bound,
                sliders,
                choices,
                keys,
            },
        ));
    }
}

pub(super) fn sync_native_settings(
    profile: Res<PlayerProfileSave>,
    ui_state: Res<MainLobbyUiState>,
    l10n: Res<LocalizationDb>,
    panels: Query<&NativeSettingsPanel>,
    mut sliders: Query<&mut UiSlider>,
    mut visibility: Query<&mut Visibility>,
    mut nodes: Query<&mut Node>,
    mut texts: Query<&mut Text>,
) {
    for panel in &panels {
        for (id, page_slot, button_slot) in SETTING_PAGES {
            let selected = id == ui_state.settings_subtab.max(1);
            if let Some(entity) = panel.bound.game_object_slot(page_slot) {
                BoundUiPanel::set_entity_active(entity, selected, &mut visibility, &mut nodes);
            }
            if let Some(button) = panel.bound.game_object_slot(button_slot) {
                for (child, active) in [("On", selected), ("Off", !selected)] {
                    if let Some(entity) = child_entity(&panel.bound, button, child) {
                        BoundUiPanel::set_entity_active(
                            entity,
                            active,
                            &mut visibility,
                            &mut nodes,
                        );
                    }
                }
            }
        }
        for &entity in &panel.sliders {
            if let Ok(mut slider) = sliders.get_mut(entity) {
                slider.value =
                    setting_value(&profile, slider.tag.trim_start_matches("native_setting:"));
                sync_one_slider_visuals(&slider, &mut nodes, &mut texts);
                if let Some(fill) = slider.fill_entity
                    && let Ok(mut node) = nodes.get_mut(fill)
                {
                    node.left = Val::Px(0.0);
                    node.top = Val::Px(0.0);
                    node.height = Val::Percent(100.0);
                }
                if let Some(label) = slider.label_entity
                    && let Ok(mut text) = texts.get_mut(label)
                {
                    text.0 = format!("{}", (slider.value * 100.0) as i32);
                }
            }
        }
        for choice in &panel.choices {
            let selected = (setting_value(&profile, choice.key) - choice.value).abs() < 0.001;
            if let Some(entity) = choice.on {
                BoundUiPanel::set_entity_active(entity, selected, &mut visibility, &mut nodes);
            }
            if let Some(entity) = choice.off {
                BoundUiPanel::set_entity_active(entity, !selected, &mut visibility, &mut nodes);
            }
        }
        native_settings_input::sync_native_keys(
            &panel.bound,
            &panel.keys,
            &profile,
            &ui_state,
            &l10n,
            &mut texts,
            &mut visibility,
            &mut nodes,
        );
    }
}

pub(super) fn spawn_native_lobby_panel(
    root: &mut ChildSpawnerCommands,
    panel: NativeLobbyPanel,
    ui_state: &MainLobbyUiState,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    if panel == NativeLobbyPanel::Store {
        native_store::spawn_native_store(root, ui_state, ctx, win_size, font);
        return;
    }
    let Ok(doc) = load_ui_panel_doc(&format!("ui_panels/{}.json", panel.name())) else {
        return;
    };
    let opts = UiRetainedOptions {
        fullscreen: true,
        viewport_size: Some(win_size),
        fit_scale: compute_panel_canvas_metrics(&doc, win_size, true).scale,
        localize_text: true,
        preview_mode: false,
        ..default()
    };
    let bound =
        spawn_retained_panel_into(root, &doc, &ctx.asset_server, Some(&ctx.l10n), font, &opts);
    let mut bindings = NativeLobbyBindings::new(&bound);
    // Each destination keeps its original retained hierarchy. Controller-specific bindings
    // below are populated from recovered Awake/onInit methods, never shared name handlers.
    for binding in doc.script_bindings.iter().filter(|binding| {
        binding.owner_path.as_deref() == Some("")
            && ["btn_back", "btn_close", "Btn_Back", "Btn_Close"]
                .contains(&binding.target_name.as_str())
    }) {
        if let Some(reference) = binding.target_go_ref
            && let Some(&entity) = bound.by_go_ref.get(&reference)
        {
            bindings.bind(&bound, entity, LobbyAction::SwitchTab(LobbyTab::Overview));
        }
    }
    if let Some(entity) = bound.by_canvas_path("") {
        root.commands().entity(entity).insert(bindings);
    }
}
