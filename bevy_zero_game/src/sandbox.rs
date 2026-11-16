//! Pure-Bevy 0.20 Zero-Logic 3D Asset & UI Panel Sandbox (`AppState::DevSandbox`).
//!
//! Refactored from the original sandbox `main.rs` into a cleanly scoped state plugin supporting
//! Scene Explorer (`F1`), Battle Stage (`F2`), Model & Anim Studio (`F3`), UI Panels (`F4`),
//! and instant return to LoginLobby (`Esc` / `F12`).

use std::time::Duration;

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::system::SystemParam;
use bevy::gltf::Gltf;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource};
use bevy::ui_widgets::{Activate, Button, observe};

use crate::actor_rig::{
    AnimState, BoundActorRole, BoundAnimationSet, build_gltf_animation_set,
    find_hand_socket_entity, pick_clip_for_state,
};
use crate::config::{AssetManifest, GameConfigs, GlbAssetEntry, LocalizationDb};
use crate::states::{AppState, SandboxRoot, despawn_roots_with};
use crate::ui::panel_loader::{
    UiPaletteButton, UiPanelPreviewRoot, UiPanelPreviewSummary, UiPreviewInteractionState,
    spawn_ui_panel_preview,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    SceneExplorer,
    BattleStage,
    ModelStudio,
    UiPanels,
}

impl ViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::SceneExplorer => "1: Scene Explorer",
            Self::BattleStage => "2: Battle Stage",
            Self::ModelStudio => "3: Model & Anim Studio",
            Self::UiPanels => "4: UI Panels",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlMode {
    PlayThirdPerson,
    Orbit,
    FreeFly,
}

impl ControlMode {
    fn label(self) -> &'static str {
        match self {
            Self::PlayThirdPerson => "Play (WASD+Shift+Space+F)",
            Self::Orbit => "Orbit (Drag+MMB+Scroll)",
            Self::FreeFly => "FreeFly (WASDQE+Drag)",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::PlayThirdPerson => Self::Orbit,
            Self::Orbit => Self::FreeFly,
            Self::FreeFly => Self::PlayThirdPerson,
        }
    }
}

#[derive(Resource)]
struct ShowcaseState {
    mode: ViewMode,
    control_mode: ControlMode,
    scene_index: usize,
    model_index: usize,
    prop_index: usize,
    prop_attached: bool,
    clip_index: usize,
    ui_panel_index: usize,
    active_ui_panel_name: String,
    ui_panel_summary: UiPanelPreviewSummary,
    ui_runtime_view: bool,
    ui_localize: bool,
    audio_index: usize,
    anim_paused: bool,
    show_gizmos: bool,
    show_hud: bool,
    dirty_rebuild: bool,
    dirty_clip_switch: bool,
    dirty_prop_attach: bool,
    stage_gen: u64,
    font: Option<Handle<Font>>,
    emblem: Option<Handle<Image>>,
}

impl Default for ShowcaseState {
    fn default() -> Self {
        Self {
            mode: ViewMode::BattleStage,
            control_mode: ControlMode::PlayThirdPerson,
            scene_index: 0,
            model_index: 0,
            prop_index: 0,
            prop_attached: true,
            clip_index: 0,
            ui_panel_index: 0,
            active_ui_panel_name: String::new(),
            ui_panel_summary: UiPanelPreviewSummary::default(),
            ui_runtime_view: true,
            ui_localize: true,
            audio_index: 0,
            anim_paused: false,
            show_gizmos: true,
            show_hud: true,
            dirty_rebuild: true,
            dirty_clip_switch: false,
            dirty_prop_attach: false,
            stage_gen: 0,
            font: None,
            emblem: None,
        }
    }
}

#[derive(Component)]
struct StageContent;

#[derive(Component)]
struct StageActorGltf {
    label: String,
    gltf: Handle<Gltf>,
    phase_offset_secs: f32,
}

#[derive(Component)]
struct PlayerCharacter {
    stage_gen: u64,
    velocity: Vec2,
    vertical_vel: f32,
    grounded: bool,
    attack_timer: f32,
    anim_state: AnimState,
    last_applied_state: Option<AnimState>,
    manual_override: bool,
}

impl Default for PlayerCharacter {
    fn default() -> Self {
        Self {
            stage_gen: 0,
            velocity: Vec2::ZERO,
            vertical_vel: 0.0,
            grounded: true,
            attack_timer: 0.0,
            anim_state: AnimState::Idle,
            last_applied_state: None,
            manual_override: false,
        }
    }
}

#[derive(Component)]
struct PartnerCharacter;

#[derive(Component)]
struct AttachedProp;

#[derive(Component)]
struct PropSocketSynced;

#[derive(Component)]
struct OrbitRig {
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    radius: f32,
}

#[derive(Component)]
struct ShowcaseAudio;

#[derive(Component)]
struct HudRoot;

#[derive(Component)]
struct HudStatusText;

#[derive(Component, Clone, Copy)]
enum HudAction {
    SetMode(ViewMode),
    PrevScene,
    NextScene,
    PrevModel,
    NextModel,
    PrevUiPanel,
    NextUiPanel,
    ToggleUiRuntimeView,
    ToggleUiLocalize,
    ToggleAttachProp,
    NextAttachProp,
    TriggerAction,
    NextClip,
    TogglePause,
    ToggleControlMode,
    PlayNextAudio,
    ToggleGizmos,
    BackToLobby,
}

/// Registers the sandbox showcase cleanly under [`AppState::DevSandbox`].
pub struct SandboxPlugin;

impl Plugin for SandboxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShowcaseState>()
            .add_systems(OnEnter(AppState::DevSandbox), setup_stage_and_hud)
            .add_systems(OnExit(AppState::DevSandbox), exit_sandbox_cleanup)
            .add_systems(
                Update,
                (
                    handle_keyboard_shortcuts,
                    ApplyDeferred,
                    rebuild_stage_if_dirty,
                    ApplyDeferred,
                    bind_loaded_gltf_animations,
                    third_person_player_movement_system,
                    sync_attached_prop_socket_system,
                    update_animation_controls,
                    camera_controller_system,
                    draw_stage_gizmos,
                    update_hud_status_text,
                )
                    .chain()
                    .run_if(in_state(AppState::DevSandbox)),
            );
    }
}

fn exit_sandbox_cleanup(
    mut commands: Commands,
    sandbox_q: Query<Entity, (With<SandboxRoot>, Without<ChildOf>)>,
    audio_q: Query<Entity, With<ShowcaseAudio>>,
    mut state: ResMut<ShowcaseState>,
) {
    state.dirty_rebuild = true;
    for e in &audio_q {
        commands.entity(e).try_despawn();
    }
    despawn_roots_with::<SandboxRoot>(commands, sandbox_q);
}

fn combined_models(manifest: &AssetManifest) -> Vec<&GlbAssetEntry> {
    manifest
        .characters
        .iter()
        .chain(manifest.props.iter())
        .chain(manifest.build_parts.iter())
        .chain(manifest.world_props.iter())
        .chain(manifest.scenes.iter())
        .collect()
}

fn setup_stage_and_hud(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    manifest: Res<AssetManifest>,
    mut state: ResMut<ShowcaseState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    *state = ShowcaseState::default();
    if let Some(first_font) = manifest.fonts.first() {
        state.font = Some(asset_server.load(first_font.asset_path.clone()));
    }
    if let Some(first_ui) = manifest.ui_sprites.first() {
        state.emblem = Some(asset_server.load(first_ui.asset_path.clone()));
    }
    if manifest.characters.is_empty() && !manifest.scenes.is_empty() {
        state.mode = ViewMode::SceneExplorer;
        state.control_mode = ControlMode::Orbit;
    } else if manifest.scenes.is_empty() && manifest.characters.is_empty() && !manifest.props.is_empty()
    {
        state.mode = ViewMode::ModelStudio;
        state.control_mode = ControlMode::Orbit;
    }

    let initial_rig = OrbitRig {
        focus: Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.45,
        pitch: 0.35,
        radius: 9.0,
    };
    commands.spawn((
        SandboxRoot,
        Camera3d::default(),
        Tonemapping::TonyMcMapface,
        Transform::from_xyz(4.0, 4.5, 8.5).looking_at(initial_rig.focus, Vec3::Y),
        initial_rig,
    ));

    commands.spawn((
        SandboxRoot,
        DirectionalLight {
            illuminance: 14_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(12.0, 18.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        SandboxRoot,
        PointLight {
            color: Color::srgb(0.45, 0.70, 1.0),
            intensity: 350_000.0,
            range: 45.0,
            ..default()
        },
        Transform::from_xyz(-7.0, 5.0, -7.0),
    ));

    let floor_mesh = meshes.add(Cylinder::new(14.0, 0.25));
    let floor_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.14, 0.16, 0.20),
        perceptual_roughness: 0.78,
        metallic: 0.12,
        ..default()
    });
    commands.spawn((
        SandboxRoot,
        Name::new("StageFloor"),
        Mesh3d(floor_mesh),
        MeshMaterial3d(floor_mat),
        Transform::from_xyz(0.0, -0.13, 0.0),
    ));

    spawn_hud(&mut commands, &manifest, &state);
}

fn spawn_hud(commands: &mut Commands, manifest: &AssetManifest, state: &ShowcaseState) {
    let font_source = state
        .font
        .clone()
        .map(FontSource::from)
        .unwrap_or_else(FontSource::sans_serif);

    commands
        .spawn((
            SandboxRoot,
            HudRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    row_gap: Val::Px(6.0),
                    padding: UiRect::all(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.07, 0.09, 0.13, 0.88)),
                BorderColor::all(Color::srgba(0.32, 0.45, 0.65, 0.7)),
            ))
            .with_children(|bar| {
                if let Some(emblem) = &state.emblem {
                    bar.spawn((
                        ImageNode::new(emblem.clone()),
                        Node {
                            width: Val::Px(34.0),
                            height: Val::Px(34.0),
                            ..default()
                        },
                    ));
                }
                bar.spawn((
                    Text::new(format!("{} | Dev Sandbox", manifest.project_name)),
                    TextFont {
                        font: font_source.clone(),
                        font_size: FontSize::Px(15.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));

                spawn_hud_button(bar, "Lobby [Esc/F12]", HudAction::BackToLobby, &font_source);
                spawn_hud_button(bar, "Scene [F1]", HudAction::SetMode(ViewMode::SceneExplorer), &font_source);
                spawn_hud_button(bar, "Battle [F2]", HudAction::SetMode(ViewMode::BattleStage), &font_source);
                spawn_hud_button(bar, "Model [F3]", HudAction::SetMode(ViewMode::ModelStudio), &font_source);
                spawn_hud_button(bar, "UI Panels [F4]", HudAction::SetMode(ViewMode::UiPanels), &font_source);
                spawn_hud_button(bar, "< UI [U]", HudAction::PrevUiPanel, &font_source);
                spawn_hud_button(bar, "UI > [I]", HudAction::NextUiPanel, &font_source);
                spawn_hud_button(bar, "View [R]", HudAction::ToggleUiRuntimeView, &font_source);
                spawn_hud_button(bar, "L10n [T]", HudAction::ToggleUiLocalize, &font_source);
                spawn_hud_button(bar, "Mode [Tab/C]", HudAction::ToggleControlMode, &font_source);
                spawn_hud_button(bar, "< Scene", HudAction::PrevScene, &font_source);
                spawn_hud_button(bar, "Scene >", HudAction::NextScene, &font_source);
                spawn_hud_button(bar, "< Hero", HudAction::PrevModel, &font_source);
                spawn_hud_button(bar, "Hero >", HudAction::NextModel, &font_source);
                spawn_hud_button(bar, "Socket [V]", HudAction::ToggleAttachProp, &font_source);
                spawn_hud_button(bar, "Prop > [B]", HudAction::NextAttachProp, &font_source);
                spawn_hud_button(bar, "Action [F]", HudAction::TriggerAction, &font_source);
                spawn_hud_button(bar, "Anim [N]", HudAction::NextClip, &font_source);
                spawn_hud_button(bar, "Pause [P]", HudAction::TogglePause, &font_source);
                spawn_hud_button(bar, "SFX [M]", HudAction::PlayNextAudio, &font_source);
                spawn_hud_button(bar, "Grid [G]", HudAction::ToggleGizmos, &font_source);
            });

            root.spawn((
                Node {
                    padding: UiRect::all(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    align_self: AlignSelf::FlexStart,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.07, 0.09, 0.13, 0.85)),
                BorderColor::all(Color::srgba(0.32, 0.45, 0.65, 0.65)),
            ))
            .with_children(|panel| {
                panel.spawn((
                    HudStatusText,
                    Text::new("Loading assets..."),
                    TextFont {
                        font: font_source,
                        font_size: FontSize::Px(13.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.88, 0.93, 0.98)),
                ));
            });
        });
}

fn spawn_hud_button(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    action: HudAction,
    font: &FontSource,
) {
    let palette = UiPaletteButton::default();
    parent
        .spawn((
            Button,
            Hovered::default(),
            palette,
            action,
            Node {
                padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                border: UiRect::all(Val::Px(1.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(palette.normal),
            BorderColor::all(Color::srgb(0.35, 0.48, 0.68)),
            observe(
                |activate: On<Activate>,
                 actions: Query<&HudAction>,
                 mut state: ResMut<ShowcaseState>,
                 manifest: Res<AssetManifest>,
                 asset_server: Res<AssetServer>,
                 existing_audio: Query<Entity, With<ShowcaseAudio>>,
                 mut player_q: Query<&mut PlayerCharacter>,
                 mut next_state: ResMut<NextState<AppState>>,
                 mut commands: Commands| {
                    let Ok(&act) = actions.get(activate.entity) else {
                        return;
                    };
                    apply_hud_action(
                        act,
                        &mut state,
                        &manifest,
                        &asset_server,
                        &existing_audio,
                        &mut player_q,
                        &mut next_state,
                        &mut commands,
                    );
                },
            ),
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new(label.to_owned()),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn spawn_next_audio_sfx(
    state: &mut ShowcaseState,
    manifest: &AssetManifest,
    asset_server: &AssetServer,
    existing_audio: &Query<Entity, With<ShowcaseAudio>>,
    commands: &mut Commands,
) {
    if manifest.audio.is_empty() {
        return;
    }
    for e in existing_audio.iter() {
        commands.entity(e).try_despawn();
    }
    let track = &manifest.audio[state.audio_index % manifest.audio.len()];
    state.audio_index = (state.audio_index + 1) % manifest.audio.len();
    commands.spawn((
        SandboxRoot,
        ShowcaseAudio,
        AudioPlayer::new(asset_server.load(track.asset_path.clone())),
        PlaybackSettings::DESPAWN,
    ));
}

#[allow(clippy::too_many_arguments)]
fn apply_hud_action(
    action: HudAction,
    state: &mut ShowcaseState,
    manifest: &AssetManifest,
    asset_server: &AssetServer,
    existing_audio: &Query<Entity, With<ShowcaseAudio>>,
    player_q: &mut Query<&mut PlayerCharacter>,
    next_state: &mut ResMut<NextState<AppState>>,
    commands: &mut Commands,
) {
    let models = combined_models(manifest);
    match action {
        HudAction::BackToLobby => next_state.set_if_different(AppState::LoginLobby),
        HudAction::SetMode(m) => {
            state.mode = m;
            state.dirty_rebuild = true;
        }
        HudAction::PrevScene if !manifest.scenes.is_empty() => {
            state.scene_index =
                (state.scene_index + manifest.scenes.len() - 1) % manifest.scenes.len();
            state.dirty_rebuild = true;
        }
        HudAction::NextScene if !manifest.scenes.is_empty() => {
            state.scene_index = (state.scene_index + 1) % manifest.scenes.len();
            state.dirty_rebuild = true;
        }
        HudAction::PrevModel if !models.is_empty() => {
            state.model_index = (state.model_index + models.len() - 1) % models.len();
            state.clip_index = 0;
            state.dirty_rebuild = true;
        }
        HudAction::NextModel if !models.is_empty() => {
            state.model_index = (state.model_index + 1) % models.len();
            state.clip_index = 0;
            state.dirty_rebuild = true;
        }
        HudAction::PrevUiPanel if !manifest.ui_panels.is_empty() => {
            state.ui_panel_index =
                (state.ui_panel_index + manifest.ui_panels.len() - 1) % manifest.ui_panels.len();
            state.mode = ViewMode::UiPanels;
            state.dirty_rebuild = true;
        }
        HudAction::NextUiPanel if !manifest.ui_panels.is_empty() => {
            state.ui_panel_index = (state.ui_panel_index + 1) % manifest.ui_panels.len();
            state.mode = ViewMode::UiPanels;
            state.dirty_rebuild = true;
        }
        HudAction::ToggleUiRuntimeView => {
            state.ui_runtime_view = !state.ui_runtime_view;
            state.mode = ViewMode::UiPanels;
            state.dirty_rebuild = true;
        }
        HudAction::ToggleUiLocalize => {
            state.ui_localize = !state.ui_localize;
            state.mode = ViewMode::UiPanels;
            state.dirty_rebuild = true;
        }
        HudAction::ToggleAttachProp => {
            state.prop_attached = !state.prop_attached;
            state.dirty_prop_attach = true;
        }
        HudAction::NextAttachProp if !manifest.props.is_empty() => {
            state.prop_index = (state.prop_index + 1) % manifest.props.len();
            state.prop_attached = true;
            state.dirty_prop_attach = true;
        }
        HudAction::TriggerAction => {
            for mut p in player_q.iter_mut() {
                p.attack_timer = 0.55;
                p.manual_override = false;
            }
            spawn_next_audio_sfx(state, manifest, asset_server, existing_audio, commands);
        }
        HudAction::NextClip => {
            state.clip_index = state.clip_index.wrapping_add(1);
            state.dirty_clip_switch = true;
            for mut p in player_q.iter_mut() {
                p.manual_override = true;
            }
        }
        HudAction::TogglePause => {
            state.anim_paused = !state.anim_paused;
            state.dirty_clip_switch = true;
        }
        HudAction::ToggleControlMode => {
            state.control_mode = state.control_mode.next();
        }
        HudAction::PlayNextAudio => {
            spawn_next_audio_sfx(state, manifest, asset_server, existing_audio, commands);
        }
        HudAction::ToggleGizmos => state.show_gizmos = !state.show_gizmos,
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_keyboard_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<ShowcaseState>,
    manifest: Res<AssetManifest>,
    asset_server: Res<AssetServer>,
    existing_audio: Query<Entity, With<ShowcaseAudio>>,
    mut player_q: Query<&mut PlayerCharacter>,
    mut hud_vis: Query<&mut Visibility, With<HudRoot>>,
    mut next_state: ResMut<NextState<AppState>>,
    mut commands: Commands,
) {
    if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::F12) {
        apply_hud_action(HudAction::BackToLobby, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
        return;
    }
    if keys.just_pressed(KeyCode::F1) {
        apply_hud_action(HudAction::SetMode(ViewMode::SceneExplorer), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::F2) {
        apply_hud_action(HudAction::SetMode(ViewMode::BattleStage), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::F3) {
        apply_hud_action(HudAction::SetMode(ViewMode::ModelStudio), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::F4) {
        apply_hud_action(HudAction::SetMode(ViewMode::UiPanels), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyU) {
        apply_hud_action(HudAction::PrevUiPanel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::KeyI) {
        apply_hud_action(HudAction::NextUiPanel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::KeyR) {
        apply_hud_action(HudAction::ToggleUiRuntimeView, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::KeyT) {
        apply_hud_action(HudAction::ToggleUiLocalize, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        apply_hud_action(HudAction::PrevScene, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::BracketRight) {
        apply_hud_action(HudAction::NextScene, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::Comma) {
        apply_hud_action(HudAction::PrevModel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    } else if keys.just_pressed(KeyCode::Period) {
        apply_hud_action(HudAction::NextModel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyV) {
        apply_hud_action(HudAction::ToggleAttachProp, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyB) {
        apply_hud_action(HudAction::NextAttachProp, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyF) {
        apply_hud_action(HudAction::TriggerAction, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyN) {
        apply_hud_action(HudAction::NextClip, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyP) {
        apply_hud_action(HudAction::TogglePause, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyC) || keys.just_pressed(KeyCode::Tab) {
        apply_hud_action(HudAction::ToggleControlMode, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyM) {
        apply_hud_action(HudAction::PlayNextAudio, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyG) {
        apply_hud_action(HudAction::ToggleGizmos, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut next_state, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyH) {
        state.show_hud = !state.show_hud;
        for mut v in &mut hud_vis {
            *v = if state.show_hud { Visibility::Visible } else { Visibility::Hidden };
        }
    }
}

#[derive(SystemParam)]
struct StageRebuildQueries<'w, 's> {
    existing_stage: Query<'w, 's, Entity, (With<StageContent>, Without<ChildOf>)>,
    existing_ui_panels: Query<'w, 's, Entity, (With<UiPanelPreviewRoot>, Without<ChildOf>)>,
    windows: Query<'w, 's, &'static Window>,
    orbit_q: Query<'w, 's, &'static mut OrbitRig>,
}

fn rebuild_stage_if_dirty(
    mut commands: Commands,
    mut state: ResMut<ShowcaseState>,
    manifest: Res<AssetManifest>,
    l10n: Res<LocalizationDb>,
    configs: Res<GameConfigs>,
    asset_server: Res<AssetServer>,
    mut q: StageRebuildQueries,
) {
    if !state.dirty_rebuild {
        return;
    }
    state.dirty_rebuild = false;
    state.stage_gen = state.stage_gen.wrapping_add(1);
    state.dirty_prop_attach = true;

    for e in q.existing_stage.iter() {
        commands.entity(e).try_despawn();
    }
    for e in q.existing_ui_panels.iter() {
        commands.entity(e).try_despawn();
    }

    let spawn_glb = |commands: &mut Commands,
                     asset_server: &AssetServer,
                     entry: &GlbAssetEntry,
                     transform: Transform,
                     phase: f32|
     -> Entity {
        let scene_handle: Handle<WorldAsset> =
            asset_server.load(format!("{}#Scene0", entry.asset_path));
        let gltf_handle: Handle<Gltf> = asset_server.load(entry.asset_path.clone());
        commands
            .spawn((
                SandboxRoot,
                StageContent,
                Name::new(entry.name.clone()),
                WorldAssetRoot(scene_handle),
                StageActorGltf {
                    label: entry.name.clone(),
                    gltf: gltf_handle,
                    phase_offset_secs: phase,
                },
                transform,
            ))
            .id()
    };

    match state.mode {
        ViewMode::SceneExplorer => {
            if let Some(scene) = manifest.scenes.get(state.scene_index % manifest.scenes.len().max(1))
            {
                spawn_glb(&mut commands, &asset_server, scene, Transform::IDENTITY, 0.0);
            }
            if let Ok(mut rig) = q.orbit_q.single_mut() {
                rig.focus = Vec3::new(0.0, 2.0, 0.0);
                rig.radius = 18.0;
            }
        }
        ViewMode::BattleStage => {
            if !manifest.scenes.is_empty() {
                let scene = &manifest.scenes[state.scene_index % manifest.scenes.len()];
                spawn_glb(
                    &mut commands,
                    &asset_server,
                    scene,
                    Transform::from_xyz(0.0, 0.0, -8.0),
                    0.0,
                );
            }
            if !manifest.characters.is_empty() {
                let count = manifest.characters.len();
                let hero = &manifest.characters[state.model_index % count];
                let hero_entity = spawn_glb(
                    &mut commands,
                    &asset_server,
                    hero,
                    Transform::from_xyz(-2.2, 0.0, 0.0)
                        .looking_at(Vec3::new(2.2, 0.0, 0.0), Vec3::Y),
                    0.0,
                );
                commands.entity(hero_entity).insert(PlayerCharacter {
                    stage_gen: state.stage_gen,
                    ..default()
                });

                if count > 1 {
                    let partner = &manifest.characters[(state.model_index + 1) % count];
                    let partner_entity = spawn_glb(
                        &mut commands,
                        &asset_server,
                        partner,
                        Transform::from_xyz(2.2, 0.0, 0.0)
                            .looking_at(Vec3::new(-2.2, 0.0, 0.0), Vec3::Y),
                        0.35,
                    );
                    commands.entity(partner_entity).insert(PartnerCharacter);
                }
            }
            for (i, prop) in manifest.props.iter().take(4).enumerate() {
                let x = -3.0 + (i as f32) * 2.0;
                spawn_glb(
                    &mut commands,
                    &asset_server,
                    prop,
                    Transform::from_xyz(x, 0.4, 3.2),
                    0.0,
                );
            }
            if let Ok(mut rig) = q.orbit_q.single_mut() {
                rig.focus = Vec3::new(-2.2, 1.15, 0.0);
                rig.radius = 6.2;
            }
        }
        ViewMode::ModelStudio => {
            let models = combined_models(&manifest);
            if !models.is_empty() {
                let entry = models[state.model_index % models.len()];
                let e = spawn_glb(&mut commands, &asset_server, entry, Transform::IDENTITY, 0.0);
                if entry.skin_count > 0 {
                    commands.entity(e).insert(PlayerCharacter {
                        stage_gen: state.stage_gen,
                        ..default()
                    });
                }
            }
            if let Ok(mut rig) = q.orbit_q.single_mut() {
                rig.focus = Vec3::new(0.0, 1.0, 0.0);
                rig.radius = 4.5;
            }
        }
        ViewMode::UiPanels => {
            let win_size = q
                .windows
                .iter()
                .next()
                .map(|w| Vec2::new(w.width(), w.height()))
                .unwrap_or(Vec2::new(1440.0, 900.0));
            let font_source = state
                .font
                .clone()
                .map(FontSource::from)
                .unwrap_or_else(FontSource::sans_serif);
            let summary = spawn_ui_panel_preview(
                &mut commands,
                &manifest,
                state.ui_panel_index,
                &asset_server,
                Some(&l10n),
                Some(&configs),
                &font_source,
                win_size,
                state.ui_runtime_view,
                state.ui_localize,
                SandboxRoot,
            );
            state.active_ui_panel_name = summary.name.clone();
            state.ui_panel_summary = summary;
        }
    }
}

fn bind_loaded_gltf_animations(
    mut commands: Commands,
    parents: Query<&ChildOf>,
    actors: Query<(&StageActorGltf, Has<PlayerCharacter>)>,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut players: Query<(Entity, &mut AnimationPlayer), Without<AnimationGraphHandle>>,
    state: Res<ShowcaseState>,
) {
    for (player_entity, mut player) in &mut players {
        let mut cursor = player_entity;
        let mut found_actor = None;
        for _ in 0..16 {
            if let Ok(actor_info) = actors.get(cursor) {
                found_actor = Some(actor_info);
                break;
            }
            if let Ok(parent) = parents.get(cursor) {
                cursor = parent.parent();
            } else {
                break;
            }
        }
        let Some((actor, is_player)) = found_actor else {
            continue;
        };
        let Some(gltf) = gltfs.get(&actor.gltf) else {
            continue;
        };
        let role = if is_player {
            BoundActorRole::Player
        } else {
            BoundActorRole::Other
        };
        if let Some(bundle) = build_gltf_animation_set(
            gltf,
            &actor.label,
            role,
            AnimState::Idle,
            state.clip_index,
            actor.phase_offset_secs,
            state.anim_paused,
            &mut player,
            &mut graphs,
        ) {
            commands.entity(player_entity).try_insert(bundle);
        }
    }
}

fn third_person_player_movement_system(
    time: Res<Time>,
    state: Res<ShowcaseState>,
    keys: Res<ButtonInput<KeyCode>>,
    cam_q: Query<&Transform, (With<Camera3d>, Without<PlayerCharacter>)>,
    mut player_q: Query<(&mut Transform, &mut PlayerCharacter)>,
) {
    if state.control_mode != ControlMode::PlayThirdPerson {
        return;
    }
    let Ok((mut tf, mut player)) = player_q.single_mut() else {
        return;
    };
    let dt = time.delta_secs();

    let (cam_fwd, cam_right) = if let Ok(cam_tf) = cam_q.single() {
        (
            Vec3::new(cam_tf.forward().x, 0.0, cam_tf.forward().z).normalize_or_zero(),
            Vec3::new(cam_tf.right().x, 0.0, cam_tf.right().z).normalize_or_zero(),
        )
    } else {
        (Vec3::NEG_Z, Vec3::X)
    };

    let mut input_dir = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        input_dir += cam_fwd;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        input_dir -= cam_fwd;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        input_dir += cam_right;
    }
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        input_dir -= cam_right;
    }

    let is_sprinting = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let move_speed = if is_sprinting { 6.8 } else { 3.5 };
    let wish = input_dir.normalize_or_zero() * move_speed;
    player.velocity = Vec2::new(wish.x, wish.z);

    if wish.length_squared() > 0.01 {
        player.manual_override = false;
        tf.translation.x += wish.x * dt;
        tf.translation.z += wish.z * dt;

        let target_rot = Transform::from_translation(tf.translation)
            .looking_at(tf.translation + wish.normalize_or_zero(), Vec3::Y)
            .rotation;
        let turn_alpha = (12.0 * dt).min(1.0);
        tf.rotation = tf.rotation.slerp(target_rot, turn_alpha);
    }

    let horiz = Vec2::new(tf.translation.x, tf.translation.z);
    if horiz.length() > 13.0 {
        let clamped = horiz.normalize() * 13.0;
        tf.translation.x = clamped.x;
        tf.translation.z = clamped.y;
    }

    if keys.just_pressed(KeyCode::Space) && player.grounded {
        player.vertical_vel = 6.2;
        player.grounded = false;
        player.manual_override = false;
    }
    if !player.grounded || tf.translation.y > 0.0 {
        player.vertical_vel -= 18.0 * dt;
        tf.translation.y += player.vertical_vel * dt;
        if tf.translation.y <= 0.0 {
            tf.translation.y = 0.0;
            player.vertical_vel = 0.0;
            player.grounded = true;
        }
    }

    player.attack_timer = (player.attack_timer - dt).max(0.0);
    player.anim_state = if player.attack_timer > 0.0 {
        AnimState::Attack
    } else if !player.grounded {
        AnimState::Jump
    } else if player.velocity.length() > 5.0 {
        AnimState::Run
    } else if player.velocity.length() > 0.25 {
        AnimState::Walk
    } else {
        AnimState::Idle
    };
}

#[allow(clippy::too_many_arguments)]
fn sync_attached_prop_socket_system(
    mut commands: Commands,
    mut state: ResMut<ShowcaseState>,
    manifest: Res<AssetManifest>,
    asset_server: Res<AssetServer>,
    existing_attached: Query<Entity, With<AttachedProp>>,
    player_q: Query<(Entity, &PlayerCharacter, Option<&Children>, Has<PropSocketSynced>)>,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
) {
    let Ok((player_entity, player, root_children, synced)) = player_q.single() else {
        return;
    };
    if player.stage_gen != state.stage_gen {
        return;
    }
    if !state.dirty_prop_attach && synced {
        return;
    }
    let Some(children) = root_children else {
        return;
    };
    if children.is_empty() {
        return;
    }
    state.dirty_prop_attach = false;

    for e in existing_attached.iter() {
        commands.entity(e).try_despawn();
    }

    if state.prop_attached && !manifest.props.is_empty() {
        let prop = &manifest.props[state.prop_index % manifest.props.len()];
        let scene_handle: Handle<WorldAsset> =
            asset_server.load(format!("{}#Scene0", prop.asset_path));
        let (mount_target, local_tf) =
            if let Some(socket) = find_hand_socket_entity(player_entity, &children_q, &names_q) {
                (socket, Transform::IDENTITY)
            } else {
                (player_entity, Transform::from_xyz(0.32, 0.95, 0.25))
            };
        commands.spawn((
            SandboxRoot,
            AttachedProp,
            Name::new(format!("Attached_{}", prop.name)),
            WorldAssetRoot(scene_handle),
            ChildOf(mount_target),
            local_tf,
        ));
    }

    if !synced {
        commands.entity(player_entity).try_insert(PropSocketSynced);
    }
}

fn update_animation_controls(
    mut state: ResMut<ShowcaseState>,
    mut player_char_q: Query<&mut PlayerCharacter>,
    mut q: Query<(
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &BoundAnimationSet,
    )>,
) {
    let manual_switch = state.dirty_clip_switch;
    state.dirty_clip_switch = false;

    let player_state = player_char_q.single_mut().ok();
    let (cur_anim_state, last_applied, manual_override) = if let Some(ref p) = player_state {
        (p.anim_state, p.last_applied_state, p.manual_override)
    } else {
        (AnimState::Idle, None, true)
    };

    let auto_state_changed = state.control_mode == ControlMode::PlayThirdPerson
        && !manual_override
        && last_applied != Some(cur_anim_state);

    let mut applied_now = false;
    for (mut anim_player, mut transitions, set) in &mut q {
        if set.nodes.is_empty() {
            continue;
        }
        if manual_switch {
            let (_, node) = &set.nodes[state.clip_index % set.nodes.len()];
            let active = transitions
                .play(&mut anim_player, *node, Duration::from_millis(180))
                .repeat();
            if state.anim_paused {
                active.pause();
            } else {
                active.resume();
            }
        } else if auto_state_changed
            && set.role == BoundActorRole::Player
            && let Some((_, node)) = pick_clip_for_state(cur_anim_state, &set.nodes)
        {
            let active = transitions
                .play(&mut anim_player, node, Duration::from_millis(160))
                .repeat();
            if state.anim_paused {
                active.pause();
            } else {
                active.resume();
            }
            applied_now = true;
        }
    }

    if applied_now && let Some(mut p) = player_state {
        p.last_applied_state = Some(cur_anim_state);
    }
}

#[allow(clippy::too_many_arguments)]
fn camera_controller_system(
    time: Res<Time>,
    state: Res<ShowcaseState>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse_btn: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    player_q: Query<&Transform, (With<PlayerCharacter>, Without<Camera3d>)>,
    mut cam_q: Query<(&mut Transform, &mut OrbitRig), With<Camera3d>>,
) {
    if state.mode == ViewMode::UiPanels {
        return;
    }
    let Ok((mut tf, mut rig)) = cam_q.single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    let drag_delta = motion.delta;

    match state.control_mode {
        ControlMode::PlayThirdPerson => {
            if mouse_btn.pressed(MouseButton::Left) || mouse_btn.pressed(MouseButton::Right) {
                rig.yaw -= drag_delta.x * 0.007;
                rig.pitch = (rig.pitch + drag_delta.y * 0.007).clamp(-1.25, 1.35);
            }
            let scroll_y = match scroll.unit {
                MouseScrollUnit::Line => scroll.delta.y * 0.65,
                MouseScrollUnit::Pixel => scroll.delta.y * 0.015,
            };
            rig.radius = (rig.radius - scroll_y).clamp(1.5, 60.0);

            if let Ok(p_tf) = player_q.single() {
                let desired = p_tf.translation + Vec3::new(0.0, 1.15, 0.0);
                rig.focus = rig.focus.lerp(desired, (10.0 * dt).min(1.0));
            } else {
                let speed = 6.0 * dt;
                let fwd = Vec3::new(tf.forward().x, 0.0, tf.forward().z).normalize_or_zero();
                let right = Vec3::new(tf.right().x, 0.0, tf.right().z).normalize_or_zero();
                if keys.pressed(KeyCode::KeyW) {
                    rig.focus += fwd * speed;
                }
                if keys.pressed(KeyCode::KeyS) {
                    rig.focus -= fwd * speed;
                }
                if keys.pressed(KeyCode::KeyA) {
                    rig.focus -= right * speed;
                }
                if keys.pressed(KeyCode::KeyD) {
                    rig.focus += right * speed;
                }
            }

            let cos_p = rig.pitch.cos();
            let offset = Vec3::new(
                rig.radius * cos_p * rig.yaw.sin(),
                rig.radius * rig.pitch.sin(),
                rig.radius * cos_p * rig.yaw.cos(),
            );
            *tf = Transform::from_translation(rig.focus + offset).looking_at(rig.focus, Vec3::Y);
        }
        ControlMode::Orbit => {
            if mouse_btn.pressed(MouseButton::Left) || mouse_btn.pressed(MouseButton::Right) {
                rig.yaw -= drag_delta.x * 0.007;
                rig.pitch = (rig.pitch + drag_delta.y * 0.007).clamp(-1.35, 1.35);
            }
            if mouse_btn.pressed(MouseButton::Middle) {
                let right = tf.right();
                let up = tf.up();
                let pan_scale = rig.radius * 0.0015;
                rig.focus += (-right * drag_delta.x + up * drag_delta.y) * pan_scale;
            }
            let scroll_y = match scroll.unit {
                MouseScrollUnit::Line => scroll.delta.y * 0.65,
                MouseScrollUnit::Pixel => scroll.delta.y * 0.015,
            };
            rig.radius = (rig.radius - scroll_y).clamp(1.0, 90.0);

            let speed = 6.0 * dt;
            let fwd = Vec3::new(tf.forward().x, 0.0, tf.forward().z).normalize_or_zero();
            let right = Vec3::new(tf.right().x, 0.0, tf.right().z).normalize_or_zero();
            if keys.pressed(KeyCode::KeyW) {
                rig.focus += fwd * speed;
            }
            if keys.pressed(KeyCode::KeyS) {
                rig.focus -= fwd * speed;
            }
            if keys.pressed(KeyCode::KeyA) {
                rig.focus -= right * speed;
            }
            if keys.pressed(KeyCode::KeyD) {
                rig.focus += right * speed;
            }

            let cos_p = rig.pitch.cos();
            let offset = Vec3::new(
                rig.radius * cos_p * rig.yaw.sin(),
                rig.radius * rig.pitch.sin(),
                rig.radius * cos_p * rig.yaw.cos(),
            );
            *tf = Transform::from_translation(rig.focus + offset).looking_at(rig.focus, Vec3::Y);
        }
        ControlMode::FreeFly => {
            if mouse_btn.pressed(MouseButton::Right) || mouse_btn.pressed(MouseButton::Left) {
                rig.yaw -= drag_delta.x * 0.004;
                rig.pitch = (rig.pitch - drag_delta.y * 0.004).clamp(-1.45, 1.45);
                tf.rotation = Quat::from_euler(EulerRot::YXZ, rig.yaw, rig.pitch, 0.0);
            }
            let mut move_dir = Vec3::ZERO;
            if keys.pressed(KeyCode::KeyW) {
                move_dir += *tf.forward();
            }
            if keys.pressed(KeyCode::KeyS) {
                move_dir -= *tf.forward();
            }
            if keys.pressed(KeyCode::KeyD) {
                move_dir += *tf.right();
            }
            if keys.pressed(KeyCode::KeyA) {
                move_dir -= *tf.right();
            }
            if keys.pressed(KeyCode::KeyE) {
                move_dir += Vec3::Y;
            }
            if keys.pressed(KeyCode::KeyQ) {
                move_dir -= Vec3::Y;
            }
            let mul = if keys.pressed(KeyCode::ShiftLeft) { 2.8 } else { 1.0 };
            tf.translation += move_dir.normalize_or_zero() * 9.0 * mul * dt;
        }
    }
}

fn draw_stage_gizmos(state: Res<ShowcaseState>, mut gizmos: Gizmos) {
    if !state.show_gizmos || state.mode == ViewMode::UiPanels {
        return;
    }
    gizmos.grid(
        Isometry3d::from_translation(Vec3::new(0.0, 0.01, 0.0))
            * Isometry3d::from_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        UVec2::new(26, 26),
        Vec2::splat(1.0),
        Color::srgba(0.35, 0.45, 0.62, 0.35),
    );
    gizmos.axes(Transform::IDENTITY, 1.0);
}

fn update_hud_status_text(
    state: Res<ShowcaseState>,
    manifest: Res<AssetManifest>,
    ui_preview: Res<UiPreviewInteractionState>,
    player_q: Query<&PlayerCharacter>,
    bound_anims: Query<&BoundAnimationSet>,
    mut text_q: Query<&mut Text, With<HudStatusText>>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };
    let models = combined_models(&manifest);
    let scene_label = manifest
        .scenes
        .get(state.scene_index % manifest.scenes.len().max(1))
        .map(|s| format!("{} ({} tris)", s.name, s.triangle_count))
        .unwrap_or_else(|| "None".to_owned());
    let model_label = models
        .get(state.model_index % models.len().max(1))
        .map(|m| format!("{} ({} anims)", m.name, m.animation_count))
        .unwrap_or_else(|| "None".to_owned());
    let prop_label = if state.prop_attached && !manifest.props.is_empty() {
        manifest.props[state.prop_index % manifest.props.len()].name.as_str()
    } else {
        "Detached"
    };
    let player_ok = player_q.single().ok();
    let player_state = player_ok.map(|p| p.anim_state.label()).unwrap_or("None");
    let player_set = bound_anims
        .iter()
        .find(|s| s.role == BoundActorRole::Player && !s.nodes.is_empty())
        .or_else(|| bound_anims.iter().find(|s| !s.nodes.is_empty()));
    let active_clip = if let Some(set) = player_set {
        if let Some(p) = player_ok
            && state.control_mode == ControlMode::PlayThirdPerson
            && !p.manual_override
            && let Some((idx, _)) = pick_clip_for_state(p.anim_state, &set.nodes)
        {
            set.nodes[idx].0.as_str()
        } else {
            set.nodes[state.clip_index % set.nodes.len()].0.as_str()
        }
    } else {
        "None"
    };

    if state.mode == ViewMode::UiPanels {
        let s = &state.ui_panel_summary;
        let anim_label = if s.animation_names.is_empty() {
            "0".to_owned()
        } else {
            format!("{} ({})", s.clip_count, s.animation_names.join(", "))
        };
        **text = format!(
            "Unity {} | {} | Panel: {} ({}/{}) [{}x{}] | View [R]: {} | L10n [T]: {} | Bindings: {} | Anims: {} | {}",
            manifest.unity_version,
            state.mode.label(),
            state.active_ui_panel_name,
            (state.ui_panel_index % manifest.ui_panels.len().max(1)) + 1,
            manifest.ui_panels.len(),
            s.ref_w.round() as i32,
            s.ref_h.round() as i32,
            if state.ui_runtime_view { "Runtime" } else { "Raw Prefab" },
            if state.ui_localize { "On" } else { "Raw" },
            s.binding_count,
            anim_label,
            if ui_preview.last_clicked_label.is_empty() {
                "Click highlighted interactive widgets"
            } else {
                ui_preview.last_clicked_label.as_str()
            }
        );
    } else {
        **text = format!(
            "Unity {} | {} | Ctrl: {} | State: {} | Scene: {} | Hero: {} | Socket: {} | Clip: {} {}",
            manifest.unity_version,
            state.mode.label(),
            state.control_mode.label(),
            player_state,
            scene_label,
            model_label,
            prop_label,
            active_clip,
            if state.anim_paused { "(Paused)" } else { "" },
        );
    }
}
