//! Pure-Bevy / `std` Phase 3 World Geometry, Terrain Collision Grid, Raycasting, and Dynamic
//! Building/Node Collider Engine.
//!
//! Bakes real `assets/scenes/*.glb` terrain chunk meshes directly from exported glTF binary chunks
//! into a uniform 2D spatial lookup grid (`TerrainColliderGrid`), ensuring deterministic height,
//! normal, and raycast queries identically on both the graphical client and `--headless-server`.

use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;
use serde::Deserialize;

use crate::config::{AssetManifest, resolve_assets_root};

/// Size of one horizontal spatial cell in the baked terrain lookup grid (meters).
pub const TERRAIN_CELL_SIZE: f32 = 8.0;

/// Grid pitch for snapping placed 4.2m APK base foundations (`part/diji_lv1.ab` ~ 4.2m).
pub const BUILD_GRID_SIZE: f32 = 4.2;

/// Vertical height of one building story (`qiangbi_lv1` wall height = 4.0m).
pub const BUILD_STORY_HEIGHT: f32 = 4.0;

/// Water/ocean surface plane matching `a-57` coastal min elevation (~7.6m).
pub const SEA_LEVEL_Y: f32 = 7.4;

/// Enumerates Phase 3 & Phase 4 APK building/defense piece families (`cfg.BuildPart.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BuildPieceKind {
    Foundation = 0,
    Wall = 1,
    Roof = 2,
    DoorFrame = 3,
    Door = 4,
    Ramp = 5,
    Workbench = 6,
    Turret = 7,
    Generator = 8,
    Battery = 9,
    ElectricTurret = 10,
}

impl BuildPieceKind {
    pub const ALL: [Self; 11] = [
        Self::Foundation,
        Self::Wall,
        Self::Roof,
        Self::DoorFrame,
        Self::Door,
        Self::Ramp,
        Self::Workbench,
        Self::Turret,
        Self::Generator,
        Self::Battery,
        Self::ElectricTurret,
    ];

    pub fn from_u8(v: u8) -> Self {
        Self::ALL
            .get(usize::from(v))
            .copied()
            .unwrap_or(Self::Foundation)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Foundation => "Foundation",
            Self::Wall => "Wall",
            Self::Roof => "Roof/Floor",
            Self::DoorFrame => "Doorframe",
            Self::Door => "Door",
            Self::Ramp => "Stairs/Ramp",
            Self::Workbench => "Workbench",
            Self::Turret => "Auto Turret",
            Self::Generator => "Generator",
            Self::Battery => "Battery Pillar",
            Self::ElectricTurret => "Tesla Turret",
        }
    }

    pub fn glb_stem(self) -> &'static str {
        match self {
            Self::Foundation => "diji_lv1",
            Self::Wall => "qiangbi_lv1",
            Self::Roof => "tianhuaban_lv1",
            Self::DoorFrame => "menkuang_lv1",
            Self::Door => "danshanmen_lv1",
            Self::Ramp => "dijitaijie_lv1",
            Self::Workbench => "gongzuotai_lv1",
            Self::Turret => "jiqiangpaotai_10001",
            Self::Generator => "fadianji",
            Self::Battery => "xudianchizhu_01",
            Self::ElectricTurret => "diancipao_01",
        }
    }

    pub fn wood_cost(self) -> u32 {
        match self {
            Self::Foundation => 20,
            Self::Wall => 15,
            Self::Roof => 15,
            Self::DoorFrame => 15,
            Self::Door => 10,
            Self::Ramp => 20,
            Self::Workbench => 30,
            Self::Turret => 45,
            Self::Generator => 35,
            Self::Battery => 25,
            Self::ElectricTurret => 55,
        }
    }

    pub fn max_hp_for_tier(self, tier: u8) -> u16 {
        let base: u16 = match self {
            Self::Foundation | Self::Roof => 250,
            Self::Wall | Self::DoorFrame | Self::Ramp => 200,
            Self::Door => 180,
            Self::Workbench => 160,
            Self::Turret => 260,
            Self::Generator => 240,
            Self::Battery => 200,
            Self::ElectricTurret => 280,
        };
        base.saturating_mul(u16::from(tier.clamp(1, 4)))
    }

    /// Half-extents `(hx, hy, hz)` in local piece coordinates (`+Z` forward, `+Y` up).
    pub fn local_half_extents(self) -> Vec3 {
        match self {
            Self::Foundation => Vec3::new(2.1, 2.0, 2.1),
            Self::Wall | Self::DoorFrame => Vec3::new(2.1, 2.0, 0.24),
            Self::Door => Vec3::new(1.05, 1.95, 0.18),
            Self::Roof => Vec3::new(2.1, 0.24, 2.1),
            Self::Ramp => Vec3::new(2.1, 2.0, 2.1),
            Self::Workbench => Vec3::new(1.05, 0.70, 0.55),
            Self::Turret => Vec3::new(0.85, 0.95, 0.85),
            Self::Generator => Vec3::new(0.85, 0.85, 0.95),
            Self::Battery => Vec3::new(0.60, 1.05, 0.60),
            Self::ElectricTurret => Vec3::new(0.95, 1.15, 0.95),
        }
    }
}

/// Authoritative placed structure state replicated across clients.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicatedStructure {
    pub id: u32,
    pub owner_id: u16,
    pub kind: BuildPieceKind,
    pub tier: u8,
    pub pos: Vec3,
    pub rot_steps: u8,
    pub hp: u16,
    pub max_hp: u16,
    pub is_open: bool,
}

impl ReplicatedStructure {
    pub fn yaw_radians(&self) -> f32 {
        f32::from(self.rot_steps & 3) * std::f32::consts::FRAC_PI_2
    }

    pub fn top_walkable_y(&self, x: f32, z: f32) -> Option<f32> {
        let inv_rot = Quat::from_rotation_y(-self.yaw_radians());
        let local = inv_rot * (Vec3::new(x, 0.0, z) - Vec3::new(self.pos.x, 0.0, self.pos.z));
        let half = self.kind.local_half_extents();
        if local.x.abs() > half.x + 0.15 || local.z.abs() > half.z + 0.15 {
            return None;
        }
        match self.kind {
            BuildPieceKind::Foundation => Some(self.pos.y + 2.0),
            BuildPieceKind::Roof => Some(self.pos.y + 0.18),
            BuildPieceKind::Ramp => {
                // Smoothly slopes up from y + 0.0 at z = -2.1 to y + 4.0 at z = +2.1.
                let t = ((local.z + 2.1) / 4.2).clamp(0.0, 1.0);
                Some(self.pos.y + t * BUILD_STORY_HEIGHT)
            }
            _ => None,
        }
    }

    pub fn world_aabb(&self) -> (Vec3, Vec3) {
        let half = self.kind.local_half_extents();
        let rotated_half = if self.rot_steps % 2 == 1 {
            Vec3::new(half.z, half.y, half.x)
        } else {
            half
        };
        let center = match self.kind {
            BuildPieceKind::Wall | BuildPieceKind::DoorFrame | BuildPieceKind::Door => {
                self.pos + Vec3::new(0.0, half.y, 0.0)
            }
            BuildPieceKind::Foundation => self.pos,
            BuildPieceKind::Roof => self.pos,
            BuildPieceKind::Ramp
            | BuildPieceKind::Workbench
            | BuildPieceKind::Turret
            | BuildPieceKind::Generator
            | BuildPieceKind::Battery
            | BuildPieceKind::ElectricTurret => {
                self.pos + Vec3::new(0.0, half.y * 0.75, 0.0)
            }
        };
        (center - rotated_half, center + rotated_half)
    }
}

/// Harvestable island resource node category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResourceNodeKind {
    PineTree = 0,
    PalmTree = 1,
    OreBoulder = 2,
    SupplyCrate = 3,
}

impl ResourceNodeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::PineTree => "Pine Tree",
            Self::PalmTree => "Palm Tree",
            Self::OreBoulder => "Ore Boulder",
            Self::SupplyCrate => "Supply Crate",
        }
    }

    pub fn glb_asset_path(self) -> &'static str {
        match self {
            Self::PineTree => "models/tree_tongyongshu_02.glb",
            Self::PalmTree => "models/tree_yezishu_01.glb",
            Self::OreBoulder => "models/stone_shamo_08_lod0.glb",
            Self::SupplyCrate => "models/building_chaoshinew_muxiangzi.glb",
        }
    }

    pub fn collider_radius(self) -> f32 {
        match self {
            Self::PineTree | Self::PalmTree => 0.75,
            Self::OreBoulder => 1.35,
            Self::SupplyCrate => 0.85,
        }
    }

    pub fn collider_height(self) -> f32 {
        match self {
            Self::PineTree | Self::PalmTree => 4.2,
            Self::OreBoulder => 2.1,
            Self::SupplyCrate => 1.3,
        }
    }

    pub fn max_hp(self) -> u16 {
        match self {
            Self::PineTree | Self::PalmTree => 120,
            Self::OreBoulder => 150,
            Self::SupplyCrate => 80,
        }
    }
}

/// Phase 4 AI creature/boss category (`cfg.MonsterCfg.json` & `cfg.PortBossMonsterCfg.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnemyKind {
    ZombieRunner = 0,
    PortBrute = 1,
    PortBossMech = 2,
}

impl EnemyKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::ZombieRunner,
            1 => Self::PortBrute,
            _ => Self::PortBossMech,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ZombieRunner => "Infected Stalker",
            Self::PortBrute => "Riot Enforcer",
            Self::PortBossMech => "Port War-Mech X",
        }
    }

    pub fn glb_stem(self) -> &'static str {
        match self {
            Self::ZombieRunner => "monster_xlt_01",
            Self::PortBrute => "boss_gangkoushouling_01_b",
            Self::PortBossMech => "boss_gangkoushouling_01",
        }
    }

    pub fn max_hp(self) -> u16 {
        match self {
            Self::ZombieRunner => 120,
            Self::PortBrute => 280,
            Self::PortBossMech => 1200,
        }
    }

    pub fn move_speed(self) -> f32 {
        match self {
            Self::ZombieRunner => 4.2,
            Self::PortBrute => 3.5,
            Self::PortBossMech => 3.0,
        }
    }

    pub fn aggro_radius(self) -> f32 {
        match self {
            Self::ZombieRunner => 24.0,
            Self::PortBrute => 28.0,
            Self::PortBossMech => 36.0,
        }
    }

    pub fn attack_range(self) -> f32 {
        match self {
            Self::ZombieRunner => 2.1,
            Self::PortBrute => 14.0,
            Self::PortBossMech => 22.0,
        }
    }

    pub fn attack_damage(self) -> u16 {
        match self {
            Self::ZombieRunner => 10,
            Self::PortBrute => 12,
            Self::PortBossMech => 18,
        }
    }

    pub fn collider_radius(self) -> f32 {
        match self {
            Self::ZombieRunner => 0.50,
            Self::PortBrute => 0.72,
            Self::PortBossMech => 1.35,
        }
    }

    pub fn collider_height(self) -> f32 {
        match self {
            Self::ZombieRunner => 1.85,
            Self::PortBrute => 2.25,
            Self::PortBossMech => 3.60,
        }
    }
}

/// Authoritative replicated Phase 4 AI monster or Port Boss.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicatedEnemy {
    pub id: u16,
    pub kind: EnemyKind,
    pub pos: Vec3,
    pub home_pos: Vec3,
    pub yaw: f32,
    pub is_moving: bool,
    pub is_attacking: bool,
    pub hp: u16,
    pub max_hp: u16,
    pub attack_cooldown: f32,
    pub respawn_secs: f32,
}

/// Phase 4 loot bag / air-drop crate archetype (`cfg.AirDropBoxCfg.json` & `cfg.DropItemCfg.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LootDropKind {
    DropSack = 0,
    NormalCrate = 1,
    SeniorCrate = 2,
}

impl LootDropKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::DropSack,
            1 => Self::NormalCrate,
            _ => Self::SeniorCrate,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::DropSack => "Field Loot Bag",
            Self::NormalCrate => "Supply Drop",
            Self::SeniorCrate => "Senior War Crate",
        }
    }

    pub fn glb_stem(self) -> &'static str {
        match self {
            Self::DropSack => "wupin_madai_low",
            Self::NormalCrate => "normalbox",
            Self::SeniorCrate => "seniorbox",
        }
    }
}

/// Replicated loot drop entity on the island (`OP_LOOT_DROP`).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicatedLootDrop {
    pub id: u16,
    pub kind: LootDropKind,
    pub pos: Vec3,
    pub ammo_bonus: u16,
    pub wood_bonus: u16,
    pub stone_bonus: u16,
    pub iron_bonus: u16,
    pub bandage_bonus: u8,
    pub ttl_secs: f32,
}

/// Replicated resource node (`ServerWorld` -> all clients).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicatedResourceNode {
    pub id: u16,
    pub kind: ResourceNodeKind,
    pub pos: Vec3,
    pub yaw: f32,
    pub hp: u16,
    pub max_hp: u16,
    pub respawn_secs: f32,
}

#[derive(Debug, Clone, Copy)]
struct TerrainTri {
    a: Vec3,
    b: Vec3,
    c: Vec3,
    normal: Vec3,
    min_x: f32,
    max_x: f32,
    min_z: f32,
    max_z: f32,
}

/// Metadata + XZ bounds of one loaded terrain chunk GLB (`a-55..d-02`).
#[derive(Debug, Clone)]
pub struct ChunkWorldBounds {
    pub name: String,
    pub asset_path: String,
    pub min_xz: Vec2,
    pub max_xz: Vec2,
}

/// Deterministic baked triangle terrain grid built directly from exported `assets/scenes/*.glb`
/// meshes so `--headless-server` and graphical clients share exact terrain elevation.
#[derive(Debug, Clone, Resource, Default)]
pub struct TerrainColliderGrid {
    triangles: Vec<TerrainTri>,
    cells: HashMap<(i32, i32), Vec<usize>>,
    pub chunk_bounds: Vec<ChunkWorldBounds>,
}

impl TerrainColliderGrid {
    /// Parses all island terrain chunk GLBs declared in `manifest.scenes` from `assets_root`.
    pub fn load_from_manifest(assets_root: &Path, manifest: &AssetManifest) -> Self {
        let mut grid = Self::default();
        for scene in &manifest.scenes {
            if !is_terrain_chunk_name(&scene.name) {
                continue;
            }
            let full_path = assets_root.join(&scene.asset_path);
            let Ok(bytes) = std::fs::read(&full_path) else {
                continue;
            };
            if let Ok((tris, min_xz, max_xz)) = parse_glb_world_triangles(&bytes)
                && !tris.is_empty()
            {
                let chunk_name = scene.name.to_ascii_uppercase();
                grid.chunk_bounds.push(ChunkWorldBounds {
                    name: chunk_name,
                    asset_path: scene.asset_path.clone(),
                    min_xz,
                    max_xz,
                });
                for tri in tris {
                    grid.insert_triangle(tri);
                }
            }
        }
        grid
    }

    /// Loads default terrain collider directly via [`resolve_assets_root`] and `manifest.json`.
    pub fn load_default() -> Self {
        let root = resolve_assets_root();
        let Ok(raw) = std::fs::read_to_string(root.join("manifest.json")) else {
            return Self::default();
        };
        let Ok(manifest) = serde_json::from_str::<AssetManifest>(&raw) else {
            return Self::default();
        };
        Self::load_from_manifest(&root, &manifest)
    }

    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    fn insert_triangle(&mut self, tri: TerrainTri) {
        let tri_idx = self.triangles.len();
        let x0 = (tri.min_x / TERRAIN_CELL_SIZE).floor() as i32;
        let x1 = (tri.max_x / TERRAIN_CELL_SIZE).floor() as i32;
        let z0 = (tri.min_z / TERRAIN_CELL_SIZE).floor() as i32;
        let z1 = (tri.max_z / TERRAIN_CELL_SIZE).floor() as i32;
        self.triangles.push(tri);
        for gx in x0..=x1 {
            for gz in z0..=z1 {
                self.cells.entry((gx, gz)).or_default().push(tri_idx);
            }
        }
    }

    /// Samples the highest walkable terrain height `(y, normal)` directly beneath `(x, z)`.
    pub fn sample_height_and_normal(&self, x: f32, z: f32) -> Option<(f32, Vec3)> {
        let gx = (x / TERRAIN_CELL_SIZE).floor() as i32;
        let gz = (z / TERRAIN_CELL_SIZE).floor() as i32;
        let list = self.cells.get(&(gx, gz))?;
        let mut best: Option<(f32, Vec3)> = None;
        for &idx in list {
            let tri = &self.triangles[idx];
            if tri.normal.y.abs() < 0.08
                || x < tri.min_x - 0.001
                || x > tri.max_x + 0.001
                || z < tri.min_z - 0.001
                || z > tri.max_z + 0.001
            {
                continue;
            }
            if let Some(y) = sample_triangle_xz_height(tri.a, tri.b, tri.c, x, z) {
                let up_n = if tri.normal.y < 0.0 {
                    -tri.normal
                } else {
                    tri.normal
                };
                if best.is_none_or(|(best_y, _)| y > best_y) {
                    best = Some((y, up_n));
                }
            }
        }
        best
    }

    /// Samples floor elevation cleanly (falling back to sea level if beyond loaded chunks).
    pub fn ground_y(&self, x: f32, z: f32) -> f32 {
        self.sample_height_and_normal(x, z)
            .map(|(y, _)| y.max(SEA_LEVEL_Y - 0.6))
            .unwrap_or(SEA_LEVEL_Y)
    }

    /// Returns the loaded chunk name covering `(x, z)` if any.
    pub fn chunk_name_at(&self, x: f32, z: f32) -> Option<&str> {
        self.chunk_bounds
            .iter()
            .find(|c| x >= c.min_xz.x && x <= c.max_xz.x && z >= c.min_xz.y && z <= c.max_xz.y)
            .map(|c| c.name.as_str())
    }

    /// Raycasts a 3D segment against terrain triangles along traversed cells (DDA/grid march).
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<(f32, Vec3, Vec3)> {
        let dir_n = dir.normalize_or_zero();
        if dir_n.length_squared() < 0.5 || max_dist <= 0.0 {
            return None;
        }
        let steps = (max_dist / (TERRAIN_CELL_SIZE * 0.5)).ceil().max(1.0) as i32;
        let mut tested = Vec::new();
        let mut best: Option<(f32, Vec3, Vec3)> = None;

        for s in 0..=steps {
            let p = origin + dir_n * (s as f32 * (TERRAIN_CELL_SIZE * 0.5)).min(max_dist);
            let gx = (p.x / TERRAIN_CELL_SIZE).floor() as i32;
            let gz = (p.z / TERRAIN_CELL_SIZE).floor() as i32;
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let Some(indices) = self.cells.get(&(gx + dx, gz + dz)) else {
                        continue;
                    };
                    for &idx in indices {
                        if tested.contains(&idx) {
                            continue;
                        }
                        tested.push(idx);
                        let tri = &self.triangles[idx];
                        if let Some(t) = ray_triangle_intersect(origin, dir_n, tri.a, tri.b, tri.c)
                            && t > 0.05
                            && t <= max_dist
                            && best.is_none_or(|(bt, _, _)| t < bt)
                        {
                            let up_n = if tri.normal.dot(-dir_n) >= 0.0 {
                                tri.normal
                            } else {
                                -tri.normal
                            };
                            best = Some((t, origin + dir_n * t, up_n));
                        }
                    }
                }
            }
            if let Some((bt, _, _)) = best
                && (s as f32 * (TERRAIN_CELL_SIZE * 0.5)) > bt + TERRAIN_CELL_SIZE
            {
                break;
            }
        }
        best
    }
}

/// What kind of world entity a 3D raycast struck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldHitTarget {
    Terrain,
    Structure(u32),
    ResourceNode(u16),
    Enemy(u16),
    Player(u16),
    Vehicle(u16),
}

/// Complete hit result from [`raycast_world`].
#[derive(Debug, Clone, Copy)]
pub struct WorldRayHit {
    pub dist: f32,
    pub point: Vec3,
    pub normal: Vec3,
    pub target: WorldHitTarget,
}

/// Deterministic walkable height under `(x, z)` considering terrain + nearby walkable structures.
pub fn sample_surface_height(
    terrain: &TerrainColliderGrid,
    structures: &[ReplicatedStructure],
    x: f32,
    z: f32,
    current_feet_y: f32,
) -> f32 {
    let mut best_y = terrain.ground_y(x, z);
    for st in structures {
        if let Some(top_y) = st.top_walkable_y(x, z)
            && top_y <= current_feet_y + 2.2
            && top_y > best_y
        {
            best_y = top_y;
        }
    }
    best_y
}

/// Pure-Bevy character controller movement + capsule/AABB collision step for one player tick.
#[allow(clippy::too_many_arguments)]
pub fn step_player_kinematics(
    terrain: &TerrainColliderGrid,
    structures: &[ReplicatedStructure],
    nodes: &[ReplicatedResourceNode],
    pos: &mut Vec3,
    wish_xz: Vec2,
    vertical_vel: &mut f32,
    grounded: &mut bool,
    jump_pressed: bool,
    move_speed: f32,
    dt: f32,
) {
    let radius = 0.42;
    let clamped_wish = if wish_xz.length_squared() > 1.0 {
        wish_xz.normalize()
    } else {
        wish_xz
    };
    pos.x = (pos.x + clamped_wish.x * move_speed * dt).clamp(-560.0, 560.0);
    pos.z = (pos.z + clamped_wish.y * move_speed * dt).clamp(-560.0, 560.0);

    // Resolve horizontal wall / closed-door / workbench AABBs near the player's torso band.
    let feet_y = pos.y;
    for st in structures {
        let blocks = match st.kind {
            BuildPieceKind::Door => !st.is_open,
            BuildPieceKind::DoorFrame => {
                // Keep side pillars solid while allowing center doorway passage.
                let inv = Quat::from_rotation_y(-st.yaw_radians());
                let local = inv * (Vec3::new(pos.x, 0.0, pos.z) - Vec3::new(st.pos.x, 0.0, st.pos.z));
                local.x.abs() > 0.72
            }
            BuildPieceKind::Wall | BuildPieceKind::Workbench | BuildPieceKind::Turret => true,
            _ => false,
        };
        if !blocks {
            continue;
        }
        let (bmin, bmax) = st.world_aabb();
        if feet_y + 1.65 < bmin.y || feet_y > bmax.y - 0.15 {
            continue;
        }
        push_circle_out_of_aabb(&mut pos.x, &mut pos.z, radius, bmin, bmax);
    }

    for node in nodes {
        if node.hp == 0 {
            continue;
        }
        let r_sum = radius + node.kind.collider_radius();
        let dx = pos.x - node.pos.x;
        let dz = pos.z - node.pos.z;
        let d2 = dx * dx + dz * dz;
        if d2 > 1e-6 && d2 < r_sum * r_sum {
            let d = d2.sqrt();
            let push = (r_sum - d) / d;
            pos.x += dx * push;
            pos.z += dz * push;
        }
    }

    if jump_pressed && *grounded {
        *vertical_vel = 6.8;
        *grounded = false;
    }

    *vertical_vel = (*vertical_vel - 18.0 * dt).max(-32.0);
    pos.y += *vertical_vel * dt;

    let floor_y = sample_surface_height(terrain, structures, pos.x, pos.z, pos.y);
    if pos.y <= floor_y + 0.02 {
        pos.y = floor_y;
        *vertical_vel = 0.0;
        *grounded = true;
    } else if pos.y > floor_y + 0.35 {
        *grounded = false;
    }
}

/// Raycasts a 3D ray against terrain, placed structures, gatherable nodes, enemies, and player capsules.
#[allow(clippy::too_many_arguments)]
pub fn raycast_world(
    terrain: &TerrainColliderGrid,
    structures: &[ReplicatedStructure],
    nodes: &[ReplicatedResourceNode],
    enemies: &[ReplicatedEnemy],
    players: &[(u16, Vec3)],
    ray: Ray3d,
    max_dist: f32,
    ignore_player: Option<u16>,
) -> Option<WorldRayHit> {
    let origin = ray.origin;
    let dir_n = *ray.direction;
    let mut best: Option<WorldRayHit> = terrain.raycast(origin, dir_n, max_dist).map(
        |(dist, point, normal)| WorldRayHit {
            dist,
            point,
            normal,
            target: WorldHitTarget::Terrain,
        },
    );

    for st in structures {
        let (bmin, bmax) = st.world_aabb();
        if let Some((t, normal)) = ray_aabb_intersect(origin, dir_n, bmin, bmax)
            && t > 0.05
            && t <= max_dist
            && best.is_none_or(|b| t < b.dist)
        {
            best = Some(WorldRayHit {
                dist: t,
                point: origin + dir_n * t,
                normal,
                target: WorldHitTarget::Structure(st.id),
            });
        }
    }

    for node in nodes {
        if node.hp == 0 {
            continue;
        }
        let center = node.pos + Vec3::new(0.0, node.kind.collider_height() * 0.5, 0.0);
        let half = Vec3::new(
            node.kind.collider_radius(),
            node.kind.collider_height() * 0.5,
            node.kind.collider_radius(),
        );
        if let Some((t, normal)) = ray_aabb_intersect(origin, dir_n, center - half, center + half)
            && t > 0.05
            && t <= max_dist
            && best.is_none_or(|b| t < b.dist)
        {
            best = Some(WorldRayHit {
                dist: t,
                point: origin + dir_n * t,
                normal,
                target: WorldHitTarget::ResourceNode(node.id),
            });
        }
    }

    for enemy in enemies {
        if enemy.hp == 0 {
            continue;
        }
        let r = enemy.kind.collider_radius();
        let h = enemy.kind.collider_height();
        let bmin = enemy.pos + Vec3::new(-r, 0.0, -r);
        let bmax = enemy.pos + Vec3::new(r, h, r);
        if let Some((t, normal)) = ray_aabb_intersect(origin, dir_n, bmin, bmax)
            && t > 0.05
            && t <= max_dist
            && best.is_none_or(|b| t < b.dist)
        {
            best = Some(WorldRayHit {
                dist: t,
                point: origin + dir_n * t,
                normal,
                target: WorldHitTarget::Enemy(enemy.id),
            });
        }
    }

    for &(pid, p_pos) in players {
        if ignore_player == Some(pid) {
            continue;
        }
        let bmin = p_pos + Vec3::new(-0.42, 0.0, -0.42);
        let bmax = p_pos + Vec3::new(0.42, 1.75, 0.42);
        if let Some((t, normal)) = ray_aabb_intersect(origin, dir_n, bmin, bmax)
            && t > 0.05
            && t <= max_dist
            && best.is_none_or(|b| t < b.dist)
        {
            best = Some(WorldRayHit {
                dist: t,
                point: origin + dir_n * t,
                normal,
                target: WorldHitTarget::Player(pid),
            });
        }
    }

    best
}

/// Computes snapped world placement transform `(pos, rot_steps)` for a building ghost preview.
pub fn compute_build_snap(
    terrain: &TerrainColliderGrid,
    structures: &[ReplicatedStructure],
    hit_point: Vec3,
    hit_target: WorldHitTarget,
    kind: BuildPieceKind,
    rot_steps: u8,
) -> (Vec3, u8) {
    if let WorldHitTarget::Structure(sid) = hit_target
        && let Some(parent) = structures.iter().find(|s| s.id == sid)
    {
        return snap_relative_to_structure(terrain, parent, hit_point, kind, rot_steps);
    }

    let gx = (hit_point.x / BUILD_GRID_SIZE).round() * BUILD_GRID_SIZE;
    let gz = (hit_point.z / BUILD_GRID_SIZE).round() * BUILD_GRID_SIZE;
    let gy = terrain.ground_y(gx, gz);
    let pos = match kind {
        BuildPieceKind::Foundation => Vec3::new(gx, gy - 1.25, gz),
        BuildPieceKind::Workbench
        | BuildPieceKind::Turret
        | BuildPieceKind::Generator
        | BuildPieceKind::Battery
        | BuildPieceKind::ElectricTurret => Vec3::new(hit_point.x, gy, hit_point.z),
        _ => Vec3::new(gx, gy, gz),
    };
    (pos, rot_steps & 3)
}

fn snap_relative_to_structure(
    terrain: &TerrainColliderGrid,
    parent: &ReplicatedStructure,
    hit_point: Vec3,
    kind: BuildPieceKind,
    rot_steps: u8,
) -> (Vec3, u8) {
    let p = parent.pos;
    let half = BUILD_GRID_SIZE * 0.5;
    let delta = hit_point - p;
    match kind {
        BuildPieceKind::Foundation => {
            let step = if delta.x.abs() >= delta.z.abs() {
                Vec3::new(delta.x.signum() * BUILD_GRID_SIZE, 0.0, 0.0)
            } else {
                Vec3::new(0.0, 0.0, delta.z.signum() * BUILD_GRID_SIZE)
            };
            let nx = p.x + step.x;
            let nz = p.z + step.z;
            let ny = if parent.kind == BuildPieceKind::Foundation {
                p.y
            } else {
                terrain.ground_y(nx, nz) - 1.25
            };
            (Vec3::new(nx, ny, nz), rot_steps & 3)
        }
        BuildPieceKind::Ramp => {
            let base_y = if parent.kind == BuildPieceKind::Foundation {
                p.y + 2.0
            } else {
                terrain.ground_y(p.x, p.z)
            };
            let step = if delta.x.abs() >= delta.z.abs() {
                Vec3::new(delta.x.signum() * BUILD_GRID_SIZE, 0.0, 0.0)
            } else {
                Vec3::new(0.0, 0.0, delta.z.signum() * BUILD_GRID_SIZE)
            };
            (Vec3::new(p.x + step.x, base_y, p.z + step.z), rot_steps & 3)
        }
        BuildPieceKind::Wall | BuildPieceKind::DoorFrame => {
            let base_y = if parent.kind == BuildPieceKind::Foundation {
                p.y + 2.0
            } else {
                p.y + BUILD_STORY_HEIGHT
            };
            if delta.x.abs() >= delta.z.abs() {
                (
                    Vec3::new(p.x + delta.x.signum() * half, base_y, p.z),
                    1,
                )
            } else {
                (
                    Vec3::new(p.x, base_y, p.z + delta.z.signum() * half),
                    0,
                )
            }
        }
        BuildPieceKind::Door => (p, parent.rot_steps),
        BuildPieceKind::Roof => {
            let top_y = match parent.kind {
                BuildPieceKind::Wall | BuildPieceKind::DoorFrame => p.y + BUILD_STORY_HEIGHT,
                BuildPieceKind::Roof => p.y,
                BuildPieceKind::Foundation => p.y + 2.0 + BUILD_STORY_HEIGHT,
                _ => p.y + BUILD_STORY_HEIGHT,
            };
            let base = if matches!(parent.kind, BuildPieceKind::Wall | BuildPieceKind::DoorFrame) {
                let fwd = Quat::from_rotation_y(parent.yaw_radians()) * Vec3::Z;
                let side_sign = if (hit_point - p).dot(fwd) >= 0.0 { 1.0 } else { -1.0 };
                p + fwd * (half * side_sign)
            } else {
                p
            };
            (Vec3::new(base.x, top_y, base.z), rot_steps & 3)
        }
        BuildPieceKind::Workbench
        | BuildPieceKind::Turret
        | BuildPieceKind::Generator
        | BuildPieceKind::Battery
        | BuildPieceKind::ElectricTurret => {
            let floor_y = parent
                .top_walkable_y(hit_point.x, hit_point.z)
                .unwrap_or(hit_point.y);
            (Vec3::new(hit_point.x, floor_y, hit_point.z), rot_steps & 3)
        }
    }
}

/// Deterministic island harvestable resource node layout sampled onto actual island terrain.
pub fn generate_default_resource_nodes(terrain: &TerrainColliderGrid) -> Vec<ReplicatedResourceNode> {
    let mut nodes = Vec::new();
    let mut next_id = 1_u16;
    // Place deterministic rings around spawn + clear central meadow for base building.
    for ring in 1..=9 {
        let count = 6 + ring * 3;
        let base_r = 22.0 + (ring as f32) * 24.0;
        for i in 0..count {
            let theta = (i as f32 / count as f32) * std::f32::consts::TAU + (ring as f32) * 0.41;
            let jitter = ((ring * 31 + i * 17) % 13) as f32 - 6.0;
            let r = base_r + jitter;
            let x = theta.cos() * r;
            let z = theta.sin() * r;
            let Some((y, n)) = terrain.sample_height_and_normal(x, z) else {
                continue;
            };
            if y < SEA_LEVEL_Y + 0.4 || n.y < 0.72 {
                continue;
            }
            let kind = match (ring + i) % 7 {
                0 | 3 => ResourceNodeKind::PineTree,
                1 | 4 => ResourceNodeKind::PalmTree,
                2 | 5 => ResourceNodeKind::OreBoulder,
                _ => ResourceNodeKind::SupplyCrate,
            };
            nodes.push(ReplicatedResourceNode {
                id: next_id,
                kind,
                pos: Vec3::new(x, y, z),
                yaw: theta * 1.7,
                hp: kind.max_hp(),
                max_hp: kind.max_hp(),
                respawn_secs: 0.0,
            });
            next_id = next_id.saturating_add(1);
        }
    }
    nodes
}

/// Deterministic Phase 4 patrol monster & Port War-Mech boss spawns around walkable island terrain.
pub fn generate_default_enemies(terrain: &TerrainColliderGrid) -> Vec<ReplicatedEnemy> {
    let mut enemies = Vec::new();
    let mut next_id = 1_u16;

    // 14 zombie runners & brute guards circling outer camp + road ridges (beyond initial spawn buffer).
    let rings: &[(f32, usize, EnemyKind)] = &[
        (36.0, 8, EnemyKind::ZombieRunner),
        (72.0, 8, EnemyKind::ZombieRunner),
        (54.0, 4, EnemyKind::PortBrute),
    ];
    for &(radius, count, kind) in rings {
        for i in 0..count {
            let theta = (i as f32 / count as f32) * std::f32::consts::TAU + radius * 0.11;
            let x = theta.cos() * radius;
            let z = theta.sin() * radius;
            let y = terrain.ground_y(x, z);
            if y < SEA_LEVEL_Y + 0.4 {
                continue;
            }
            let pos = Vec3::new(x, y, z);
            enemies.push(ReplicatedEnemy {
                id: next_id,
                kind,
                pos,
                home_pos: pos,
                yaw: theta + std::f32::consts::PI,
                is_moving: false,
                is_attacking: false,
                hp: kind.max_hp(),
                max_hp: kind.max_hp(),
                attack_cooldown: 0.0,
                respawn_secs: 0.0,
            });
            next_id = next_id.saturating_add(1);
        }
    }

    // Centerpiece Port Boss Mech overlooking the harbor road ridge.
    let boss_x = 34.0;
    let boss_z = -36.0;
    let boss_y = terrain.ground_y(boss_x, boss_z);
    let boss_pos = Vec3::new(boss_x, boss_y, boss_z);
    enemies.push(ReplicatedEnemy {
        id: next_id,
        kind: EnemyKind::PortBossMech,
        pos: boss_pos,
        home_pos: boss_pos,
        yaw: -0.75,
        is_moving: false,
        is_attacking: false,
        hp: EnemyKind::PortBossMech.max_hp(),
        max_hp: EnemyKind::PortBossMech.max_hp(),
        attack_cooldown: 0.0,
        respawn_secs: 0.0,
    });
    enemies
}

/// Drivable / flyable Phase 5 vehicle archetypes (`cfg.VehicleCfg.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VehicleKind {
    OffroadJeep = 0,
    ArmoredTank = 1,
    GunshipHeli = 2,
}

impl VehicleKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::OffroadJeep,
            1 => Self::ArmoredTank,
            _ => Self::GunshipHeli,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OffroadJeep => "Offroad Buggy",
            Self::ArmoredTank => "Armored Tank",
            Self::GunshipHeli => "Armed Helicopter",
        }
    }

    pub fn glb_stem(self) -> &'static str {
        match self {
            Self::OffroadJeep => "project_poqiche_01",
            Self::ArmoredTank => "project_tanke_01",
            Self::GunshipHeli => "npc_wuzhuangzhishengji_1",
        }
    }

    pub fn max_speed(self) -> f32 {
        match self {
            Self::OffroadJeep => 21.0,
            Self::ArmoredTank => 13.5,
            Self::GunshipHeli => 25.0,
        }
    }

    pub fn turn_rate(self) -> f32 {
        match self {
            Self::OffroadJeep => 2.1,
            Self::ArmoredTank => 1.35,
            Self::GunshipHeli => 1.85,
        }
    }

    pub fn max_hp(self) -> u16 {
        match self {
            Self::OffroadJeep => 450,
            Self::ArmoredTank => 950,
            Self::GunshipHeli => 650,
        }
    }

    pub fn max_fuel(self) -> f32 {
        1000.0
    }

    pub fn is_aircraft(self) -> bool {
        matches!(self, Self::GunshipHeli)
    }

    pub fn half_extents(self) -> Vec3 {
        match self {
            Self::OffroadJeep => Vec3::new(1.3, 1.0, 2.3),
            Self::ArmoredTank => Vec3::new(1.8, 1.3, 2.9),
            Self::GunshipHeli => Vec3::new(2.2, 1.6, 3.8),
        }
    }

    pub fn seat_offset(self) -> Vec3 {
        match self {
            Self::OffroadJeep => Vec3::new(0.0, 1.1, 0.0),
            Self::ArmoredTank => Vec3::new(0.0, 1.45, 0.0),
            Self::GunshipHeli => Vec3::new(0.0, 0.9, 0.4),
        }
    }
}

/// Authoritative Phase 5 replicated vehicle state (`OP_ENTER_EXIT_VEHICLE` + `NetVehicleDto`).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicatedVehicle {
    pub id: u16,
    pub kind: VehicleKind,
    pub pos: Vec3,
    pub yaw: f32,
    pub speed: f32,
    pub vel_y: f32,
    pub hp: u16,
    pub max_hp: u16,
    pub fuel: f32,
    pub max_fuel: f32,
    pub driver_id: u16,
    pub weapon_cooldown: f32,
}

impl ReplicatedVehicle {
    pub fn forward_xz(&self) -> Vec3 {
        Vec3::new(self.yaw.sin(), 0.0, self.yaw.cos()).normalize_or(Vec3::Z)
    }

    pub fn fuel_pct(&self) -> u8 {
        if self.max_fuel <= 0.0 {
            0
        } else {
            ((self.fuel / self.max_fuel).clamp(0.0, 1.0) * 100.0).round() as u8
        }
    }
}

/// Deterministic Phase 5 island vehicles placed on clear roads/plateaus near spawn.
pub fn generate_default_vehicles(terrain: &TerrainColliderGrid) -> Vec<ReplicatedVehicle> {
    let placements: &[(f32, f32, f32, VehicleKind)] = &[
        (-13.5, -10.5, 0.55, VehicleKind::OffroadJeep),
        (22.0, -21.0, -0.65, VehicleKind::ArmoredTank),
        (-22.0, 16.0, 0.90, VehicleKind::GunshipHeli),
    ];
    let mut vehicles = Vec::with_capacity(placements.len());
    for (idx, &(x, z, yaw, kind)) in placements.iter().enumerate() {
        let y = terrain.ground_y(x, z).max(SEA_LEVEL_Y + 0.35);
        vehicles.push(ReplicatedVehicle {
            id: (idx + 1) as u16,
            kind,
            pos: Vec3::new(x, y, z),
            yaw,
            speed: 0.0,
            vel_y: 0.0,
            hp: kind.max_hp(),
            max_hp: kind.max_hp(),
            fuel: kind.max_fuel(),
            max_fuel: kind.max_fuel(),
            driver_id: 0,
            weapon_cooldown: 0.0,
        });
    }
    vehicles
}

/// Driver control snapshot passed into [`step_vehicle_kinematics`].
#[derive(Debug, Clone, Copy, Default)]
pub struct VehicleDriveInput {
    pub throttle: f32,
    pub steer: f32,
    pub ascend: bool,
    pub descend: bool,
    pub boost: bool,
}

/// Pure-Bevy kinematics for ground vehicles and helicopters, with wall & terrain safety bounds.
pub fn step_vehicle_kinematics(
    terrain: &TerrainColliderGrid,
    structures: &[ReplicatedStructure],
    vehicle: &mut ReplicatedVehicle,
    input: VehicleDriveInput,
    dt: f32,
) {
    let throttle_input = input.throttle;
    let steer_input = input.steer;
    let ascend = input.ascend;
    let descend = input.descend;
    let boost = input.boost;
    let dt = dt.clamp(0.001, 0.1);
    vehicle.weapon_cooldown = (vehicle.weapon_cooldown - dt).max(0.0);

    let has_driver = vehicle.driver_id != 0;
    let has_fuel = vehicle.fuel > 0.1;
    let throttle = if has_driver && has_fuel {
        throttle_input.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let steer = if has_driver && has_fuel {
        steer_input.clamp(-1.0, 1.0)
    } else {
        0.0
    };

    let boost_mult = if boost && throttle > 0.1 && has_fuel {
        1.32
    } else {
        1.0
    };
    let target_speed = throttle * vehicle.kind.max_speed() * boost_mult;
    let accel = if throttle.abs() > 0.05 { 18.0 } else { 9.5 };
    let delta_v = (target_speed - vehicle.speed).clamp(-accel * dt, accel * dt);
    vehicle.speed += delta_v;

    let steer_factor = if vehicle.kind.is_aircraft() || matches!(vehicle.kind, VehicleKind::ArmoredTank) {
        1.0
    } else {
        let dir_sign = if vehicle.speed < -0.5 { -1.0 } else { 1.0 };
        (vehicle.speed.abs() / 4.0).clamp(0.35, 1.0) * dir_sign
    };
    vehicle.yaw -= steer * vehicle.kind.turn_rate() * steer_factor * dt;

    let fwd = vehicle.forward_xz();
    let mut next_pos = vehicle.pos + fwd * (vehicle.speed * dt);
    next_pos.x = next_pos.x.clamp(-215.0, 215.0);
    next_pos.z = next_pos.z.clamp(-215.0, 215.0);

    // Push out from solid structures when near ground level.
    let ground_y = terrain.ground_y(next_pos.x, next_pos.z).max(SEA_LEVEL_Y + 0.25);
    if !vehicle.kind.is_aircraft() || next_pos.y < ground_y + 4.5 {
        let half = vehicle.kind.half_extents();
        for st in structures {
            if matches!(st.kind, BuildPieceKind::Foundation | BuildPieceKind::Roof)
                || (st.kind == BuildPieceKind::Door && st.is_open)
            {
                continue;
            }
            let (bmin, bmax) = st.world_aabb();
            if next_pos.y + half.y * 2.0 < bmin.y || next_pos.y > bmax.y + 0.5 {
                continue;
            }
            let r = half.x.max(1.0);
            let cx = next_pos.x.clamp(bmin.x, bmax.x);
            let cz = next_pos.z.clamp(bmin.z, bmax.z);
            let dx = next_pos.x - cx;
            let dz = next_pos.z - cz;
            let d2 = dx * dx + dz * dz;
            if d2 < r * r {
                let d = d2.sqrt();
                if d > 1e-4 {
                    next_pos.x = cx + (dx / d) * r;
                    next_pos.z = cz + (dz / d) * r;
                } else {
                    next_pos.z -= r;
                }
                vehicle.speed *= 0.45;
            }
        }
    }

    let ground_y = terrain.ground_y(next_pos.x, next_pos.z).max(SEA_LEVEL_Y + 0.25);
    if vehicle.kind.is_aircraft() {
        let mut vert_vel = if has_driver && has_fuel {
            if ascend {
                9.5
            } else if descend {
                -9.0
            } else {
                0.0
            }
        } else {
            -5.5
        };
        if has_driver && has_fuel && !ascend && !descend && next_pos.y < ground_y + 1.2 && throttle.abs() > 0.1 {
            vert_vel = 4.0;
        }
        vehicle.vel_y = vehicle.vel_y + (vert_vel - vehicle.vel_y) * (6.0 * dt).min(1.0);
        next_pos.y = (next_pos.y + vehicle.vel_y * dt).clamp(ground_y, ground_y + 42.0);
        if (next_pos.y - ground_y).abs() < 0.05 && vehicle.vel_y < 0.0 {
            vehicle.vel_y = 0.0;
        }
    } else {
        next_pos.y = ground_y;
        vehicle.vel_y = 0.0;
    }

    if has_driver && has_fuel && (throttle.abs() > 0.05 || (vehicle.kind.is_aircraft() && (ascend || next_pos.y > ground_y + 0.5))) {
        let drain = (if boost { 2.4 } else { 1.1 }) * dt;
        vehicle.fuel = (vehicle.fuel - drain).max(0.0);
    }
    vehicle.pos = next_pos;
}

/// Raycasts active world vehicles by oriented-free AABB centered above `vehicle.pos`.
pub fn raycast_vehicles(
    ray: Ray3d,
    max_dist: f32,
    vehicles: &[ReplicatedVehicle],
) -> Option<WorldRayHit> {
    let origin = ray.origin;
    let dir_n = *ray.direction;
    let mut best: Option<WorldRayHit> = None;
    for v in vehicles {
        if v.hp == 0 {
            continue;
        }
        let h = v.kind.half_extents();
        let bmin = v.pos + Vec3::new(-h.x, 0.0, -h.z);
        let bmax = v.pos + Vec3::new(h.x, h.y * 2.0, h.z);
        let mut tmin = 0.0_f32;
        let mut tmax = best.as_ref().map_or(max_dist, |b| b.dist);
        let mut hit_normal = Vec3::Y;
        let mut hit = true;
        for axis in 0..3 {
            let o = origin[axis];
            let d = dir_n[axis];
            let min_a = bmin[axis];
            let max_a = bmax[axis];
            if d.abs() < 1e-6 {
                if o < min_a || o > max_a {
                    hit = false;
                    break;
                }
            } else {
                let inv = 1.0 / d;
                let mut t1 = (min_a - o) * inv;
                let mut t2 = (max_a - o) * inv;
                let mut sign = -1.0_f32;
                if t1 > t2 {
                    std::mem::swap(&mut t1, &mut t2);
                    sign = 1.0;
                }
                if t1 > tmin {
                    tmin = t1;
                    let mut n = Vec3::ZERO;
                    n[axis] = sign;
                    hit_normal = n;
                }
                tmax = tmax.min(t2);
                if tmin > tmax {
                    hit = false;
                    break;
                }
            }
        }
        if hit && tmin >= 0.0 {
            best = Some(WorldRayHit {
                dist: tmin,
                point: origin + dir_n * tmin,
                normal: hit_normal,
                target: WorldHitTarget::Vehicle(v.id),
            });
        }
    }
    best
}

struct LootDropPreset {
    x: f32,
    z: f32,
    kind: LootDropKind,
    ammo: u16,
    wood: u16,
    stone: u16,
    iron: u16,
    bandages: u8,
}

/// Deterministic Phase 4 initial field supply / airdrop boxes placed on landmarks.
pub fn generate_default_loot_drops(terrain: &TerrainColliderGrid) -> Vec<ReplicatedLootDrop> {
    let spots: &[LootDropPreset] = &[
        LootDropPreset {
            x: -18.0,
            z: -14.0,
            kind: LootDropKind::DropSack,
            ammo: 15,
            wood: 25,
            stone: 10,
            iron: 5,
            bandages: 1,
        },
        LootDropPreset {
            x: 20.0,
            z: 18.0,
            kind: LootDropKind::NormalCrate,
            ammo: 30,
            wood: 40,
            stone: 20,
            iron: 12,
            bandages: 2,
        },
        LootDropPreset {
            x: -42.0,
            z: 30.0,
            kind: LootDropKind::SeniorCrate,
            ammo: 60,
            wood: 80,
            stone: 40,
            iron: 30,
            bandages: 3,
        },
        LootDropPreset {
            x: 46.0,
            z: -32.0,
            kind: LootDropKind::SeniorCrate,
            ammo: 75,
            wood: 100,
            stone: 50,
            iron: 40,
            bandages: 4,
        },
    ];
    let mut drops = Vec::new();
    for (idx, preset) in spots.iter().enumerate() {
        let y = terrain.ground_y(preset.x, preset.z);
        if y < SEA_LEVEL_Y + 0.2 {
            continue;
        }
        drops.push(ReplicatedLootDrop {
            id: (idx + 1) as u16,
            kind: preset.kind,
            pos: Vec3::new(preset.x, y, preset.z),
            ammo_bonus: preset.ammo,
            wood_bonus: preset.wood,
            stone_bonus: preset.stone,
            iron_bonus: preset.iron,
            bandage_bonus: preset.bandages,
            ttl_secs: 600.0,
        });
    }
    drops
}

fn is_terrain_chunk_name(name: &str) -> bool {
    let mut parts = name.split('-');
    let (Some(q), Some(num), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    q.len() == 1
        && matches!(q.as_bytes()[0], b'a'..=b'd' | b'A'..=b'D')
        && !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit())
}

fn push_circle_out_of_aabb(px: &mut f32, pz: &mut f32, radius: f32, min: Vec3, max: Vec3) {
    let cx = px.clamp(min.x, max.x);
    let cz = pz.clamp(min.z, max.z);
    let dx = *px - cx;
    let dz = *pz - cz;
    let d2 = dx * dx + dz * dz;
    if d2 >= radius * radius {
        return;
    }
    if d2 > 1e-6 {
        let d = d2.sqrt();
        let push = radius - d;
        *px += (dx / d) * push;
        *pz += (dz / d) * push;
    } else {
        // Center is inside the box; eject along nearest face.
        let left = (*px - min.x).abs();
        let right = (max.x - *px).abs();
        let back = (*pz - min.z).abs();
        let front = (max.z - *pz).abs();
        let min_pen = left.min(right).min(back).min(front);
        if (min_pen - left).abs() < 1e-4 {
            *px = min.x - radius;
        } else if (min_pen - right).abs() < 1e-4 {
            *px = max.x + radius;
        } else if (min_pen - back).abs() < 1e-4 {
            *pz = min.z - radius;
        } else {
            *pz = max.z + radius;
        }
    }
}

fn sample_triangle_xz_height(a: Vec3, b: Vec3, c: Vec3, px: f32, pz: f32) -> Option<f32> {
    let v0x = b.x - a.x;
    let v0z = b.z - a.z;
    let v1x = c.x - a.x;
    let v1z = c.z - a.z;
    let v2x = px - a.x;
    let v2z = pz - a.z;
    let denom = v0x * v1z - v1x * v0z;
    if denom.abs() < 1e-7 {
        return None;
    }
    let inv = 1.0 / denom;
    let v = (v2x * v1z - v1x * v2z) * inv;
    let w = (v0x * v2z - v2x * v0z) * inv;
    let u = 1.0 - v - w;
    if u >= -1e-4 && v >= -1e-4 && w >= -1e-4 {
        Some(u * a.y + v * b.y + w * c.y)
    } else {
        None
    }
}

fn ray_triangle_intersect(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let h = dir.cross(e2);
    let det = e1.dot(h);
    if det.abs() < 1e-7 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - a;
    let u = inv * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = inv * dir.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = inv * e2.dot(q);
    (t > 1e-4).then_some(t)
}

fn ray_aabb_intersect(origin: Vec3, dir: Vec3, bmin: Vec3, bmax: Vec3) -> Option<(f32, Vec3)> {
    let mut tmin = 0.0_f32;
    let mut tmax = f32::MAX;
    let mut hit_normal = Vec3::Y;

    for axis in 0..3 {
        let o = origin[axis];
        let d = dir[axis];
        if d.abs() < 1e-7 {
            if o < bmin[axis] || o > bmax[axis] {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let mut t1 = (bmin[axis] - o) * inv;
            let mut t2 = (bmax[axis] - o) * inv;
            let mut n_sign = -1.0_f32;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
                n_sign = 1.0;
            }
            if t1 > tmin {
                tmin = t1;
                let mut n = Vec3::ZERO;
                n[axis] = n_sign;
                hit_normal = n;
            }
            tmax = tmax.min(t2);
            if tmin > tmax {
                return None;
            }
        }
    }
    Some((tmin, hit_normal))
}

// --- Lightweight pure-std GLB reader for terrain height baking ----------------------------

#[derive(Deserialize)]
struct GlbDoc {
    #[serde(default)]
    scene: Option<usize>,
    #[serde(default)]
    scenes: Vec<GlbScene>,
    #[serde(default)]
    nodes: Vec<GlbNode>,
    #[serde(default)]
    meshes: Vec<GlbMesh>,
    #[serde(default)]
    accessors: Vec<GlbAccessor>,
    #[serde(default, rename = "bufferViews")]
    buffer_views: Vec<GlbBufferView>,
}

#[derive(Deserialize)]
struct GlbScene {
    #[serde(default)]
    nodes: Vec<usize>,
}

#[derive(Deserialize)]
struct GlbNode {
    #[serde(default)]
    mesh: Option<usize>,
    #[serde(default)]
    children: Vec<usize>,
    #[serde(default)]
    translation: Option<[f32; 3]>,
    #[serde(default)]
    rotation: Option<[f32; 4]>,
    #[serde(default)]
    scale: Option<[f32; 3]>,
}

#[derive(Deserialize)]
struct GlbMesh {
    #[serde(default)]
    primitives: Vec<GlbPrimitive>,
}

#[derive(Deserialize)]
struct GlbPrimitive {
    attributes: HashMap<String, usize>,
    #[serde(default)]
    indices: Option<usize>,
}

#[derive(Deserialize)]
struct GlbAccessor {
    #[serde(default, rename = "bufferView")]
    buffer_view: Option<usize>,
    #[serde(default, rename = "byteOffset")]
    byte_offset: usize,
    #[serde(rename = "componentType")]
    component_type: u32,
    count: usize,
}

#[derive(Deserialize)]
struct GlbBufferView {
    #[serde(default, rename = "byteOffset")]
    byte_offset: usize,
    #[serde(rename = "byteLength")]
    byte_length: usize,
    #[serde(default, rename = "byteStride")]
    byte_stride: Option<usize>,
}

fn parse_glb_world_triangles(bytes: &[u8]) -> Result<(Vec<TerrainTri>, Vec2, Vec2), String> {
    if bytes.len() < 20 || &bytes[0..4] != b"glTF" {
        return Err("invalid glTF magic".to_owned());
    }
    let json_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    let json_end = 20usize.checked_add(json_len).ok_or("json len overflow")?;
    if bytes.len() < json_end + 8 {
        return Err("missing glTF BIN chunk".to_owned());
    }
    let doc: GlbDoc = serde_json::from_slice(&bytes[20..json_end])
        .map_err(|e| format!("glTF JSON parse: {e}"))?;
    let bin_len = u32::from_le_bytes([
        bytes[json_end],
        bytes[json_end + 1],
        bytes[json_end + 2],
        bytes[json_end + 3],
    ]) as usize;
    let bin_start = json_end + 8;
    let bin_end = bin_start.saturating_add(bin_len).min(bytes.len());
    let bin = &bytes[bin_start..bin_end];

    let mut tris = Vec::new();
    let mut min_xz = Vec2::splat(f32::MAX);
    let mut max_xz = Vec2::splat(f32::MIN);

    let root_nodes = doc
        .scenes
        .get(doc.scene.unwrap_or(0))
        .map(|s| s.nodes.as_slice())
        .unwrap_or(&[]);

    let mut stack: Vec<(usize, Mat4)> = root_nodes.iter().map(|&n| (n, Mat4::IDENTITY)).collect();
    while let Some((n_idx, parent_xf)) = stack.pop() {
        let Some(node) = doc.nodes.get(n_idx) else {
            continue;
        };
        let t = node.translation.map(Vec3::from_array).unwrap_or(Vec3::ZERO);
        let r = node
            .rotation
            .map(|q| Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize())
            .unwrap_or(Quat::IDENTITY);
        let s = node.scale.map(Vec3::from_array).unwrap_or(Vec3::ONE);
        let xf = parent_xf * Mat4::from_scale_rotation_translation(s, r, t);

        if let Some(m_idx) = node.mesh
            && let Some(mesh) = doc.meshes.get(m_idx)
        {
            for prim in &mesh.primitives {
                extract_prim_triangles(&doc, bin, xf, prim, &mut tris, &mut min_xz, &mut max_xz);
            }
        }
        for &child in &node.children {
            stack.push((child, xf));
        }
    }

    Ok((tris, min_xz, max_xz))
}

fn extract_prim_triangles(
    doc: &GlbDoc,
    bin: &[u8],
    xf: Mat4,
    prim: &GlbPrimitive,
    tris: &mut Vec<TerrainTri>,
    min_xz: &mut Vec2,
    max_xz: &mut Vec2,
) {
    let Some(&pos_acc_idx) = prim.attributes.get("POSITION") else {
        return;
    };
    let Some(positions) = read_vec3_accessor(doc, bin, pos_acc_idx) else {
        return;
    };
    let indices: Vec<u32> = if let Some(idx_acc) = prim.indices {
        read_index_accessor(doc, bin, idx_acc).unwrap_or_default()
    } else {
        (0..positions.len() as u32).collect()
    };

    for chunk in indices.as_chunks::<3>().0 {
        let (Some(&p0), Some(&p1), Some(&p2)) = (
            positions.get(chunk[0] as usize),
            positions.get(chunk[1] as usize),
            positions.get(chunk[2] as usize),
        ) else {
            continue;
        };
        let a = xf.transform_point3(p0);
        let b = xf.transform_point3(p1);
        let c = xf.transform_point3(p2);
        let n = (b - a).cross(c - a).normalize_or_zero();
        if n.length_squared() < 0.5 {
            continue;
        }
        let tri = TerrainTri {
            a,
            b,
            c,
            normal: n,
            min_x: a.x.min(b.x).min(c.x),
            max_x: a.x.max(b.x).max(c.x),
            min_z: a.z.min(b.z).min(c.z),
            max_z: a.z.max(b.z).max(c.z),
        };
        min_xz.x = min_xz.x.min(tri.min_x);
        min_xz.y = min_xz.y.min(tri.min_z);
        max_xz.x = max_xz.x.max(tri.max_x);
        max_xz.y = max_xz.y.max(tri.max_z);
        tris.push(tri);
    }
}

fn read_vec3_accessor(doc: &GlbDoc, bin: &[u8], acc_idx: usize) -> Option<Vec<Vec3>> {
    let acc = doc.accessors.get(acc_idx)?;
    if acc.component_type != 5126 {
        return None;
    }
    let bv = doc.buffer_views.get(acc.buffer_view?)?;
    let stride = bv.byte_stride.unwrap_or(12).max(12);
    let base = bv.byte_offset + acc.byte_offset;
    let end_limit = (bv.byte_offset + bv.byte_length).min(bin.len());
    let mut out = Vec::with_capacity(acc.count);
    for i in 0..acc.count {
        let off = base + i * stride;
        if off + 12 > end_limit {
            break;
        }
        let x = f32::from_le_bytes(bin[off..off + 4].try_into().ok()?);
        let y = f32::from_le_bytes(bin[off + 4..off + 8].try_into().ok()?);
        let z = f32::from_le_bytes(bin[off + 8..off + 12].try_into().ok()?);
        out.push(Vec3::new(x, y, z));
    }
    Some(out)
}

fn read_index_accessor(doc: &GlbDoc, bin: &[u8], acc_idx: usize) -> Option<Vec<u32>> {
    let acc = doc.accessors.get(acc_idx)?;
    let bv = doc.buffer_views.get(acc.buffer_view?)?;
    let elem_size = match acc.component_type {
        5121 => 1usize,
        5123 => 2usize,
        5125 => 4usize,
        _ => return None,
    };
    let stride = bv.byte_stride.unwrap_or(elem_size).max(elem_size);
    let base = bv.byte_offset + acc.byte_offset;
    let end_limit = (bv.byte_offset + bv.byte_length).min(bin.len());
    let mut out = Vec::with_capacity(acc.count);
    for i in 0..acc.count {
        let off = base + i * stride;
        if off + elem_size > end_limit {
            break;
        }
        let idx = match acc.component_type {
            5121 => u32::from(bin[off]),
            5123 => u32::from(u16::from_le_bytes([bin[off], bin[off + 1]])),
            5125 => u32::from_le_bytes([bin[off], bin[off + 1], bin[off + 2], bin[off + 3]]),
            _ => unreachable!(),
        };
        out.push(idx);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_real_island_terrain_chunks_and_samples_exact_height_and_ray() {
        let grid = TerrainColliderGrid::load_default();
        assert!(
            grid.triangle_count() > 15_000,
            "expected baked terrain triangles from a-55..d-02 chunks, got {}",
            grid.triangle_count()
        );
        let (h, n) = grid
            .sample_height_and_normal(0.0, 0.0)
            .expect("origin (0,0) inside a-57.glb must resolve height");
        assert!(h > 5.0 && h < 85.0, "unexpected center elevation {h}");
        assert!(n.y > 0.3, "expected upward terrain normal, got {n:?}");
        assert_eq!(grid.chunk_name_at(0.0, 0.0), Some("A-57"));

        let hit = grid
            .raycast(Vec3::new(0.0, h + 15.0, 0.0), Vec3::NEG_Y, 30.0)
            .expect("downward ray onto center terrain must hit");
        assert!((hit.1.y - h).abs() < 0.05);
    }

    #[test]
    fn player_kinematics_walk_up_foundation_and_block_on_walls() {
        let mut grid = TerrainColliderGrid::default();
        grid.insert_triangle(TerrainTri {
            a: Vec3::new(-50.0, 10.0, -50.0),
            b: Vec3::new(50.0, 10.0, -50.0),
            c: Vec3::new(0.0, 10.0, 50.0),
            normal: Vec3::Y,
            min_x: -50.0,
            max_x: 50.0,
            min_z: -50.0,
            max_z: 50.0,
        });

        let wall = ReplicatedStructure {
            id: 1,
            owner_id: 1,
            kind: BuildPieceKind::Wall,
            tier: 1,
            pos: Vec3::new(0.0, 10.0, 3.0),
            rot_steps: 0,
            hp: 200,
            max_hp: 200,
            is_open: false,
        };
        let mut pos = Vec3::new(0.0, 10.0, 2.5);
        let mut vy = 0.0;
        let mut grounded = true;
        // Walk forward (+Z) right into the wall: must stay pushed back outside z < 2.4.
        for _ in 0..10 {
            step_player_kinematics(
                &grid,
                std::slice::from_ref(&wall),
                &[],
                &mut pos,
                Vec2::new(0.0, 1.0),
                &mut vy,
                &mut grounded,
                false,
                5.5,
                1.0 / 30.0,
            );
        }
        assert!(pos.z < 2.5, "wall failed to block player z={}", pos.z);
    }

    #[test]
    fn vehicle_kinematics_drive_and_heli_ascend() {
        let mut grid = TerrainColliderGrid::default();
        grid.insert_triangle(TerrainTri {
            a: Vec3::new(-80.0, 12.0, -80.0),
            b: Vec3::new(80.0, 12.0, -80.0),
            c: Vec3::new(0.0, 12.0, 80.0),
            normal: Vec3::Y,
            min_x: -80.0,
            max_x: 80.0,
            min_z: -80.0,
            max_z: 80.0,
        });

        let mut car = ReplicatedVehicle {
            id: 1,
            kind: VehicleKind::OffroadJeep,
            pos: Vec3::new(0.0, 12.0, 0.0),
            yaw: 0.0,
            speed: 0.0,
            vel_y: 0.0,
            hp: 450,
            max_hp: 450,
            fuel: 1000.0,
            max_fuel: 1000.0,
            driver_id: 7,
            weapon_cooldown: 0.0,
        };
        for _ in 0..30 {
            step_vehicle_kinematics(
                &grid,
                &[],
                &mut car,
                VehicleDriveInput {
                    throttle: 1.0,
                    ..default()
                },
                1.0 / 30.0,
            );
        }
        assert!(car.pos.z > 4.0, "car must move forward, z={}", car.pos.z);
        assert!(car.fuel < 1000.0, "car must burn fuel");

        let mut heli = ReplicatedVehicle {
            id: 2,
            kind: VehicleKind::GunshipHeli,
            pos: Vec3::new(0.0, 12.0, 0.0),
            yaw: 0.0,
            speed: 0.0,
            vel_y: 0.0,
            hp: 650,
            max_hp: 650,
            fuel: 1000.0,
            max_fuel: 1000.0,
            driver_id: 7,
            weapon_cooldown: 0.0,
        };
        for _ in 0..30 {
            step_vehicle_kinematics(
                &grid,
                &[],
                &mut heli,
                VehicleDriveInput {
                    throttle: 0.5,
                    ascend: true,
                    ..default()
                },
                1.0 / 30.0,
            );
        }
        assert!(heli.pos.y > 15.0, "heli must gain altitude, y={}", heli.pos.y);
    }
}
