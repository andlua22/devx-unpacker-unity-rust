//! Pure `std::net::UdpSocket` authoritative server (up to 100 players) and client transport
//! running on Bevy `FixedUpdate`.

use std::collections::{BTreeMap, HashMap};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;

use crate::config::ServerEntry;
use crate::crafting::{
    CraftCatalog, CraftContext, CraftMode, CraftQueue, CraftRequest, ItemAmount, ItemInventory,
};
use crate::world_physics::{
    BuildPieceKind, EnemyKind, LootDropKind, ReplicatedEnemy, ReplicatedLootDrop,
    ReplicatedResourceNode, ReplicatedStructure, ResourceNodeKind, TerrainColliderGrid,
    generate_default_enemies, generate_default_loot_drops, generate_default_resource_nodes,
    step_player_kinematics,
};

use super::interest::{InterestGrid, SpatialPeer};
use super::protocol::{
    MAX_PLAYERS, MAX_SNAPSHOT_PEERS, NetEnemyDto, NetLootDropDto, NetPlayerState, PacketEnvelope,
    PacketKind, TICK_HZ, decode_packet, encode_packet,
};

const PLAYER_MOVE_SPEED: f32 = 5.5;
const PLAYER_SPRINT_SPEED: f32 = 8.4;
const ARENA_HALF_EXTENT: f32 = 560.0;

const WOOD_ITEM_ID: i32 = 50001;
const CLOTH_ITEM_ID: i32 = 50003;
const STONE_ITEM_ID: i32 = 50006;
const IRON_ITEM_ID: i32 = 50008;
const RIFLE_AMMO_ITEM_ID: i32 = 53202;
const BANDAGE_ITEM_ID: i32 = 55101;

fn initial_inventory(catalog: &CraftCatalog) -> ItemInventory {
    // Retain existing local-host starter quantities; original spawn inventory remains unknown.
    ItemInventory::from_counts(
        catalog,
        [
            (WOOD_ITEM_ID, 120),
            (STONE_ITEM_ID, 60),
            (IRON_ITEM_ID, 25),
            (CLOTH_ITEM_ID, 30),
            (RIFLE_AMMO_ITEM_ID, 60),
            (BANDAGE_ITEM_ID, 2),
        ],
    )
    .expect("starter inventory uses installed native ItemCfg IDs")
}

/// Server-owned authoritative state for one connected player or virtual stress-test peer.
#[derive(Debug, Clone)]
pub struct ServerPlayer {
    pub player_id: u16,
    pub account_name: String,
    pub pos: Vec3,
    pub wish_xz: Vec2,
    pub yaw: f32,
    pub vertical_vel: f32,
    pub grounded: bool,
    pub jump_pressed: bool,
    pub ascend_held: bool,
    pub descend: bool,
    pub sprint: bool,
    pub anim_state: u8,
    pub hp: u16,
    pub inventory: ItemInventory,
    pub craft_queue: CraftQueue,
    pub craft_context: CraftContext,
    craft_requests: HashMap<u32, (CraftRequest, i64, bool, String)>,
    newest_craft_request: Option<u32>,
    pub is_bot: bool,
    pub last_heard_tick: u32,
}

impl ServerPlayer {
    fn wood(&self) -> u32 {
        self.inventory.amount(WOOD_ITEM_ID) as u32
    }
    fn stone(&self) -> u32 {
        self.inventory.amount(STONE_ITEM_ID) as u32
    }
    fn iron(&self) -> u32 {
        self.inventory.amount(IRON_ITEM_ID) as u32
    }
    fn fiber(&self) -> u32 {
        self.inventory.amount(CLOTH_ITEM_ID) as u32
    }
    fn ammo(&self) -> u16 {
        self.inventory
            .amount(RIFLE_AMMO_ITEM_ID)
            .min(i32::from(u16::MAX)) as u16
    }
    fn bandages(&self) -> u16 {
        self.inventory
            .amount(BANDAGE_ITEM_ID)
            .min(i32::from(u16::MAX)) as u16
    }

    fn grant(&mut self, catalog: &CraftCatalog, items: &[(i32, i32)]) {
        let amounts: Vec<_> = items
            .iter()
            .map(|&(item_id, num)| ItemAmount { item_id, num })
            .collect();
        let _ = self.inventory.add_atomic(catalog, &amounts);
    }

    fn spend(&mut self, items: &[(i32, i32)]) -> bool {
        let amounts: Vec<_> = items
            .iter()
            .filter(|(_, num)| *num > 0)
            .map(|&(item_id, num)| ItemAmount { item_id, num })
            .collect();
        self.inventory.consume_atomic(&amounts).is_ok()
    }
}

/// Authoritative server simulation enforcing [`MAX_PLAYERS`] and per-observer [`InterestGrid`]
/// replication over real terrain geometry and dynamic structures.
pub struct ServerWorld {
    pub server_name: String,
    pub tick: u32,
    next_player_id: u16,
    pub(super) players: HashMap<u16, ServerPlayer>,
    pub craft_catalog: CraftCatalog,
    pub storage_server_id: u32,
    battle_craft_mode: CraftMode,
    battle_mode: i32,
    saved_production: HashMap<String, (ItemInventory, CraftQueue)>,
    detached_building_owners: HashMap<u32, String>,
    addr_to_id: HashMap<SocketAddr, u16>,
    grid: InterestGrid,
    pub terrain: TerrainColliderGrid,
    pub structures: Vec<ReplicatedStructure>,
    pub nodes: Vec<ReplicatedResourceNode>,
    pub enemies: Vec<ReplicatedEnemy>,
    pub loot_drops: Vec<ReplicatedLootDrop>,
    pub vehicles: Vec<crate::world_physics::ReplicatedVehicle>,
    turret_cooldowns: HashMap<u32, f32>,
    pub generator_fuels: HashMap<u32, f32>,
    pub generator_active: HashMap<u32, bool>,
    pub base_power_pct: u8,
    next_build_id: u32,
    next_drop_id: u16,
    pub world_rev: u32,
    last_broadcast_rev: u32,
}

impl ServerWorld {
    pub fn new(server_name: impl Into<String>) -> Self {
        let craft_catalog = CraftCatalog::load_default().expect(
            "native ItemCfg and DrawingCfg must be installed before starting the game server",
        );
        let terrain = TerrainColliderGrid::load_default();
        let nodes = generate_default_resource_nodes(&terrain);
        let enemies = generate_default_enemies(&terrain);
        let loot_drops = generate_default_loot_drops(&terrain);
        let vehicles = crate::world_physics::generate_default_vehicles(&terrain);
        Self {
            server_name: server_name.into(),
            tick: 0,
            next_player_id: 1,
            players: HashMap::with_capacity(MAX_PLAYERS),
            craft_catalog,
            storage_server_id: 0,
            battle_craft_mode: CraftMode::Island,
            battle_mode: 0,
            saved_production: HashMap::new(),
            detached_building_owners: HashMap::new(),
            addr_to_id: HashMap::with_capacity(MAX_PLAYERS),
            grid: InterestGrid::default(),
            terrain,
            structures: Vec::new(),
            nodes,
            enemies,
            loot_drops,
            vehicles,
            turret_cooldowns: HashMap::new(),
            generator_fuels: HashMap::new(),
            generator_active: HashMap::new(),
            base_power_pct: 0,
            next_build_id: 1,
            next_drop_id: 100,
            world_rev: 1,
            last_broadcast_rev: 0,
        }
    }

    pub fn active_count(&self) -> usize {
        self.players.len()
    }

    pub fn bot_count(&self) -> usize {
        self.players.values().filter(|p| p.is_bot).count()
    }

    /// Registers a network client (or refreshes an already-connected address), enforcing
    /// [`MAX_PLAYERS`] capacity (evicting virtual bots only if real humans join, or rejecting when
    /// 100 real humans fill all slots).
    pub fn accept_or_reject_client(
        &mut self,
        addr: SocketAddr,
        account_name: String,
    ) -> Result<u16, &'static str> {
        if let Some(&existing_id) = self.addr_to_id.get(&addr) {
            if let Some(p) = self.players.get_mut(&existing_id) {
                if p.account_name != account_name {
                    return Err("account_changed_reconnect");
                }
                p.last_heard_tick = self.tick;
            }
            return Ok(existing_id);
        }

        if self.players.len() >= MAX_PLAYERS {
            // Free a virtual stress-test slot if one exists before rejecting a human connection.
            if let Some(&bot_id) = self
                .players
                .iter()
                .find(|(_, p)| p.is_bot)
                .map(|(id, _)| id)
            {
                self.players.remove(&bot_id);
            } else {
                return Err("server_full_100");
            }
        }

        let player_id = self.allocate_player_id().ok_or("server_full_100")?;
        let ring_angle = (f32::from(player_id) * 0.618_034) * std::f32::consts::TAU;
        let spawn_r = 3.0 + (f32::from(player_id % 8)) * 0.45;
        let sx = ring_angle.cos() * spawn_r;
        let sz = ring_angle.sin() * spawn_r;
        let sy = self.terrain.ground_y(sx, sz);

        let (inventory, craft_queue) =
            self.saved_production
                .remove(&account_name)
                .unwrap_or_else(|| {
                    (
                        initial_inventory(&self.craft_catalog),
                        CraftQueue::default(),
                    )
                });
        for structure in &mut self.structures {
            if self.detached_building_owners.get(&structure.id) == Some(&account_name) {
                structure.owner_id = player_id;
                self.detached_building_owners.remove(&structure.id);
            }
        }
        self.players.insert(
            player_id,
            ServerPlayer {
                player_id,
                account_name,
                pos: Vec3::new(sx, sy, sz),
                wish_xz: Vec2::ZERO,
                yaw: 0.0,
                vertical_vel: 0.0,
                grounded: true,
                jump_pressed: false,
                ascend_held: false,
                descend: false,
                sprint: false,
                anim_state: 0,
                hp: 100,
                inventory,
                craft_queue,
                craft_context: CraftContext {
                    mode: self.battle_craft_mode,
                    battle_mode: self.battle_mode,
                    queue_limit: 5,
                    ..Default::default()
                },
                craft_requests: HashMap::new(),
                newest_craft_request: None,
                is_bot: false,
                last_heard_tick: self.tick,
            },
        );
        self.addr_to_id.insert(addr, player_id);
        Ok(player_id)
    }

    pub fn configure_battle(&mut self, mode: CraftMode, battle_mode: i32) {
        self.battle_craft_mode = mode;
        self.battle_mode = battle_mode;
        for player in self.players.values_mut() {
            player.craft_context.mode = mode;
            player.craft_context.battle_mode = battle_mode;
        }
    }

    fn allocate_player_id(&mut self) -> Option<u16> {
        for _ in 0..=u16::MAX {
            let cand = self.next_player_id.max(1);
            self.next_player_id = self.next_player_id.wrapping_add(1).max(1);
            if !self.players.contains_key(&cand) {
                return Some(cand);
            }
        }
        None
    }

    pub fn remove_addr(&mut self, addr: &SocketAddr) {
        if let Some(id) = self.addr_to_id.remove(addr)
            && let Some(player) = self.players.remove(&id)
        {
            for structure in self
                .structures
                .iter_mut()
                .filter(|structure| structure.owner_id == id)
            {
                self.detached_building_owners
                    .insert(structure.id, player.account_name.clone());
                structure.owner_id = 0;
            }
            self.saved_production
                .insert(player.account_name, (player.inventory, player.craft_queue));
            self.world_rev = self.world_rev.wrapping_add(1);
        }
    }

    /// Applies an idempotent production request. Retries return the original result.
    pub fn apply_craft_request(
        &mut self,
        addr: &SocketAddr,
        request_id: u32,
        drawing_id: i32,
        create_num: i32,
        near_building_instance_id: i64,
    ) -> PacketKind {
        let result = |accepted, reason| PacketKind::CraftResult {
            request_id,
            accepted,
            reason,
        };
        let Some(player) = self
            .addr_to_id
            .get(addr)
            .and_then(|id| self.players.get_mut(id))
        else {
            return result(false, "Connect before requesting production".to_owned());
        };
        let request = CraftRequest {
            drawing_id,
            create_num,
        };
        if let Some((original, facility, accepted, reason)) = player.craft_requests.get(&request_id)
        {
            if original != &request || *facility != near_building_instance_id {
                return result(
                    false,
                    "Request identifier already belongs to another recipe".to_owned(),
                );
            }
            return result(*accepted, reason.clone());
        }
        if let Some(newest) = player.newest_craft_request
            && !super::inventory::revision_is_newer(request_id, newest)
            && newest.wrapping_sub(request_id) >= 128
        {
            return result(false, "Expired production request".to_owned());
        }
        if player
            .newest_craft_request
            .is_none_or(|newest| super::inventory::revision_is_newer(request_id, newest))
        {
            player.newest_craft_request = Some(request_id);
        }
        let newest = player.newest_craft_request.unwrap();
        player
            .craft_requests
            .retain(|id, _| newest.wrapping_sub(*id) < 128);
        let mut context = player.craft_context.clone();
        // BuildManager.AimDis is initialized to 4.5 in the native controller.
        context.workbenches = self
            .structures
            .iter()
            .filter(|structure| {
                structure.kind == BuildPieceKind::Workbench
                    && structure.hp > 0
                    && structure.pos.distance_squared(player.pos) <= 4.5 * 4.5
            })
            .map(|structure| crate::crafting::WorkbenchAccess {
                facility_id: i64::from(structure.id),
                kind: 1,
                level: i32::from(structure.tier),
            })
            .collect();
        // Only facilities established by the authoritative world can satisfy a requirement.
        if near_building_instance_id != -1 {
            context
                .workbenches
                .retain(|facility| facility.facility_id == near_building_instance_id);
        }
        let outcome = player.craft_queue.enqueue(
            &self.craft_catalog,
            &mut player.inventory,
            request,
            &context,
        );
        let accepted = outcome.is_ok();
        let reason = match outcome {
            Ok(()) => {
                self.world_rev = self.world_rev.wrapping_add(1);
                "Production queued".to_owned()
            }
            Err(error) => error.to_string(),
        };
        player.craft_requests.insert(
            request_id,
            (request, near_building_instance_id, accepted, reason.clone()),
        );
        result(accepted, reason)
    }

    /// Fills remaining server slots up to [`MAX_PLAYERS`] with deterministic moving bots to stress-test
    /// 100-player spatial interest replication.
    pub fn set_stress_bots_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.players.retain(|_, p| !p.is_bot);
            return;
        }
        let mut idx = 0usize;
        while self.players.len() < MAX_PLAYERS {
            let Some(player_id) = self.allocate_player_id() else {
                break;
            };
            let angle = (idx as f32 / MAX_PLAYERS as f32) * std::f32::consts::TAU;
            let radius = 12.0 + (idx % 6) as f32 * 9.0;
            let bx = angle.cos() * radius;
            let bz = angle.sin() * radius;
            let by = self.terrain.ground_y(bx, bz);
            self.players.insert(
                player_id,
                ServerPlayer {
                    player_id,
                    account_name: format!("SimBot_{idx:02}"),
                    pos: Vec3::new(bx, by, bz),
                    wish_xz: Vec2::ZERO,
                    yaw: angle,
                    vertical_vel: 0.0,
                    grounded: true,
                    jump_pressed: false,
                    ascend_held: false,
                    descend: false,
                    sprint: false,
                    anim_state: 1,
                    hp: 100,
                    inventory: ItemInventory::default(),
                    craft_queue: CraftQueue::default(),
                    craft_context: CraftContext {
                        queue_limit: 5,
                        ..Default::default()
                    },
                    craft_requests: HashMap::new(),
                    newest_craft_request: None,
                    is_bot: true,
                    last_heard_tick: self.tick,
                },
            );
            idx += 1;
        }
    }

    pub fn apply_input(&mut self, addr: &SocketAddr, move_xz: [f32; 2], yaw: f32, buttons: u16) {
        let Some(&id) = self.addr_to_id.get(addr) else {
            return;
        };
        let Some(p) = self.players.get_mut(&id) else {
            return;
        };
        let wish = Vec2::new(move_xz[0], move_xz[1]);
        p.wish_xz = if wish.length_squared() > 1.0 {
            wish.normalize()
        } else {
            wish
        };
        p.yaw = yaw;
        p.sprint = (buttons & 2) != 0;
        p.ascend_held = (buttons & 4) != 0;
        p.descend = (buttons & 8) != 0;
        if (buttons & 4) != 0 {
            p.jump_pressed = true;
        }
        p.anim_state = if (buttons & 1) != 0 {
            3 // attack/action bit
        } else if p.wish_xz.length_squared() > 0.04 {
            if p.sprint { 2 } else { 1 }
        } else {
            0 // idle
        };
        p.last_heard_tick = self.tick;
    }

    /// Processes an authoritative building, harvest, combat, or crafting request from a client.
    pub fn apply_world_op(
        &mut self,
        addr: &SocketAddr,
        op: u8,
        target_id: u32,
        param_a: u8,
        param_b: u8,
        pos: [f32; 3],
    ) {
        let Some(&pid) = self.addr_to_id.get(addr) else {
            return;
        };
        let Some(p) = self.players.get_mut(&pid) else {
            return;
        };
        match op {
            super::protocol::OP_HARVEST_NODE => {
                let node_id = target_id as u16;
                if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id && n.hp > 0) {
                    let tool = param_a;
                    let dmg: u16 = match tool {
                        1 => 42, // stone hatchet bonus
                        2 => 30, // knife
                        _ => 22, // firearm/unarmed
                    };
                    node.hp = node.hp.saturating_sub(dmg);
                    if node.hp == 0 {
                        node.respawn_secs = 30.0;
                    }
                    let node_kind = node.kind;
                    let node_pos = node.pos;
                    match node_kind {
                        ResourceNodeKind::PineTree | ResourceNodeKind::PalmTree => {
                            p.grant(&self.craft_catalog, &[(WOOD_ITEM_ID, 16)]);
                            p.grant(&self.craft_catalog, &[(CLOTH_ITEM_ID, 6)]);
                        }
                        ResourceNodeKind::OreBoulder => {
                            p.grant(&self.craft_catalog, &[(STONE_ITEM_ID, 14)]);
                            p.grant(&self.craft_catalog, &[(IRON_ITEM_ID, 6)]);
                        }
                        ResourceNodeKind::SupplyCrate => {
                            p.grant(&self.craft_catalog, &[(RIFLE_AMMO_ITEM_ID, 12)]);
                            p.grant(&self.craft_catalog, &[(BANDAGE_ITEM_ID, 1)]);
                            p.grant(&self.craft_catalog, &[(WOOD_ITEM_ID, 8)]);
                        }
                    }
                    if node.hp == 0 && node_kind == ResourceNodeKind::SupplyCrate {
                        self.spawn_loot_drop(LootDropKind::DropSack, node_pos);
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            super::protocol::OP_HIT_PLAYER => {
                let victim_id = target_id as u16;
                let dmg = u16::from(param_b.clamp(5, 65));
                if let Some(victim) = self.players.get_mut(&victim_id) {
                    victim.hp = victim.hp.saturating_sub(dmg);
                    if victim.hp == 0 {
                        victim.hp = 100;
                        let angle = (f32::from(victim_id) * 1.1) % std::f32::consts::TAU;
                        let rx = angle.cos() * 28.0;
                        let rz = angle.sin() * 28.0;
                        victim.pos = Vec3::new(rx, self.terrain.ground_y(rx, rz), rz);
                        if let Some(attacker) = self.players.get_mut(&pid) {
                            attacker.grant(&self.craft_catalog, &[(WOOD_ITEM_ID, 25)]);
                            attacker.grant(&self.craft_catalog, &[(STONE_ITEM_ID, 15)]);
                            attacker.grant(&self.craft_catalog, &[(RIFLE_AMMO_ITEM_ID, 10)]);
                        }
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            super::protocol::OP_PLACE_BUILD => {
                let kind = BuildPieceKind::from_u8(param_a);
                let cost = kind.wood_cost();
                if p.wood() < cost {
                    return;
                }
                let p_pos = Vec3::new(
                    pos[0].clamp(-ARENA_HALF_EXTENT, ARENA_HALF_EXTENT),
                    pos[1],
                    pos[2].clamp(-ARENA_HALF_EXTENT, ARENA_HALF_EXTENT),
                );
                // Prevent duplicate overlapping placements at the exact same grid socket.
                let duplicate = self
                    .structures
                    .iter()
                    .any(|s| s.kind == kind && s.pos.distance_squared(p_pos) < 0.25);
                if duplicate {
                    return;
                }
                if !p.spend(&[(WOOD_ITEM_ID, cost as i32)]) {
                    return;
                }
                let max_hp = kind.max_hp_for_tier(1);
                let id = self.next_build_id;
                self.next_build_id = self.next_build_id.wrapping_add(1);
                let is_open = kind == BuildPieceKind::Generator;
                self.structures.push(ReplicatedStructure {
                    id,
                    owner_id: pid,
                    kind,
                    tier: 1,
                    pos: p_pos,
                    rot_steps: param_b & 3,
                    hp: max_hp,
                    max_hp,
                    is_open,
                });
                if kind == BuildPieceKind::Generator {
                    self.generator_fuels.insert(id, 600.0);
                    self.generator_active.insert(id, true);
                }
                self.world_rev = self.world_rev.wrapping_add(1);
            }
            super::protocol::OP_UPGRADE_BUILD => {
                let Some(st) = self.structures.iter_mut().find(|s| s.id == target_id) else {
                    return;
                };
                if st.tier >= 4 {
                    return;
                }
                let stone_cost = u32::from(st.tier) * 20;
                let iron_cost = if st.tier >= 2 {
                    u32::from(st.tier - 1) * 15
                } else {
                    0
                };
                if p.stone() < stone_cost || p.iron() < iron_cost {
                    return;
                }
                if !p.spend(&[
                    (STONE_ITEM_ID, stone_cost as i32),
                    (IRON_ITEM_ID, iron_cost as i32),
                ]) {
                    return;
                }
                st.tier += 1;
                st.max_hp = st.kind.max_hp_for_tier(st.tier);
                st.hp = st.max_hp;
                self.world_rev = self.world_rev.wrapping_add(1);
            }
            super::protocol::OP_TOGGLE_DOOR => {
                if let Some(st) = self
                    .structures
                    .iter_mut()
                    .find(|s| s.id == target_id && s.kind == BuildPieceKind::Door)
                {
                    st.is_open = !st.is_open;
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            // Native production uses the signed 32-bit CraftRequest message.
            super::protocol::OP_CRAFT_RECIPE => (),
            super::protocol::OP_CONSUME_BANDAGE => {
                let healing = self
                    .craft_catalog
                    .item(BANDAGE_ITEM_ID)
                    .and_then(|item| item.extras.first())
                    .copied()
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(0.0) as u16;
                if p.bandages() > 0 && p.hp < 100 {
                    if !p.spend(&[(BANDAGE_ITEM_ID, 1)]) {
                        return;
                    }
                    p.hp = p.hp.saturating_add(healing).min(100);
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            super::protocol::OP_SPEND_AMMO => {
                p.spend(&[(RIFLE_AMMO_ITEM_ID, 1)]);
                self.world_rev = self.world_rev.wrapping_add(1);
            }
            super::protocol::OP_HIT_ENEMY => {
                let enemy_id = target_id as u16;
                let dmg = u16::from(param_b.clamp(5, 85));
                if let Some((killed_kind, drop_pos)) = self.damage_enemy(enemy_id, dmg) {
                    if let Some(attacker) = self.players.get_mut(&pid) {
                        let mult = if killed_kind == EnemyKind::PortBossMech {
                            4
                        } else {
                            1
                        };
                        attacker.grant(&self.craft_catalog, &[(WOOD_ITEM_ID, 15 * mult)]);
                        attacker.grant(&self.craft_catalog, &[(IRON_ITEM_ID, 8 * mult)]);
                    }
                    let drop_kind = if killed_kind == EnemyKind::PortBossMech {
                        LootDropKind::SeniorCrate
                    } else {
                        LootDropKind::DropSack
                    };
                    self.spawn_loot_drop(drop_kind, drop_pos);
                }
                self.world_rev = self.world_rev.wrapping_add(1);
            }
            super::protocol::OP_LOOT_DROP => {
                let drop_id = target_id as u16;
                let p_pos = p.pos;
                if let Some(idx) = self.loot_drops.iter().position(|d| d.id == drop_id)
                    && self.loot_drops[idx].pos.distance(p_pos) <= 5.5
                {
                    let drop = self.loot_drops.swap_remove(idx);
                    if let Some(picker) = self.players.get_mut(&pid) {
                        picker.grant(
                            &self.craft_catalog,
                            &[(RIFLE_AMMO_ITEM_ID, (drop.ammo_bonus) as i32)],
                        );
                        picker.grant(
                            &self.craft_catalog,
                            &[(WOOD_ITEM_ID, (u32::from(drop.wood_bonus)) as i32)],
                        );
                        picker.grant(
                            &self.craft_catalog,
                            &[(STONE_ITEM_ID, (u32::from(drop.stone_bonus)) as i32)],
                        );
                        picker.grant(
                            &self.craft_catalog,
                            &[(IRON_ITEM_ID, (u32::from(drop.iron_bonus)) as i32)],
                        );
                        picker.grant(
                            &self.craft_catalog,
                            &[(BANDAGE_ITEM_ID, i32::from(drop.bandage_bonus))],
                        );
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            super::protocol::OP_ENTER_EXIT_VEHICLE => {
                let p_pos = p.pos;
                if let Some(v) = self.vehicles.iter_mut().find(|v| v.driver_id == pid) {
                    v.driver_id = 0;
                    v.speed *= 0.25;
                    let side = Quat::from_rotation_y(v.yaw) * Vec3::X;
                    let raw_exit = v.pos + side * 2.8;
                    let exit_y = self
                        .terrain
                        .ground_y(raw_exit.x, raw_exit.z)
                        .max(crate::world_physics::SEA_LEVEL_Y + 0.35);
                    if let Some(driver) = self.players.get_mut(&pid) {
                        driver.pos = Vec3::new(raw_exit.x, exit_y, raw_exit.z);
                        driver.vertical_vel = 0.0;
                        driver.grounded = true;
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                } else {
                    let target_vid = target_id as u16;
                    let pick_idx = self
                        .vehicles
                        .iter()
                        .position(|v| {
                            v.id == target_vid
                                && v.driver_id == 0
                                && v.hp > 0
                                && v.pos.distance_squared(p_pos) <= 9.0 * 9.0
                        })
                        .or_else(|| {
                            self.vehicles
                                .iter()
                                .enumerate()
                                .filter(|(_, v)| {
                                    v.driver_id == 0
                                        && v.hp > 0
                                        && v.pos.distance_squared(p_pos) <= 9.0 * 9.0
                                })
                                .min_by(|(_, a), (_, b)| {
                                    a.pos
                                        .distance_squared(p_pos)
                                        .total_cmp(&b.pos.distance_squared(p_pos))
                                })
                                .map(|(i, _)| i)
                        });
                    if let Some(vi) = pick_idx {
                        self.vehicles[vi].driver_id = pid;
                        let seat_pos = self.vehicles[vi].pos + self.vehicles[vi].kind.seat_offset();
                        let seat_yaw = self.vehicles[vi].yaw;
                        if let Some(driver) = self.players.get_mut(&pid) {
                            driver.pos = seat_pos;
                            driver.yaw = seat_yaw;
                            driver.vertical_vel = 0.0;
                            driver.grounded = true;
                        }
                        self.world_rev = self.world_rev.wrapping_add(1);
                    }
                }
            }
            super::protocol::OP_REFUEL_OR_TOGGLE_POWER => {
                let p_pos = p.pos;
                let gen_id = self
                    .structures
                    .iter()
                    .find(|s| s.id == target_id && s.kind == BuildPieceKind::Generator && s.hp > 0)
                    .or_else(|| {
                        self.structures
                            .iter()
                            .filter(|s| {
                                s.kind == BuildPieceKind::Generator
                                    && s.hp > 0
                                    && s.pos.distance_squared(p_pos) <= 9.0 * 9.0
                            })
                            .min_by(|a, b| {
                                a.pos
                                    .distance_squared(p_pos)
                                    .total_cmp(&b.pos.distance_squared(p_pos))
                            })
                    })
                    .map(|s| s.id);
                if let Some(gid) = gen_id {
                    let cur_fuel = *self.generator_fuels.get(&gid).unwrap_or(&0.0);
                    let cur_active = *self.generator_active.get(&gid).unwrap_or(&true);
                    if cur_fuel < 400.0 && p.wood() >= 15 && p.iron() >= 10 {
                        if !p.spend(&[(WOOD_ITEM_ID, 15), (IRON_ITEM_ID, 10)]) {
                            return;
                        }
                        let f = self.generator_fuels.entry(gid).or_insert(0.0);
                        *f = (*f + 350.0).min(1000.0);
                        self.generator_active.insert(gid, true);
                    } else {
                        self.generator_active.insert(gid, !cur_active);
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            super::protocol::OP_REPAIR_VEHICLE => {
                let p_pos = p.pos;
                if p.wood() >= 15
                    && p.iron() >= 10
                    && let Some(v) = self
                        .vehicles
                        .iter_mut()
                        .filter(|v| {
                            v.driver_id == pid
                                || v.id == target_id as u16
                                || v.pos.distance_squared(p_pos) <= 10.0 * 10.0
                        })
                        .min_by(|a, b| {
                            a.pos
                                .distance_squared(p_pos)
                                .total_cmp(&b.pos.distance_squared(p_pos))
                        })
                {
                    v.hp = v.hp.saturating_add(180).min(v.max_hp);
                    v.fuel = (v.fuel + 150.0).min(v.max_fuel);
                    if let Some(p_mut) = self.players.get_mut(&pid)
                        && !p_mut.spend(&[(WOOD_ITEM_ID, 15), (IRON_ITEM_ID, 10)])
                    {
                        return;
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            _ => {}
        }
    }

    fn spawn_loot_drop(&mut self, kind: LootDropKind, pos: Vec3) {
        let id = self.next_drop_id.max(100);
        self.next_drop_id = self.next_drop_id.wrapping_add(1).max(100);
        let y = self.terrain.ground_y(pos.x, pos.z);
        let (ammo, wood, stone, iron, bandages) = match kind {
            LootDropKind::DropSack => (15, 25, 12, 8, 1),
            LootDropKind::NormalCrate => (30, 40, 20, 14, 2),
            LootDropKind::SeniorCrate => (60, 80, 50, 40, 3),
        };
        if self.loot_drops.len() >= 32 {
            self.loot_drops.remove(0);
        }
        self.loot_drops.push(ReplicatedLootDrop {
            id,
            kind,
            pos: Vec3::new(pos.x, y, pos.z),
            ammo_bonus: ammo,
            wood_bonus: wood,
            stone_bonus: stone,
            iron_bonus: iron,
            bandage_bonus: bandages,
            ttl_secs: 240.0,
        });
    }

    fn damage_enemy(&mut self, enemy_id: u16, dmg: u16) -> Option<(EnemyKind, Vec3)> {
        let enemy = self
            .enemies
            .iter_mut()
            .find(|e| e.id == enemy_id && e.hp > 0)?;
        enemy.hp = enemy.hp.saturating_sub(dmg);
        if enemy.hp == 0 {
            enemy.is_moving = false;
            enemy.is_attacking = false;
            enemy.respawn_secs = if enemy.kind == EnemyKind::PortBossMech {
                90.0
            } else {
                35.0
            };
            Some((enemy.kind, enemy.pos))
        } else {
            None
        }
    }

    fn step_enemies(&mut self, dt: f32, t: f32) {
        let human_targets: Vec<(u16, Vec3)> = self
            .players
            .values()
            .filter(|p| !p.is_bot && p.hp > 0)
            .map(|p| (p.player_id, p.pos))
            .collect();
        let mut player_damage: Vec<(u16, u16)> = Vec::new();

        for enemy in &mut self.enemies {
            if enemy.hp == 0 {
                if enemy.respawn_secs > 0.0 {
                    enemy.respawn_secs = (enemy.respawn_secs - dt).max(0.0);
                    if enemy.respawn_secs <= 0.0 {
                        enemy.hp = enemy.max_hp;
                        enemy.pos = enemy.home_pos;
                        self.world_rev = self.world_rev.wrapping_add(1);
                    }
                }
                continue;
            }
            enemy.attack_cooldown = (enemy.attack_cooldown - dt).max(0.0);
            enemy.is_attacking = enemy.attack_cooldown > 1.05;

            let aggro_r = enemy.kind.aggro_radius();
            let nearest = human_targets
                .iter()
                .filter_map(|&(pid, ppos)| {
                    let d = ppos.distance(enemy.pos);
                    (d <= aggro_r).then_some((pid, ppos, d))
                })
                .min_by(|a, b| a.2.total_cmp(&b.2));

            if let Some((pid, ppos, dist)) = nearest {
                let to_p = Vec2::new(ppos.x - enemy.pos.x, ppos.z - enemy.pos.z);
                if to_p.length_squared() > 1e-4 {
                    enemy.yaw = to_p.x.atan2(to_p.y);
                }
                let atk_r = enemy.kind.attack_range();
                if dist > atk_r * 0.82 {
                    let dir = to_p.normalize_or_zero();
                    let spd = enemy.kind.move_speed();
                    enemy.pos.x = (enemy.pos.x + dir.x * spd * dt)
                        .clamp(-ARENA_HALF_EXTENT, ARENA_HALF_EXTENT);
                    enemy.pos.z = (enemy.pos.z + dir.y * spd * dt)
                        .clamp(-ARENA_HALF_EXTENT, ARENA_HALF_EXTENT);
                    enemy.pos.y = self.terrain.ground_y(enemy.pos.x, enemy.pos.z);
                    enemy.is_moving = true;
                } else {
                    enemy.is_moving = false;
                }
                if dist <= atk_r && enemy.attack_cooldown <= 0.0 {
                    enemy.attack_cooldown = 1.45;
                    enemy.is_attacking = true;
                    player_damage.push((pid, enemy.kind.attack_damage()));
                }
            } else {
                // Gentle patrol orbit around home spawn point.
                let phase = t * 0.45 + f32::from(enemy.id) * 1.3;
                let target_x = enemy.home_pos.x + phase.cos() * 3.5;
                let target_z = enemy.home_pos.z + phase.sin() * 3.5;
                let to_t = Vec2::new(target_x - enemy.pos.x, target_z - enemy.pos.z);
                let dist_t = to_t.length();
                if dist_t > 0.25 {
                    let dir = to_t / dist_t;
                    let spd = enemy.kind.move_speed() * 0.42;
                    enemy.pos.x += dir.x * spd * dt;
                    enemy.pos.z += dir.y * spd * dt;
                    enemy.pos.y = self.terrain.ground_y(enemy.pos.x, enemy.pos.z);
                    enemy.yaw = dir.x.atan2(dir.y);
                    enemy.is_moving = true;
                } else {
                    enemy.is_moving = false;
                }
            }
        }

        for (pid, dmg) in player_damage {
            if let Some(p) = self.players.get_mut(&pid) {
                p.hp = p.hp.saturating_sub(dmg);
                if p.hp == 0 {
                    p.hp = 100;
                    let ang = f32::from(pid) * 1.1;
                    let rx = ang.cos() * 14.0;
                    let rz = ang.sin() * 14.0;
                    p.pos = Vec3::new(rx, self.terrain.ground_y(rx, rz), rz);
                }
                self.world_rev = self.world_rev.wrapping_add(1);
            }
        }
    }

    fn step_power_grid(&mut self, dt: f32) {
        let mut active_gen_positions: Vec<Vec3> = Vec::new();
        let mut fuel_sum = 0.0_f32;
        let mut active_gens = 0usize;

        for st in &mut self.structures {
            if st.kind != BuildPieceKind::Generator {
                continue;
            }
            if st.hp == 0 {
                st.is_open = false;
                continue;
            }
            let fuel = self.generator_fuels.entry(st.id).or_insert(500.0);
            let want_on = *self.generator_active.entry(st.id).or_insert(true);
            let running = want_on && *fuel > 0.1;
            if running {
                *fuel = (*fuel - 0.65 * dt).max(0.0);
                fuel_sum += (*fuel / 1000.0).clamp(0.0, 1.0);
                active_gens += 1;
                active_gen_positions.push(st.pos);
            }
            if st.is_open != running {
                st.is_open = running;
                self.world_rev = self.world_rev.wrapping_add(1);
            }
        }

        // Batteries charge and relay grid power within 28m of any active Generator.
        for st in &mut self.structures {
            if st.kind == BuildPieceKind::Battery {
                let powered = st.hp > 0
                    && active_gen_positions
                        .iter()
                        .any(|gp| gp.distance_squared(st.pos) <= 28.0 * 28.0);
                if st.is_open != powered {
                    st.is_open = powered;
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
        }

        let new_pct = if active_gens > 0 {
            ((fuel_sum / active_gens as f32) * 100.0)
                .round()
                .clamp(1.0, 100.0) as u8
        } else {
            0
        };
        if new_pct != self.base_power_pct {
            self.base_power_pct = new_pct;
        }
    }

    fn is_pos_powered(&self, pos: Vec3) -> bool {
        self.structures.iter().any(|s| {
            matches!(s.kind, BuildPieceKind::Generator | BuildPieceKind::Battery)
                && s.hp > 0
                && s.is_open
                && s.pos.distance_squared(pos) <= 28.0 * 28.0
        })
    }

    fn step_turrets(&mut self, dt: f32) {
        let mut drops_to_spawn: Vec<(LootDropKind, Vec3, u16, u16)> = Vec::new();
        for idx in 0..self.structures.len() {
            let kind = self.structures[idx].kind;
            if !matches!(
                kind,
                BuildPieceKind::Turret | BuildPieceKind::ElectricTurret
            ) {
                continue;
            }
            let sid = self.structures[idx].id;
            let owner_id = self.structures[idx].owner_id;
            let t_pos = self.structures[idx].pos;
            let prev_open = self.structures[idx].is_open;
            if kind == BuildPieceKind::ElectricTurret && !self.is_pos_powered(t_pos) {
                if prev_open {
                    self.structures[idx].is_open = false;
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
                continue;
            }
            let (range, fire_cd, dmg) = if kind == BuildPieceKind::ElectricTurret {
                (26.0_f32, 0.45_f32, 34_u16)
            } else {
                (24.0_f32, 0.55_f32, 22_u16)
            };

            let cd = self.turret_cooldowns.entry(sid).or_insert(0.0);
            *cd = (*cd - dt).max(0.0);

            // Find closest living monster/boss within range.
            let target_idx = self
                .enemies
                .iter()
                .enumerate()
                .filter(|(_, e)| e.hp > 0)
                .map(|(i, e)| (i, e.pos.distance_squared(t_pos)))
                .filter(|&(_, d2)| d2 <= range * range)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i);

            if let Some(ei) = target_idx {
                let e_pos = self.enemies[ei].pos;
                let dx = e_pos.x - t_pos.x;
                let dz = e_pos.z - t_pos.z;
                if *cd <= 0.0 {
                    *cd = fire_cd;
                    let _ = (dx, dz);
                    let e_id = self.enemies[ei].id;
                    if let Some((killed_kind, drop_pos)) = self.damage_enemy(e_id, dmg) {
                        let dk = if killed_kind == EnemyKind::PortBossMech {
                            LootDropKind::SeniorCrate
                        } else {
                            LootDropKind::DropSack
                        };
                        drops_to_spawn.push((dk, drop_pos, owner_id, 10));
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
            let now_open = *self.turret_cooldowns.get(&sid).unwrap_or(&0.0) > 0.25;
            self.structures[idx].is_open = now_open;
            if now_open != prev_open {
                self.world_rev = self.world_rev.wrapping_add(1);
            }
        }
        for (dk, dpos, owner_id, bonus) in drops_to_spawn {
            self.spawn_loot_drop(dk, dpos);
            if let Some(owner) = self.players.get_mut(&owner_id) {
                owner.grant(
                    &self.craft_catalog,
                    &[(WOOD_ITEM_ID, (u32::from(bonus)) as i32)],
                );
            }
        }
    }

    fn step_vehicles(&mut self, dt: f32) {
        let mut killed_drops: Vec<(LootDropKind, Vec3, u16)> = Vec::new();
        for vi in 0..self.vehicles.len() {
            let driver_id = self.vehicles[vi].driver_id;
            let (throttle, steer, ascend, descend, boost) = if driver_id != 0 {
                if let Some(p) = self.players.get(&driver_id) {
                    (p.wish_xz.y, p.wish_xz.x, p.ascend_held, p.descend, p.sprint)
                } else {
                    self.vehicles[vi].driver_id = 0;
                    (0.0, 0.0, false, false, false)
                }
            } else {
                (0.0, 0.0, false, false, false)
            };

            let mut v = self.vehicles[vi].clone();
            crate::world_physics::step_vehicle_kinematics(
                &self.terrain,
                &self.structures,
                &mut v,
                crate::world_physics::VehicleDriveInput {
                    throttle,
                    steer,
                    ascend,
                    descend,
                    boost,
                },
                dt,
            );
            let v_pos = v.pos;
            let v_speed = v.speed.abs();
            let seat_pos = v.pos + v.kind.seat_offset();
            let seat_yaw = v.yaw;
            let v_driver = v.driver_id;
            self.vehicles[vi] = v;

            if v_driver != 0
                && let Some(p) = self.players.get_mut(&v_driver)
            {
                p.pos = seat_pos;
                p.yaw = seat_yaw;
                p.vertical_vel = 0.0;
                p.grounded = true;
                p.anim_state = 0;
            }

            // Roadkill damage when ramming enemies at speed (> 8.5 m/s).
            if v_driver != 0 && v_speed > 8.5 && self.vehicles[vi].weapon_cooldown <= 0.0 {
                let hit_id = self
                    .enemies
                    .iter()
                    .find(|e| e.hp > 0 && e.pos.distance_squared(v_pos) <= 2.8 * 2.8)
                    .map(|e| e.id);
                if let Some(eid) = hit_id {
                    self.vehicles[vi].weapon_cooldown = 0.45;
                    self.vehicles[vi].speed *= 0.65;
                    if let Some((killed_kind, drop_pos)) = self.damage_enemy(eid, 42) {
                        let dk = if killed_kind == EnemyKind::PortBossMech {
                            LootDropKind::SeniorCrate
                        } else {
                            LootDropKind::DropSack
                        };
                        killed_drops.push((dk, drop_pos, v_driver));
                    }
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
        }
        for (dk, dpos, driver_id) in killed_drops {
            self.spawn_loot_drop(dk, dpos);
            if let Some(p) = self.players.get_mut(&driver_id) {
                p.grant(&self.craft_catalog, &[(WOOD_ITEM_ID, 15)]);
            }
        }
    }

    /// Exports placed structures and owner inventory into a [`crate::config::BaseSaveData`] snapshot.
    pub fn export_saved_base(&self, owner_id: u16) -> crate::config::BaseSaveData {
        let p = self.players.get(&owner_id);
        let gen_fuel = self
            .generator_fuels
            .values()
            .next()
            .copied()
            .unwrap_or(400.0)
            .clamp(0.0, 1000.0) as u16;
        let structures = self
            .structures
            .iter()
            .filter(|s| s.hp > 0 && s.owner_id == owner_id)
            .map(|s| crate::config::SavedStructureEntry {
                kind: s.kind as u8,
                tier: s.tier,
                x: s.pos.x,
                y: s.pos.y,
                z: s.pos.z,
                yaw_steps: s.rot_steps,
                hp: s.hp,
                max_hp: s.max_hp,
                is_open: s.is_open,
                fuel: self
                    .generator_fuels
                    .get(&s.id)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1000.0) as u16,
            })
            .collect();
        crate::config::BaseSaveData {
            version: 2,
            wood: p.map_or(180, |x| x.wood()),
            stone: p.map_or(90, |x| x.stone()),
            iron: p.map_or(45, |x| x.iron()),
            fiber: p.map_or(40, |x| x.fiber()),
            ammo: p.map_or(60, |x| u32::from(x.ammo())),
            bandages: p.map_or(2, |x| x.bandages().min(255) as u8),
            generator_fuel: gen_fuel,
            structures,
            owner_key: p.map(|player| crate::config::LocalRoleKey {
                account_name: player.account_name.clone(),
                server_id: self.storage_server_id,
            }),
            inventory: p.map(|player| player.inventory.clone()),
            craft_queue: p.map_or_else(CraftQueue::default, |player| player.craft_queue.clone()),
        }
    }

    pub fn saved_base_path(&self, owner_id: u16) -> std::path::PathBuf {
        crate::config::BaseSaveData::resolve_role_save_path(
            self.players
                .get(&owner_id)
                .map_or("", |player| player.account_name.as_str()),
            self.storage_server_id,
        )
    }

    /// Restores persisted base structures and generator fuels into authoritative `ServerWorld`.
    pub fn import_saved_base(&mut self, save: &crate::config::BaseSaveData) -> Result<(), String> {
        if !(1..=2).contains(&save.version) {
            return Err("Unsupported base save version".to_owned());
        }
        if let Some(inventory) = &save.inventory {
            let owner = save
                .owner_key
                .as_ref()
                .ok_or("Inventory snapshot has no owner")?;
            if owner.server_id != self.storage_server_id {
                return Err("Inventory belongs to another server".to_owned());
            }
            let inventory = ItemInventory::from_counts(
                &self.craft_catalog,
                inventory.counts().iter().map(|(&id, &num)| (id, num)),
            )
            .map_err(|error| error.to_string())?;
            save.craft_queue
                .validate(&self.craft_catalog)
                .map_err(|error| error.to_string())?;
            if save.craft_queue.entries.len() > 5 {
                return Err("Saved production queue exceeds native mock capacity".to_owned());
            }
            if let Some(player) = self
                .players
                .values_mut()
                .find(|player| player.account_name == owner.account_name && !player.is_bot)
            {
                player.inventory = inventory;
                player.craft_queue = save.craft_queue.clone();
            } else {
                self.saved_production.insert(
                    owner.account_name.clone(),
                    (inventory, save.craft_queue.clone()),
                );
            }
        }
        let owner_id = save
            .owner_key
            .as_ref()
            .and_then(|owner| {
                self.players
                    .values()
                    .find(|player| player.account_name == owner.account_name && !player.is_bot)
                    .map(|player| player.player_id)
            })
            .unwrap_or(0);
        let replaced_ids: Vec<_> = self
            .structures
            .iter()
            .filter(|structure| {
                (owner_id != 0 && structure.owner_id == owner_id)
                    || save.owner_key.as_ref().is_some_and(|owner| {
                        self.detached_building_owners.get(&structure.id)
                            == Some(&owner.account_name)
                    })
            })
            .map(|structure| structure.id)
            .collect();
        self.structures
            .retain(|structure| !replaced_ids.contains(&structure.id));
        for id in replaced_ids {
            self.generator_fuels.remove(&id);
            self.generator_active.remove(&id);
            self.detached_building_owners.remove(&id);
        }
        for entry in &save.structures {
            let kind = BuildPieceKind::from_u8(entry.kind);
            let id = self.next_build_id;
            self.next_build_id = self.next_build_id.wrapping_add(1);
            self.structures.push(ReplicatedStructure {
                id,
                owner_id,
                kind,
                tier: entry.tier.clamp(1, 4),
                pos: Vec3::new(entry.x, entry.y, entry.z),
                rot_steps: entry.yaw_steps & 3,
                hp: entry.hp.max(1),
                max_hp: entry
                    .max_hp
                    .max(kind.max_hp_for_tier(entry.tier.clamp(1, 4))),
                is_open: entry.is_open,
            });
            if owner_id == 0
                && let Some(owner) = &save.owner_key
            {
                self.detached_building_owners
                    .insert(id, owner.account_name.clone());
            }
            if kind == BuildPieceKind::Generator {
                let fuel = if entry.fuel > 0 {
                    f32::from(entry.fuel)
                } else {
                    f32::from(save.generator_fuel.max(200))
                };
                self.generator_fuels.insert(id, fuel);
                self.generator_active
                    .insert(id, entry.is_open || fuel > 0.0);
            }
        }
        self.world_rev = self.world_rev.wrapping_add(1);
        Ok(())
    }

    /// Advances simulation by `dt` and computes interest-filtered snapshots per connected human client.
    pub fn step_and_build_snapshots(&mut self, dt: f32) -> Vec<(SocketAddr, PacketKind)> {
        self.tick = self.tick.wrapping_add(1);
        for player in self.players.values_mut().filter(|player| !player.is_bot) {
            match player.craft_queue.advance(
                &self.craft_catalog,
                &mut player.inventory,
                f64::from(dt),
            ) {
                Ok(grants) if !grants.is_empty() => self.world_rev = self.world_rev.wrapping_add(1),
                Ok(_) => {}
                Err(error) => warn!(
                    "Production queue for player {} could not advance: {error}",
                    player.player_id
                ),
            }
        }
        let t = self.tick as f32 * dt;

        for node in &mut self.nodes {
            if node.hp == 0 && node.respawn_secs > 0.0 {
                node.respawn_secs = (node.respawn_secs - dt).max(0.0);
                if node.respawn_secs <= 0.0 {
                    node.hp = node.max_hp;
                    self.world_rev = self.world_rev.wrapping_add(1);
                }
            }
        }

        let prev_drops = self.loot_drops.len();
        self.loot_drops.retain_mut(|d| {
            d.ttl_secs -= dt;
            d.ttl_secs > 0.0
        });
        if self.loot_drops.len() != prev_drops {
            self.world_rev = self.world_rev.wrapping_add(1);
        }

        self.step_power_grid(dt);
        self.step_enemies(dt, t);
        self.step_turrets(dt);
        self.step_vehicles(dt);

        let driving_ids: Vec<u16> = self
            .vehicles
            .iter()
            .filter_map(|v| (v.driver_id != 0).then_some(v.driver_id))
            .collect();
        for player in self.players.values_mut() {
            if player.is_bot {
                let orbit_speed = 0.25 + ((player.player_id % 5) as f32) * 0.05;
                let phase = f32::from(player.player_id) * 0.37 + t * orbit_speed;
                let radius = 12.0 + (player.player_id % 6) as f32 * 9.0;
                let nx = phase.cos() * radius;
                let nz = phase.sin() * radius;
                let ny = self.terrain.ground_y(nx, nz);
                let next_pos = Vec3::new(nx, ny, nz);
                let delta = Vec2::new(next_pos.x - player.pos.x, next_pos.z - player.pos.z);
                player.pos = next_pos;
                if delta.length_squared() > 1e-6 {
                    player.yaw = delta.x.atan2(delta.y);
                }
            } else if !driving_ids.contains(&player.player_id) {
                let speed = if player.sprint {
                    PLAYER_SPRINT_SPEED
                } else {
                    PLAYER_MOVE_SPEED
                };
                let jump = std::mem::take(&mut player.jump_pressed);
                step_player_kinematics(
                    &self.terrain,
                    &self.structures,
                    &self.nodes,
                    &mut player.pos,
                    player.wish_xz,
                    &mut player.vertical_vel,
                    &mut player.grounded,
                    jump,
                    speed,
                    dt,
                );
            }
        }

        let mut spatial_peers: Vec<SpatialPeer> = Vec::with_capacity(self.players.len());
        let mut sorted_ids: Vec<u16> = self.players.keys().copied().collect();
        sorted_ids.sort_unstable();
        for &id in &sorted_ids {
            if let Some(p) = self.players.get(&id) {
                spatial_peers.push(SpatialPeer {
                    player_id: p.player_id,
                    pos: p.pos,
                });
            }
        }
        self.grid.rebuild(&spatial_peers);

        let total_online = self.players.len().min(u8::MAX as usize) as u8;
        let send_sync = self.tick.is_multiple_of(3) || self.world_rev != self.last_broadcast_rev;
        if send_sync {
            self.last_broadcast_rev = self.world_rev;
        }
        let mut out = Vec::with_capacity(self.addr_to_id.len() * if send_sync { 2 } else { 1 });

        for (&addr, &viewer_id) in &self.addr_to_id {
            let Some(viewer) = self.players.get(&viewer_id) else {
                continue;
            };
            let mut peers = Vec::with_capacity(MAX_SNAPSHOT_PEERS);
            peers.push(NetPlayerState {
                player_id: viewer.player_id,
                pos: [viewer.pos.x, viewer.pos.y, viewer.pos.z],
                yaw: viewer.yaw,
                anim_state: viewer.anim_state,
                flags: NetPlayerState::FLAG_LOCAL_PLAYER,
            });
            let visible_indices = self.grid.query_nearby(
                &spatial_peers,
                viewer.pos,
                Some(viewer.player_id),
                MAX_SNAPSHOT_PEERS.saturating_sub(1),
            );
            for idx in visible_indices {
                let peer_id = spatial_peers[idx].player_id;
                if let Some(peer) = self.players.get(&peer_id) {
                    peers.push(NetPlayerState {
                        player_id: peer.player_id,
                        pos: [peer.pos.x, peer.pos.y, peer.pos.z],
                        yaw: peer.yaw,
                        anim_state: peer.anim_state,
                        flags: if peer.is_bot {
                            NetPlayerState::FLAG_BOT
                        } else {
                            0
                        },
                    });
                }
            }

            out.push((
                addr,
                PacketKind::WorldSnapshot {
                    server_tick: self.tick,
                    total_online,
                    peers,
                },
            ));

            if send_sync {
                let mut near_structs: Vec<(f32, &ReplicatedStructure)> = self
                    .structures
                    .iter()
                    .map(|s| (s.pos.distance_squared(viewer.pos), s))
                    .collect();
                near_structs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let structures = near_structs
                    .into_iter()
                    .take(super::protocol::MAX_SYNC_STRUCTURES)
                    .map(|(_, s)| super::protocol::NetStructureDto {
                        id: s.id,
                        owner_id: s.owner_id,
                        kind: s.kind as u8,
                        tier: s.tier,
                        pos: [s.pos.x, s.pos.y, s.pos.z],
                        rot_steps: s.rot_steps,
                        hp: s.hp,
                        max_hp: s.max_hp,
                        is_open: s.is_open,
                    })
                    .collect();

                let mut near_nodes: Vec<(f32, &ReplicatedResourceNode)> = self
                    .nodes
                    .iter()
                    .filter(|n| n.hp < n.max_hp)
                    .map(|n| (n.pos.distance_squared(viewer.pos), n))
                    .collect();
                near_nodes.sort_by(|a, b| a.0.total_cmp(&b.0));
                let nodes = near_nodes
                    .into_iter()
                    .take(super::protocol::MAX_SYNC_NODES)
                    .map(|(_, n)| super::protocol::NetNodeHpDto { id: n.id, hp: n.hp })
                    .collect();

                let mut near_enemies: Vec<(f32, &ReplicatedEnemy)> = self
                    .enemies
                    .iter()
                    .map(|e| (e.pos.distance_squared(viewer.pos), e))
                    .collect();
                near_enemies.sort_by(|a, b| a.0.total_cmp(&b.0));
                let enemies = near_enemies
                    .into_iter()
                    .take(super::protocol::MAX_SYNC_ENEMIES)
                    .map(|(_, e)| {
                        let mut flags = 0_u8;
                        if e.is_moving {
                            flags |= NetEnemyDto::FLAG_MOVING;
                        }
                        if e.is_attacking {
                            flags |= NetEnemyDto::FLAG_ATTACKING;
                        }
                        NetEnemyDto {
                            id: e.id,
                            kind: e.kind as u8,
                            flags,
                            pos: [e.pos.x, e.pos.y, e.pos.z],
                            yaw: e.yaw,
                            hp: e.hp,
                            max_hp: e.max_hp,
                        }
                    })
                    .collect();

                let mut near_drops: Vec<(f32, &ReplicatedLootDrop)> = self
                    .loot_drops
                    .iter()
                    .map(|d| (d.pos.distance_squared(viewer.pos), d))
                    .collect();
                near_drops.sort_by(|a, b| a.0.total_cmp(&b.0));
                let drops = near_drops
                    .into_iter()
                    .take(super::protocol::MAX_SYNC_DROPS)
                    .map(|(_, d)| NetLootDropDto {
                        id: d.id,
                        kind: d.kind as u8,
                        pos: [d.pos.x, d.pos.y, d.pos.z],
                    })
                    .collect();

                let mut near_vehicles: Vec<(f32, &crate::world_physics::ReplicatedVehicle)> = self
                    .vehicles
                    .iter()
                    .map(|v| (v.pos.distance_squared(viewer.pos), v))
                    .collect();
                near_vehicles.sort_by(|a, b| a.0.total_cmp(&b.0));
                let vehicles = near_vehicles
                    .into_iter()
                    .take(super::protocol::MAX_SYNC_VEHICLES)
                    .map(|(_, v)| super::protocol::NetVehicleDto {
                        id: v.id,
                        kind: v.kind as u8,
                        fuel_pct: v.fuel_pct(),
                        driver_id: v.driver_id,
                        pos: [v.pos.x, v.pos.y, v.pos.z],
                        yaw: v.yaw,
                        hp: v.hp,
                    })
                    .collect();

                out.push((
                    addr,
                    PacketKind::WorldStateSync {
                        world_rev: self.world_rev,
                        hp: viewer.hp,
                        wood: viewer.wood(),
                        stone: viewer.stone(),
                        iron: viewer.iron(),
                        fiber: viewer.fiber(),
                        ammo: viewer.ammo(),
                        bandages: viewer.bandages(),
                        base_power_pct: self.base_power_pct,
                        structures,
                        nodes,
                        enemies,
                        drops,
                        vehicles,
                    },
                ));
                let items: Vec<_> = viewer
                    .inventory
                    .counts()
                    .iter()
                    .map(|(&item_id, &num)| super::protocol::NetInventoryItemDto { item_id, num })
                    .collect();
                let page_size = super::protocol::MAX_INVENTORY_ITEMS_PER_PAGE;
                let page_count = items.len().div_ceil(page_size).max(1);
                for page_index in 0..page_count {
                    let start = page_index * page_size;
                    let end = (start + page_size).min(items.len());
                    out.push((
                        addr,
                        PacketKind::InventorySnapshot {
                            revision: self.world_rev,
                            page_index: page_index as u16,
                            page_count: page_count as u16,
                            items: items[start..end].to_vec(),
                        },
                    ));
                }
                out.push((
                    addr,
                    PacketKind::CraftStateSync {
                        server_tick: self.tick,
                        queue: viewer
                            .craft_queue
                            .entries
                            .iter()
                            .map(|entry| super::protocol::NetCraftQueueDto {
                                drawing_id: entry.drawing_id,
                                remaining_count: entry.remaining_count,
                                remaining_ms: (entry.remaining_seconds * 1000.0).ceil() as u32,
                                batch_ms: (entry.batch_seconds * 1000.0).ceil() as u32,
                                facility_id: entry.facility_id,
                            })
                            .collect(),
                    },
                ));
            }
        }

        out
    }
}

/// Active non-blocking UDP game server resource (either local loopback host or standalone headless).
#[derive(Resource)]
pub struct NetServerHandle {
    socket: UdpSocket,
    local_addr: SocketAddr,
    pub world: ServerWorld,
    seq: u32,
}

impl NetServerHandle {
    pub fn bind(bind_addr: &str, server_name: impl Into<String>) -> Result<Self, String> {
        let socket = UdpSocket::bind(bind_addr)
            .map_err(|e| format!("failed to bind UDP server on `{bind_addr}`: {e}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| format!("failed to set UDP server nonblocking: {e}"))?;
        let local_addr = socket
            .local_addr()
            .map_err(|e| format!("failed to inspect bound UDP address: {e}"))?;
        Ok(Self {
            socket,
            local_addr,
            world: ServerWorld::new(server_name),
            seq: 0,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Reads all queued UDP datagrams cleanly without blocking, steps server simulation by `dt`,
    /// and transmits interest-culled `WorldSnapshot` packets back to connected clients.
    pub fn poll_and_step(&mut self, dt: f32) {
        let mut buf = [0_u8; 2048];
        while let Ok((len, src)) = self.socket.recv_from(&mut buf) {
            let Ok(env) = decode_packet(&buf[..len]) else {
                continue;
            };
            match env.kind {
                PacketKind::Hello { account_name, .. } => {
                    let reply = match self.world.accept_or_reject_client(src, account_name) {
                        Ok(player_id) => PacketKind::Welcome {
                            player_id,
                            tick: self.world.tick,
                            max_players: MAX_PLAYERS as u8,
                            active_players: self.world.active_count().min(255) as u8,
                            server_name: self.world.server_name.clone(),
                        },
                        Err(reason) => PacketKind::Reject {
                            reason: reason.to_owned(),
                        },
                    };
                    self.send_to(src, reply);
                }
                PacketKind::Ping { client_time_ms } => {
                    let pong = PacketKind::Pong {
                        client_time_ms,
                        server_tick: self.world.tick,
                        online_count: self.world.active_count().min(255) as u8,
                    };
                    self.send_to(src, pong);
                }
                PacketKind::InputCmd {
                    move_xz,
                    yaw,
                    buttons,
                    ..
                } => {
                    self.world.apply_input(&src, move_xz, yaw, buttons);
                }
                PacketKind::WorldOp {
                    op,
                    target_id,
                    param_a,
                    param_b,
                    pos,
                } => {
                    self.world
                        .apply_world_op(&src, op, target_id, param_a, param_b, pos);
                }
                PacketKind::CraftRequest {
                    request_id,
                    drawing_id,
                    create_num,
                    near_building_instance_id,
                } => {
                    let reply = self.world.apply_craft_request(
                        &src,
                        request_id,
                        drawing_id,
                        create_num,
                        near_building_instance_id,
                    );
                    self.send_to(src, reply);
                }
                PacketKind::Disconnect => {
                    self.world.remove_addr(&src);
                }
                _ => {}
            }
        }

        let outgoing = self.world.step_and_build_snapshots(dt);
        for (addr, kind) in outgoing {
            self.send_to(addr, kind);
        }
    }

    fn send_to(&mut self, addr: SocketAddr, kind: PacketKind) {
        self.seq = self.seq.wrapping_add(1);
        let bytes = encode_packet(&PacketEnvelope {
            seq: self.seq,
            kind,
        });
        let _ = self.socket.send_to(&bytes, addr);
    }
}

/// Current connection phase tracked on [`NetClientState`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ClientLinkPhase {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

/// Client-side non-blocking UDP networking & snapshot interpolation state.
#[derive(Resource)]
pub struct NetClientState {
    socket: Option<UdpSocket>,
    target_addr: Option<SocketAddr>,
    pub phase: ClientLinkPhase,
    pub server_name: String,
    pub local_player_id: Option<u16>,
    pub server_tick: u32,
    pub total_online: u8,
    pub visible_peers: HashMap<u16, NetPlayerState>,
    pub rtt_ms: f32,
    pub last_status: String,
    pub world_rev: u32,
    pub hp: u16,
    pub wood: u32,
    pub stone: u32,
    pub iron: u32,
    pub fiber: u32,
    pub ammo: u16,
    pub bandages: u16,
    pub inventory: BTreeMap<i32, i32>,
    pub craft_queue: Vec<super::protocol::NetCraftQueueDto>,
    pub craft_status: String,
    inventory_assembler: super::inventory::InventoryAssembler,
    last_craft_tick: Option<u32>,
    craft_request_seq: u32,
    pending_crafts: HashMap<u32, (PacketKind, u64)>,
    pub structures: HashMap<u32, ReplicatedStructure>,
    pub damaged_node_hps: HashMap<u16, u16>,
    pub enemies: HashMap<u16, ReplicatedEnemy>,
    pub loot_drops: HashMap<u16, ReplicatedLootDrop>,
    pub vehicles: HashMap<u16, crate::world_physics::ReplicatedVehicle>,
    pub base_power_pct: u8,
    seq: u32,
    start_instant: Instant,
    last_hello_ms: u64,
    last_ping_ms: u64,
}

impl Default for NetClientState {
    fn default() -> Self {
        Self {
            socket: None,
            target_addr: None,
            phase: ClientLinkPhase::Disconnected,
            server_name: String::new(),
            local_player_id: None,
            server_tick: 0,
            total_online: 0,
            visible_peers: HashMap::new(),
            rtt_ms: 0.0,
            last_status: "Offline".to_owned(),
            world_rev: 0,
            hp: 100,
            wood: 120,
            stone: 60,
            iron: 25,
            fiber: 30,
            ammo: 60,
            bandages: 2,
            inventory: BTreeMap::new(),
            craft_queue: Vec::new(),
            craft_status: String::new(),
            inventory_assembler: super::inventory::InventoryAssembler::default(),
            last_craft_tick: None,
            craft_request_seq: 0,
            pending_crafts: HashMap::new(),
            structures: HashMap::new(),
            damaged_node_hps: HashMap::new(),
            enemies: HashMap::new(),
            loot_drops: HashMap::new(),
            vehicles: HashMap::new(),
            base_power_pct: 0,
            seq: 0,
            start_instant: Instant::now(),
            last_hello_ms: 0,
            last_ping_ms: 0,
        }
    }
}

impl NetClientState {
    /// Opens an ephemeral non-blocking client UDP socket and initiates `Hello` handshake.
    pub fn connect(
        &mut self,
        addr: SocketAddr,
        client_version: &str,
        account_name: &str,
        desired_lang: i32,
    ) -> Result<(), String> {
        let sock = UdpSocket::bind("127.0.0.1:0")
            .or_else(|_| UdpSocket::bind("0.0.0.0:0"))
            .map_err(|e| format!("failed to open UDP client socket: {e}"))?;
        sock.set_nonblocking(true)
            .map_err(|e| format!("failed to set UDP client nonblocking: {e}"))?;

        self.socket = Some(sock);
        self.target_addr = Some(addr);
        self.phase = ClientLinkPhase::Connecting;
        self.local_player_id = None;
        self.server_tick = 0;
        self.total_online = 0;
        self.visible_peers.clear();
        self.structures.clear();
        self.damaged_node_hps.clear();
        self.enemies.clear();
        self.loot_drops.clear();
        self.vehicles.clear();
        self.base_power_pct = 0;
        self.world_rev = 0;
        self.hp = 100;
        self.wood = 120;
        self.stone = 60;
        self.iron = 25;
        self.fiber = 30;
        self.ammo = 60;
        self.bandages = 2;
        self.inventory.clear();
        self.craft_queue.clear();
        self.craft_status.clear();
        self.inventory_assembler = super::inventory::InventoryAssembler::default();
        self.last_craft_tick = None;
        self.pending_crafts.clear();
        self.craft_request_seq = 0;
        self.last_status = format!("Connecting UDP -> {addr}");
        self.last_hello_ms = self.elapsed_ms();
        self.send_kind(PacketKind::Hello {
            client_version: client_version.to_owned(),
            account_name: account_name.to_owned(),
            desired_lang,
        });
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if self.phase != ClientLinkPhase::Disconnected {
            self.send_kind(PacketKind::Disconnect);
        }
        self.socket = None;
        self.target_addr = None;
        self.phase = ClientLinkPhase::Disconnected;
        self.local_player_id = None;
        self.visible_peers.clear();
        self.structures.clear();
        self.damaged_node_hps.clear();
        self.enemies.clear();
        self.loot_drops.clear();
        self.vehicles.clear();
        self.last_status = "Disconnected".to_owned();
    }

    pub fn send_world_op(&mut self, op: u8, target_id: u32, param_a: u8, param_b: u8, pos: Vec3) {
        if self.phase != ClientLinkPhase::Connected {
            return;
        }
        self.send_kind(PacketKind::WorldOp {
            op,
            target_id,
            param_a,
            param_b,
            pos: [pos.x, pos.y, pos.z],
        });
    }

    /// Starts an authoritative local loopback 100-player server (if `srv.is_local_loopback`) or
    /// resolves `srv.gate_url:srv.port`, then connects this client socket and sends `Hello`.
    pub fn connect_to_server_entry(
        &mut self,
        commands: &mut Commands,
        srv: &ServerEntry,
        client_version: &str,
        account_name: &str,
        desired_lang: i32,
    ) -> Result<(), String> {
        let target_addr = if srv.is_local_loopback {
            let mut server = NetServerHandle::bind("127.0.0.1:0", &srv.name)?;
            server.world.storage_server_id = srv.id;
            let addr = server.local_addr();
            commands.insert_resource(server);
            addr
        } else {
            commands.remove_resource::<NetServerHandle>();
            let mut addrs = (srv.gate_url.as_str(), srv.port)
                .to_socket_addrs()
                .map_err(|e| {
                    format!(
                        "Cannot reach `{}:{}` ({e}). Select Localhost 100P UDP Host.",
                        srv.gate_url, srv.port
                    )
                })?;
            addrs
                .next()
                .ok_or_else(|| format!("Resolved 0 addresses for {}:{}", srv.gate_url, srv.port))?
        };
        self.server_name = srv.name.clone();
        if let Err(err) = self.connect(target_addr, client_version, account_name, desired_lang) {
            self.phase = ClientLinkPhase::Failed(err.clone());
            return Err(err);
        }
        Ok(())
    }

    pub fn send_input(&mut self, move_xz: [f32; 2], yaw: f32, buttons: u16) {
        if self.phase != ClientLinkPhase::Connected {
            return;
        }
        let tick = self.server_tick;
        self.send_kind(PacketKind::InputCmd {
            tick,
            move_xz,
            yaw,
            buttons,
        });
    }

    pub fn send_craft_request(
        &mut self,
        drawing_id: i32,
        create_num: i32,
        facility_id: i64,
    ) -> Result<u32, String> {
        if self.phase != ClientLinkPhase::Connected {
            return Err("Connect before requesting production".to_owned());
        }
        if drawing_id <= 0 || create_num <= 0 {
            return Err("Choose a recipe and a positive quantity".to_owned());
        }
        if self.pending_crafts.len() >= 128 {
            return Err("Waiting for production acknowledgments".to_owned());
        }
        self.craft_request_seq = self.craft_request_seq.wrapping_add(1);
        let request_id = self.craft_request_seq;
        let packet = PacketKind::CraftRequest {
            request_id,
            drawing_id,
            create_num,
            near_building_instance_id: facility_id,
        };
        self.pending_crafts
            .insert(request_id, (packet.clone(), self.elapsed_ms()));
        self.send_kind(packet);
        self.craft_status = "Sending production request".to_owned();
        Ok(request_id)
    }

    fn apply_inventory_projection(&mut self) {
        let amount = |id| self.inventory.get(&id).copied().unwrap_or(0);
        self.wood = amount(WOOD_ITEM_ID) as u32;
        self.stone = amount(STONE_ITEM_ID) as u32;
        self.iron = amount(IRON_ITEM_ID) as u32;
        self.fiber = amount(CLOTH_ITEM_ID) as u32;
        self.ammo = amount(RIFLE_AMMO_ITEM_ID).min(i32::from(u16::MAX)) as u16;
        self.bandages = amount(BANDAGE_ITEM_ID).min(i32::from(u16::MAX)) as u16;
    }

    pub fn local_player_state(&self) -> Option<&NetPlayerState> {
        self.local_player_id
            .and_then(|id| self.visible_peers.get(&id))
    }

    /// Drains pending UDP packets cleanly without blocking and refreshes RTT pings periodically.
    pub fn poll_receive(&mut self, account_name: &str, client_version: &str, desired_lang: i32) {
        let Some(ref sock) = self.socket else {
            return;
        };
        let now_ms = self.elapsed_ms();
        let mut buf = [0_u8; 2048];
        let mut received = Vec::new();
        while let Ok((len, src)) = sock.recv_from(&mut buf) {
            if Some(src) != self.target_addr {
                continue;
            }
            if let Ok(env) = decode_packet(&buf[..len]) {
                received.push(env);
            }
        }

        for env in received {
            match env.kind {
                PacketKind::Welcome {
                    player_id,
                    tick,
                    max_players,
                    active_players,
                    server_name,
                } => {
                    self.phase = ClientLinkPhase::Connected;
                    self.local_player_id = Some(player_id);
                    self.server_tick = tick;
                    self.total_online = active_players;
                    self.server_name = server_name.clone();
                    self.last_status = format!(
                        "Connected to `{server_name}` as slot #{player_id} ({active_players}/{max_players})"
                    );
                }
                PacketKind::Reject { reason } => {
                    self.phase = ClientLinkPhase::Failed(reason.clone());
                    self.last_status = format!("Rejected: {reason}");
                }
                PacketKind::Pong {
                    client_time_ms,
                    server_tick,
                    online_count,
                } => {
                    self.rtt_ms = now_ms.saturating_sub(client_time_ms) as f32;
                    self.server_tick = server_tick;
                    self.total_online = online_count;
                }
                PacketKind::WorldSnapshot {
                    server_tick,
                    total_online,
                    peers,
                } => {
                    self.server_tick = server_tick;
                    self.total_online = total_online;
                    self.visible_peers.clear();
                    for peer in peers {
                        self.visible_peers.insert(peer.player_id, peer);
                    }
                }
                PacketKind::WorldStateSync {
                    world_rev,
                    hp,
                    wood,
                    stone,
                    iron,
                    fiber,
                    ammo,
                    bandages,
                    base_power_pct,
                    structures,
                    nodes,
                    enemies,
                    drops,
                    vehicles,
                } => {
                    if world_rev != self.world_rev
                        && !super::inventory::revision_is_newer(world_rev, self.world_rev)
                    {
                        continue;
                    }
                    self.world_rev = world_rev;
                    self.hp = hp;
                    self.wood = wood;
                    self.stone = stone;
                    self.iron = iron;
                    self.fiber = fiber;
                    self.ammo = ammo;
                    self.bandages = bandages;
                    if self
                        .inventory_assembler
                        .committed_revision()
                        .is_some_and(|revision| {
                            !super::inventory::revision_is_newer(world_rev, revision)
                        })
                    {
                        self.apply_inventory_projection();
                    }
                    self.base_power_pct = base_power_pct;
                    self.structures.clear();
                    for dto in structures {
                        self.structures.insert(
                            dto.id,
                            ReplicatedStructure {
                                id: dto.id,
                                owner_id: dto.owner_id,
                                kind: BuildPieceKind::from_u8(dto.kind),
                                tier: dto.tier,
                                pos: Vec3::from_array(dto.pos),
                                rot_steps: dto.rot_steps,
                                hp: dto.hp,
                                max_hp: dto.max_hp,
                                is_open: dto.is_open,
                            },
                        );
                    }
                    self.damaged_node_hps.clear();
                    for n in nodes {
                        self.damaged_node_hps.insert(n.id, n.hp);
                    }
                    self.enemies.clear();
                    for dto in enemies {
                        let kind = EnemyKind::from_u8(dto.kind);
                        let pos = Vec3::from_array(dto.pos);
                        self.enemies.insert(
                            dto.id,
                            ReplicatedEnemy {
                                id: dto.id,
                                kind,
                                pos,
                                home_pos: pos,
                                yaw: dto.yaw,
                                is_moving: (dto.flags & NetEnemyDto::FLAG_MOVING) != 0,
                                is_attacking: (dto.flags & NetEnemyDto::FLAG_ATTACKING) != 0,
                                hp: dto.hp,
                                max_hp: dto.max_hp,
                                attack_cooldown: 0.0,
                                respawn_secs: 0.0,
                            },
                        );
                    }
                    self.loot_drops.clear();
                    for dto in drops {
                        self.loot_drops.insert(
                            dto.id,
                            ReplicatedLootDrop {
                                id: dto.id,
                                kind: LootDropKind::from_u8(dto.kind),
                                pos: Vec3::from_array(dto.pos),
                                ammo_bonus: 0,
                                wood_bonus: 0,
                                stone_bonus: 0,
                                iron_bonus: 0,
                                bandage_bonus: 0,
                                ttl_secs: 0.0,
                            },
                        );
                    }
                    self.vehicles.clear();
                    for dto in vehicles {
                        let kind = crate::world_physics::VehicleKind::from_u8(dto.kind);
                        let max_fuel = kind.max_fuel();
                        self.vehicles.insert(
                            dto.id,
                            crate::world_physics::ReplicatedVehicle {
                                id: dto.id,
                                kind,
                                pos: Vec3::from_array(dto.pos),
                                yaw: dto.yaw,
                                speed: 0.0,
                                vel_y: 0.0,
                                hp: dto.hp,
                                max_hp: kind.max_hp(),
                                fuel: f32::from(dto.fuel_pct) * max_fuel / 100.0,
                                max_fuel,
                                driver_id: dto.driver_id,
                                weapon_cooldown: 0.0,
                            },
                        );
                    }
                }
                PacketKind::InventorySnapshot {
                    revision,
                    page_index,
                    page_count,
                    items,
                } => {
                    if let Some(counts) = self
                        .inventory_assembler
                        .receive(revision, page_index, page_count, items)
                    {
                        self.inventory = counts;
                        self.apply_inventory_projection();
                    }
                }
                PacketKind::CraftStateSync { server_tick, queue } => {
                    if self
                        .last_craft_tick
                        .is_none_or(|tick| super::inventory::revision_is_newer(server_tick, tick))
                    {
                        self.last_craft_tick = Some(server_tick);
                        self.craft_queue = queue;
                    }
                }
                PacketKind::CraftResult {
                    request_id,
                    accepted: _,
                    reason,
                } if self.pending_crafts.remove(&request_id).is_some() => {
                    self.craft_status = reason;
                }
                _ => {}
            }
        }

        let retries: Vec<_> = self
            .pending_crafts
            .values_mut()
            .filter_map(|(packet, sent_ms)| {
                if now_ms.saturating_sub(*sent_ms) >= 250 {
                    *sent_ms = now_ms;
                    Some(packet.clone())
                } else {
                    None
                }
            })
            .collect();
        for packet in retries {
            self.send_kind(packet);
        }

        if self.phase == ClientLinkPhase::Connecting
            && now_ms.saturating_sub(self.last_hello_ms) >= 250
        {
            self.last_hello_ms = now_ms;
            self.send_kind(PacketKind::Hello {
                client_version: client_version.to_owned(),
                account_name: account_name.to_owned(),
                desired_lang,
            });
        } else if self.phase == ClientLinkPhase::Connected
            && now_ms.saturating_sub(self.last_ping_ms) >= 500
        {
            self.last_ping_ms = now_ms;
            self.send_kind(PacketKind::Ping {
                client_time_ms: now_ms,
            });
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.start_instant.elapsed().as_millis() as u64
    }

    fn send_kind(&mut self, kind: PacketKind) {
        let (Some(sock), Some(addr)) = (&self.socket, self.target_addr) else {
            return;
        };
        self.seq = self.seq.wrapping_add(1);
        let bytes = encode_packet(&PacketEnvelope {
            seq: self.seq,
            kind,
        });
        let _ = sock.send_to(&bytes, addr);
    }
}

/// Standalone headless server runner using Pure Bevy `MinimalPlugins` + `FixedUpdate`.
pub fn run_headless_server(bind_addr: &str) {
    let server = match NetServerHandle::bind(bind_addr, "BevyZero Dedicated 100P") {
        Ok(s) => {
            info!(
                "Headless 100-player UDP server listening on {}",
                s.local_addr()
            );
            s
        }
        Err(err) => {
            error!("{err}");
            return;
        }
    };

    App::new()
        .add_plugins(
            MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
                1.0 / TICK_HZ,
            ))),
        )
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
        .insert_resource(server)
        .add_systems(FixedUpdate, step_embedded_server_system)
        .run();
}

/// Authoritative server `FixedUpdate` system shared by loopback listen-host and dedicated server.
pub fn step_embedded_server_system(
    time: Res<Time<Fixed>>,
    server_opt: Option<ResMut<NetServerHandle>>,
) {
    if let Some(mut server) = server_opt {
        server.poll_and_step(time.delta_secs());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(kind: PacketKind) -> bool {
        let PacketKind::CraftResult { accepted, .. } = kind else {
            panic!("production result");
        };
        accepted
    }

    fn world_with_player() -> (ServerWorld, SocketAddr, u16) {
        let mut world = ServerWorld::new("Native production test");
        let address = "127.0.0.1:31042".parse().unwrap();
        let player_id = world
            .accept_or_reject_client(address, "Account_A".to_owned())
            .unwrap();
        (world, address, player_id)
    }

    #[test]
    fn native_craft_datagram_preserves_id_count_and_retry_never_charges_twice() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut client = NetClientState {
            socket: Some(UdpSocket::bind("127.0.0.1:0").unwrap()),
            target_addr: Some(receiver.local_addr().unwrap()),
            phase: ClientLinkPhase::Connected,
            ..default()
        };
        let request_id = client.send_craft_request(57100, 3, -1).unwrap();
        let mut packet = [0u8; 1500];
        let (length, source) = receiver.recv_from(&mut packet).unwrap();
        let PacketKind::CraftRequest {
            request_id: received_id,
            drawing_id,
            create_num,
            near_building_instance_id,
        } = decode_packet(&packet[..length]).unwrap().kind
        else {
            panic!("native production request");
        };
        assert_eq!(
            (
                received_id,
                drawing_id,
                create_num,
                near_building_instance_id
            ),
            (request_id, 57100, 3, -1)
        );
        let mut world = ServerWorld::new("Datagram production");
        let player_id = world
            .accept_or_reject_client(source, "Producer".to_owned())
            .unwrap();
        assert!(accepted(world.apply_craft_request(
            &source, request_id, drawing_id, create_num, -1
        )));
        assert_eq!(
            world.players[&player_id].inventory.amount(CLOTH_ITEM_ID),
            15
        );
        assert_eq!(
            world.players[&player_id].inventory.amount(BANDAGE_ITEM_ID),
            2
        );
        assert!(accepted(world.apply_craft_request(
            &source, request_id, drawing_id, create_num, -1
        )));
        assert_eq!(
            world.players[&player_id].inventory.amount(CLOTH_ITEM_ID),
            15
        );
        assert_eq!(world.players[&player_id].craft_queue.entries.len(), 1);
        assert!(!accepted(
            world.apply_craft_request(&source, request_id, 57089, 1, -1)
        ));
        world.step_and_build_snapshots(0.999);
        assert_eq!(
            world.players[&player_id].inventory.amount(BANDAGE_ITEM_ID),
            2
        );
        world.step_and_build_snapshots(0.002);
        assert_eq!(
            world.players[&player_id].inventory.amount(BANDAGE_ITEM_ID),
            3
        );
        world.step_and_build_snapshots(2.0);
        assert_eq!(
            world.players[&player_id].inventory.amount(BANDAGE_ITEM_ID),
            5
        );
        assert!(world.players[&player_id].craft_queue.entries.is_empty());
        let before = world.players[&player_id].inventory.clone();
        for (id, drawing, num) in [
            (2, 777777, 1),
            (3, 57100, 0),
            (4, 57100, -1),
            (5, 57100, 1000),
        ] {
            assert!(!accepted(
                world.apply_craft_request(&source, id, drawing, num, -1)
            ));
            assert_eq!(world.players[&player_id].inventory, before);
        }
        world.apply_world_op(
            &source,
            super::super::protocol::OP_CRAFT_RECIPE,
            1,
            0,
            0,
            [0.0; 3],
        );
        assert_eq!(world.players[&player_id].inventory, before);
    }

    #[test]
    fn native_mock_queue_allows_five_entries_then_rejects_without_cost() {
        let (mut world, address, player_id) = world_with_player();
        for id in 1..=5 {
            assert!(accepted(
                world.apply_craft_request(&address, id, 57100, 1, -1)
            ));
        }
        let before = world.players[&player_id].inventory.clone();
        assert!(!accepted(
            world.apply_craft_request(&address, 6, 57100, 1, -1)
        ));
        assert_eq!(world.players[&player_id].inventory, before);
        assert_eq!(world.players[&player_id].craft_queue.entries.len(), 5);
        world.step_and_build_snapshots(5.0);
        assert_eq!(
            world.players[&player_id].inventory.amount(BANDAGE_ITEM_ID),
            7
        );
    }

    #[test]
    fn workbench_must_be_live_and_within_native_four_point_five_radius() {
        let (mut world, address, player_id) = world_with_player();
        world
            .players
            .get_mut(&player_id)
            .unwrap()
            .grant(&world.craft_catalog, &[(50017, 5)]);
        let player_pos = world.players[&player_id].pos;
        world.structures.push(ReplicatedStructure {
            id: 41,
            owner_id: player_id,
            kind: BuildPieceKind::Workbench,
            tier: 2,
            pos: player_pos + Vec3::X * 4.6,
            rot_steps: 0,
            hp: 100,
            max_hp: 100,
            is_open: false,
        });
        let before = world.players[&player_id].inventory.clone();
        assert!(!accepted(
            world.apply_craft_request(&address, 1, 57021, 1, 41)
        ));
        assert_eq!(world.players[&player_id].inventory, before);
        world.structures[0].pos = player_pos + Vec3::X * 4.4;
        assert!(!accepted(
            world.apply_craft_request(&address, 2, 57021, 1, 9999)
        ));
        assert_eq!(world.players[&player_id].inventory, before);
        assert!(accepted(
            world.apply_craft_request(&address, 3, 57021, 1, 41)
        ));
        assert_eq!(
            world.players[&player_id].craft_queue.entries[0].facility_id,
            41
        );
        assert_eq!(world.players[&player_id].inventory.amount(50017), 0);
        assert_eq!(world.players[&player_id].inventory.amount(IRON_ITEM_ID), 15);
    }

    #[test]
    fn complete_native_inventory_replicates_over_real_udp_without_byte_id_aliases() {
        let mut server = NetServerHandle::bind("127.0.0.1:0", "Inventory UDP").unwrap();
        let mut client = NetClientState::default();
        client
            .connect(server.local_addr(), "3.14.0.0", "Producer", 8)
            .unwrap();
        for _ in 0..8 {
            server.poll_and_step(0.0);
            client.poll_receive("Producer", "3.14.0.0", 8);
        }
        let player_id = client.local_player_id.unwrap();
        let ids: Vec<_> = server
            .world
            .craft_catalog
            .items
            .keys()
            .take(300)
            .copied()
            .collect();
        let amounts: Vec<_> = ids
            .iter()
            .map(|id| ItemAmount {
                item_id: *id,
                num: 2,
            })
            .collect();
        server
            .world
            .players
            .get_mut(&player_id)
            .unwrap()
            .inventory
            .add_atomic(&server.world.craft_catalog, &amounts)
            .unwrap();
        server.world.world_rev += 1;
        for _ in 0..4 {
            server.poll_and_step(0.0);
            client.poll_receive("Producer", "3.14.0.0", 8);
        }
        for id in ids {
            assert_eq!(
                client.inventory[&id],
                server.world.players[&player_id].inventory.amount(id)
            );
        }
        client.send_craft_request(57100, 1, -1).unwrap();
        for _ in 0..4 {
            server.poll_and_step(0.0);
            client.poll_receive("Producer", "3.14.0.0", 8);
        }
        assert_eq!(client.inventory[&CLOTH_ITEM_ID], 25);
        assert_eq!(client.craft_queue[0].drawing_id, 57100);
        server.poll_and_step(1.0);
        client.poll_receive("Producer", "3.14.0.0", 8);
        assert_eq!(client.inventory[&BANDAGE_ITEM_ID], 3);
        assert!(client.craft_queue.is_empty());
        assert!(client.pending_crafts.is_empty());
    }

    #[test]
    fn production_save_retains_progress_and_isolates_accounts_and_servers() {
        let (mut world, address, player_id) = world_with_player();
        world.storage_server_id = 12;
        assert!(accepted(
            world.apply_craft_request(&address, 1, 57089, 1, -1)
        ));
        world.step_and_build_snapshots(3.0);
        let saved = world.export_saved_base(player_id);
        assert_eq!(saved.craft_queue.entries[0].remaining_seconds, 5.0);
        let raw = serde_json::to_string(&saved).unwrap();
        let decoded = serde_json::from_str(&raw).unwrap();
        let mut restored = ServerWorld::new("Restore test");
        assert!(restored.import_saved_base(&decoded).is_err());
        restored.storage_server_id = 12;
        restored.import_saved_base(&decoded).unwrap();
        let other_address = "127.0.0.1:31043".parse().unwrap();
        let other_id = restored
            .accept_or_reject_client(other_address, "Account_B".to_owned())
            .unwrap();
        assert!(restored.players[&other_id].craft_queue.entries.is_empty());
        let original_id = restored
            .accept_or_reject_client(address, "Account_A".to_owned())
            .unwrap();
        assert_eq!(
            restored.players[&original_id]
                .inventory
                .amount(WOOD_ITEM_ID),
            70
        );
        restored.step_and_build_snapshots(5.0);
        assert_eq!(restored.players[&original_id].inventory.amount(55001), 1);
        assert_eq!(restored.players[&other_id].inventory.amount(55001), 0);
        assert_ne!(
            restored.saved_base_path(original_id),
            restored.saved_base_path(other_id)
        );
    }

    #[test]
    fn account_changes_cannot_transfer_inventory_and_reconnect_keeps_paid_queue() {
        let (mut world, address, player_id) = world_with_player();
        assert!(accepted(
            world.apply_craft_request(&address, 1, 57089, 1, -1)
        ));
        world.step_and_build_snapshots(3.0);
        let inventory = world.players[&player_id].inventory.clone();
        assert_eq!(
            world.accept_or_reject_client(address, "Account_B".to_owned()),
            Err("account_changed_reconnect")
        );
        assert_eq!(world.players[&player_id].inventory, inventory);
        world.remove_addr(&address);
        let other_id = world
            .accept_or_reject_client(address, "Account_B".to_owned())
            .unwrap();
        assert!(world.players[&other_id].craft_queue.entries.is_empty());
        assert_eq!(world.players[&other_id].inventory.amount(WOOD_ITEM_ID), 120);
        let new_address = "127.0.0.1:31111".parse().unwrap();
        let restored_id = world
            .accept_or_reject_client(new_address, "Account_A".to_owned())
            .unwrap();
        assert_eq!(world.players[&restored_id].inventory, inventory);
        assert_eq!(
            world.players[&restored_id].craft_queue.entries[0].remaining_seconds,
            5.0
        );
        world.step_and_build_snapshots(5.0);
        assert_eq!(world.players[&restored_id].inventory.amount(55001), 1);
    }

    #[test]
    fn scoped_base_exports_only_owned_structures_and_restores_the_correct_peer() {
        let (mut world, address, player_id) = world_with_player();
        let other_address = "127.0.0.1:31112".parse().unwrap();
        let other_id = world
            .accept_or_reject_client(other_address, "Account_B".to_owned())
            .unwrap();
        for (id, owner_id) in [(10, player_id), (20, other_id)] {
            world.structures.push(ReplicatedStructure {
                id,
                owner_id,
                kind: BuildPieceKind::Foundation,
                tier: 1,
                pos: Vec3::new(f32::from(owner_id), 0.0, 0.0),
                rot_steps: 0,
                hp: 100,
                max_hp: 100,
                is_open: false,
            });
        }
        let saved = world.export_saved_base(player_id);
        assert_eq!(saved.structures.len(), 1);
        assert_eq!(saved.structures[0].x, f32::from(player_id));
        let mut restored = ServerWorld::new("Owner restore");
        restored.import_saved_base(&saved).unwrap();
        let other_id = restored
            .accept_or_reject_client(other_address, "Account_B".to_owned())
            .unwrap();
        let owner_id = restored
            .accept_or_reject_client(address, "Account_A".to_owned())
            .unwrap();
        assert_ne!(owner_id, other_id);
        assert_eq!(restored.structures[0].owner_id, owner_id);
        assert!(restored.export_saved_base(other_id).structures.is_empty());
    }
}
