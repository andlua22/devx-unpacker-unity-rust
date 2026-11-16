//! Phase 2 `MainLobby` 3D showcase stage: desert/outpost diorama, customized character body +
//! modular head + right-hand weapon socket attachment, turntable drag-orbit, and looping lobby BGM.

use bevy::asset::RenderAssetUsages;
use bevy::audio::{AudioSink, Volume};
use bevy::camera::RenderTarget;
use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::system::SystemParam;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};

use crate::actor_rig::{
    CharacterAnimDriver, animate_bip001_rigs, apply_character_skin_and_hair_tints,
    find_hand_socket_entity, select_weapon_prop, spawn_modular_character_from_profile,
};
use crate::config::{AssetManifest, GameConfigs, PlayerProfileSave};
use crate::states::AppState;

/// Root marker for non-rebuilt 3D lobby lighting, camera, pedestal, and BGM.
#[derive(Component)]
pub struct LobbyStageRoot;

/// Marker on the 3D outpost backdrop & character diorama entities rebuilt on customization changes.
#[derive(Component)]
struct LobbyDioramaEntity;

/// Marker on backdrop entities excluded from the off-screen hero portrait `RenderTarget`.
#[derive(Component)]
struct LobbyBackdropLayer;

/// Marker on the turntable hero root holding body + modular head GLBs.
#[derive(Component)]
struct LobbyHeroRoot;

/// Marker on the currently equipped weapon GLB mounted to the hero's hand socket.
#[derive(Component)]
struct LobbyAttachedWeapon;

/// Marker on the lobby background music entity for live volume updates from Settings.
#[derive(Component)]
struct LobbyBgmPlayer;

/// Off-screen studio render target image piped into `EstablishPanel`'s `m_hero_tex` (`RawImage`).
#[derive(Resource, Clone)]
pub struct LobbyHeroPortraitRt(pub Handle<Image>);

/// Shared state synchronizing lobby 3D stage rebuilds and interactive turntable controls.
#[derive(Resource)]
pub struct MainLobbyStageState {
    pub hero_dirty: bool,
    pub turntable_yaw: f32,
    pub auto_rotate: bool,
    pub socket_synced: bool,
}

impl Default for MainLobbyStageState {
    fn default() -> Self {
        Self {
            hero_dirty: true,
            turntable_yaw: -0.16,
            auto_rotate: false,
            socket_synced: false,
        }
    }
}

/// Registers the 3D Main Lobby pedestal stage and audio ambience under [`AppState::MainLobby`].
pub struct LobbyPlugin;

impl Plugin for LobbyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MainLobbyStageState>()
            .add_systems(OnEnter(AppState::MainLobby), enter_lobby_stage)
            .add_systems(
                Update,
                (
                    rebuild_lobby_hero_if_dirty,
                    propagate_backdrop_render_layers,
                    sync_lobby_weapon_socket,
                    apply_character_skin_and_hair_tints,
                    animate_bip001_rigs,
                    update_lobby_turntable_and_bgm,
                )
                    .chain()
                    .run_if(in_state(AppState::MainLobby)),
            );
    }
}

fn propagate_backdrop_render_layers(
    mut commands: Commands,
    backdrops: Query<Entity, With<LobbyBackdropLayer>>,
    children_q: Query<&Children>,
    layers_q: Query<&RenderLayers>,
    names_q: Query<&Name>,
) {
    let target = RenderLayers::layer(2);
    let mut stack: Vec<Entity> = backdrops.iter().collect();
    while let Some(e) = stack.pop() {
        if let Ok(name) = names_q.get(e)
            && name.as_str().starts_with("zhujiemianBG_01 (1)")
        {
            commands.entity(e).insert(Visibility::Hidden);
        }
        if layers_q.get(e).ok().is_none_or(|rl| rl != &target) {
            commands.entity(e).insert(target.clone());
        }
        if let Ok(ch) = children_q.get(e) {
            stack.extend(ch.iter());
        }
    }
}

#[derive(SystemParam)]
struct LobbyEnterCtx<'w> {
    asset_server: Res<'w, AssetServer>,
    profile: Res<'w, PlayerProfileSave>,
    stage_state: ResMut<'w, MainLobbyStageState>,
    images: ResMut<'w, Assets<Image>>,
}

fn enter_lobby_stage(mut commands: Commands, mut ctx: LobbyEnterCtx) {
    ctx.stage_state.hero_dirty = true;
    ctx.stage_state.socket_synced = false;

    commands.spawn((
        LobbyStageRoot,
        DespawnOnExit(AppState::MainLobby),
        IsDefaultUiCamera,
        Camera3d::default(),
        RenderLayers::from_layers(&[0, 2]),
        Tonemapping::TonyMcMapface,
        Transform::from_xyz(-0.033, 1.456, 3.595).looking_at(Vec3::new(0.0, 0.96, 0.0), Vec3::Y),
    ));

    // Dedicated offscreen studio camera feeding `EstablishPanel`'s `m_hero_tex` (1080x1080) slot.
    let rt_size = Extent3d {
        width: 1024,
        height: 1024,
        depth_or_array_layers: 1,
    };
    let mut rt_img = Image::new_fill(
        rt_size,
        TextureDimension::D2,
        &[0; 4],
        TextureFormat::Bgra8UnormSrgb,
        RenderAssetUsages::default(),
    );
    rt_img.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::RENDER_ATTACHMENT;
    let rt_handle = ctx.images.add(rt_img);
    commands.insert_resource(LobbyHeroPortraitRt(rt_handle.clone()));
    commands.spawn((
        LobbyStageRoot,
        DespawnOnExit(AppState::MainLobby),
        Camera3d::default(),
        Camera {
            order: -1,
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        Projection::Perspective(PerspectiveProjection {
            fov: 0.62,
            near: 0.1,
            far: 8.0,
            ..default()
        }),
        RenderTarget::Image(rt_handle.into()),
        Tonemapping::TonyMcMapface,
        Transform::from_xyz(0.0, 1.05, 3.35).looking_at(Vec3::new(0.0, 0.92, 0.0), Vec3::Y),
    ));

    commands.spawn((
        LobbyStageRoot,
        DespawnOnExit(AppState::MainLobby),
        RenderLayers::from_layers(&[0, 2]),
        DirectionalLight {
            illuminance: 16_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(8.0, 14.0, 9.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.spawn((
        LobbyStageRoot,
        DespawnOnExit(AppState::MainLobby),
        RenderLayers::from_layers(&[0, 2]),
        PointLight {
            color: Color::srgb(0.42, 0.72, 1.0),
            intensity: 190_000.0,
            range: 22.0,
            ..default()
        },
        Transform::from_xyz(-3.2, 2.8, -2.5),
    ));

    commands.spawn((
        LobbyStageRoot,
        LobbyBgmPlayer,
        DespawnOnExit(AppState::MainLobby),
        Name::new("LobbyBGM"),
        AudioPlayer::new(ctx.asset_server.load("audio/bgm_loading.wav")),
        PlaybackSettings::LOOP.with_volume(Volume::Linear(ctx.profile.music_volume.clamp(0.0, 1.0))),
    ));
}

#[derive(SystemParam)]
struct LobbyRebuildCtx<'w> {
    manifest: Res<'w, AssetManifest>,
    configs: Res<'w, GameConfigs>,
    profile: Res<'w, PlayerProfileSave>,
    asset_server: Res<'w, AssetServer>,
    stage_state: ResMut<'w, MainLobbyStageState>,
}

fn rebuild_lobby_hero_if_dirty(
    mut commands: Commands,
    mut ctx: LobbyRebuildCtx,
    old_diorama_q: Query<Entity, (With<LobbyDioramaEntity>, Without<ChildOf>)>,
    old_weapon_q: Query<Entity, With<LobbyAttachedWeapon>>,
) {
    if !ctx.stage_state.hero_dirty {
        return;
    }
    ctx.stage_state.hero_dirty = false;
    ctx.stage_state.socket_synced = false;

    for e in &old_weapon_q {
        commands.entity(e).try_despawn();
    }
    for e in &old_diorama_q {
        commands.entity(e).try_despawn();
    }

    // Spawn the authentic `zhujiemian_01` lobby scene (`zhujiemianBG_01` 2048x1024 desert backdrop
    // + `zhujiemianBG_01 (1)` contact shadow quad aligned at `(0, 0, 0)` under the character's feet).
    let lobby_scene_h: Handle<WorldAsset> = ctx.asset_server.load("scenes/zhujiemian_01.glb#Scene0");
    commands.spawn((
        LobbyStageRoot,
        LobbyDioramaEntity,
        LobbyBackdropLayer,
        DespawnOnExit(AppState::MainLobby),
        Name::new("LobbyScene_zhujiemian_01"),
        WorldAssetRoot(lobby_scene_h),
        RenderLayers::layer(2),
        Transform::from_xyz(3.128, 26.95, -2.7871)
            .with_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
    ));

    // Spawn hero turntable root holding modular body + head + gloves + shoes + hair GLBs.
    commands
        .spawn((
            LobbyStageRoot,
            LobbyDioramaEntity,
            LobbyHeroRoot,
            CharacterAnimDriver::default(),
            DespawnOnExit(AppState::MainLobby),
            Name::new("LobbyHeroTurntable"),
            Visibility::default(),
            Transform::from_xyz(0.0, 0.0, 0.0)
                .with_rotation(Quat::from_rotation_y(ctx.stage_state.turntable_yaw)),
        ))
        .with_children(|parent| {
            spawn_modular_character_from_profile(
                parent,
                &ctx.asset_server,
                &ctx.manifest,
                &ctx.configs,
                &ctx.profile,
            );
        });
}

#[derive(SystemParam)]
struct LobbySocketCtx<'w> {
    manifest: Res<'w, AssetManifest>,
    profile: Res<'w, PlayerProfileSave>,
    asset_server: Res<'w, AssetServer>,
    stage_state: ResMut<'w, MainLobbyStageState>,
}

fn sync_lobby_weapon_socket(
    mut commands: Commands,
    mut ctx: LobbySocketCtx,
    old_weapons: Query<Entity, With<LobbyAttachedWeapon>>,
    hero_q: Query<(Entity, Option<&Children>), With<LobbyHeroRoot>>,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
) {
    if ctx.stage_state.socket_synced {
        return;
    }
    let Ok((hero_entity, root_children)) = hero_q.single() else {
        return;
    };
    let Some(children) = root_children else {
        return;
    };
    if children.is_empty() {
        return;
    }
    // Wait until the body skeleton nodes are instantiated before searching for Bip001 R Hand.
    let Some(socket) = find_hand_socket_entity(hero_entity, &children_q, &names_q) else {
        return;
    };
    ctx.stage_state.socket_synced = true;

    for e in &old_weapons {
        commands.entity(e).try_despawn();
    }

    if let Some(prop) = select_weapon_prop(&ctx.manifest, ctx.profile.equipped_prop_idx) {
        let h: Handle<WorldAsset> = ctx.asset_server.load(format!("{}#Scene0", prop.asset_path));
        commands.spawn((
            LobbyStageRoot,
            LobbyAttachedWeapon,
            LobbyBackdropLayer,
            DespawnOnExit(AppState::MainLobby),
            Name::new(format!("LobbyWeapon_{}", prop.name)),
            WorldAssetRoot(h),
            RenderLayers::layer(2),
            ChildOf(socket),
            Transform::IDENTITY,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn update_lobby_turntable_and_bgm(
    time: Res<Time>,
    mouse_btn: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    windows: Query<&Window>,
    profile: Res<PlayerProfileSave>,
    lobby_ui: Option<Res<crate::ui::main_lobby::MainLobbyUiState>>,
    mut stage_state: ResMut<MainLobbyStageState>,
    mut hero_q: Query<(&mut Transform, &mut CharacterAnimDriver), With<LobbyHeroRoot>>,
    mut bgm_q: Query<&mut AudioSink, With<LobbyBgmPlayer>>,
) {
    let in_establish = lobby_ui
        .as_deref()
        .is_some_and(|u| u.active_tab == crate::ui::main_lobby::LobbyTab::Establish);
    let left_in_hero_viewport = mouse_btn.pressed(MouseButton::Left)
        && windows
            .iter()
            .next()
            .and_then(|w| {
                let pos = w.cursor_position()?;
                let nx = pos.x / w.width().max(1.0);
                let ny = pos.y / w.height().max(1.0);
                Some((0.28..=0.78).contains(&nx) && (0.08..=0.88).contains(&ny))
            })
            .unwrap_or(false);

    if mouse_btn.pressed(MouseButton::Right)
        || mouse_btn.pressed(MouseButton::Middle)
        || left_in_hero_viewport
    {
        stage_state.turntable_yaw += motion.delta.x * 0.012;
        stage_state.auto_rotate = false;
    } else if stage_state.auto_rotate {
        stage_state.turntable_yaw += time.delta_secs() * 0.35;
    }

    for (mut tf, mut driver) in &mut hero_q {
        tf.rotation = Quat::from_rotation_y(stage_state.turntable_yaw);
        driver.has_weapon = !in_establish;
    }

    if profile.is_changed() {
        let vol = Volume::Linear(profile.music_volume.clamp(0.0, 1.0));
        for mut sink in &mut bgm_q {
            sink.set_volume(vol);
        }
    }
}
