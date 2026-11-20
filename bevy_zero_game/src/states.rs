//! Top-level application state machine and scene-lifetime markers.

use bevy::prelude::*;

/// Primary game flow states mirroring `launcher -> Main -> Login -> Battle` cleanly in Bevy 0.20.
#[derive(States, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppState {
    #[default]
    Boot,
    PreloadConfigs,
    LauncherSplash,
    LoginLobby,
    MainLobby,
    WarChoice,
    LoadingScene,
    InGame,
    DevSandbox,
}

/// Marker on root entities belonging to `LauncherSplash` so `OnExit` cleans them up completely.
#[derive(Component)]
pub struct SplashUiRoot;

/// Marker on root entities belonging to `LoginLobby` so `OnExit` cleans them up completely.
#[derive(Component)]
pub struct LoginUiRoot;

/// Marker on root entities belonging to `LoadingScene` so `OnExit` cleans them up completely.
#[derive(Component)]
pub struct LoadingSceneUiRoot;

/// Marker on entities belonging to active `InGame` session (`OnExit` cleanup).
#[derive(Component)]
pub struct InGameRoot;

/// Marker on entities belonging to `DevSandbox` (`OnExit` cleanup).
#[derive(Component)]
pub struct SandboxRoot;

/// Marker on transient 2D UI camera active during Splash / Login / LoadingScene states.
#[derive(Component)]
pub struct MenuCamera2d;

/// Registers the Bevy [`AppState`] state machine and camera cleanup helpers.
pub struct AppStatePlugin;

impl Plugin for AppStatePlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppState>();
    }
}

/// Despawns all top-level root entities marked with `M`.
pub fn despawn_roots_with<M: Component>(
    mut commands: Commands,
    q: Query<Entity, (With<M>, Without<ChildOf>)>,
) {
    for entity in &q {
        commands.entity(entity).try_despawn();
    }
}
