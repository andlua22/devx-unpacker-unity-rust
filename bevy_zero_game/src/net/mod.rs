//! Pure-Bevy 100-player networking & spatial replication plugin.

pub mod interest;
mod inventory;
pub mod protocol;
pub mod transport;

use bevy::prelude::*;

use self::protocol::TICK_HZ;
use self::transport::{NetClientState, step_embedded_server_system};
use crate::config::{GameConfigs, LocalizationDb, PlayerSessionPrefs};

/// Registers `FixedUpdate` server stepping and client UDP polling.
pub struct BevyNetPlugin;

impl Plugin for BevyNetPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
            .init_resource::<NetClientState>()
            .add_systems(
                FixedUpdate,
                (step_embedded_server_system, poll_client_transport_system).chain(),
            );
    }
}

fn poll_client_transport_system(
    mut client: ResMut<NetClientState>,
    prefs: Res<PlayerSessionPrefs>,
    configs: Res<GameConfigs>,
    l10n: Res<LocalizationDb>,
) {
    let ver = if configs.version.str_v.is_empty() {
        "3.14.0.0"
    } else {
        configs.version.str_v.as_str()
    };
    client.poll_receive(&prefs.account_name, ver, l10n.selected_lang_index);
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use bevy::time::{TimePlugin, TimeUpdateStrategy};

    use super::protocol::MAX_PLAYERS;
    use super::transport::{ClientLinkPhase, NetClientState, NetServerHandle};
    use super::*;

    #[test]
    fn enforces_100_player_server_capacity_and_loopback_handshake() {
        let server = NetServerHandle::bind("127.0.0.1:0", "LoopbackTest100")
            .expect("loopback server must bind cleanly");
        let addr = server.local_addr();

        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
            .insert_resource(TimeUpdateStrategy::FixedTimesteps(1))
            .insert_resource(server)
            .init_resource::<NetClientState>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<GameConfigs>()
            .init_resource::<LocalizationDb>()
            .add_systems(
                FixedUpdate,
                (step_embedded_server_system, poll_client_transport_system).chain(),
            );

        // Initiate real UDP connection from client -> local loopback server.
        app.world_mut()
            .resource_mut::<NetClientState>()
            .connect(addr, "3.14.0.0", "Andlua", 8)
            .expect("client connect socket must bind cleanly");

        // Step deterministically (no sleep!) until Welcome & first WorldSnapshot arrive.
        for _ in 0..16 {
            app.update();
            if app.world().resource::<NetClientState>().phase == ClientLinkPhase::Connected
                && app
                    .world()
                    .resource::<NetClientState>()
                    .local_player_state()
                    .is_some()
            {
                break;
            }
        }

        {
            let client = app.world().resource::<NetClientState>();
            assert_eq!(client.phase, ClientLinkPhase::Connected);
            assert_eq!(client.local_player_id, Some(1));
            assert!(client.local_player_state().is_some());
        }

        // Fill remaining 99 slots on ServerWorld and verify 101st real human client is rejected.
        let mut world = app.world_mut().resource_mut::<NetServerHandle>();
        for i in 2..=MAX_PLAYERS {
            let fake_peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30000 + i as u16);
            assert!(
                world
                    .world
                    .accept_or_reject_client(fake_peer, format!("Peer_{i}"))
                    .is_ok(),
                "slot {i} within MAX_PLAYERS=100 must succeed"
            );
        }
        assert_eq!(world.world.active_count(), MAX_PLAYERS);

        let overflow_peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41001);
        assert_eq!(
            world
                .world
                .accept_or_reject_client(overflow_peer, "Overflow".to_owned()),
            Err("server_full_100")
        );
    }

    #[test]
    fn replicates_100_player_stress_bots_via_interest_grid_over_udp() {
        let mut server = NetServerHandle::bind("127.0.0.1:0", "Stress100")
            .expect("stress test server must bind");
        server.world.set_stress_bots_enabled(true);
        assert_eq!(server.world.active_count(), MAX_PLAYERS);
        let addr = server.local_addr();

        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
            .insert_resource(TimeUpdateStrategy::FixedTimesteps(1))
            .insert_resource(server)
            .init_resource::<NetClientState>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<GameConfigs>()
            .init_resource::<LocalizationDb>()
            .add_systems(
                FixedUpdate,
                (step_embedded_server_system, poll_client_transport_system).chain(),
            );

        app.world_mut()
            .resource_mut::<NetClientState>()
            .connect(addr, "3.14.0.0", "StressViewer", 0)
            .expect("client connect must bind");

        for _ in 0..16 {
            app.update();
            let client = app.world().resource::<NetClientState>();
            if client.phase == ClientLinkPhase::Connected && client.visible_peers.len() > 1 {
                break;
            }
        }

        let client = app.world().resource::<NetClientState>();
        assert_eq!(client.phase, ClientLinkPhase::Connected);
        assert_eq!(client.total_online, MAX_PLAYERS as u8);
        assert!(
            client.visible_peers.len() > 1
                && client.visible_peers.len() <= super::protocol::MAX_SNAPSHOT_PEERS,
            "expected MTU-bounded AOI peers (1..={}), got {}",
            super::protocol::MAX_SNAPSHOT_PEERS,
            client.visible_peers.len()
        );
    }

    #[test]
    fn replicates_building_upgrade_door_and_harvest_over_udp() {
        let server = NetServerHandle::bind("127.0.0.1:0", "Phase3Loopback")
            .expect("phase3 test server must bind");
        let addr = server.local_addr();

        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
            .insert_resource(TimeUpdateStrategy::FixedTimesteps(1))
            .insert_resource(server)
            .init_resource::<NetClientState>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<GameConfigs>()
            .init_resource::<LocalizationDb>()
            .add_systems(
                FixedUpdate,
                (step_embedded_server_system, poll_client_transport_system).chain(),
            );

        app.world_mut()
            .resource_mut::<NetClientState>()
            .connect(addr, "3.14.0.0", "BuilderOne", 0)
            .expect("client must connect");

        for _ in 0..16 {
            app.update();
            if app.world().resource::<NetClientState>().phase == ClientLinkPhase::Connected {
                break;
            }
        }
        assert_eq!(
            app.world().resource::<NetClientState>().phase,
            ClientLinkPhase::Connected
        );

        // Place Foundation -> wait for WorldStateSync -> Upgrade Lv.2 -> Place Door -> Toggle Door!
        {
            let mut client = app.world_mut().resource_mut::<NetClientState>();
            client.send_world_op(
                protocol::OP_PLACE_BUILD,
                0,
                crate::world_physics::BuildPieceKind::Foundation as u8,
                1,
                Vec3::new(4.2, 18.0, 4.2),
            );
            client.send_world_op(
                protocol::OP_PLACE_BUILD,
                0,
                crate::world_physics::BuildPieceKind::Door as u8,
                0,
                Vec3::new(4.2, 18.55, 6.3),
            );
        }
        for _ in 0..10 {
            app.update();
        }

        let (found_id, door_id) = {
            let client = app.world().resource::<NetClientState>();
            assert_eq!(client.structures.len(), 2);
            assert!(client.wood < 120, "wood cost must be deducted server-side");
            let found = client
                .structures
                .values()
                .find(|s| s.kind == crate::world_physics::BuildPieceKind::Foundation)
                .unwrap()
                .id;
            let door = client
                .structures
                .values()
                .find(|s| s.kind == crate::world_physics::BuildPieceKind::Door)
                .unwrap()
                .id;
            (found, door)
        };

        {
            let mut client = app.world_mut().resource_mut::<NetClientState>();
            client.send_world_op(protocol::OP_UPGRADE_BUILD, found_id, 0, 0, Vec3::ZERO);
            client.send_world_op(protocol::OP_TOGGLE_DOOR, door_id, 0, 0, Vec3::ZERO);
            client.send_world_op(protocol::OP_HARVEST_NODE, 1, 1, 0, Vec3::ZERO);
        }
        for _ in 0..10 {
            app.update();
        }

        let client = app.world().resource::<NetClientState>();
        assert_eq!(client.structures.get(&found_id).unwrap().tier, 2);
        assert!(client.structures.get(&door_id).unwrap().is_open);
        assert!(client.damaged_node_hps.contains_key(&1));
        assert!(
            !client.enemies.is_empty(),
            "Phase 4 enemies must replicate in WorldStateSync"
        );
        assert!(
            !client.loot_drops.is_empty(),
            "Phase 4 loot drops must replicate in WorldStateSync"
        );

        // Hit an enemy until dead -> DropSack appears -> Place Auto Turret!
        let enemy_id = *client.enemies.keys().next().unwrap();
        {
            let mut client = app.world_mut().resource_mut::<NetClientState>();
            for _ in 0..4 {
                client.send_world_op(
                    protocol::OP_HIT_ENEMY,
                    u32::from(enemy_id),
                    0,
                    50,
                    Vec3::ZERO,
                );
            }
            client.send_world_op(
                protocol::OP_PLACE_BUILD,
                0,
                crate::world_physics::BuildPieceKind::Turret as u8,
                0,
                Vec3::new(0.0, 18.0, 4.2),
            );
        }
        for _ in 0..10 {
            app.update();
        }
        let client = app.world().resource::<NetClientState>();
        assert!(
            client
                .structures
                .values()
                .any(|s| s.kind == crate::world_physics::BuildPieceKind::Turret),
            "Placed Auto Turret must replicate cleanly"
        );
    }

    #[test]
    fn replicates_phase5_power_grid_and_vehicle_driving_over_udp() {
        let server = NetServerHandle::bind("127.0.0.1:0", "Phase5Loopback")
            .expect("phase5 test server must bind");
        let addr = server.local_addr();

        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
            .insert_resource(TimeUpdateStrategy::FixedTimesteps(1))
            .insert_resource(server)
            .init_resource::<NetClientState>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<GameConfigs>()
            .init_resource::<LocalizationDb>()
            .add_systems(
                FixedUpdate,
                (step_embedded_server_system, poll_client_transport_system).chain(),
            );

        app.world_mut()
            .resource_mut::<NetClientState>()
            .connect(addr, "3.14.0.0", "EngineerOne", 0)
            .expect("client must connect");

        for _ in 0..16 {
            app.update();
            let c = app.world().resource::<NetClientState>();
            if c.phase == ClientLinkPhase::Connected && c.local_player_state().is_some() {
                break;
            }
        }

        // Place Generator + ElectricTurret near spawn -> verify grid power > 0 and structures replicate.
        let v_pos = {
            let client = app.world().resource::<NetClientState>();
            let jeep = client
                .vehicles
                .values()
                .find(|v| v.kind == crate::world_physics::VehicleKind::OffroadJeep)
                .expect("default island jeep must replicate in WorldStateSync");
            jeep.pos
        };
        {
            let mut client = app.world_mut().resource_mut::<NetClientState>();
            client.send_world_op(
                protocol::OP_PLACE_BUILD,
                0,
                crate::world_physics::BuildPieceKind::Generator as u8,
                0,
                Vec3::new(0.0, 18.0, 0.0),
            );
            client.send_world_op(
                protocol::OP_PLACE_BUILD,
                0,
                crate::world_physics::BuildPieceKind::ElectricTurret as u8,
                0,
                Vec3::new(3.0, 18.0, 0.0),
            );
        }
        for _ in 0..12 {
            app.update();
        }
        {
            let client = app.world().resource::<NetClientState>();
            assert!(
                client.base_power_pct > 0,
                "Active Generator must report base_power_pct > 0"
            );
            assert!(
                client
                    .structures
                    .values()
                    .any(|s| s.kind == crate::world_physics::BuildPieceKind::ElectricTurret),
                "ElectricTurret must replicate over UDP"
            );
        }

        // Move server player next to jeep and board via OP_ENTER_EXIT_VEHICLE + drive forward.
        let jeep_id = {
            let mut srv = app.world_mut().resource_mut::<NetServerHandle>();
            let j_id = srv
                .world
                .vehicles
                .iter()
                .find(|v| v.kind == crate::world_physics::VehicleKind::OffroadJeep)
                .unwrap()
                .id;
            let start_p = srv.world.players.get_mut(&1).expect("player 1 must exist");
            start_p.pos = v_pos + Vec3::new(1.5, 0.0, 0.0);
            j_id
        };
        let pos_before = {
            let mut client = app.world_mut().resource_mut::<NetClientState>();
            client.send_world_op(
                protocol::OP_ENTER_EXIT_VEHICLE,
                u32::from(jeep_id),
                0,
                0,
                Vec3::ZERO,
            );
            v_pos
        };
        for _ in 0..8 {
            app.update();
        }
        for _ in 0..24 {
            app.world_mut()
                .resource_mut::<NetClientState>()
                .send_input([0.0, 1.0], 0.55, 0);
            app.update();
        }

        let client = app.world().resource::<NetClientState>();
        let rep_jeep = client.vehicles.get(&jeep_id).expect("jeep replicated");
        assert_eq!(rep_jeep.driver_id, 1, "local player must own driver seat");
        assert!(
            rep_jeep.pos.distance(pos_before) > 2.5,
            "jeep must drive forward over UDP, moved {}",
            rep_jeep.pos.distance(pos_before)
        );

        // Save/restore roundtrip verification on ServerWorld
        let srv = app.world().resource::<NetServerHandle>();
        let snapshot = srv.world.export_saved_base(1);
        assert_eq!(snapshot.structures.len(), 2);
        let mut fresh = super::transport::ServerWorld::new("Fresh");
        fresh
            .import_saved_base(&snapshot)
            .expect("restore saved base");
        assert_eq!(fresh.structures.len(), 2);
    }
}
