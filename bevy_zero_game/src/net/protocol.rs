//! Binary UDP wire protocol for Pure-Bevy 100-player client/server replication.
//!
//! Designed strictly over `std::io` with deterministic little-endian framing bounded below
//! typical Ethernet MTU (`MAX_SNAPSHOT_PEERS = 48`).

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

/// Protocol magic header `"BZNP"` (`Bevy Zero Network Protocol`).
pub const PROTOCOL_MAGIC: [u8; 4] = *b"BZNP";

/// Wire format version verified during `Hello` / `Welcome`.
pub const PROTOCOL_VERSION: u16 = 1;

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
}

/// Serializes `env` into a compact binary UDP datagram.
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
                return Err(format!("snapshot peer count {count} exceeds {MAX_SNAPSHOT_PEERS}"));
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
                return Err(format!("sync structure count {sc} exceeds {MAX_SYNC_STRUCTURES}"));
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
                return Err(format!("sync vehicle count {vc} exceeds {MAX_SYNC_VEHICLES}"));
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
        other => return Err(format!("unknown packet tag {other}")),
    };

    Ok(PacketEnvelope { seq, kind })
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
        assert!(bytes.len() < 1100, "packet exceeded safe MTU: {}B", bytes.len());
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
