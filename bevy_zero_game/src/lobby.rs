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
    BoundAnimationSet, CharacterAnimDriver, CharacterBodyAnimation, CharacterBodyGltf,
    UsesBakedGltfAnim, apply_character_skin_and_hair_tints,
    bind_native_character_animations, bind_rigid_character_hair, find_hand_socket_entity,
    pick_clip_for_state, retarget_native_character_parts, spawn_modular_character_from_profile,
};
use crate::config::{AssetManifest, GameConfigs, GlbAssetEntry, PlayerProfileSave};
use crate::states::AppState;

/// Root marker for non-rebuilt 3D lobby lighting, camera, pedestal, and BGM.
#[derive(Component)]
pub struct LobbyStageRoot;

/// Marker on backdrop entities excluded from the off-screen hero portrait `RenderTarget`.
#[derive(Component)]
struct LobbyBackdropLayer;

/// Marker on the turntable hero root holding body + modular head GLBs.
#[derive(Component)]
struct LobbyHeroRoot;

/// Visible hero kept in place while a replacement scene is instantiated.
#[derive(Component)]
struct LobbyHeroActive;

/// Replacement hero becomes visible after all selected modular scenes have spawned.
#[derive(Component)]
struct LobbyHeroPending;

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
                    bind_rigid_character_hair,
                    bind_native_character_animations,
                    retarget_native_character_parts,
                    activate_lobby_hero_when_ready,
                    sync_lobby_weapon_socket,
                    apply_character_skin_and_hair_tints,
                    update_lobby_turntable_and_bgm,
                    sync_lobby_native_animation,
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

    // Keep the authored diorama alive when customization replaces only the modular hero.
    commands.spawn((
        LobbyStageRoot,
        LobbyBackdropLayer,
        DespawnOnExit(AppState::MainLobby),
        Name::new("LobbyScene_zhujiemian_01"),
        WorldAssetRoot(ctx.asset_server.load("scenes/zhujiemian_01.glb#Scene0")),
        RenderLayers::layer(2),
        Transform::from_xyz(3.128, 26.95, -2.7871)
            .with_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
    ));

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
    rt_img.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
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
        PlaybackSettings::LOOP
            .with_volume(Volume::Linear(ctx.profile.music_volume.clamp(0.0, 1.0))),
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
    pending_hero_q: Query<Entity, With<LobbyHeroPending>>,
) {
    if !ctx.stage_state.hero_dirty {
        return;
    }
    ctx.stage_state.hero_dirty = false;
    ctx.stage_state.socket_synced = false;

    for e in &pending_hero_q {
        commands.entity(e).try_despawn();
    }

    // Keep the current hero visible until all new modular GLB scenes have spawned.
    commands
        .spawn((
            LobbyStageRoot,
            LobbyHeroRoot,
            LobbyHeroPending,
            CharacterAnimDriver::default(),
            UsesBakedGltfAnim,
            DespawnOnExit(AppState::MainLobby),
            Name::new("LobbyHeroTurntable"),
            Visibility::Hidden,
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

fn activate_lobby_hero_when_ready(
    mut commands: Commands,
    mut stage_state: ResMut<MainLobbyStageState>,
    pending: Query<(Entity, &Children), With<LobbyHeroPending>>,
    active: Query<Entity, With<LobbyHeroActive>>,
    parts: Query<Option<&Children>, With<WorldAssetRoot>>,
) {
    for (hero, selected_parts) in &pending {
        // A loaded WorldAssetRoot gets descendants when its scene is instantiated. Waiting
        // for every selected part prevents a transient body without a head or hair.
        if selected_parts.is_empty()
            || !selected_parts.iter().all(|part| {
                parts
                    .get(part)
                    .ok()
                    .flatten()
                    .is_some_and(|scene| !scene.is_empty())
            })
        {
            continue;
        }
        for old in &active {
            commands.entity(old).try_despawn();
        }
        commands.entity(hero).remove::<LobbyHeroPending>().insert((
            LobbyHeroActive,
            Visibility::Visible,
        ));
        stage_state.socket_synced = false;
    }
}

fn sync_lobby_native_animation(
    configs: Res<GameConfigs>,
    profile: Res<PlayerProfileSave>,
    heroes: Query<&CharacterAnimDriver, With<LobbyHeroRoot>>,
    bodies: Query<(&ChildOf, &CharacterBodyAnimation)>,
    mut players: Query<(
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &BoundAnimationSet,
    )>,
) {
    for (parent, binding) in &bodies {
        let Ok(driver) = heroes.get(parent.parent()) else {
            continue;
        };
        let Ok((mut player, mut transitions, set)) = players.get_mut(binding.player) else {
            continue;
        };
        let requested = if driver.has_weapon {
            lobby_owned_item_id(&profile)
                .and_then(|id| configs.guns.iter().find(|gun| gun.id == id))
                .map(|gun| gun.main_panel_aim.as_str())
                .filter(|aim| !aim.is_empty())
        } else {
            None
        };
        let clip = requested
            .and_then(|requested| {
                set.nodes.iter().find(|(name, _)| {
                    name.rsplit('.')
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case(requested))
                })
            })
            .map(|(_, clip)| *clip)
            .or_else(|| pick_clip_for_state(driver.state, &set.nodes).map(|(_, clip)| clip));
        let Some(clip) = clip else {
            continue;
        };
        if !player.is_playing_animation(clip) {
            transitions
                .play(&mut player, clip, std::time::Duration::from_millis(180))
                .repeat();
        }
    }
}

#[derive(SystemParam)]
struct LobbySocketCtx<'w> {
    manifest: Res<'w, AssetManifest>,
    profile: Res<'w, PlayerProfileSave>,
    configs: Res<'w, GameConfigs>,
    asset_server: Res<'w, AssetServer>,
    stage_state: ResMut<'w, MainLobbyStageState>,
}

fn sync_lobby_weapon_socket(
    mut commands: Commands,
    mut ctx: LobbySocketCtx,
    old_weapons: Query<Entity, With<LobbyAttachedWeapon>>,
    pending: Query<Entity, With<LobbyHeroPending>>,
    hero_q: Query<(Entity, Option<&Children>), With<LobbyHeroActive>>,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
    bodies: Query<(), With<CharacterBodyGltf>>,
) {
    if ctx.profile.is_changed() {
        ctx.stage_state.socket_synced = false;
    }
    if ctx.stage_state.socket_synced || !pending.is_empty() {
        return;
    }
    let Ok((_, root_children)) = hero_q.single() else {
        return;
    };
    let Some(children) = root_children else {
        return;
    };
    if children.is_empty() {
        return;
    }
    // Wait until the body skeleton nodes are instantiated before searching for Bip001 R Hand.
    let Some(body) = children.iter().find(|&entity| bodies.contains(entity)) else {
        return;
    };
    let Some(socket) = find_hand_socket_entity(body, &children_q, &names_q) else {
        return;
    };
    ctx.stage_state.socket_synced = true;

    for e in &old_weapons {
        commands.entity(e).try_despawn();
    }

    if let Some(prop) = lobby_owned_weapon(&ctx.manifest, &ctx.configs, &ctx.profile) {
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

fn lobby_owned_item_id(profile: &PlayerProfileSave) -> Option<i32> {
    let instance = profile.lobby_equipped_instance_id?;
    profile
        .lobby_store_items
        .iter()
        .find(|item| item.instance_id == instance && item.count > 0)
        .map(|item| item.item_id)
}

fn lobby_owned_weapon<'a>(
    manifest: &'a AssetManifest,
    configs: &GameConfigs,
    profile: &PlayerProfileSave,
) -> Option<&'a GlbAssetEntry> {
    let item_id = lobby_owned_item_id(profile)?;
    let item = configs.item_cfgs.iter().find(|item| item.id == item_id)?;
    let stem = item
        .model_path
        .rsplit('/')
        .next()?
        .trim_end_matches(".ab")
        .trim_end_matches(".glb");
    manifest
        .props
        .iter()
        .find(|prop| prop.name.eq_ignore_ascii_case(stem))
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
        driver.has_weapon = !in_establish && lobby_owned_item_id(&profile).is_some();
    }

    if profile.is_changed() {
        let vol = Volume::Linear(profile.music_volume.clamp(0.0, 1.0));
        for mut sink in &mut bgm_q {
            sink.set_volume(vol);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::animation::{AnimatedBy, AnimationTargetId};
    use crate::actor_rig::RigidCharacterHair;
    use std::collections::HashMap;

    #[test]
    fn late_loaded_modular_bones_share_body_targets_and_rigid_hair_stays_bound() {
        let mut app = App::new();
        app.add_systems(Update, retarget_native_character_parts);
        let actor = app.world_mut().spawn_empty().id();
        let player = app.world_mut().spawn(AnimationPlayer::default()).id();
        let target = AnimationTargetId::from_name(&Name::new("native-body-head"));
        app.world_mut().spawn((
            CharacterBodyAnimation {
                player,
                targets: HashMap::from([("Bip001 Head".into(), target)]),
            },
            ChildOf(actor),
        ));
        let skinned_part = app.world_mut().spawn(ChildOf(actor)).id();
        let rigid_part = app
            .world_mut()
            .spawn((RigidCharacterHair, ChildOf(actor)))
            .id();
        let rigid_head = app
            .world_mut()
            .spawn((Name::new("Bip001 Head"), ChildOf(rigid_part)))
            .id();
        app.update();
        let late_head = app
            .world_mut()
            .spawn((Name::new("Bip001 Head"), ChildOf(skinned_part)))
            .id();
        app.update();
        assert_eq!(
            *app.world().get::<AnimationTargetId>(late_head).unwrap(),
            target
        );
        assert_eq!(app.world().get::<AnimatedBy>(late_head).unwrap().0, player);
        assert!(app.world().get::<AnimatedBy>(rigid_head).is_none());
    }

    #[test]
    fn customization_rebuild_preserves_the_loaded_backdrop_entity() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_resource::<AssetManifest>()
            .init_resource::<GameConfigs>()
            .init_resource::<PlayerProfileSave>()
            .init_resource::<MainLobbyStageState>()
            .add_systems(Update, rebuild_lobby_hero_if_dirty);
        let backdrop = app
            .world_mut()
            .spawn((
                LobbyStageRoot,
                LobbyBackdropLayer,
                Transform::from_xyz(3.128, 26.95, -2.7871),
            ))
            .id();
        let old_hero = app
            .world_mut()
            .spawn((LobbyHeroRoot, LobbyHeroActive, Visibility::Visible))
            .id();
        app.update();
        assert!(app.world().get_entity(old_hero).is_ok());
        assert!(app.world().get::<LobbyBackdropLayer>(backdrop).is_some());
        let first_pending = app
            .world_mut()
            .query_filtered::<Entity, With<LobbyHeroPending>>()
            .single(app.world())
            .unwrap();
        app.world_mut()
            .resource_mut::<MainLobbyStageState>()
            .hero_dirty = true;
        app.update();
        assert!(app.world().get_entity(old_hero).is_ok());
        assert!(app.world().get_entity(first_pending).is_err());
        assert!(app.world().get::<LobbyBackdropLayer>(backdrop).is_some());
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<LobbyHeroPending>>()
                .iter(app.world())
                .count(),
            1
        );
    }

    #[test]
    fn replacement_hero_becomes_visible_only_after_selected_scenes_spawn() {
        let mut app = App::new();
        app.init_resource::<MainLobbyStageState>()
            .add_systems(Update, activate_lobby_hero_when_ready);
        let old = app
            .world_mut()
            .spawn((LobbyHeroActive, Visibility::Visible))
            .id();
        let pending = app
            .world_mut()
            .spawn((LobbyHeroPending, Visibility::Hidden))
            .id();
        let body = app
            .world_mut()
            .spawn((WorldAssetRoot(Handle::default()), ChildOf(pending)))
            .id();
        app.update();
        assert!(app.world().get_entity(old).is_ok());
        assert_eq!(app.world().get::<Visibility>(pending), Some(&Visibility::Hidden));

        app.world_mut().spawn(ChildOf(body));
        app.update();
        assert!(app.world().get_entity(old).is_err());
        assert!(app.world().get::<LobbyHeroActive>(pending).is_some());
        assert_eq!(app.world().get::<Visibility>(pending), Some(&Visibility::Visible));
    }
}
