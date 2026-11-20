//! Battle.Init / HideLoadingPanel lifecycle: prepare the world while the loading panel
//! remains mounted, then ground the player and start gameplay after resources settle.

use bevy::asset::LoadState;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::ingame::LocalArenaAvatar;
use crate::net::transport::{ClientLinkPhase, NetClientState, NetServerHandle};
use crate::states::{AppState, InGameRoot};
use crate::terrain_material::{NativeTerrainMaterial, NativeTerrainReadiness, NativeTerrainReadinessSet};
use crate::ui::panel_loader::load_ui_panel_doc;
use crate::war_choice::{LOCAL_BATTLE_MAP_ID, WarChoiceState};
use crate::world_physics::TerrainColliderGrid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BattleLoadPhase {
    #[default]
    Connecting,
    Resources,
    Settling,
    Grounding,
    Ready,
    Failed,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum BattleLoadingSet {
    PrepareWorld,
    Readiness,
}

#[derive(Resource, Default)]
pub struct BattleLoadState {
    pub phase: BattleLoadPhase,
    pub fraction: f32,
    pub error: Option<String>,
    pub required_assets: usize,
    pub completed_assets: usize,
    ui_images: Vec<Handle<Image>>,
    music: Option<Handle<AudioSource>>,
    ready_secs: f32,
    grounded_frame: bool,
}

pub struct BattlePlugin;

impl Plugin for BattlePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BattleLoadState>()
            .add_systems(OnEnter(AppState::LoadingScene), begin_battle_loading)
            .add_systems(Update, monitor_battle_loading
                .after(BattleLoadingSet::PrepareWorld)
                .after(NativeTerrainReadinessSet)
                .in_set(BattleLoadingSet::Readiness)
                .run_if(in_state(AppState::LoadingScene)));
    }
}

fn begin_battle_loading(
    mut load: ResMut<BattleLoadState>,
    asset_server: Res<AssetServer>,
    selection: Res<WarChoiceState>,
    server: Option<ResMut<NetServerHandle>>,
) {
    *load = BattleLoadState::default();
    if selection.selected_battle.as_ref().is_some_and(|battle| battle.map_id != LOCAL_BATTLE_MAP_ID) {
        load.phase = BattleLoadPhase::Failed;
        load.error = Some("The selected map resources have not been installed.".to_owned());
        return;
    }
    if let Some(mut server) = server && let Some(battle) = &selection.selected_battle {
        let mode = match battle.game_mode {
            13 | 14 => crate::crafting::CraftMode::Civilization,
            15 | 16 => crate::crafting::CraftMode::Firepower,
            18 => crate::crafting::CraftMode::Nostalgia,
            _ => crate::crafting::CraftMode::Island,
        };
        server.world.configure_battle(mode, if battle.game_mode == 9 { 9 } else { 0 });
    }
    // Native Battle completion creates BattlePanel after player setup. Hold all of its
    // dependencies, including the bag panel, before exposing the player controller.
    for panel in ["BattleLoadingPanel", "BattlePanel", "BagAndBuildPanel"] {
        match load_ui_panel_doc(&format!("ui_panels/{panel}.json")) {
            Ok(doc) => {
                for path in doc.textures.into_iter().filter_map(|texture| texture.asset_path) {
                    load.ui_images.push(asset_server.load(path));
                }
            }
            Err(error) => {
                load.phase = BattleLoadPhase::Failed;
                load.error = Some(format!("Unable to prepare {panel}: {error}"));
                return;
            }
        }
    }
    load.music = Some(asset_server.load("audio/bgm_wenmingchongqi.wav"));
    info!("Battle.Init: queued native panels, player and active scene resources");
}

#[derive(SystemParam)]
struct BattleReadiness<'w, 's> {
    scenes: Query<'w, 's, (Entity, &'static WorldAssetRoot, Option<&'static Name>), With<InGameRoot>>,
    meshes: Query<'w, 's, &'static Mesh3d>,
    native_terrain_meshes: Query<'w, 's, (), With<NativeTerrainMaterial>>,
    avatar: Query<'w, 's, &'static mut Transform, With<LocalArenaAvatar>>,
    children: Query<'w, 's, &'static Children>,
    avatar_entities: Query<'w, 's, Entity, With<LocalArenaAvatar>>,
}

fn has_avatar_mesh(entity: Entity, queries: &BattleReadiness) -> bool {
    queries.meshes.contains(entity) || queries.children.get(entity).is_ok_and(|children| {
        children.iter().any(|child| has_avatar_mesh(child, queries))
    })
}

fn has_native_terrain_mesh(entity: Entity, queries: &BattleReadiness) -> bool {
    queries.native_terrain_meshes.contains(entity) || queries.children.get(entity).is_ok_and(|children| {
        children.iter().any(|child| has_native_terrain_mesh(child, queries))
    })
}

#[allow(clippy::too_many_arguments)]
fn monitor_battle_loading(
    time: Res<Time>,
    asset_server: Res<AssetServer>,
    client: Res<NetClientState>,
    mut load: ResMut<BattleLoadState>,
    terrain: Option<Res<TerrainColliderGrid>>,
    native_terrain: Res<NativeTerrainReadiness>,
    mut queries: BattleReadiness,
) {
    if matches!(load.phase, BattleLoadPhase::Failed | BattleLoadPhase::Ready) { return; }
    if let ClientLinkPhase::Failed(error) = &client.phase {
        load.phase = BattleLoadPhase::Failed;
        load.error = Some(error.clone());
        return;
    }
    let mut completed = 0;
    let mut required = 0;
    let mut failure = None;
    for (_, scene, _) in &queries.scenes {
        required += 1;
        if let Some(LoadState::Failed(error)) = asset_server.get_load_state(scene.0.id()) {
            failure = Some(error.to_string());
        }
        completed += usize::from(asset_server.is_loaded_with_dependencies(scene.0.id()));
    }
    for image in &load.ui_images {
        required += 1;
        if let Some(LoadState::Failed(error)) = asset_server.get_load_state(image.id()) {
            failure = Some(error.to_string());
        }
        completed += usize::from(asset_server.is_loaded_with_dependencies(image.id()));
    }
    if let Some(music) = &load.music {
        required += 1;
        if let Some(LoadState::Failed(error)) = asset_server.get_load_state(music.id()) {
            failure = Some(error.to_string());
        }
        completed += usize::from(asset_server.is_loaded_with_dependencies(music.id()));
    }
    load.required_assets = required;
    load.completed_assets = completed;
    if let Some(error) = failure {
        load.phase = BattleLoadPhase::Failed;
        load.error = Some(format!("A battle resource failed to load: {error}"));
        return;
    }
    let connected = client.phase == ClientLinkPhase::Connected && client.local_player_state().is_some();
    load.fraction = if connected { 0.1 } else { 0.0 }
        + if required > 0 { 0.8 * completed as f32 / required as f32 } else { 0.0 };
    let terrain_roots: Vec<_> = queries.scenes.iter().filter_map(|(entity, _, name)| {
        name.is_some_and(|name| name.as_str().starts_with("TerrainChunk_"))
            .then_some(entity)
    }).collect();
    let avatar_ready = queries.avatar_entities.single().is_ok_and(|entity| has_avatar_mesh(entity, &queries));
    let resources_ready = required > 0 && completed == required && !terrain_roots.is_empty()
        && terrain_roots.iter().all(|&entity| has_native_terrain_mesh(entity, &queries))
        && native_terrain.pending_count == 0
        && avatar_ready && terrain.is_some();
    load.advance(connected, resources_ready, time.delta_secs());
    if load.phase == BattleLoadPhase::Grounding && !load.grounded_frame {
        if let Some(terrain) = terrain.as_deref() && let Ok(mut transform) = queries.avatar.single_mut() {
            transform.translation.y = terrain.ground_y(transform.translation.x, transform.translation.z);
            load.grounded_frame = true;
        }
    } else if load.phase == BattleLoadPhase::Grounding && load.grounded_frame {
        load.phase = BattleLoadPhase::Ready;
        load.fraction = 1.0;
        info!("Battle.OnLoadSceneFinish: {completed}/{required} assets ready; grounded player, gameplay may start");
    }
}

impl BattleLoadState {
    fn advance(&mut self, connected: bool, resources_ready: bool, delta_secs: f32) {
        if !connected || !resources_ready {
            self.ready_secs = 0.0;
            self.grounded_frame = false;
            self.phase = if connected { BattleLoadPhase::Resources } else { BattleLoadPhase::Connecting };
            return;
        }
        // HideLoadingPanel.MoveNext 0x215FA98: ResMgr.IsAllLoadFinished followed by
        // a half-second settle, SetGroundPos, one yielded frame, then StartFsm.
        self.ready_secs += delta_secs;
        self.phase = if self.ready_secs >= 0.5 { BattleLoadPhase::Grounding } else { BattleLoadPhase::Settling };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_and_elapsed_time_cannot_bypass_required_resources() {
        let mut load = BattleLoadState::default();
        load.advance(true, false, 10.0);
        assert_eq!(load.phase, BattleLoadPhase::Resources);
        load.advance(true, true, 0.49);
        assert_eq!(load.phase, BattleLoadPhase::Settling);
        load.advance(true, false, 1.0);
        load.advance(true, true, 0.49);
        assert_eq!(load.phase, BattleLoadPhase::Settling);
        load.advance(true, true, 0.02);
        assert_eq!(load.phase, BattleLoadPhase::Grounding);
        assert!(!load.grounded_frame);
    }

    #[test]
    fn missing_server_state_resets_settled_resources() {
        let mut load = BattleLoadState::default();
        load.advance(true, true, 0.6);
        load.advance(false, true, 0.6);
        assert_eq!(load.phase, BattleLoadPhase::Connecting);
        load.advance(true, true, 0.1);
        assert_eq!(load.phase, BattleLoadPhase::Settling);
    }
}
