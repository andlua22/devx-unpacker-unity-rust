//! Binary UDP wire protocol for Pure-Bevy 100-player client/server replication.
//!
//! Designed strictly over `std::io` with deterministic little-endian framing bounded below
//! typical Ethernet MTU (`MAX_SNAPSHOT_PEERS = 48`).
//! This is the replacement game's local framing, not the APK's original wire format.

use std::io::Cursor;

/// Maximum concurrent player slots enforced by authoritative server state.
pub const MAX_PLAYERS: usize = 100;

/// Maximum replicated peers serialized inside a single `WorldSnapshot` datagram (~980 bytes).
pub const MAX_SNAPSHOT_PEERS: usize = 48;

/// Maximum replicated buildings serialized inside one MTU-bounded `WorldStateSync` packet.
pub const MAX_SYNC_STRUCTURES: usize = 18;

/// Maximum replicated resource node HPs inside one `WorldStateSync` packet.
pub const MAX_SYNC_NODES: usize = 18;

/// Maximum replicated AI creatures/bosses inside one MTU-bounded `WorldStateSync` packet.
pub const MAX_SYNC_ENEMIES: usize = 8;

/// Maximum replicated loot sacks / air-drop crates inside one `WorldStateSync` packet.
pub const MAX_SYNC_DROPS: usize = 8;

/// Maximum replicated Phase 5 vehicles inside one `WorldStateSync` packet (`991B` total worst-case).
pub const MAX_SYNC_VEHICLES: usize = 4;

/// Local datagram bound for exact inventory pages (1045 bytes at capacity).
pub const MAX_INVENTORY_ITEMS_PER_PAGE: usize = 128;

/// Local datagram bound for crafting queue replication, not an original gameplay limit.
pub const MAX_CRAFT_QUEUE_ENTRIES: usize = 32;

/// Protocol magic header `"BZNP"` (`Bevy Zero Network Protocol`).
pub const PROTOCOL_MAGIC: [u8; 4] = *b"BZNP";

/// Local framing version; version two adds native-width crafting and exact inventory pages.
pub const PROTOCOL_VERSION: u16 = 2;

/// Fixed authoritative server simulation rate in Hz.
pub const TICK_HZ: f64 = 30.0;

const MAX_STR_LEN: usize = 256;

/// Packed spatial state of one player replicated across clients.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetPlayerState {
    pub player_id: u16,
    pub pos: [f32; 3],
    pub yaw: f32,
    pub anim_state: u8,
    pub flags: u8,
}

impl NetPlayerState {
    pub const FLAG_LOCAL_PLAYER: u8 = 1 << 0;
    pub const FLAG_BOT: u8 = 1 << 1;
}

/// Wire DTO describing one placed building piece replicated over UDP.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetStructureDto {
    pub id: u32,
    pub owner_id: u16,
    pub kind: u8,
    pub tier: u8,
    pub pos: [f32; 3],
    pub rot_steps: u8,
    pub hp: u16,
    pub max_hp: u16,
    pub is_open: bool,
}

/// Wire DTO synchronizing remaining HP of a harvestable island node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetNodeHpDto {
    pub id: u16,
    pub hp: u16,
}

/// Wire DTO synchronizing one Phase 4 AI monster or Port Boss.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetEnemyDto {
    pub id: u16,
    pub kind: u8,
    pub flags: u8,
    pub pos: [f32; 3],
    pub yaw: f32,
    pub hp: u16,
    pub max_hp: u16,
}

impl NetEnemyDto {
    pub const FLAG_MOVING: u8 = 1 << 0;
    pub const FLAG_ATTACKING: u8 = 1 << 1;
}

/// Wire DTO synchronizing one Phase 4 loot drop bag / supply crate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetLootDropDto {
    pub id: u16,
    pub kind: u8,
    pub pos: [f32; 3],
}

/// Wire DTO synchronizing one Phase 5 vehicle (`project_poqiche_01`, `project_tanke_01`, `npc_wuzhuangzhishengji_1`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetVehicleDto {
    pub id: u16,
    pub kind: u8,
    pub fuel_pct: u8,
    pub driver_id: u16,
    pub pos: [f32; 3],
    pub yaw: f32,
    pub hp: u16,
}

/// One inventory entry preserving the native signed 32-bit item ID and quantity fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetInventoryItemDto {
    pub item_id: i32,
    pub num: i32,
}

/// One local crafting queue entry; facility IDs retain native signed 64-bit values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetCraftQueueDto {
    pub drawing_id: i32,
    pub remaining_count: i32,
    pub remaining_ms: u32,
    pub batch_ms: u32,
    pub facility_id: i64,
}

/// Action sub-codes carried by [`PacketKind::WorldOp`].
pub const OP_HARVEST_NODE: u8 = 0;
pub const OP_HIT_PLAYER: u8 = 1;
pub const OP_PLACE_BUILD: u8 = 2;
pub const OP_UPGRADE_BUILD: u8 = 3;
pub const OP_TOGGLE_DOOR: u8 = 4;
pub const OP_CRAFT_RECIPE: u8 = 5;
pub const OP_CONSUME_BANDAGE: u8 = 6;
pub const OP_SPEND_AMMO: u8 = 7;
pub const OP_HIT_ENEMY: u8 = 8;
pub const OP_LOOT_DROP: u8 = 9;
pub const OP_ENTER_EXIT_VEHICLE: u8 = 10;
pub const OP_REFUEL_OR_TOGGLE_POWER: u8 = 11;
pub const OP_REPAIR_VEHICLE: u8 = 12;

/// Framed UDP packet header + variant payload.
#[derive(Debug, Clone, PartialEq)]
pub struct PacketEnvelope {
    pub seq: u32,
    pub kind: PacketKind,
}

/// Logical message kinds exchanged over UDP.
#[derive(Debug, Clone, PartialEq)]
pub enum PacketKind {
    Hello {
        client_version: String,
        account_name: String,
        desired_lang: i32,
    },
    Welcome {
        player_id: u16,
        tick: u32,
        max_players: u8,
        active_players: u8,
        server_name: String,
    },
    Reject {
        reason: String,
    },
    Ping {
        client_time_ms: u64,
    },
    Pong {
        client_time_ms: u64,
        server_tick: u32,
        online_count: u8,
    },
    InputCmd {
        tick: u32,
        move_xz: [f32; 2],
        yaw: f32,
        buttons: u16,
    },
    WorldSnapshot {
        server_tick: u32,
        total_online: u8,
        peers: Vec<NetPlayerState>,
    },
    Disconnect,
    WorldOp {
        op: u8,
        target_id: u32,
        param_a: u8,
        param_b: u8,
        pos: [f32; 3],
    },
    WorldStateSync {
        world_rev: u32,
        hp: u16,
        wood: u32,
        stone: u32,
        iron: u32,
        fiber: u32,
        ammo: u16,
        bandages: u16,
        base_power_pct: u8,
        structures: Vec<NetStructureDto>,
        nodes: Vec<NetNodeHpDto>,
        enemies: Vec<NetEnemyDto>,
        drops: Vec<NetLootDropDto>,
        vehicles: Vec<NetVehicleDto>,
    },
    CraftRequest {
        request_id: u32,
        drawing_id: i32,
        create_num: i32,
        near_building_instance_id: i64,
    },
    /// All pages of one revision form an exact inventory, including an empty single page.
    InventorySnapshot {
        revision: u32,
        page_index: u16,
        page_count: u16,
        items: Vec<NetInventoryItemDto>,
    },
    CraftStateSync {
        server_tick: u32,
        queue: Vec<NetCraftQueueDto>,
    },
    CraftResult {
        request_id: u32,
        accepted: bool,
        reason: String,
    },
}

/// Serializes `env` into a compact binary UDP datagram.
///
/// New crafting/inventory messages assert their local bounds instead of dropping data.
/// Invalid internal messages are programmer errors; untrusted input uses [`decode_packet`].
pub fn encode_packet(env: &PacketEnvelope) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&PROTOCOL_MAGIC);
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(&env.seq.to_le_bytes());

    match &env.kind {
        PacketKind::Hello {
            client_version,
            account_name,
            desired_lang,
        } => {
            out.push(1);
            write_str(&mut out, client_version);
            write_str(&mut out, account_name);
            out.extend_from_slice(&desired_lang.to_le_bytes());
        }
        PacketKind::Welcome {
            player_id,
            tick,
            max_players,
            active_players,
            server_name,
        } => {
            out.push(2);
            out.extend_from_slice(&player_id.to_le_bytes());
            out.extend_from_slice(&tick.to_le_bytes());
            out.push(*max_players);
            out.push(*active_players);
            write_str(&mut out, server_name);
        }
        PacketKind::Reject { reason } => {
            out.push(3);
            write_str(&mut out, reason);
        }
        PacketKind::Ping { client_time_ms } => {
            out.push(4);
            out.extend_from_slice(&client_time_ms.to_le_bytes());
        }
        PacketKind::Pong {
            client_time_ms,
            server_tick,
            online_count,
        } => {
            out.push(5);
            out.extend_from_slice(&client_time_ms.to_le_bytes());
            out.extend_from_slice(&server_tick.to_le_bytes());
            out.push(*online_count);
        }
        PacketKind::InputCmd {
            tick,
            move_xz,
            yaw,
            buttons,
        } => {
            out.push(6);
            out.extend_from_slice(&tick.to_le_bytes());
            out.extend_from_slice(&move_xz[0].to_le_bytes());
            out.extend_from_slice(&move_xz[1].to_le_bytes());
            out.extend_from_slice(&yaw.to_le_bytes());
            out.extend_from_slice(&buttons.to_le_bytes());
        }
        PacketKind::WorldSnapshot {
            server_tick,
            total_online,
            peers,
        } => {
            out.push(7);
            out.extend_from_slice(&server_tick.to_le_bytes());
            out.push(*total_online);
            let count = peers.len().min(MAX_SNAPSHOT_PEERS);
            out.push(count as u8);
            for p in &peers[..count] {
                out.extend_from_slice(&p.player_id.to_le_bytes());
                out.extend_from_slice(&p.pos[0].to_le_bytes());
                out.extend_from_slice(&p.pos[1].to_le_bytes());
                out.extend_from_slice(&p.pos[2].to_le_bytes());
                out.extend_from_slice(&p.yaw.to_le_bytes());
                out.push(p.anim_state);
                out.push(p.flags);
            }
        }
        PacketKind::Disconnect => {
            out.push(8);
        }
        PacketKind::WorldOp {
            op,
            target_id,
            param_a,
            param_b,
            pos,
        } => {
            out.push(9);
            out.push(*op);
            out.extend_from_slice(&target_id.to_le_bytes());
            out.push(*param_a);
            out.push(*param_b);
            out.extend_from_slice(&pos[0].to_le_bytes());
            out.extend_from_slice(&pos[1].to_le_bytes());
            out.extend_from_slice(&pos[2].to_le_bytes());
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
            out.push(10);
            out.extend_from_slice(&world_rev.to_le_bytes());
            out.extend_from_slice(&hp.to_le_bytes());
            out.extend_from_slice(&wood.to_le_bytes());
            out.extend_from_slice(&stone.to_le_bytes());
            out.extend_from_slice(&iron.to_le_bytes());
            out.extend_from_slice(&fiber.to_le_bytes());
            out.extend_from_slice(&ammo.to_le_bytes());
            out.extend_from_slice(&bandages.to_le_bytes());
            out.push(*base_power_pct);
            let sc = structures.len().min(MAX_SYNC_STRUCTURES);
            out.push(sc as u8);
            for s in &structures[..sc] {
                out.extend_from_slice(&s.id.to_le_bytes());
                out.extend_from_slice(&s.owner_id.to_le_bytes());
                out.push(s.kind);
                out.push(s.tier);
                out.extend_from_slice(&s.pos[0].to_le_bytes());
                out.extend_from_slice(&s.pos[1].to_le_bytes());
                out.extend_from_slice(&s.pos[2].to_le_bytes());
                out.push(s.rot_steps);
                out.extend_from_slice(&s.hp.to_le_bytes());
                out.extend_from_slice(&s.max_hp.to_le_bytes());
                out.push(u8::from(s.is_open));
            }
            let nc = nodes.len().min(MAX_SYNC_NODES);
            out.push(nc as u8);
            for n in &nodes[..nc] {
                out.extend_from_slice(&n.id.to_le_bytes());
                out.extend_from_slice(&n.hp.to_le_bytes());
            }
            let ec = enemies.len().min(MAX_SYNC_ENEMIES);
            out.push(ec as u8);
            for e in &enemies[..ec] {
                out.extend_from_slice(&e.id.to_le_bytes());
                out.push(e.kind);
                out.push(e.flags);
                out.extend_from_slice(&e.pos[0].to_le_bytes());
                out.extend_from_slice(&e.pos[1].to_le_bytes());
                out.extend_from_slice(&e.pos[2].to_le_bytes());
                out.extend_from_slice(&e.yaw.to_le_bytes());
                out.extend_from_slice(&e.hp.to_le_bytes());
                out.extend_from_slice(&e.max_hp.to_le_bytes());
            }
            let dc = drops.len().min(MAX_SYNC_DROPS);
            out.push(dc as u8);
            for d in &drops[..dc] {
                out.extend_from_slice(&d.id.to_le_bytes());
                out.push(d.kind);
                out.extend_from_slice(&d.pos[0].to_le_bytes());
                out.extend_from_slice(&d.pos[1].to_le_bytes());
                out.extend_from_slice(&d.pos[2].to_le_bytes());
            }
            let vc = vehicles.len().min(MAX_SYNC_VEHICLES);
            out.push(vc as u8);
            for v in &vehicles[..vc] {
                out.extend_from_slice(&v.id.to_le_bytes());
                out.push(v.kind);
                out.push(v.fuel_pct);
                out.extend_from_slice(&v.driver_id.to_le_bytes());
                out.extend_from_slice(&v.pos[0].to_le_bytes());
                out.extend_from_slice(&v.pos[1].to_le_bytes());
                out.extend_from_slice(&v.pos[2].to_le_bytes());
                out.extend_from_slice(&v.yaw.to_le_bytes());
                out.extend_from_slice(&v.hp.to_le_bytes());
            }
        }
        PacketKind::CraftRequest {
            request_id,
            drawing_id,
            create_num,
            near_building_instance_id,
        } => {
            assert!(*drawing_id > 0, "craft drawing ID must be positive");
            assert!(*create_num > 0, "craft count must be positive");
            out.push(11);
            out.extend_from_slice(&request_id.to_le_bytes());
            out.extend_from_slice(&drawing_id.to_le_bytes());
            out.extend_from_slice(&create_num.to_le_bytes());
            out.extend_from_slice(&near_building_instance_id.to_le_bytes());
        }
        PacketKind::InventorySnapshot {
            revision,
            page_index,
            page_count,
            items,
        } => {
            assert!(
                validate_inventory_page(*page_index, *page_count, items.len()).is_ok(),
                "invalid inventory page index or count"
            );
            assert!(
                items.iter().all(|item| item.item_id > 0 && item.num >= 0),
                "inventory IDs must be positive and amounts nonnegative"
            );
            out.push(12);
            out.extend_from_slice(&revision.to_le_bytes());
            out.extend_from_slice(&page_index.to_le_bytes());
            out.extend_from_slice(&page_count.to_le_bytes());
            out.extend_from_slice(&(items.len() as u16).to_le_bytes());
            for item in items {
                out.extend_from_slice(&item.item_id.to_le_bytes());
                out.extend_from_slice(&item.num.to_le_bytes());
            }
        }
        PacketKind::CraftStateSync { server_tick, queue } => {
            assert!(
                queue.len() <= MAX_CRAFT_QUEUE_ENTRIES,
                "craft queue exceeds local datagram bound"
            );
            assert!(
                queue
                    .iter()
                    .all(|entry| entry.drawing_id > 0 && entry.remaining_count > 0),
                "craft queue drawing IDs and remaining counts must be positive"
            );
            out.push(13);
            out.extend_from_slice(&server_tick.to_le_bytes());
            out.push(queue.len() as u8);
            for entry in queue {
                out.extend_from_slice(&entry.drawing_id.to_le_bytes());
                out.extend_from_slice(&entry.remaining_count.to_le_bytes());
                out.extend_from_slice(&entry.remaining_ms.to_le_bytes());
                out.extend_from_slice(&entry.batch_ms.to_le_bytes());
                out.extend_from_slice(&entry.facility_id.to_le_bytes());
            }
        }
        PacketKind::CraftResult {
            request_id,
            accepted,
            reason,
        } => {
            out.push(14);
            out.extend_from_slice(&request_id.to_le_bytes());
            out.push(u8::from(*accepted));
            write_str_exact(&mut out, reason);
        }
    }
    out
}

/// Validates framing magic/version and decodes a UDP datagram.
pub fn decode_packet(bytes: &[u8]) -> Result<PacketEnvelope, String> {
    if bytes.len() < 11 {
        return Err(format!("truncated packet header ({} bytes)", bytes.len()));
    }
    if bytes[..4] != PROTOCOL_MAGIC {
        return Err("invalid packet magic".to_owned());
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != PROTOCOL_VERSION {
        return Err(format!("unsupported protocol version {version}"));
    }
    let seq = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
    let tag = bytes[10];

    let mut cur = Cursor::new(&bytes[11..]);
    let kind = match tag {
        1 => PacketKind::Hello {
            client_version: read_str(&mut cur)?,
            account_name: read_str(&mut cur)?,
            desired_lang: read_i32(&mut cur)?,
        },
        2 => PacketKind::Welcome {
            player_id: read_u16(&mut cur)?,
            tick: read_u32(&mut cur)?,
            max_players: read_u8(&mut cur)?,
            active_players: read_u8(&mut cur)?,
            server_name: read_str(&mut cur)?,
        },
        3 => PacketKind::Reject {
            reason: read_str(&mut cur)?,
        },
        4 => PacketKind::Ping {
            client_time_ms: read_u64(&mut cur)?,
        },
        5 => PacketKind::Pong {
            client_time_ms: read_u64(&mut cur)?,
            server_tick: read_u32(&mut cur)?,
            online_count: read_u8(&mut cur)?,
        },
        6 => PacketKind::InputCmd {
            tick: read_u32(&mut cur)?,
            move_xz: [read_f32(&mut cur)?, read_f32(&mut cur)?],
            yaw: read_f32(&mut cur)?,
            buttons: read_u16(&mut cur)?,
        },
        7 => {
            let server_tick = read_u32(&mut cur)?;
            let total_online = read_u8(&mut cur)?;
            let count = usize::from(read_u8(&mut cur)?);
            if count > MAX_SNAPSHOT_PEERS {
                return Err(format!(
                    "snapshot peer count {count} exceeds {MAX_SNAPSHOT_PEERS}"
                ));
            }
            let mut peers = Vec::with_capacity(count);
            for _ in 0..count {
                peers.push(NetPlayerState {
                    player_id: read_u16(&mut cur)?,
                    pos: [
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                    ],
                    yaw: read_f32(&mut cur)?,
                    anim_state: read_u8(&mut cur)?,
                    flags: read_u8(&mut cur)?,
                });
            }
            PacketKind::WorldSnapshot {
                server_tick,
                total_online,
                peers,
            }
        }
        8 => PacketKind::Disconnect,
        9 => PacketKind::WorldOp {
            op: read_u8(&mut cur)?,
            target_id: read_u32(&mut cur)?,
            param_a: read_u8(&mut cur)?,
            param_b: read_u8(&mut cur)?,
            pos: [
                read_f32(&mut cur)?,
                read_f32(&mut cur)?,
                read_f32(&mut cur)?,
            ],
        },
        10 => {
            let world_rev = read_u32(&mut cur)?;
            let hp = read_u16(&mut cur)?;
            let wood = read_u32(&mut cur)?;
            let stone = read_u32(&mut cur)?;
            let iron = read_u32(&mut cur)?;
            let fiber = read_u32(&mut cur)?;
            let ammo = read_u16(&mut cur)?;
            let bandages = read_u16(&mut cur)?;
            let base_power_pct = read_u8(&mut cur)?;
            let sc = usize::from(read_u8(&mut cur)?);
            if sc > MAX_SYNC_STRUCTURES {
                return Err(format!(
                    "sync structure count {sc} exceeds {MAX_SYNC_STRUCTURES}"
                ));
            }
            let mut structures = Vec::with_capacity(sc);
            for _ in 0..sc {
                structures.push(NetStructureDto {
                    id: read_u32(&mut cur)?,
                    owner_id: read_u16(&mut cur)?,
                    kind: read_u8(&mut cur)?,
                    tier: read_u8(&mut cur)?,
                    pos: [
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                    ],
                    rot_steps: read_u8(&mut cur)?,
                    hp: read_u16(&mut cur)?,
                    max_hp: read_u16(&mut cur)?,
                    is_open: read_u8(&mut cur)? != 0,
                });
            }
            let nc = usize::from(read_u8(&mut cur)?);
            if nc > MAX_SYNC_NODES {
                return Err(format!("sync node count {nc} exceeds {MAX_SYNC_NODES}"));
            }
            let mut nodes = Vec::with_capacity(nc);
            for _ in 0..nc {
                nodes.push(NetNodeHpDto {
                    id: read_u16(&mut cur)?,
                    hp: read_u16(&mut cur)?,
                });
            }
            let ec = usize::from(read_u8(&mut cur)?);
            if ec > MAX_SYNC_ENEMIES {
                return Err(format!("sync enemy count {ec} exceeds {MAX_SYNC_ENEMIES}"));
            }
            let mut enemies = Vec::with_capacity(ec);
            for _ in 0..ec {
                enemies.push(NetEnemyDto {
                    id: read_u16(&mut cur)?,
                    kind: read_u8(&mut cur)?,
                    flags: read_u8(&mut cur)?,
                    pos: [
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                    ],
                    yaw: read_f32(&mut cur)?,
                    hp: read_u16(&mut cur)?,
                    max_hp: read_u16(&mut cur)?,
                });
            }
            let dc = usize::from(read_u8(&mut cur)?);
            if dc > MAX_SYNC_DROPS {
                return Err(format!("sync drop count {dc} exceeds {MAX_SYNC_DROPS}"));
            }
            let mut drops = Vec::with_capacity(dc);
            for _ in 0..dc {
                drops.push(NetLootDropDto {
                    id: read_u16(&mut cur)?,
                    kind: read_u8(&mut cur)?,
                    pos: [
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                    ],
                });
            }
            let vc = usize::from(read_u8(&mut cur)?);
            if vc > MAX_SYNC_VEHICLES {
                return Err(format!(
                    "sync vehicle count {vc} exceeds {MAX_SYNC_VEHICLES}"
                ));
            }
            let mut vehicles = Vec::with_capacity(vc);
            for _ in 0..vc {
                vehicles.push(NetVehicleDto {
                    id: read_u16(&mut cur)?,
                    kind: read_u8(&mut cur)?,
                    fuel_pct: read_u8(&mut cur)?,
                    driver_id: read_u16(&mut cur)?,
                    pos: [
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                        read_f32(&mut cur)?,
                    ],
                    yaw: read_f32(&mut cur)?,
                    hp: read_u16(&mut cur)?,
                });
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
            }
        }
        11 => {
            let request_id = read_u32(&mut cur)?;
            let drawing_id = read_i32(&mut cur)?;
            let create_num = read_i32(&mut cur)?;
            let near_building_instance_id = read_i64(&mut cur)?;
            if drawing_id <= 0 || create_num <= 0 {
                return Err("craft drawing ID and count must be positive".to_owned());
            }
            PacketKind::CraftRequest {
                request_id,
                drawing_id,
                create_num,
                near_building_instance_id,
            }
        }
        12 => {
            let revision = read_u32(&mut cur)?;
            let page_index = read_u16(&mut cur)?;
            let page_count = read_u16(&mut cur)?;
            let count = usize::from(read_u16(&mut cur)?);
            validate_inventory_page(page_index, page_count, count)?;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let item_id = read_i32(&mut cur)?;
                let num = read_i32(&mut cur)?;
                if item_id <= 0 || num < 0 {
                    return Err("inventory IDs must be positive and amounts nonnegative".to_owned());
                }
                items.push(NetInventoryItemDto { item_id, num });
            }
            PacketKind::InventorySnapshot {
                revision,
                page_index,
                page_count,
                items,
            }
        }
        13 => {
            let server_tick = read_u32(&mut cur)?;
            let count = usize::from(read_u8(&mut cur)?);
            if count > MAX_CRAFT_QUEUE_ENTRIES {
                return Err(format!(
                    "craft queue count {count} exceeds {MAX_CRAFT_QUEUE_ENTRIES}"
                ));
            }
            let mut queue = Vec::with_capacity(count);
            for _ in 0..count {
                let entry = NetCraftQueueDto {
                    drawing_id: read_i32(&mut cur)?,
                    remaining_count: read_i32(&mut cur)?,
                    remaining_ms: read_u32(&mut cur)?,
                    batch_ms: read_u32(&mut cur)?,
                    facility_id: read_i64(&mut cur)?,
                };
                if entry.drawing_id <= 0 || entry.remaining_count <= 0 {
                    return Err(
                        "craft queue drawing IDs and remaining counts must be positive".to_owned(),
                    );
                }
                queue.push(entry);
            }
            PacketKind::CraftStateSync { server_tick, queue }
        }
        14 => PacketKind::CraftResult {
            request_id: read_u32(&mut cur)?,
            accepted: read_bool(&mut cur)?,
            reason: read_str(&mut cur)?,
        },
        other => return Err(format!("unknown packet tag {other}")),
    };

    if cur.position() != cur.get_ref().len() as u64 {
        return Err("trailing bytes after packet body".to_owned());
    }
    Ok(PacketEnvelope { seq, kind })
}

fn validate_inventory_page(page_index: u16, page_count: u16, count: usize) -> Result<(), String> {
    if page_count == 0 || page_index >= page_count {
        return Err(format!(
            "invalid inventory page {page_index} of {page_count}"
        ));
    }
    if count > MAX_INVENTORY_ITEMS_PER_PAGE {
        return Err(format!(
            "inventory item count {count} exceeds {MAX_INVENTORY_ITEMS_PER_PAGE}"
        ));
    }
    if page_index < page_count - 1 && count != MAX_INVENTORY_ITEMS_PER_PAGE {
        return Err("nonfinal inventory page must be full".to_owned());
    }
    if count == 0 && page_count != 1 {
        return Err("only a single-page inventory can be empty".to_owned());
    }
    Ok(())
}

fn write_str_exact(out: &mut Vec<u8>, s: &str) {
    assert!(
        s.len() <= MAX_STR_LEN,
        "string exceeds local datagram bound"
    );
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(MAX_STR_LEN);
    out.extend_from_slice(&(len as u16).to_le_bytes());
    out.extend_from_slice(&bytes[..len]);
}

fn read_bytes<'a>(cur: &mut Cursor<&'a [u8]>, n: usize) -> Result<&'a [u8], String> {
    let pos = cur.position() as usize;
    let slice = *cur.get_ref();
    let end = pos.checked_add(n).ok_or("offset overflow")?;
    if end > slice.len() {
        return Err(format!("truncated packet body (need {n} bytes at {pos})"));
    }
    cur.set_position(end as u64);
    Ok(&slice[pos..end])
}

fn read_u8(cur: &mut Cursor<&[u8]>) -> Result<u8, String> {
    Ok(read_bytes(cur, 1)?[0])
}

fn read_bool(cur: &mut Cursor<&[u8]>) -> Result<bool, String> {
    match read_u8(cur)? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(format!("invalid boolean value {other}")),
    }
}

fn read_u16(cur: &mut Cursor<&[u8]>) -> Result<u16, String> {
    let b: [u8; 2] = read_bytes(cur, 2)?.try_into().map_err(|_| "u16 slice")?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(cur: &mut Cursor<&[u8]>) -> Result<u32, String> {
    let b: [u8; 4] = read_bytes(cur, 4)?.try_into().map_err(|_| "u32 slice")?;
    Ok(u32::from_le_bytes(b))
}

fn read_i32(cur: &mut Cursor<&[u8]>) -> Result<i32, String> {
    let b: [u8; 4] = read_bytes(cur, 4)?.try_into().map_err(|_| "i32 slice")?;
    Ok(i32::from_le_bytes(b))
}

fn read_u64(cur: &mut Cursor<&[u8]>) -> Result<u64, String> {
    let b: [u8; 8] = read_bytes(cur, 8)?.try_into().map_err(|_| "u64 slice")?;
    Ok(u64::from_le_bytes(b))
}

fn read_i64(cur: &mut Cursor<&[u8]>) -> Result<i64, String> {
    let b: [u8; 8] = read_bytes(cur, 8)?.try_into().map_err(|_| "i64 slice")?;
    Ok(i64::from_le_bytes(b))
}

fn read_f32(cur: &mut Cursor<&[u8]>) -> Result<f32, String> {
    let b: [u8; 4] = read_bytes(cur, 4)?.try_into().map_err(|_| "f32 slice")?;
    Ok(f32::from_le_bytes(b))
}

fn read_str(cur: &mut Cursor<&[u8]>) -> Result<String, String> {
    let len = usize::from(read_u16(cur)?);
    if len > MAX_STR_LEN {
        return Err(format!("string length {len} exceeds {MAX_STR_LEN}"));
    }
    let raw = read_bytes(cur, len)?;
    std::str::from_utf8(raw)
        .map(ToOwned::to_owned)
        .map_err(|e| format!("invalid UTF-8 string: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_hello_welcome_and_mtu_bounded_snapshot() {
        let hello = PacketEnvelope {
            seq: 7,
            kind: PacketKind::Hello {
                client_version: "3.14.0.0".to_owned(),
                account_name: "Andlua".to_owned(),
                desired_lang: 8,
            },
        };
        let encoded = encode_packet(&hello);
        assert_eq!(decode_packet(&encoded).unwrap(), hello);

        let peers: Vec<NetPlayerState> = (0..MAX_PLAYERS as u16)
            .map(|id| NetPlayerState {
                player_id: id + 1,
                pos: [id as f32 * 2.0, 0.0, -(id as f32)],
                yaw: 0.25,
                anim_state: 1,
                flags: 0,
            })
            .collect();
        let snap = PacketEnvelope {
            seq: 99,
            kind: PacketKind::WorldSnapshot {
                server_tick: 1234,
                total_online: MAX_PLAYERS as u8,
                peers,
            },
        };
        let bytes = encode_packet(&snap);
        // Capped at MAX_SNAPSHOT_PEERS so every datagram fits safely under 1200B MTU.
        assert!(
            bytes.len() < 1100,
            "packet exceeded safe MTU: {}B",
            bytes.len()
        );
        let decoded = decode_packet(&bytes).unwrap();
        match decoded.kind {
            PacketKind::WorldSnapshot {
                server_tick,
                total_online,
                peers,
            } => {
                assert_eq!(server_tick, 1234);
                assert_eq!(total_online, 100);
                assert_eq!(peers.len(), MAX_SNAPSHOT_PEERS);
            }
            other => panic!("unexpected packet: {other:?}"),
        }
    }

    #[test]
    fn rejects_corrupt_headers() {
        assert!(decode_packet(b"SHORT").is_err());
        let mut bad = encode_packet(&PacketEnvelope {
            seq: 1,
            kind: PacketKind::Disconnect,
        });
        bad[0] = b'X';
        assert!(decode_packet(&bad).is_err());
    }

    #[test]
    fn craft_request_preserves_native_widths_and_no_workbench_sentinel() {
        for facility in [-1, i64::MIN, 0x1234_5678_9abc_def0, i64::MAX] {
            let request = PacketEnvelope {
                seq: 300,
                kind: PacketKind::CraftRequest {
                    request_id: u32::MAX,
                    drawing_id: 57100,
                    create_num: 999,
                    near_building_instance_id: facility,
                },
            };
            let encoded = encode_packet(&request);
            assert_eq!(encoded.len(), 31);
            assert_eq!(decode_packet(&encoded).unwrap(), request);
        }
        // The native field width is independent of any UI slider limit.
        let request = PacketEnvelope {
            seq: 301,
            kind: PacketKind::CraftRequest {
                request_id: 1,
                drawing_id: i32::MAX,
                create_num: i32::MAX,
                near_building_instance_id: -1,
            },
        };
        assert_eq!(decode_packet(&encode_packet(&request)).unwrap(), request);
    }

    #[test]
    fn inventory_pages_roundtrip_every_native_item_within_mtu() {
        let items: Vec<_> = (1..=257)
            .map(|item_id| NetInventoryItemDto {
                item_id,
                num: if item_id == 257 {
                    i32::MAX
                } else {
                    item_id - 1
                },
            })
            .collect();
        let pages: Vec<_> = items.chunks(MAX_INVENTORY_ITEMS_PER_PAGE).collect();
        let mut recovered = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let snapshot = PacketEnvelope {
                seq: index as u32,
                kind: PacketKind::InventorySnapshot {
                    revision: u32::MAX,
                    page_index: index as u16,
                    page_count: pages.len() as u16,
                    items: page.to_vec(),
                },
            };
            let encoded = encode_packet(&snapshot);
            assert!(encoded.len() < 1100);
            if index == 0 {
                assert_eq!(encoded.len(), 1045);
            }
            let decoded = decode_packet(&encoded).unwrap();
            assert_eq!(decoded, snapshot);
            if let PacketKind::InventorySnapshot { items, .. } = decoded.kind {
                recovered.extend(items);
            }
        }
        assert_eq!(recovered, items);

        let empty = PacketEnvelope {
            seq: 4,
            kind: PacketKind::InventorySnapshot {
                revision: 0,
                page_index: 0,
                page_count: 1,
                items: Vec::new(),
            },
        };
        assert_eq!(decode_packet(&encode_packet(&empty)).unwrap(), empty);
    }

    #[test]
    fn craft_queue_and_result_roundtrip_at_local_bounds() {
        let queue = PacketEnvelope {
            seq: 17,
            kind: PacketKind::CraftStateSync {
                server_tick: u32::MAX,
                queue: (0..MAX_CRAFT_QUEUE_ENTRIES)
                    .map(|index| NetCraftQueueDto {
                        drawing_id: 57100,
                        remaining_count: 999,
                        remaining_ms: u32::MAX,
                        batch_ms: index as u32,
                        facility_id: if index == 0 { -1 } else { i64::MAX },
                    })
                    .collect(),
            },
        };
        let encoded = encode_packet(&queue);
        assert_eq!(encoded.len(), 784);
        assert!(encoded.len() < 1100);
        assert_eq!(decode_packet(&encoded).unwrap(), queue);

        for accepted in [false, true] {
            let result = PacketEnvelope {
                seq: 18,
                kind: PacketKind::CraftResult {
                    request_id: u32::MAX,
                    accepted,
                    reason: "é".repeat(MAX_STR_LEN / 2),
                },
            };
            assert_eq!(decode_packet(&encode_packet(&result)).unwrap(), result);
        }

        let empty = PacketEnvelope {
            seq: 19,
            kind: PacketKind::CraftStateSync {
                server_tick: 0,
                queue: Vec::new(),
            },
        };
        assert_eq!(decode_packet(&encode_packet(&empty)).unwrap(), empty);
    }

    #[test]
    fn inventory_decoder_rejects_bad_pages_counts_and_item_fields() {
        let valid = encode_packet(&PacketEnvelope {
            seq: 3,
            kind: PacketKind::InventorySnapshot {
                revision: 1,
                page_index: 0,
                page_count: 1,
                items: vec![NetInventoryItemDto {
                    item_id: 57100,
                    num: 999,
                }],
            },
        });
        for (offset, field) in [
            (15, 1_u16), // index equals page count
            (17, 0),     // zero pages
            (17, 2),     // nonfinal page is not full
            (19, MAX_INVENTORY_ITEMS_PER_PAGE as u16 + 1),
        ] {
            let mut malformed = valid.clone();
            malformed[offset..offset + 2].copy_from_slice(&field.to_le_bytes());
            assert!(decode_packet(&malformed).is_err());
        }
        for (offset, field) in [(21, 0_i32), (21, -1), (25, -1)] {
            let mut malformed = valid.clone();
            malformed[offset..offset + 4].copy_from_slice(&field.to_le_bytes());
            assert!(decode_packet(&malformed).is_err());
        }

        let mut empty_final_page = valid[..21].to_vec();
        empty_final_page[15..17].copy_from_slice(&1_u16.to_le_bytes());
        empty_final_page[17..19].copy_from_slice(&2_u16.to_le_bytes());
        empty_final_page[19..21].copy_from_slice(&0_u16.to_le_bytes());
        assert!(decode_packet(&empty_final_page).is_err());
    }

    #[test]
    fn craft_decoder_rejects_bad_counts_boolean_and_utf8() {
        let request = encode_packet(&PacketEnvelope {
            seq: 1,
            kind: PacketKind::CraftRequest {
                request_id: 1,
                drawing_id: 57100,
                create_num: 999,
                near_building_instance_id: -1,
            },
        });
        for (offset, field) in [(15, 0_i32), (15, -1), (19, 0), (19, -1)] {
            let mut malformed = request.clone();
            malformed[offset..offset + 4].copy_from_slice(&field.to_le_bytes());
            assert!(decode_packet(&malformed).is_err());
        }

        let queue = encode_packet(&PacketEnvelope {
            seq: 2,
            kind: PacketKind::CraftStateSync {
                server_tick: 1,
                queue: vec![NetCraftQueueDto {
                    drawing_id: 57100,
                    remaining_count: 999,
                    remaining_ms: 1000,
                    batch_ms: 2000,
                    facility_id: -1,
                }],
            },
        });
        let mut oversized = queue.clone();
        oversized[15] = MAX_CRAFT_QUEUE_ENTRIES as u8 + 1;
        assert!(decode_packet(&oversized).is_err());
        for (offset, field) in [(16, 0_i32), (16, -1), (20, 0), (20, -1)] {
            let mut malformed = queue.clone();
            malformed[offset..offset + 4].copy_from_slice(&field.to_le_bytes());
            assert!(decode_packet(&malformed).is_err());
        }

        let result = encode_packet(&PacketEnvelope {
            seq: 3,
            kind: PacketKind::CraftResult {
                request_id: 1,
                accepted: true,
                reason: "ok".to_owned(),
            },
        });
        let mut invalid_bool = result.clone();
        invalid_bool[15] = 2;
        assert!(decode_packet(&invalid_bool).is_err());
        let mut invalid_utf8 = result.clone();
        invalid_utf8[18] = 0xff;
        assert!(decode_packet(&invalid_utf8).is_err());
        let mut oversized_reason = result;
        oversized_reason[16..18].copy_from_slice(&(MAX_STR_LEN as u16 + 1).to_le_bytes());
        assert!(decode_packet(&oversized_reason).is_err());
    }

    #[test]
    fn new_messages_reject_every_truncation_trailing_bytes_and_stale_version() {
        let messages = [
            PacketKind::CraftRequest {
                request_id: 1,
                drawing_id: 57100,
                create_num: 999,
                near_building_instance_id: i64::MAX,
            },
            PacketKind::InventorySnapshot {
                revision: 1,
                page_index: 0,
                page_count: 1,
                items: vec![NetInventoryItemDto {
                    item_id: 57100,
                    num: 999,
                }],
            },
            PacketKind::CraftStateSync {
                server_tick: 1,
                queue: vec![NetCraftQueueDto {
                    drawing_id: 57100,
                    remaining_count: 999,
                    remaining_ms: 1000,
                    batch_ms: 2000,
                    facility_id: i64::MAX,
                }],
            },
            PacketKind::CraftResult {
                request_id: 1,
                accepted: false,
                reason: "materials missing".to_owned(),
            },
        ];
        for kind in messages {
            let valid = encode_packet(&PacketEnvelope { seq: 1, kind });
            for length in 0..valid.len() {
                assert!(
                    decode_packet(&valid[..length]).is_err(),
                    "accepted {length}-byte prefix"
                );
            }
            let mut trailing = valid.clone();
            trailing.push(0);
            assert!(decode_packet(&trailing).is_err());
            let mut stale = valid;
            stale[4..6].copy_from_slice(&1_u16.to_le_bytes());
            assert!(
                decode_packet(&stale)
                    .unwrap_err()
                    .contains("unsupported protocol version 1")
            );
        }
    }

    #[test]
    fn new_message_encoders_assert_bounds_instead_of_truncating() {
        let invalid = [
            PacketKind::InventorySnapshot {
                revision: 1,
                page_index: 0,
                page_count: 1,
                items: vec![
                    NetInventoryItemDto {
                        item_id: 57100,
                        num: 999
                    };
                    MAX_INVENTORY_ITEMS_PER_PAGE + 1
                ],
            },
            PacketKind::CraftStateSync {
                server_tick: 1,
                queue: vec![
                    NetCraftQueueDto {
                        drawing_id: 57100,
                        remaining_count: 999,
                        remaining_ms: 1000,
                        batch_ms: 2000,
                        facility_id: -1,
                    };
                    MAX_CRAFT_QUEUE_ENTRIES + 1
                ],
            },
            PacketKind::CraftResult {
                request_id: 1,
                accepted: false,
                reason: "x".repeat(MAX_STR_LEN + 1),
            },
        ];
        for kind in invalid {
            assert!(
                std::panic::catch_unwind(|| encode_packet(&PacketEnvelope { seq: 1, kind }))
                    .is_err()
            );
        }
    }

    #[test]
    fn roundtrips_phase3_world_op_and_mtu_bounded_world_sync() {
        let op = PacketEnvelope {
            seq: 11,
            kind: PacketKind::WorldOp {
                op: OP_PLACE_BUILD,
                target_id: 42,
                param_a: 0,
                param_b: 2,
                pos: [12.6, 18.2, -8.4],
            },
        };
        assert_eq!(decode_packet(&encode_packet(&op)).unwrap(), op);

        let structures: Vec<NetStructureDto> = (0..MAX_SYNC_STRUCTURES as u32)
            .map(|id| NetStructureDto {
                id: id + 1,
                owner_id: 1,
                kind: (id % 7) as u8,
                tier: ((id % 4) + 1) as u8,
                pos: [id as f32 * 4.2, 16.0, -4.2],
                rot_steps: (id % 4) as u8,
                hp: 250,
                max_hp: 250,
                is_open: id % 2 == 0,
            })
            .collect();
        let nodes: Vec<NetNodeHpDto> = (0..MAX_SYNC_NODES as u16)
            .map(|id| NetNodeHpDto { id: id + 1, hp: 75 })
            .collect();
        let enemies: Vec<NetEnemyDto> = (0..MAX_SYNC_ENEMIES as u16)
            .map(|id| NetEnemyDto {
                id: id + 1,
                kind: (id % 3) as u8,
                flags: NetEnemyDto::FLAG_MOVING,
                pos: [id as f32 * 5.0, 18.0, 12.0],
                yaw: 0.7,
                hp: 120,
                max_hp: 120,
            })
            .collect();
        let drops: Vec<NetLootDropDto> = (0..MAX_SYNC_DROPS as u16)
            .map(|id| NetLootDropDto {
                id: id + 1,
                kind: (id % 3) as u8,
                pos: [id as f32 * 3.0, 18.0, -9.0],
            })
            .collect();
        let vehicles: Vec<NetVehicleDto> = (0..MAX_SYNC_VEHICLES as u16)
            .map(|id| NetVehicleDto {
                id: id + 1,
                kind: (id % 3) as u8,
                fuel_pct: 87,
                driver_id: 0,
                pos: [id as f32 * 12.0, 18.0, 4.0],
                yaw: 0.45,
                hp: 450,
            })
            .collect();
        let sync = PacketEnvelope {
            seq: 12,
            kind: PacketKind::WorldStateSync {
                world_rev: 5,
                hp: 95,
                wood: 240,
                stone: 110,
                iron: 45,
                fiber: 30,
                ammo: 90,
                bandages: 3,
                base_power_pct: 92,
                structures,
                nodes,
                enemies,
                drops,
                vehicles,
            },
        };
        let encoded = encode_packet(&sync);
        assert!(
            encoded.len() < 1100,
            "WorldStateSync exceeded MTU: {}B",
            encoded.len()
        );
        assert_eq!(decode_packet(&encoded).unwrap(), sync);
    }
}
