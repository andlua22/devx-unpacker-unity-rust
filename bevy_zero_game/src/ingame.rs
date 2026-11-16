//! Phase 3 networked `InGame` survival vertical slice:
//! - Streams real exported `lite4.0.apk` terrain chunks (`a-55..d-02`) via [`TerrainColliderGrid`]
//! - Replicates up to 100 players/bots over real island elevation and placed buildings
//! - Supports resource gathering (trees, ore boulders, crates), grid/socket building placement
//!   (`diji_lv1..gongzuotai_lv1`), tier upgrades (`Lv.1..Lv.4`), hinged doors (`[E]`), crafting
//!   (`[Tab]`), and hitscan combat + VFX tracers (`Pure Bevy 0.20`).

use std::collections::HashMap;

use bevy::audio::Volume;
use bevy::camera::ClearColorConfig;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::system::SystemParam;
use bevy::gltf::Gltf;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui_widgets::Button;

use crate::actor_rig::{
    AnimState, BoundActorRole, BoundAnimationSet, CharacterAnimDriver, UsesBakedGltfAnim,
    animate_bip001_rigs, build_gltf_animation_set, find_hand_socket_entity, gunshot_audio_for_prop,
    pick_clip_for_state, select_weapon_prop, spawn_modular_character,
};
use crate::config::{AssetManifest, BaseSaveData, KeybindProfile, PlayerProfileSave};
use crate::net::protocol::{
    MAX_PLAYERS, NetPlayerState, OP_CONSUME_BANDAGE, OP_ENTER_EXIT_VEHICLE, OP_HARVEST_NODE,
    OP_HIT_ENEMY, OP_HIT_PLAYER, OP_LOOT_DROP, OP_PLACE_BUILD, OP_REFUEL_OR_TOGGLE_POWER,
    OP_SPEND_AMMO, OP_TOGGLE_DOOR, OP_UPGRADE_BUILD,
};
use crate::net::transport::{NetClientState, NetServerHandle};
use crate::states::{AppState, InGameRoot};
use crate::survival::{HotbarItem, SurvivalSessionState, apply_structure_tier_and_door_pose};
use crate::world_physics::{
    BuildPieceKind, EnemyKind, ReplicatedEnemy, ReplicatedResourceNode, SEA_LEVEL_Y,
    TerrainColliderGrid, VehicleKind, WorldHitTarget, compute_build_snap,
    generate_default_resource_nodes, raycast_vehicles, raycast_world,
};

const CHUNK_STREAM_IN_RADIUS: f32 = 540.0;
const CHUNK_STREAM_OUT_RADIUS: f32 = 760.0;
const NODE_RENDER_RADIUS: f32 = 220.0;

#[derive(Component)]
struct LocalArenaAvatar;

#[derive(Component)]
struct LocalArenaWeaponMount {
    prop_idx: usize,
}

#[derive(Component)]
struct ArenaFollowCamera {
    yaw: f32,
    pitch: f32,
    radius: f32,
    focus: Vec3,
}

#[derive(Component)]
struct RemotePeerAvatar {
    player_id: u16,
}

#[derive(Component)]
struct StreamedChunkEntity {
    name: String,
}

#[derive(Component)]
struct SpawnedResourceVisual {
    id: u16,
    depleted: bool,
}

#[derive(Component)]
struct SpawnedStructureVisual {
    id: u32,
    tier: u8,
    is_open: bool,
    synced: bool,
}

#[derive(Component)]
struct BuildGhostPreview;

#[derive(Component)]
struct TracerFx {
    ttl: f32,
}

#[derive(Component)]
struct ParticleBurstFx {
    vel: Vec3,
    ttl: f32,
    max_ttl: f32,
}

#[derive(Component)]
struct SpawnedEnemyVisual {
    id: u16,
    kind: EnemyKind,
    was_dead: bool,
    was_attacking: bool,
    gltf_handle: Handle<Gltf>,
    anim_bound: bool,
}

#[derive(Component)]
struct SpawnedLootVisual {
    id: u16,
    base_y: f32,
}

#[derive(Resource)]
struct ArenaSharedAssets {
    peer_mesh: Handle<Mesh>,
    bot_mat: Handle<StandardMaterial>,
    human_mat: Handle<StandardMaterial>,
    ghost_mat_ok: Handle<StandardMaterial>,
    ghost_mat_bad: Handle<StandardMaterial>,
    tracer_mesh: Handle<Mesh>,
    tracer_mat: Handle<StandardMaterial>,
    particle_mesh: Handle<Mesh>,
    spark_mat: Handle<StandardMaterial>,
    chip_mat: Handle<StandardMaterial>,
    blood_mat: Handle<StandardMaterial>,
    electric_mat: Handle<StandardMaterial>,
    base_nodes: Vec<ReplicatedResourceNode>,
}

#[derive(Component)]
struct SpawnedVehicleVisual {
    id: u16,
    kind: crate::world_physics::VehicleKind,
    rotor_phase: f32,
    engine_sfx_cd: f32,
    was_driven: bool,
}

/// Registers the Phase 3 & 4 playable island arena, terrain streamer, AI enemies/bosses,
/// turrets, loot drops, spatial audio, and net sync.
pub struct InGamePlugin;

impl Plugin for InGamePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SurvivalSessionState>()
            .add_systems(OnEnter(AppState::InGame), enter_ingame_arena)
            .add_systems(OnExit(AppState::InGame), exit_ingame_arena)
            .add_systems(
                Update,
                (
                    handle_ingame_hotkeys_and_actions,
                    stream_terrain_chunks_system,
                    sync_resource_nodes_system,
                    sync_replicated_structures_system,
                    sync_replicated_enemies_system,
                    bind_and_drive_enemy_anims_system,
                    sync_replicated_loot_drops_system,
                    sync_replicated_vehicles_system,
                    sync_ingame_weapon_socket,
                    reconcile_local_avatar_system,
                    replicate_remote_peers_system,
                    animate_bip001_rigs,
                    update_build_ghost_and_tracers,
                    arena_camera_controller_system,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(SystemParam)]
struct ArenaEnterCtx<'w> {
    manifest: Res<'w, AssetManifest>,
    profile: Res<'w, PlayerProfileSave>,
    asset_server: Res<'w, AssetServer>,
    survival: ResMut<'w, SurvivalSessionState>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    server_opt: Option<ResMut<'w, NetServerHandle>>,
}

fn enter_ingame_arena(mut commands: Commands, mut ctx: ArenaEnterCtx) {
    *ctx.survival = SurvivalSessionState::default();
    if let Some(ref mut srv) = ctx.server_opt
        && srv.world.structures.is_empty()
        && let Some(saved) = BaseSaveData::load_from_disk(&BaseSaveData::resolve_save_path())
    {
        srv.world.import_saved_base(&saved);
        ctx.survival.status_banner = format!(
            "Restored saved base ({} buildings)",
            saved.structures.len()
        );
        ctx.survival.status_banner_secs = 4.0;
    }
    let terrain = TerrainColliderGrid::load_default();
    let base_nodes = generate_default_resource_nodes(&terrain);
    let spawn_ground = terrain.ground_y(0.0, 0.0);

    let ghost_mesh = ctx.meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let ghost_mat_ok = ctx.materials.add(StandardMaterial {
        base_color: Color::srgba(0.18, 0.88, 0.42, 0.42),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    });
    let ghost_mat_bad = ctx.materials.add(StandardMaterial {
        base_color: Color::srgba(0.92, 0.24, 0.22, 0.42),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    });
    info!(
        "Baked {} terrain triangles across island chunks for pure-Bevy physics",
        terrain.triangle_count()
    );

    commands.insert_resource(ArenaSharedAssets {
        peer_mesh: ctx.meshes.add(Capsule3d::new(0.34, 1.05)),
        bot_mat: ctx.materials.add(StandardMaterial {
            base_color: Color::srgb(0.28, 0.72, 0.94),
            perceptual_roughness: 0.65,
            ..default()
        }),
        human_mat: ctx.materials.add(StandardMaterial {
            base_color: Color::srgb(0.95, 0.64, 0.24),
            perceptual_roughness: 0.55,
            ..default()
        }),
        ghost_mat_ok: ghost_mat_ok.clone(),
        ghost_mat_bad,
        tracer_mesh: ctx.meshes.add(Cuboid::new(0.05, 0.05, 1.0)),
        tracer_mat: ctx.materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.85, 0.35),
            emissive: LinearRgba::rgb(2.2, 1.5, 0.4),
            unlit: true,
            ..default()
        }),
        particle_mesh: ctx.meshes.add(Cuboid::new(0.11, 0.11, 0.11)),
        spark_mat: ctx.materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.72, 0.22),
            emissive: LinearRgba::rgb(2.8, 1.4, 0.2),
            unlit: true,
            ..default()
        }),
        chip_mat: ctx.materials.add(StandardMaterial {
            base_color: Color::srgb(0.58, 0.44, 0.30),
            perceptual_roughness: 0.85,
            ..default()
        }),
        blood_mat: ctx.materials.add(StandardMaterial {
            base_color: Color::srgb(0.82, 0.12, 0.12),
            emissive: LinearRgba::rgb(0.8, 0.05, 0.05),
            perceptual_roughness: 0.5,
            ..default()
        }),
        electric_mat: ctx.materials.add(StandardMaterial {
            base_color: Color::srgb(0.28, 0.92, 1.0),
            emissive: LinearRgba::rgb(0.6, 2.8, 4.2),
            unlit: true,
            ..default()
        }),
        base_nodes,
    });
    commands.insert_resource(terrain);

    let rig = ArenaFollowCamera {
        yaw: 0.15,
        pitch: 0.22,
        radius: 5.8,
        focus: Vec3::new(0.42 * 0.15_f32.cos(), spawn_ground + 1.72, -0.42 * 0.15_f32.sin()),
    };
    commands.spawn((
        InGameRoot,
        DespawnOnExit(AppState::InGame),
        Camera3d::default(),
        SpatialListener::new(1.8),
        Camera {
            clear_color: ClearColorConfig::Custom(Color::srgb(0.46, 0.72, 0.94)),
            ..default()
        },
        DistanceFog {
            color: Color::srgba(0.62, 0.80, 0.95, 1.0),
            falloff: FogFalloff::Linear {
                start: 160.0,
                end: 460.0,
            },
            ..default()
        },
        Tonemapping::TonyMcMapface,
        Transform::from_xyz(0.0, spawn_ground + 4.0, 6.0).looking_at(rig.focus, Vec3::Y),
        rig,
    ));

    commands.spawn((
        InGameRoot,
        DespawnOnExit(AppState::InGame),
        DirectionalLight {
            illuminance: 16_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(24.0, 48.0, 18.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Island coastal ocean water plane at SEA_LEVEL_Y + battle_map1 sky shell.
    let ocean_mesh = ctx
        .meshes
        .add(Plane3d::default().mesh().size(2400.0, 2400.0));
    let ocean_mat = ctx.materials.add(StandardMaterial {
        base_color: Color::srgba(0.08, 0.28, 0.44, 0.90),
        perceptual_roughness: 0.25,
        metallic: 0.25,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    commands.spawn((
        InGameRoot,
        DespawnOnExit(AppState::InGame),
        Name::new("IslandOceanPlane"),
        Mesh3d(ocean_mesh),
        MeshMaterial3d(ocean_mat),
        Transform::from_xyz(0.0, SEA_LEVEL_Y, 0.0),
    ));

    // Build hologram ghost mesh (initially hidden until build slot [4-0] selected).
    commands.spawn((
        InGameRoot,
        DespawnOnExit(AppState::InGame),
        BuildGhostPreview,
        Name::new("BuildGhostPreview"),
        Mesh3d(ghost_mesh),
        MeshMaterial3d(ghost_mat_ok),
        Visibility::Hidden,
        Transform::from_xyz(0.0, spawn_ground, 0.0),
    ));

    // Ambient world wind/theme soundtrack looping quietly in background
    if ctx.profile.music_volume > 0.01 {
        commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            AudioPlayer::new(ctx.asset_server.load("audio/bgm_wenmingchongqi.wav")),
            PlaybackSettings::LOOP.with_volume(Volume::Linear(ctx.profile.music_volume * 0.28)),
        ));
    }

    // Customized modular survivor avatar (`PlayerProfileSave`): body + head + gloves + shoes + hair.
    commands
        .spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            LocalArenaAvatar,
            CharacterAnimDriver::default(),
            Name::new("LocalPlayerAvatar"),
            Visibility::default(),
            Transform::from_xyz(0.0, spawn_ground, 0.0),
        ))
        .with_children(|parent| {
            spawn_modular_character(
                parent,
                &ctx.asset_server,
                &ctx.manifest,
                ctx.profile.sex_male,
                ctx.profile.body_variant_idx,
                ctx.profile.head_variant_idx,
                ctx.profile.hair_variant_idx,
            );
        });
}

fn exit_ingame_arena(mut commands: Commands, mut client: ResMut<NetClientState>) {
    client.disconnect();
    commands.remove_resource::<ArenaSharedAssets>();
    commands.remove_resource::<TerrainColliderGrid>();
}

#[derive(SystemParam)]
struct InGameInputCtx<'w> {
    time: Res<'w, Time>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    mouse_btn: Res<'w, ButtonInput<MouseButton>>,
    manifest: Res<'w, AssetManifest>,
    profile: Res<'w, PlayerProfileSave>,
    asset_server: Res<'w, AssetServer>,
    survival: ResMut<'w, SurvivalSessionState>,
    client: ResMut<'w, NetClientState>,
    terrain_opt: Option<Res<'w, TerrainColliderGrid>>,
    shared_opt: Option<Res<'w, ArenaSharedAssets>>,
    server_opt: Option<ResMut<'w, NetServerHandle>>,
    next_state: ResMut<'w, NextState<AppState>>,
}

type LocalAvatarQuery<'w, 's> = Query<
    'w,
    's,
    (&'static Transform, &'static mut CharacterAnimDriver),
    (With<LocalArenaAvatar>, Without<ArenaFollowCamera>),
>;

#[allow(clippy::too_many_arguments)]
fn handle_ingame_hotkeys_and_actions(
    mut commands: Commands,
    mut ctx: InGameInputCtx,
    mut footstep_timer: Local<f32>,
    cam_q: Query<&Transform, With<ArenaFollowCamera>>,
    mut avatar_q: LocalAvatarQuery,
    ui_hovered_q: Query<&Hovered, With<Button>>,
) {
    let dt = ctx.time.delta_secs();
    ctx.survival.attack_cooldown = (ctx.survival.attack_cooldown - dt).max(0.0);
    if ctx.survival.status_banner_secs > 0.0 {
        ctx.survival.status_banner_secs = (ctx.survival.status_banner_secs - dt).max(0.0);
        if ctx.survival.status_banner_secs <= 0.0 {
            ctx.survival.status_banner.clear();
        }
    }
    ctx.survival.base_power_pct = ctx.client.base_power_pct;
    let any_ui_hovered = ui_hovered_q.iter().any(|h| h.get());

    let k_interact = KeybindProfile::keycode(&ctx.profile.keybinds.interact_key);
    let k_rotate = KeybindProfile::keycode(&ctx.profile.keybinds.rotate_key);
    let k_upgrade = KeybindProfile::keycode(&ctx.profile.keybinds.upgrade_key);
    let k_craft = KeybindProfile::keycode(&ctx.profile.keybinds.craft_key);
    let k_heal = KeybindProfile::keycode(&ctx.profile.keybinds.heal_key);
    let k_turret = KeybindProfile::keycode(&ctx.profile.keybinds.turret_key);
    let k_generator = KeybindProfile::keycode(&ctx.profile.keybinds.generator_key);
    let k_save = KeybindProfile::keycode(&ctx.profile.keybinds.save_base_key);

    if ctx.keys.just_pressed(k_craft) || ctx.keys.just_pressed(KeyCode::Tab) {
        ctx.survival.craft_menu_open = !ctx.survival.craft_menu_open;
    }
    if ctx.keys.just_pressed(KeyCode::Escape) {
        if ctx.survival.craft_menu_open {
            ctx.survival.craft_menu_open = false;
        } else {
            ctx.next_state.set_if_different(AppState::MainLobby);
        }
        return;
    }
    if ctx.keys.just_pressed(KeyCode::F12) {
        ctx.next_state.set_if_different(AppState::DevSandbox);
        return;
    }
    if ctx.keys.just_pressed(KeyCode::KeyB)
        && let Some(srv) = ctx.server_opt.as_deref_mut()
    {
        let enable = srv.world.bot_count() == 0;
        srv.world.set_stress_bots_enabled(enable);
    }
    if ctx.keys.just_pressed(k_save)
        && let Some(srv) = ctx.server_opt.as_deref()
    {
        let owner = ctx.client.local_player_id.unwrap_or(1);
        let save = srv.world.export_saved_base(owner);
        ctx.survival.status_banner = match save.save_atomic(&BaseSaveData::resolve_save_path()) {
            Ok(()) => format!("Saved base ({} structures)", save.structures.len()),
            Err(e) => format!("Base save failed: {e}"),
        };
        ctx.survival.status_banner_secs = 3.5;
    }

    // Hotbar number bindings `[1..9, 0]`.
    let slot_keys = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::Digit0,
    ];
    for (idx, k) in slot_keys.iter().enumerate() {
        if ctx.keys.just_pressed(*k) {
            ctx.survival.custom_build_override = None;
            ctx.survival.active_slot = idx;
        }
    }
    if ctx.keys.just_pressed(KeyCode::KeyQ) && k_rotate != KeyCode::KeyQ {
        ctx.survival.custom_build_override = None;
        ctx.survival.active_slot = if ctx.survival.active_slot >= 3 { 0 } else { 3 };
    }
    if ctx.keys.just_pressed(k_turret) {
        ctx.survival.custom_build_override = match ctx.survival.custom_build_override {
            Some(BuildPieceKind::Turret) => Some(BuildPieceKind::ElectricTurret),
            Some(BuildPieceKind::ElectricTurret) => None,
            _ => Some(BuildPieceKind::Turret),
        };
    }
    if ctx.keys.just_pressed(k_generator) {
        ctx.survival.custom_build_override = match ctx.survival.custom_build_override {
            Some(BuildPieceKind::Generator) => Some(BuildPieceKind::Battery),
            Some(BuildPieceKind::Battery) => None,
            _ => Some(BuildPieceKind::Generator),
        };
    }
    if ctx.keys.just_pressed(k_rotate) {
        ctx.survival.build_rot_steps = (ctx.survival.build_rot_steps + 1) & 3;
    }
    if ctx.keys.just_pressed(k_heal) {
        ctx.client
            .send_world_op(OP_CONSUME_BANDAGE, 0, 0, 0, Vec3::ZERO);
        if ctx.client.bandages > 0 && ctx.client.hp < 100 {
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/bag_yaoping.wav",
                ctx.profile.sfx_volume * 0.85,
            );
        }
    }

    // Determine active hand prop matching [1] Rifle / [2] Axe / [3] Knife.
    ctx.survival.equipped_prop_override = match ctx.survival.active_item() {
        HotbarItem::Rifle => Some(ctx.profile.equipped_prop_idx),
        HotbarItem::Hatchet => ctx.manifest.props.iter().position(|p| p.name == "wq_shifu"),
        HotbarItem::Knife => ctx
            .manifest
            .props
            .iter()
            .position(|p| p.name.contains("bishou")),
        HotbarItem::Build(_) => Some(ctx.profile.equipped_prop_idx),
    };

    let local_pid = ctx.client.local_player_id.unwrap_or(0);
    let driving_v = ctx
        .client
        .vehicles
        .values()
        .find(|v| local_pid != 0 && v.driver_id == local_pid)
        .cloned();
    if let Some(ref dv) = driving_v {
        ctx.survival.driving_vehicle_id = dv.id;
        ctx.survival.active_vehicle_kind = Some(dv.kind);
        ctx.survival.active_vehicle_fuel_pct = dv.fuel_pct();
        ctx.survival.active_vehicle_hp = dv.hp;
        ctx.survival.active_vehicle_max_hp = dv.max_hp;
    } else {
        ctx.survival.driving_vehicle_id = 0;
        ctx.survival.active_vehicle_kind = None;
        ctx.survival.active_vehicle_speed = 0.0;
    }

    // Stream WASD + Sprint + Jump/Ascend/Descend movement + Yaw over UDP 30Hz input packet.
    let (cam_tf_opt, cam_fwd, cam_right) = if let Ok(cam_tf) = cam_q.single() {
        (
            Some(*cam_tf),
            Vec3::new(cam_tf.forward().x, 0.0, cam_tf.forward().z).normalize_or_zero(),
            Vec3::new(cam_tf.right().x, 0.0, cam_tf.right().z).normalize_or_zero(),
        )
    } else {
        (None, Vec3::NEG_Z, Vec3::X)
    };

    let sprinting = ctx.keys.pressed(KeyCode::ShiftLeft) || ctx.keys.pressed(KeyCode::ShiftRight);
    let mut buttons = 0_u16;
    if ctx.mouse_btn.pressed(MouseButton::Left) && !ctx.survival.craft_menu_open && !any_ui_hovered {
        buttons |= 1;
    }
    if sprinting {
        buttons |= 2;
    }
    let descend_held = ctx.keys.pressed(KeyCode::KeyC) || ctx.keys.pressed(KeyCode::ControlLeft);
    if descend_held && !ctx.survival.craft_menu_open {
        buttons |= 8;
    }

    let (wish_xz, yaw) = if let Some(ref dv) = driving_v {
        let mut throttle = 0.0_f32;
        let mut steer = 0.0_f32;
        if !ctx.survival.craft_menu_open {
            if ctx.keys.pressed(KeyCode::KeyW) {
                throttle += 1.0;
            }
            if ctx.keys.pressed(KeyCode::KeyS) {
                throttle -= 1.0;
            }
            if ctx.keys.pressed(KeyCode::KeyD) {
                steer += 1.0;
            }
            if ctx.keys.pressed(KeyCode::KeyA) {
                steer -= 1.0;
            }
            if ctx.keys.pressed(KeyCode::Space) {
                buttons |= 4;
            }
        }
        ctx.survival.active_vehicle_speed = (throttle.abs() * dv.kind.max_speed())
            * if sprinting { 1.32 } else { 1.0 };
        (Vec2::new(steer, throttle), dv.yaw)
    } else {
        let mut wish = Vec3::ZERO;
        if !ctx.survival.craft_menu_open {
            if ctx.keys.pressed(KeyCode::KeyW) {
                wish += cam_fwd;
            }
            if ctx.keys.pressed(KeyCode::KeyS) {
                wish -= cam_fwd;
            }
            if ctx.keys.pressed(KeyCode::KeyD) {
                wish += cam_right;
            }
            if ctx.keys.pressed(KeyCode::KeyA) {
                wish -= cam_right;
            }
            if ctx.keys.just_pressed(KeyCode::Space) {
                buttons |= 4;
            }
        }
        let w_xz = Vec2::new(wish.x, wish.z).normalize_or_zero();
        let y = if w_xz.length_squared() > 0.01 {
            w_xz.x.atan2(w_xz.y)
        } else {
            cam_fwd.x.atan2(cam_fwd.z)
        };
        (w_xz, y)
    };
    ctx.client.send_input([wish_xz.x, wish_xz.y], yaw, buttons);

    let player_pos = if let Ok((av_tf, mut driver)) = avatar_q.single_mut() {
        driver.has_weapon = matches!(
            ctx.survival.active_item(),
            HotbarItem::Rifle | HotbarItem::Hatchet | HotbarItem::Knife
        );
        driver.speed_norm = if wish_xz.length_squared() > 0.01 {
            if sprinting { 1.35 } else { 0.85 }
        } else {
            0.0
        };
        driver.state = if ctx.survival.attack_cooldown > 0.08 {
            AnimState::Attack
        } else if wish_xz.length_squared() > 0.01 {
            if sprinting { AnimState::Run } else { AnimState::Walk }
        } else {
            AnimState::Idle
        };
        av_tf.translation
    } else {
        Vec3::ZERO
    };

    if wish_xz.length_squared() > 0.01 {
        *footstep_timer -= dt;
        if *footstep_timer <= 0.0 {
            *footstep_timer = if sprinting { 0.30 } else { 0.46 };
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/footsteps_move_caodi_02.wav",
                ctx.profile.sfx_volume * 0.25,
            );
        }
    } else {
        *footstep_timer = 0.05;
    }

    let Some(terrain) = ctx.terrain_opt.as_deref() else {
        return;
    };
    let Some(shared) = ctx.shared_opt.as_deref() else {
        return;
    };
    if let Some(chunk) = terrain.chunk_name_at(player_pos.x, player_pos.z) {
        ctx.survival.active_chunk = chunk.to_owned();
    }

    // Check proximity vehicles, loot drops, and power generators (`[F]` Interact).
    let near_drop = ctx
        .client
        .loot_drops
        .values()
        .filter(|d| d.pos.distance(player_pos) <= 4.6)
        .min_by(|a, b| {
            a.pos
                .distance_squared(player_pos)
                .total_cmp(&b.pos.distance_squared(player_pos))
        })
        .cloned();
    let near_vehicle = ctx
        .client
        .vehicles
        .values()
        .filter(|v| v.driver_id == 0 && v.hp > 0 && v.pos.distance(player_pos) <= 7.5)
        .min_by(|a, b| {
            a.pos
                .distance_squared(player_pos)
                .total_cmp(&b.pos.distance_squared(player_pos))
        })
        .cloned();
    let near_gen = ctx
        .client
        .structures
        .values()
        .filter(|s| {
            s.kind == BuildPieceKind::Generator
                && s.hp > 0
                && s.pos.distance(player_pos) <= 6.5
        })
        .min_by(|a, b| {
            a.pos
                .distance_squared(player_pos)
                .total_cmp(&b.pos.distance_squared(player_pos))
        })
        .cloned();

    ctx.survival.aimed_loot_drop_id = near_drop.as_ref().map(|d| d.id);
    if ctx.keys.just_pressed(k_interact) {
        if driving_v.is_some() {
            ctx.client
                .send_world_op(OP_ENTER_EXIT_VEHICLE, 0, 0, 0, Vec3::ZERO);
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/vehicle_common_xia.wav",
                ctx.profile.sfx_volume * 0.85,
            );
        } else if let Some(ref veh) = near_vehicle {
            ctx.client.send_world_op(
                OP_ENTER_EXIT_VEHICLE,
                u32::from(veh.id),
                0,
                0,
                Vec3::ZERO,
            );
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/vehicle_common_shang.wav",
                ctx.profile.sfx_volume * 0.85,
            );
            let start_sfx = if veh.kind == VehicleKind::GunshipHeli {
                "audio/vehicle_zhishengji_qidong.wav"
            } else {
                "audio/vehicle_hanma_qidong.wav"
            };
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                start_sfx,
                ctx.profile.sfx_volume * 0.65,
            );
        } else if let Some(drop) = near_drop.as_ref() {
            ctx.client
                .send_world_op(OP_LOOT_DROP, u32::from(drop.id), 0, 0, Vec3::ZERO);
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/sheshi_chuwuxiang_kai.wav",
                ctx.profile.sfx_volume * 0.85,
            );
        } else if let Some(ref gen_st) = near_gen {
            ctx.client.send_world_op(
                OP_REFUEL_OR_TOGGLE_POWER,
                gen_st.id,
                0,
                0,
                Vec3::ZERO,
            );
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/sheshi_fadianji_yxz.wav",
                ctx.profile.sfx_volume * 0.70,
            );
        }
    }

    // Raycast world from camera center for crosshair context, build snap, doors, vehicles, and combat!
    let Some(cam_tf) = cam_tf_opt else {
        ctx.survival.ghost_valid = false;
        return;
    };
    let structures: Vec<_> = ctx.client.structures.values().cloned().collect();
    let enemies: Vec<ReplicatedEnemy> = ctx.client.enemies.values().cloned().collect();
    let nodes: Vec<_> = shared
        .base_nodes
        .iter()
        .map(|n| {
            let mut copy = n.clone();
            if let Some(&hp_ovr) = ctx.client.damaged_node_hps.get(&n.id) {
                copy.hp = hp_ovr;
            }
            copy
        })
        .collect();
    let local_id = ctx.client.local_player_id;
    let peers: Vec<(u16, Vec3)> = ctx
        .client
        .visible_peers
        .values()
        .filter(|p| Some(p.player_id) != local_id)
        .map(|p| (p.player_id, Vec3::from_array(p.pos)))
        .collect();
    let vehicles_vec: Vec<_> = ctx.client.vehicles.values().cloned().collect();

    let ray_origin = cam_tf.translation + cam_tf.forward() * 1.2;
    let look_ray = Ray3d::new(ray_origin, cam_tf.forward());
    let hit_world = raycast_world(
        terrain,
        &structures,
        &nodes,
        &enemies,
        &peers,
        look_ray,
        95.0,
        local_id,
    );
    let hit_veh = raycast_vehicles(look_ray, 95.0, &vehicles_vec);
    let hit = match (hit_world, hit_veh) {
        (Some(w), Some(v)) if v.dist < w.dist => Some(v),
        (Some(w), _) => Some(w),
        (None, v) => v,
    };

    ctx.survival.aimed_vehicle_id = match hit.map(|h| h.target) {
        Some(WorldHitTarget::Vehicle(vid)) if hit.is_some_and(|h| h.dist <= 16.0) => Some(vid),
        _ => None,
    };

    // Track aimed structure for [E/F] door / power / [U] upgrade context prompts.
    ctx.survival.aimed_structure_id = match hit.map(|h| h.target) {
        Some(WorldHitTarget::Structure(sid)) if hit.is_some_and(|h| h.dist <= 12.0) => Some(sid),
        _ => None,
    };

    if let Some(sid) = ctx.survival.aimed_structure_id
        && let Some(st) = ctx.client.structures.get(&sid)
    {
        if (ctx.keys.just_pressed(KeyCode::KeyE) || ctx.keys.just_pressed(k_interact))
            && st.kind == BuildPieceKind::Door
        {
            ctx.client
                .send_world_op(OP_TOGGLE_DOOR, sid, 0, 0, Vec3::ZERO);
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/sheshi_chuwuxiang_kai.wav",
                ctx.profile.sfx_volume * 0.75,
            );
        } else if ctx.keys.just_pressed(k_upgrade) {
            ctx.client
                .send_world_op(OP_UPGRADE_BUILD, sid, 0, 0, Vec3::ZERO);
            play_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/sheshi_tizi_fangxia.wav",
                ctx.profile.sfx_volume * 0.85,
            );
        }
    }

    if let Some(ref dv) = driving_v {
        ctx.survival.ghost_valid = false;
        let interact_lbl = KeybindProfile::short_label(&ctx.profile.keybinds.interact_key);
        ctx.survival.hovered_prompt = if dv.kind.is_aircraft() {
            format!(
                "Flying {} | [W/S] Thrust [A/D] Yaw [Space/Ctrl] Alt [LMB] Cannon [{interact_lbl}] Exit",
                dv.kind.label()
            )
        } else {
            format!(
                "Driving {} | [W/S] Drive [A/D] Steer [Shift] Boost [LMB] {} [{interact_lbl}] Exit",
                dv.kind.label(),
                if dv.kind == VehicleKind::ArmoredTank { "Cannon" } else { "Horn" }
            )
        };
        if ctx.mouse_btn.just_pressed(MouseButton::Left)
            && !ctx.survival.craft_menu_open
            && !any_ui_hovered
            && ctx.survival.attack_cooldown <= 0.0
        {
            ctx.survival.attack_cooldown = 0.55;
            if dv.kind == VehicleKind::OffroadJeep {
                play_sfx(
                    &mut commands,
                    &ctx.asset_server,
                    "audio/vehicle_hanma_laba.wav",
                    ctx.profile.sfx_volume * 0.85,
                );
            } else {
                let muzzle = dv.pos + Vec3::Y * 1.8;
                let end = hit.map(|h| h.point).unwrap_or(muzzle + cam_tf.forward() * 65.0);
                spawn_tracer_beam(&mut commands, shared, muzzle, end);
                spawn_particle_burst(&mut commands, shared, end, 10, 5.5, shared.spark_mat.clone());
                play_sfx(
                    &mut commands,
                    &ctx.asset_server,
                    "audio/gunshot_jiqiangpaotai_end.wav",
                    ctx.profile.sfx_volume * 0.90,
                );
                if let Some(h) = hit
                    && let WorldHitTarget::Enemy(eid) = h.target
                {
                    ctx.client
                        .send_world_op(OP_HIT_ENEMY, u32::from(eid), 0, 52, h.point);
                }
            }
        }
        return;
    }

    let active_item = ctx.survival.active_item();
    match active_item {
        HotbarItem::Build(kind) => {
            // Allow up to 48m camera-ray distance AND fallback forward horizon ground point so
            // players aiming horizontally can always snap and build foundations/turrets reliably!
            let build_hit = hit.filter(|h| h.dist <= 48.0).unwrap_or_else(|| {
                let fwd_pt = player_pos + cam_fwd * 4.5;
                let gy = terrain.ground_y(fwd_pt.x, fwd_pt.z);
                crate::world_physics::WorldRayHit {
                    dist: 8.0,
                    point: Vec3::new(fwd_pt.x, gy, fwd_pt.z),
                    normal: Vec3::Y,
                    target: WorldHitTarget::Terrain,
                }
            });

            let (snap_pos, snap_rot) = compute_build_snap(
                terrain,
                &structures,
                build_hit.point,
                build_hit.target,
                kind,
                ctx.survival.build_rot_steps,
            );
            let has_wood = ctx.client.wood >= kind.wood_cost();
            let slope_ok = kind != BuildPieceKind::Foundation || build_hit.normal.y >= 0.35;
            let above_sea = snap_pos.y >= SEA_LEVEL_Y - 2.0;
            let can_place = has_wood && slope_ok && above_sea;

            ctx.survival.ghost_valid = can_place;
            ctx.survival.ghost_pos = snap_pos;
            ctx.survival.ghost_rot_steps = snap_rot;
            ctx.survival.ghost_kind = kind;
            ctx.survival.hovered_prompt = if can_place {
                format!(
                    "[LMB] Place {} (-{} Wood) | [R] Rotate | [U] Upgrade | [E] Door",
                    kind.label(),
                    kind.wood_cost()
                )
            } else if !has_wood {
                format!(
                    "Need {} Wood for {} (Have {})",
                    kind.wood_cost(),
                    kind.label(),
                    ctx.client.wood
                )
            } else {
                format!("Cannot build {} on steep/submerged surface", kind.label())
            };

            if can_place
                && ctx.mouse_btn.just_pressed(MouseButton::Left)
                && !ctx.survival.craft_menu_open
                && !any_ui_hovered
            {
                ctx.client
                    .send_world_op(OP_PLACE_BUILD, 0, kind as u8, snap_rot, snap_pos);
                play_sfx(
                    &mut commands,
                    &ctx.asset_server,
                    "audio/sheshi_tizi_fangxia.wav",
                    ctx.profile.sfx_volume * 0.90,
                );
                spawn_particle_burst(
                    &mut commands,
                    shared,
                    snap_pos + Vec3::Y * 0.8,
                    8,
                    4.2,
                    shared.chip_mat.clone(),
                );
            }
        }
        HotbarItem::Rifle | HotbarItem::Hatchet | HotbarItem::Knife => {
            ctx.survival.ghost_valid = false;
            let interact_lbl = KeybindProfile::short_label(&ctx.profile.keybinds.interact_key);
            ctx.survival.hovered_prompt = if let Some(ref veh) = near_vehicle {
                format!(
                    "[{interact_lbl}] Board {} (Fuel {}%)",
                    veh.kind.label(),
                    veh.fuel_pct()
                )
            } else if let Some(drop) = near_drop.as_ref() {
                format!("[{interact_lbl}] Loot {}", drop.kind.label())
            } else if let Some(ref gen_st) = near_gen {
                format!(
                    "[{interact_lbl}] {} Generator (Grid {}%)",
                    if gen_st.is_open { "Toggle/Refuel" } else { "Start/Refuel" },
                    ctx.client.base_power_pct
                )
            } else {
                match hit.map(|h| (h.dist, h.target)) {
                    Some((d, WorldHitTarget::Vehicle(vid))) if d <= 32.0 => {
                        let v_lbl = ctx
                            .client
                            .vehicles
                            .get(&vid)
                            .map(|v| v.kind.label())
                            .unwrap_or("Vehicle");
                        format!("{v_lbl} ({d:.0}m) - Walk closer & press [{interact_lbl}] to board")
                    }
                    Some((d, WorldHitTarget::Enemy(eid))) if d <= 85.0 => {
                        let e_lbl = enemies
                            .iter()
                            .find(|e| e.id == eid)
                            .map(|e| format!("{} ({} HP)", e.kind.label(), e.hp))
                            .unwrap_or_else(|| format!("Enemy #{eid}"));
                        format!("[LMB] Attack {e_lbl} ({d:.0}m)")
                    }
                    Some((d, WorldHitTarget::ResourceNode(nid))) if d <= 22.0 => {
                        let kind_label = nodes
                            .iter()
                            .find(|n| n.id == nid)
                            .map(|n| n.kind.label())
                            .unwrap_or("Node");
                        format!("[LMB] Harvest {kind_label}")
                    }
                    Some((d, WorldHitTarget::Structure(sid))) if d <= 12.0 => {
                        let st = ctx.client.structures.get(&sid);
                        if st.is_some_and(|s| s.kind == BuildPieceKind::Door) {
                            "[E] Open/Close Door | [U] Upgrade Tier".to_owned()
                        } else if st.is_some_and(|s| s.kind == BuildPieceKind::Generator) {
                            format!(
                                "[{interact_lbl}] Refuel/Toggle Generator | Grid Power {}%",
                                ctx.client.base_power_pct
                            )
                        } else {
                            format!(
                                "[U] Upgrade to Lv.{} | Aim Build Piece",
                                st.map(|s| (s.tier + 1).min(4)).unwrap_or(2)
                            )
                        }
                    }
                    Some((d, WorldHitTarget::Player(pid))) if d <= 80.0 => {
                        format!("[LMB] Fire at Player #{pid} ({d:.0}m)")
                    }
                    _ => format!(
                        "[LMB] Attack/Gather | [4-0/T/G] Build | [{interact_lbl}] Vehicle/Power | [Tab] Craft"
                    ),
                }
            };

            let trigger = if active_item == HotbarItem::Rifle {
                ctx.mouse_btn.pressed(MouseButton::Left)
            } else {
                ctx.mouse_btn.just_pressed(MouseButton::Left)
            };
            if trigger
                && !any_ui_hovered
                && !ctx.survival.craft_menu_open
                && ctx.survival.attack_cooldown <= 0.0
            {
                let is_rifle = active_item == HotbarItem::Rifle;
                if is_rifle && ctx.client.ammo == 0 {
                    ctx.survival.hovered_prompt =
                        "Out of Ammo! Press [Tab] to Craft 15x Ammo".to_owned();
                    return;
                }
                ctx.survival.attack_cooldown = if is_rifle { 0.16 } else { 0.40 };
                let tool_code = match active_item {
                    HotbarItem::Hatchet => 1_u8,
                    HotbarItem::Knife => 2_u8,
                    _ => 0_u8,
                };
                if is_rifle {
                    ctx.client
                        .send_world_op(OP_SPEND_AMMO, 0, 0, 0, Vec3::ZERO);
                    let start = player_pos + Vec3::new(0.0, 1.45, 0.0) + cam_right * 0.25;
                    let end = hit
                        .map(|h| h.point)
                        .unwrap_or(ray_origin + *cam_tf.forward() * 65.0);
                    spawn_tracer_beam(&mut commands, shared, start, end);
                    spawn_particle_burst(&mut commands, shared, start, 5, 3.0, shared.spark_mat.clone());
                    let want_prop = ctx
                        .survival
                        .equipped_prop_override
                        .unwrap_or(ctx.profile.equipped_prop_idx);
                    let prop_name = select_weapon_prop(&ctx.manifest, want_prop)
                        .map(|p| p.name.as_str())
                        .unwrap_or("wq_bq_1");
                    play_sfx(
                        &mut commands,
                        &ctx.asset_server,
                        gunshot_audio_for_prop(prop_name),
                        ctx.profile.sfx_volume * 0.85,
                    );
                } else {
                    play_sfx(
                        &mut commands,
                        &ctx.asset_server,
                        "audio/combat_banzhuan_body2.wav",
                        ctx.profile.sfx_volume * 0.85,
                    );
                }

                let max_tool_range = if is_rifle { 85.0 } else { 4.8 };
                if let Some(h) = hit
                    && h.dist <= max_tool_range
                {
                    match h.target {
                        WorldHitTarget::ResourceNode(nid) => {
                            let sfx = nodes
                                .iter()
                                .find(|n| n.id == nid)
                                .map(|n| match n.kind {
                                    crate::world_physics::ResourceNodeKind::OreBoulder => {
                                        "audio/sheshi_cuihui_shitou.wav"
                                    }
                                    crate::world_physics::ResourceNodeKind::SupplyCrate => {
                                        "audio/common_mensui.wav"
                                    }
                                    _ => "audio/gunattack_mutou.wav",
                                })
                                .unwrap_or("audio/gunattack_mutou.wav");
                            play_sfx(&mut commands, &ctx.asset_server, sfx, ctx.profile.sfx_volume * 0.85);
                            spawn_particle_burst(
                                &mut commands,
                                shared,
                                h.point,
                                7,
                                4.5,
                                shared.chip_mat.clone(),
                            );
                            ctx.client.send_world_op(
                                OP_HARVEST_NODE,
                                u32::from(nid),
                                tool_code,
                                0,
                                h.point,
                            );
                        }
                        WorldHitTarget::Enemy(eid) => {
                            let dmg = if is_rifle { 34 } else { 32 };
                            spawn_particle_burst(
                                &mut commands,
                                shared,
                                h.point,
                                8,
                                4.8,
                                shared.blood_mat.clone(),
                            );
                            ctx.client.send_world_op(
                                OP_HIT_ENEMY,
                                u32::from(eid),
                                tool_code,
                                dmg,
                                h.point,
                            );
                        }
                        WorldHitTarget::Player(pid) => {
                            let dmg = if is_rifle { 34 } else { 28 };
                            spawn_particle_burst(
                                &mut commands,
                                shared,
                                h.point,
                                6,
                                4.2,
                                shared.spark_mat.clone(),
                            );
                            ctx.client.send_world_op(
                                OP_HIT_PLAYER,
                                u32::from(pid),
                                tool_code,
                                dmg,
                                h.point,
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn stream_terrain_chunks_system(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    terrain_opt: Option<Res<TerrainColliderGrid>>,
    avatar_q: Query<&Transform, With<LocalArenaAvatar>>,
    chunk_q: Query<(Entity, &StreamedChunkEntity)>,
) {
    let Some(terrain) = terrain_opt else {
        return;
    };
    let center = avatar_q
        .single()
        .map(|tf| Vec2::new(tf.translation.x, tf.translation.z))
        .unwrap_or(Vec2::ZERO);

    let mut active: HashMap<String, Entity> = HashMap::new();
    for (e, chunk) in &chunk_q {
        active.insert(chunk.name.clone(), e);
    }

    for meta in &terrain.chunk_bounds {
        let clamped = center.clamp(meta.min_xz, meta.max_xz);
        let dist = center.distance(clamped);
        if let Some(&entity) = active.get(&meta.name) {
            if dist > CHUNK_STREAM_OUT_RADIUS {
                commands.entity(entity).try_despawn();
            }
        } else if dist <= CHUNK_STREAM_IN_RADIUS {
            let h: Handle<WorldAsset> = asset_server.load(format!("{}#Scene0", meta.asset_path));
            commands.spawn((
                InGameRoot,
                DespawnOnExit(AppState::InGame),
                StreamedChunkEntity {
                    name: meta.name.clone(),
                },
                Name::new(format!("TerrainChunk_{}", meta.name)),
                WorldAssetRoot(h),
                Transform::IDENTITY,
            ));
        }
    }
}

fn sync_resource_nodes_system(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    client: Res<NetClientState>,
    shared_opt: Option<Res<ArenaSharedAssets>>,
    avatar_q: Query<&Transform, With<LocalArenaAvatar>>,
    mut existing_q: Query<(Entity, &mut SpawnedResourceVisual, &mut Visibility)>,
) {
    let Some(shared) = shared_opt else {
        return;
    };
    let center = avatar_q
        .single()
        .map(|tf| Vec2::new(tf.translation.x, tf.translation.z))
        .unwrap_or(Vec2::ZERO);

    let mut present: HashMap<u16, Entity> = HashMap::new();
    for (entity, mut vis_state, mut vis) in &mut existing_q {
        let depleted = client
            .damaged_node_hps
            .get(&vis_state.id)
            .is_some_and(|&hp| hp == 0);
        if depleted != vis_state.depleted {
            vis_state.depleted = depleted;
            *vis = if depleted {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
        }
        present.insert(vis_state.id, entity);
    }

    for node in &shared.base_nodes {
        let d = center.distance(Vec2::new(node.pos.x, node.pos.z));
        if d > NODE_RENDER_RADIUS || present.contains_key(&node.id) {
            continue;
        }
        let depleted = client
            .damaged_node_hps
            .get(&node.id)
            .is_some_and(|&hp| hp == 0);
        let h: Handle<WorldAsset> =
            asset_server.load(format!("{}#Scene0", node.kind.glb_asset_path()));
        commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            SpawnedResourceVisual {
                id: node.id,
                depleted,
            },
            Name::new(format!("ResourceNode_{}", node.id)),
            WorldAssetRoot(h),
            if depleted {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            },
            Transform::from_translation(node.pos).with_rotation(Quat::from_rotation_y(node.yaw)),
        ));
    }
}

#[derive(SystemParam)]
struct StructureSyncCtx<'w> {
    manifest: Res<'w, AssetManifest>,
    asset_server: Res<'w, AssetServer>,
    profile: Res<'w, PlayerProfileSave>,
    client: Res<'w, NetClientState>,
    shared_opt: Option<Res<'w, ArenaSharedAssets>>,
}

#[allow(clippy::too_many_arguments)]
fn sync_replicated_structures_system(
    mut commands: Commands,
    ctx: StructureSyncCtx,
    mut existing_q: Query<(Entity, &mut SpawnedStructureVisual, Option<&Children>)>,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
    mut vis_q: Query<&mut Visibility>,
    mut tf_q: Query<&mut Transform>,
) {
    let mut present = HashMap::new();
    for (entity, mut vis_meta, children_opt) in &mut existing_q {
        let Some(rep) = ctx.client.structures.get(&vis_meta.id) else {
            commands.entity(entity).try_despawn();
            continue;
        };
        present.insert(vis_meta.id, entity);
        let has_children = children_opt.is_some_and(|c| !c.is_empty());

        // Trigger Phase 4 Auto-Turret & Phase 5 Tesla-Turret muzzle flash, tracer/lightning, and spatial SFX.
        if matches!(
            rep.kind,
            BuildPieceKind::Turret | BuildPieceKind::ElectricTurret
        ) && !vis_meta.is_open
            && rep.is_open
        {
            vis_meta.is_open = true;
            let is_tesla = rep.kind == BuildPieceKind::ElectricTurret;
            let muzzle = rep.pos + Vec3::new(0.0, if is_tesla { 1.75 } else { 1.35 }, 0.0);
            if let Some(shared) = ctx.shared_opt.as_deref() {
                let target_pt = ctx
                    .client
                    .enemies
                    .values()
                    .filter(|e| e.hp > 0)
                    .min_by(|a, b| {
                        a.pos
                            .distance_squared(rep.pos)
                            .total_cmp(&b.pos.distance_squared(rep.pos))
                    })
                    .map(|e| e.pos + Vec3::Y * 1.1)
                    .unwrap_or(muzzle + Vec3::Z * 10.0);
                if is_tesla {
                    spawn_colored_beam(
                        &mut commands,
                        shared,
                        muzzle,
                        target_pt,
                        shared.electric_mat.clone(),
                        1.8,
                        0.14,
                    );
                    spawn_particle_burst(
                        &mut commands,
                        shared,
                        target_pt,
                        7,
                        4.2,
                        shared.electric_mat.clone(),
                    );
                } else {
                    spawn_tracer_beam(&mut commands, shared, muzzle, target_pt);
                    spawn_particle_burst(
                        &mut commands,
                        shared,
                        muzzle,
                        6,
                        3.8,
                        shared.spark_mat.clone(),
                    );
                }
            }
            let sfx_path = if is_tesla {
                "audio/sheshi_diancipaota_yxz.wav"
            } else {
                "audio/gunshot_jiqiangpaotai_end.wav"
            };
            play_spatial_sfx(
                &mut commands,
                &ctx.asset_server,
                sfx_path,
                ctx.profile.sfx_volume * 0.85,
                muzzle,
            );
        } else if rep.kind == BuildPieceKind::Generator && !vis_meta.is_open && rep.is_open {
            vis_meta.is_open = true;
            let gen_pos = rep.pos + Vec3::new(0.0, 1.1, 0.0);
            if let Some(shared) = ctx.shared_opt.as_deref() {
                spawn_particle_burst(&mut commands, shared, gen_pos, 5, 2.8, shared.electric_mat.clone());
            }
            play_spatial_sfx(
                &mut commands,
                &ctx.asset_server,
                "audio/sheshi_fadianji_yxz.wav",
                ctx.profile.sfx_volume * 0.70,
                gen_pos,
            );
        } else if matches!(
            rep.kind,
            BuildPieceKind::Turret | BuildPieceKind::ElectricTurret
        ) {
            vis_meta.is_open = rep.is_open;
        }

        if !matches!(
            rep.kind,
            BuildPieceKind::Turret | BuildPieceKind::ElectricTurret
        )
            && has_children
            && (!vis_meta.synced || vis_meta.tier != rep.tier || vis_meta.is_open != rep.is_open)
        {
            vis_meta.tier = rep.tier;
            vis_meta.is_open = rep.is_open;
            vis_meta.synced = true;
            apply_structure_tier_and_door_pose(
                entity,
                rep.tier,
                rep.is_open,
                &children_q,
                &names_q,
                &mut vis_q,
                &mut tf_q,
            );
        }
    }

    for (&id, rep) in &ctx.client.structures {
        if present.contains_key(&id) {
            continue;
        }
        let stem = rep.kind.glb_stem();
        let asset_path = ctx
            .manifest
            .build_parts
            .iter()
            .chain(ctx.manifest.props.iter())
            .find(|p| p.name == stem)
            .map(|p| p.asset_path.clone())
            .unwrap_or_else(|| format!("models/{stem}.glb"));
        let h: Handle<WorldAsset> = ctx.asset_server.load(format!("{asset_path}#Scene0"));
        commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            SpawnedStructureVisual {
                id,
                tier: rep.tier,
                is_open: rep.is_open,
                synced: false,
            },
            Name::new(format!("Built_{stem}_{id}")),
            WorldAssetRoot(h),
            Visibility::default(),
            Transform::from_translation(rep.pos)
                .with_rotation(Quat::from_rotation_y(rep.yaw_radians())),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_replicated_vehicles_system(
    mut commands: Commands,
    time: Res<Time>,
    asset_server: Res<AssetServer>,
    profile: Res<PlayerProfileSave>,
    client: Res<NetClientState>,
    shared_opt: Option<Res<ArenaSharedAssets>>,
    mut existing_q: Query<(Entity, &mut SpawnedVehicleVisual, &mut Transform, Option<&Children>)>,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
    mut part_tf_q: Query<&mut Transform, Without<SpawnedVehicleVisual>>,
) {
    let dt = time.delta_secs();
    let alpha = (15.0 * dt).clamp(0.0, 1.0);
    let mut present = HashMap::new();

    for (entity, mut meta, mut tf, children_opt) in &mut existing_q {
        let Some(rep) = client.vehicles.get(&meta.id) else {
            commands.entity(entity).try_despawn();
            continue;
        };
        present.insert(meta.id, entity);
        let speed_est = (rep.pos - tf.translation).length() / dt.max(0.001);
        tf.translation = tf.translation.lerp(rep.pos, alpha);
        tf.rotation = tf.rotation.slerp(Quat::from_rotation_y(rep.yaw), alpha);

        let is_driven = rep.driver_id != 0;
        meta.engine_sfx_cd = (meta.engine_sfx_cd - dt).max(0.0);
        if is_driven && speed_est > 1.8 && meta.engine_sfx_cd <= 0.0 {
            meta.engine_sfx_cd = 1.6;
            let engine_sfx = if meta.kind == VehicleKind::GunshipHeli {
                "audio/vehicle_zhishengji_feixingman.wav"
            } else {
                "audio/vehicle_hanma_fadong.wav"
            };
            play_spatial_sfx(
                &mut commands,
                &asset_server,
                engine_sfx,
                profile.sfx_volume * 0.45,
                rep.pos,
            );
            if let Some(shared) = shared_opt.as_deref()
                && !meta.kind.is_aircraft()
            {
                spawn_particle_burst(&mut commands, shared, rep.pos + Vec3::Y * 0.3, 3, 2.2, shared.chip_mat.clone());
            }
        }
        meta.was_driven = is_driven;

        // Spin helicopter main rotor blades gently when engine is running.
        if meta.kind == VehicleKind::GunshipHeli
            && is_driven
            && let Some(children) = children_opt
        {
            meta.rotor_phase += dt * 18.0;
            spin_heli_rotors_recursive(children, &children_q, &names_q, &mut part_tf_q, dt * 18.0);
        }
    }

    for (&id, rep) in &client.vehicles {
        if present.contains_key(&id) {
            continue;
        }
        let stem = rep.kind.glb_stem();
        let h: Handle<WorldAsset> = asset_server.load(format!("models/{stem}.glb#Scene0"));
        commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            SpawnedVehicleVisual {
                id,
                kind: rep.kind,
                rotor_phase: 0.0,
                engine_sfx_cd: 0.0,
                was_driven: rep.driver_id != 0,
            },
            Name::new(format!("Vehicle_{stem}_{id}")),
            WorldAssetRoot(h),
            Visibility::default(),
            Transform::from_translation(rep.pos).with_rotation(Quat::from_rotation_y(rep.yaw)),
        ));
    }
}

fn spin_heli_rotors_recursive(
    children: &Children,
    children_q: &Query<&Children>,
    names_q: &Query<&Name>,
    tf_q: &mut Query<&mut Transform, Without<SpawnedVehicleVisual>>,
    delta_rad: f32,
) {
    for child in children.iter() {
        if let Ok(name) = names_q.get(child) {
            let lower = name.as_str().to_ascii_lowercase();
            if (lower.contains("luoxuan") || lower.contains("rotor") || lower.contains("prop"))
                && let Ok(mut c_tf) = tf_q.get_mut(child)
            {
                c_tf.rotate_y(delta_rad);
            }
        }
        if let Ok(grand) = children_q.get(child) {
            spin_heli_rotors_recursive(grand, children_q, names_q, tf_q, delta_rad);
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync_replicated_enemies_system(
    mut commands: Commands,
    time: Res<Time>,
    manifest: Res<AssetManifest>,
    profile: Res<PlayerProfileSave>,
    asset_server: Res<AssetServer>,
    client: Res<NetClientState>,
    mut existing_q: Query<(
        Entity,
        &mut SpawnedEnemyVisual,
        &mut Transform,
        &mut Visibility,
        &mut CharacterAnimDriver,
    )>,
) {
    let alpha = (14.0 * time.delta_secs()).clamp(0.0, 1.0);
    let mut present = HashMap::new();
    for (entity, mut meta, mut tf, mut vis, mut driver) in &mut existing_q {
        let Some(rep) = client.enemies.get(&meta.id) else {
            commands.entity(entity).try_despawn();
            continue;
        };
        present.insert(meta.id, entity);
        let dead = rep.hp == 0;
        if !meta.was_dead && dead {
            play_spatial_sfx(
                &mut commands,
                &asset_server,
                "audio/monster_jiangshi3_die.wav",
                profile.sfx_volume * 0.85,
                rep.pos,
            );
        }
        if !dead && !meta.was_attacking && rep.is_attacking {
            play_spatial_sfx(
                &mut commands,
                &asset_server,
                "audio/monster_jiangshi1_gj.wav",
                profile.sfx_volume * 0.75,
                rep.pos,
            );
        }
        meta.was_dead = dead;
        meta.was_attacking = rep.is_attacking;

        *vis = if dead { Visibility::Hidden } else { Visibility::Inherited };
        tf.translation = tf.translation.lerp(rep.pos, alpha);
        tf.rotation = tf.rotation.slerp(Quat::from_rotation_y(rep.yaw), alpha);
        driver.has_weapon = false;
        driver.speed_norm = if rep.is_moving { 0.9 } else { 0.0 };
        driver.state = if rep.is_attacking {
            AnimState::Attack
        } else if rep.is_moving {
            AnimState::Walk
        } else {
            AnimState::Idle
        };
    }

    for (&id, rep) in &client.enemies {
        if present.contains_key(&id) {
            continue;
        }
        let stem = rep.kind.glb_stem();
        let asset_path = manifest
            .characters
            .iter()
            .find(|c| c.name == stem)
            .map(|c| c.asset_path.clone())
            .unwrap_or_else(|| format!("models/{stem}.glb"));
        let scene_h: Handle<WorldAsset> = asset_server.load(format!("{asset_path}#Scene0"));
        let gltf_h: Handle<Gltf> = asset_server.load(asset_path);
        let scale = if rep.kind == EnemyKind::PortBrute { 1.15 } else { 1.0 };
        let mut ec = commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            SpawnedEnemyVisual {
                id,
                kind: rep.kind,
                was_dead: rep.hp == 0,
                was_attacking: rep.is_attacking,
                gltf_handle: gltf_h,
                anim_bound: false,
            },
            CharacterAnimDriver {
                has_weapon: false,
                ..default()
            },
            Name::new(format!("Enemy_{stem}_{id}")),
            if rep.hp == 0 {
                Visibility::Hidden
            } else {
                Visibility::default()
            },
            Transform::from_translation(rep.pos)
                .with_rotation(Quat::from_rotation_y(rep.yaw))
                .with_scale(Vec3::splat(scale)),
        ));
        if rep.kind == EnemyKind::PortBossMech {
            ec.insert(UsesBakedGltfAnim);
        }
        ec.with_children(|p| {
            p.spawn(WorldAssetRoot(scene_h));
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn bind_and_drive_enemy_anims_system(
    mut commands: Commands,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    children_q: Query<&Children>,
    mut enemies_q: Query<(Entity, &mut SpawnedEnemyVisual, &CharacterAnimDriver)>,
    mut players_q: Query<(
        &mut AnimationPlayer,
        Option<&BoundAnimationSet>,
        Option<&mut AnimationTransitions>,
    )>,
) {
    for (root, mut meta, driver) in &mut enemies_q {
        let Some(gltf) = gltfs.get(&meta.gltf_handle) else {
            continue;
        };
        if gltf.animations.is_empty() {
            meta.anim_bound = true;
            continue;
        }
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if let Ok((mut player, bound_opt, trans_opt)) = players_q.get_mut(e) {
                if !meta.anim_bound && bound_opt.is_none() {
                    if let Some((graph_h, transitions, set)) = build_gltf_animation_set(
                        gltf,
                        meta.kind.glb_stem(),
                        BoundActorRole::Other,
                        driver.state,
                        0,
                        f32::from(meta.id) * 0.35,
                        false,
                        &mut player,
                        &mut graphs,
                    ) {
                        commands.entity(e).insert((graph_h, transitions, set));
                        meta.anim_bound = true;
                    }
                } else if let (Some(bound), Some(mut transitions)) = (bound_opt, trans_opt)
                    && let Some((_, clip_node)) = pick_clip_for_state(driver.state, &bound.nodes)
                    && !player.is_playing_animation(clip_node)
                {
                    transitions
                        .play(&mut player, clip_node, std::time::Duration::from_millis(180))
                        .repeat();
                }
            }
            if let Ok(ch) = children_q.get(e) {
                for c in ch.iter() {
                    stack.push(c);
                }
            }
        }
    }
}

fn sync_replicated_loot_drops_system(
    mut commands: Commands,
    time: Res<Time>,
    manifest: Res<AssetManifest>,
    asset_server: Res<AssetServer>,
    client: Res<NetClientState>,
    mut existing_q: Query<(Entity, &SpawnedLootVisual, &mut Transform)>,
) {
    let t = time.elapsed_secs();
    let mut present = HashMap::new();
    for (entity, loot, mut tf) in &mut existing_q {
        if let Some(rep) = client.loot_drops.get(&loot.id) {
            present.insert(loot.id, entity);
            tf.translation.x = rep.pos.x;
            tf.translation.z = rep.pos.z;
            tf.translation.y = loot.base_y + 0.14 + (t * 2.4 + f32::from(loot.id)).sin() * 0.08;
            tf.rotate_y(time.delta_secs() * 1.1);
        } else {
            commands.entity(entity).try_despawn();
        }
    }

    for (&id, rep) in &client.loot_drops {
        if present.contains_key(&id) {
            continue;
        }
        let stem = rep.kind.glb_stem();
        let asset_path = manifest
            .props
            .iter()
            .find(|p| p.name == stem)
            .map(|p| p.asset_path.clone())
            .unwrap_or_else(|| format!("models/{stem}.glb"));
        let h: Handle<WorldAsset> = asset_server.load(format!("{asset_path}#Scene0"));
        commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            SpawnedLootVisual {
                id,
                base_y: rep.pos.y,
            },
            Name::new(format!("LootDrop_{stem}_{id}")),
            WorldAssetRoot(h),
            Transform::from_translation(rep.pos + Vec3::Y * 0.14),
        ));
    }
}

type AvatarWeaponQuery<'w, 's> = Query<
    'w,
    's,
    (Entity, Option<&'static Children>),
    With<LocalArenaAvatar>,
>;

#[derive(SystemParam)]
struct WeaponSocketCtx<'w> {
    manifest: Res<'w, AssetManifest>,
    profile: Res<'w, PlayerProfileSave>,
    survival: Res<'w, SurvivalSessionState>,
    asset_server: Res<'w, AssetServer>,
}

fn sync_ingame_weapon_socket(
    mut commands: Commands,
    ctx: WeaponSocketCtx,
    avatar_q: AvatarWeaponQuery,
    mount_q: Query<(Entity, &LocalArenaWeaponMount)>,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
) {
    let want_prop = ctx
        .survival
        .equipped_prop_override
        .unwrap_or(ctx.profile.equipped_prop_idx);
    if let Ok((_, mount)) = mount_q.single()
        && mount.prop_idx == want_prop
    {
        return;
    }

    let Ok((avatar_entity, root_children)) = avatar_q.single() else {
        return;
    };
    let Some(children) = root_children else {
        return;
    };
    if children.is_empty() {
        return;
    }
    let Some(socket) = find_hand_socket_entity(avatar_entity, &children_q, &names_q) else {
        return;
    };

    for (e, _) in &mount_q {
        commands.entity(e).try_despawn();
    }

    if let Some(weapon) = select_weapon_prop(&ctx.manifest, want_prop) {
        let h: Handle<WorldAsset> = ctx
            .asset_server
            .load(format!("{}#Scene0", weapon.asset_path));
        commands.spawn((
            LocalArenaWeaponMount {
                prop_idx: want_prop,
            },
            Name::new(format!("InGameWeapon_{}", weapon.name)),
            WorldAssetRoot(h),
            ChildOf(socket),
            Transform::IDENTITY,
        ));
    }
}

fn update_build_ghost_and_tracers(
    mut commands: Commands,
    time: Res<Time>,
    survival: Res<SurvivalSessionState>,
    shared_opt: Option<Res<ArenaSharedAssets>>,
    mut ghost_q: Query<
        (&mut Transform, &mut Visibility, &mut MeshMaterial3d<StandardMaterial>),
        With<BuildGhostPreview>,
    >,
    mut tracer_q: Query<(Entity, &mut TracerFx)>,
    mut particle_q: Query<
        (Entity, &mut ParticleBurstFx, &mut Transform),
        Without<BuildGhostPreview>,
    >,
) {
    let dt = time.delta_secs();
    for (e, mut fx) in &mut tracer_q {
        fx.ttl -= dt;
        if fx.ttl <= 0.0 {
            commands.entity(e).try_despawn();
        }
    }
    for (e, mut p, mut tf) in &mut particle_q {
        p.ttl -= dt;
        if p.ttl <= 0.0 {
            commands.entity(e).try_despawn();
        } else {
            p.vel.y -= 13.5 * dt;
            tf.translation += p.vel * dt;
            let s = (p.ttl / p.max_ttl).clamp(0.15, 1.0);
            tf.scale = Vec3::splat(s);
        }
    }

    let Some(shared) = shared_opt else {
        return;
    };
    let Ok((mut tf, mut vis, mut mat)) = ghost_q.single_mut() else {
        return;
    };

    if matches!(survival.active_item(), HotbarItem::Build(_)) && survival.ghost_pos != Vec3::ZERO {
        let half = survival.ghost_kind.local_half_extents();
        let y_lift = match survival.ghost_kind {
            BuildPieceKind::Wall
            | BuildPieceKind::DoorFrame
            | BuildPieceKind::Door
            | BuildPieceKind::Turret => half.y,
            _ => 0.0,
        };
        tf.translation = survival.ghost_pos + Vec3::new(0.0, y_lift, 0.0);
        tf.rotation = Quat::from_rotation_y(
            f32::from(survival.ghost_rot_steps & 3) * std::f32::consts::FRAC_PI_2,
        );
        tf.scale = half * 2.0;
        *mat = MeshMaterial3d(if survival.ghost_valid {
            shared.ghost_mat_ok.clone()
        } else {
            shared.ghost_mat_bad.clone()
        });
        *vis = Visibility::Visible;
    } else {
        *vis = Visibility::Hidden;
    }
}

fn reconcile_local_avatar_system(
    time: Res<Time>,
    client: Res<NetClientState>,
    survival: Res<SurvivalSessionState>,
    mut local_q: Query<(&mut Transform, &mut Visibility), With<LocalArenaAvatar>>,
) {
    let Some(srv_local) = client.local_player_state() else {
        return;
    };
    let Ok((mut tf, mut vis)) = local_q.single_mut() else {
        return;
    };
    let target = Vec3::new(srv_local.pos[0], srv_local.pos[1], srv_local.pos[2]);
    let alpha = (18.0 * time.delta_secs()).clamp(0.0, 1.0);
    tf.translation = tf.translation.lerp(target, alpha);
    tf.rotation = Quat::from_rotation_y(srv_local.yaw);
    let want_vis = if survival.driving_vehicle_id != 0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want_vis {
        *vis = want_vis;
    }
}

fn replicate_remote_peers_system(
    mut commands: Commands,
    time: Res<Time>,
    client: Res<NetClientState>,
    shared_opt: Option<Res<ArenaSharedAssets>>,
    mut existing_q: Query<(Entity, &RemotePeerAvatar, &mut Transform)>,
) {
    let Some(shared) = shared_opt else {
        return;
    };
    let local_id = client.local_player_id;
    let alpha = (14.0 * time.delta_secs()).clamp(0.0, 1.0);

    let mut active_remote: HashMap<u16, &NetPlayerState> = HashMap::new();
    for (&id, state) in &client.visible_peers {
        if local_id != Some(id) {
            active_remote.insert(id, state);
        }
    }

    let mut present = HashMap::new();
    for (entity, peer, mut tf) in &mut existing_q {
        if let Some(state) = active_remote.get(&peer.player_id) {
            let target = Vec3::new(state.pos[0], state.pos[1] + 0.85, state.pos[2]);
            tf.translation = tf.translation.lerp(target, alpha);
            tf.rotation = Quat::from_rotation_y(state.yaw);
            present.insert(peer.player_id, entity);
        } else {
            commands.entity(entity).try_despawn();
        }
    }

    for (&id, state) in &active_remote {
        if present.contains_key(&id) {
            continue;
        }
        let mat = if (state.flags & NetPlayerState::FLAG_BOT) != 0 {
            shared.bot_mat.clone()
        } else {
            shared.human_mat.clone()
        };
        commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            RemotePeerAvatar { player_id: id },
            Name::new(format!("RemotePeer_{id}")),
            Mesh3d(shared.peer_mesh.clone()),
            MeshMaterial3d(mat),
            Transform::from_xyz(state.pos[0], state.pos[1] + 0.85, state.pos[2])
                .with_rotation(Quat::from_rotation_y(state.yaw)),
        ));
    }
}

#[derive(SystemParam)]
struct CamInputCtx<'w> {
    time: Res<'w, Time>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    mouse_btn: Res<'w, ButtonInput<MouseButton>>,
    motion: Res<'w, AccumulatedMouseMotion>,
    scroll: Res<'w, AccumulatedMouseScroll>,
    survival: Res<'w, SurvivalSessionState>,
    terrain_opt: Option<Res<'w, TerrainColliderGrid>>,
}

fn arena_camera_controller_system(
    ctx: CamInputCtx,
    avatar_q: Query<&Transform, (With<LocalArenaAvatar>, Without<ArenaFollowCamera>)>,
    mut cam_q: Query<(&mut Transform, &mut ArenaFollowCamera)>,
) {
    let Ok((mut tf, mut rig)) = cam_q.single_mut() else {
        return;
    };
    let dt = ctx.time.delta_secs();
    if !ctx.survival.craft_menu_open {
        if ctx.mouse_btn.pressed(MouseButton::Right) || ctx.mouse_btn.pressed(MouseButton::Middle) {
            rig.yaw -= ctx.motion.delta.x * 0.007;
            rig.pitch = (rig.pitch + ctx.motion.delta.y * 0.007).clamp(-0.55, 1.30);
        }
        let yaw_turn = f32::from(ctx.keys.pressed(KeyCode::ArrowLeft))
            - f32::from(ctx.keys.pressed(KeyCode::ArrowRight));
        let pitch_turn = f32::from(ctx.keys.pressed(KeyCode::ArrowDown))
            - f32::from(ctx.keys.pressed(KeyCode::ArrowUp));
        rig.yaw += yaw_turn * 2.1 * dt;
        rig.pitch = (rig.pitch + pitch_turn * 1.6 * dt).clamp(-0.55, 1.30);
    }

    let scroll_y = match ctx.scroll.unit {
        MouseScrollUnit::Line => ctx.scroll.delta.y * 0.8,
        MouseScrollUnit::Pixel => ctx.scroll.delta.y * 0.02,
    };
    rig.radius = (rig.radius - scroll_y).clamp(3.2, 42.0);

    if let Ok(av_tf) = avatar_q.single() {
        let driving = ctx.survival.driving_vehicle_id != 0;
        let shoulder = if driving {
            Vec3::ZERO
        } else {
            Vec3::new(rig.yaw.cos(), 0.0, -rig.yaw.sin()) * 0.42
        };
        let extra_h = if driving { 2.15 } else { 1.72 };
        let desired = av_tf.translation + shoulder + Vec3::new(0.0, extra_h, 0.0);
        rig.focus = rig.focus.lerp(desired, (14.0 * dt).min(1.0));
    }

    let drive_radius_mult = if ctx.survival.driving_vehicle_id != 0 {
        1.55
    } else {
        1.0
    };
    let eff_radius = (rig.radius * drive_radius_mult).clamp(4.2, 48.0);
    let cos_p = rig.pitch.cos();
    let offset = Vec3::new(
        eff_radius * cos_p * rig.yaw.sin(),
        eff_radius * rig.pitch.sin(),
        eff_radius * cos_p * rig.yaw.cos(),
    );
    let mut cam_pos = rig.focus + offset;
    if let Some(terrain) = ctx.terrain_opt.as_deref() {
        let ground = terrain.ground_y(cam_pos.x, cam_pos.z);
        if cam_pos.y < ground + 0.65 {
            cam_pos.y = ground + 0.65;
        }
    }
    *tf = Transform::from_translation(cam_pos).looking_at(rig.focus, Vec3::Y);
}

fn spawn_tracer_beam(
    commands: &mut Commands,
    shared: &ArenaSharedAssets,
    start: Vec3,
    end: Vec3,
) {
    spawn_colored_beam(commands, shared, start, end, shared.tracer_mat.clone(), 1.0, 0.08);
}

fn spawn_colored_beam(
    commands: &mut Commands,
    shared: &ArenaSharedAssets,
    start: Vec3,
    end: Vec3,
    mat: Handle<StandardMaterial>,
    thickness: f32,
    ttl: f32,
) {
    let delta = end - start;
    let len = delta.length().max(0.2);
    let mid = (start + end) * 0.5;
    let dir = delta / len;
    let rot = Quat::from_rotation_arc(Vec3::Z, dir);
    commands.spawn((
        InGameRoot,
        DespawnOnExit(AppState::InGame),
        TracerFx { ttl },
        Mesh3d(shared.tracer_mesh.clone()),
        MeshMaterial3d(mat),
        Transform::from_translation(mid)
            .with_rotation(rot)
            .with_scale(Vec3::new(thickness, thickness, len)),
    ));
}

fn spawn_particle_burst(
    commands: &mut Commands,
    shared: &ArenaSharedAssets,
    origin: Vec3,
    count: usize,
    speed: f32,
    mat: Handle<StandardMaterial>,
) {
    for i in 0..count {
        let theta = (i as f32 / count.max(1) as f32) * std::f32::consts::TAU;
        let elev = 0.55 + ((i * 37) % 10) as f32 * 0.08;
        let vel = Vec3::new(theta.cos() * speed * 0.65, elev * speed, theta.sin() * speed * 0.65);
        commands.spawn((
            InGameRoot,
            DespawnOnExit(AppState::InGame),
            ParticleBurstFx {
                vel,
                ttl: 0.36,
                max_ttl: 0.36,
            },
            Mesh3d(shared.particle_mesh.clone()),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(origin),
        ));
    }
}

fn play_sfx(
    commands: &mut Commands,
    asset_server: &AssetServer,
    rel_path: &str,
    volume: f32,
) {
    commands.spawn((
        InGameRoot,
        DespawnOnExit(AppState::InGame),
        AudioPlayer::new(asset_server.load(rel_path.to_owned())),
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume.clamp(0.0, 1.0))),
    ));
}

fn play_spatial_sfx(
    commands: &mut Commands,
    asset_server: &AssetServer,
    rel_path: &str,
    volume: f32,
    world_pos: Vec3,
) {
    commands.spawn((
        InGameRoot,
        DespawnOnExit(AppState::InGame),
        AudioPlayer::new(asset_server.load(rel_path.to_owned())),
        PlaybackSettings::DESPAWN
            .with_spatial(true)
            .with_volume(Volume::Linear(volume.clamp(0.0, 1.0))),
        Transform::from_translation(world_pos),
    ));
}

/// Ensures [`MAX_PLAYERS`] remains linked across InGame UI telemetry.
#[allow(dead_code)]
fn _max_players_ref() -> usize {
    MAX_PLAYERS
}
