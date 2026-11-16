//! Bevy 0.20.1 Pure-Bevy Rebuild Entry Point (`Boot -> PreloadConfigs -> LauncherSplash ->
//! LoginLobby -> MainLobby -> LoadingScene -> InGame`, plus `DevSandbox` and `--headless-server`).

mod actor_rig;
mod auth;
mod config;
mod ingame;
mod lobby;
mod net;
mod sandbox;
mod states;
mod survival;
mod ui;
mod world_physics;

use std::path::PathBuf;

use bevy::app::AppExit;
use bevy::ecs::system::SystemParam;
use bevy::light::GlobalAmbientLight;
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};

use crate::config::{
    AssetManifest, GameConfigPlugin, GameConfigs, LocalizationDb, PlayerSessionPrefs,
    load_manifest_from_disk, resolve_assets_root,
};
use crate::ingame::InGamePlugin;
use crate::lobby::LobbyPlugin;
use crate::net::BevyNetPlugin;
use crate::net::transport::{NetClientState, run_headless_server};
use crate::sandbox::SandboxPlugin;
use crate::states::{AppState, AppStatePlugin};
use crate::ui::GameUiPlugin;

#[derive(Resource)]
struct SmokeCaptureConfig {
    out_dir: PathBuf,
    capture_all_tabs: bool,
    captured_splash: bool,
    captured_login: bool,
    captured_server_modal: bool,
    captured_account_modal: bool,
    captured_lobby: bool,
    captured_establish: bool,
    captured_armory: bool,
    captured_settings: bool,
    started_ingame_connect: bool,
    captured_loading: bool,
    seeded_ingame_sample: bool,
    captured_ingame: bool,
    wait_frames: u32,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(pos) = args.iter().position(|a| a == "--headless-server") {
        let bind_addr = args
            .get(pos + 1)
            .filter(|s| !s.starts_with('-'))
            .map(String::as_str)
            .unwrap_or("0.0.0.0:9211");
        run_headless_server(bind_addr);
        return;
    }

    let smoke_dir = args
        .iter()
        .position(|a| a == "--capture-smoke")
        .and_then(|pos| args.get(pos + 1))
        .map(PathBuf::from);

    run_client(smoke_dir);
}

fn run_client(smoke_dir: Option<PathBuf>) {
    let assets_root = resolve_assets_root();
    let manifest = load_manifest_from_disk(&assets_root).unwrap_or_else(|e| {
        warn!("Failed to read manifest.json in `{}`: {e}", assets_root.display());
        AssetManifest::default()
    });

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: assets_root.to_string_lossy().into_owned(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: format!(
                        "{} — Pure Bevy 0.20 (100P Net)",
                        manifest.project_name
                    ),
                    mode: if smoke_dir.is_some() {
                        bevy::window::WindowMode::Windowed
                    } else {
                        bevy::window::WindowMode::BorderlessFullscreen(
                            bevy::window::MonitorSelection::Current,
                        )
                    },
                    resolution: (1920, 1080).into(),
                    ..default()
                }),
                ..default()
            }),
    )
    .insert_resource(manifest)
    .insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.88, 0.92, 1.0),
        brightness: 550.0,
        ..default()
    })
    .add_plugins((
        AppStatePlugin,
        GameConfigPlugin,
        BevyNetPlugin,
        GameUiPlugin,
        LobbyPlugin,
        InGamePlugin,
        SandboxPlugin,
    ));

    if let Some(dir) = smoke_dir {
        let _ = std::fs::create_dir_all(&dir);
        app.insert_resource(crate::ui::launcher::SplashCaptureHold(true))
            .insert_resource(SmokeCaptureConfig {
                out_dir: dir,
                capture_all_tabs: std::env::var("SMOKE_ALL").is_ok_and(|v| v == "1"),
                captured_splash: false,
                captured_login: false,
                captured_server_modal: false,
                captured_account_modal: false,
                captured_lobby: false,
                captured_establish: false,
                captured_armory: false,
                captured_settings: false,
                started_ingame_connect: false,
                captured_loading: false,
                seeded_ingame_sample: false,
                captured_ingame: false,
                wait_frames: 0,
            })
            .add_systems(Update, smoke_capture_driver);
    }

    app.run();
}

#[derive(SystemParam)]
struct SmokeDriverCtx<'w> {
    configs: Res<'w, GameConfigs>,
    prefs: ResMut<'w, PlayerSessionPrefs>,
    l10n: Res<'w, LocalizationDb>,
    client: ResMut<'w, NetClientState>,
    terrain_opt: Option<Res<'w, crate::world_physics::TerrainColliderGrid>>,
    login_ui: Option<ResMut<'w, crate::ui::login::LoginUiState>>,
    lobby_ui: Option<ResMut<'w, crate::ui::main_lobby::MainLobbyUiState>>,
}

#[allow(clippy::too_many_arguments)]
fn smoke_capture_driver(
    mut commands: Commands,
    state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
    asset_server: Res<AssetServer>,
    imgs: Query<&ImageNode>,
    scenes: Query<&WorldAssetRoot>,
    mut cfg: ResMut<SmokeCaptureConfig>,
    mut splash_hold: Option<ResMut<crate::ui::launcher::SplashCaptureHold>>,
    mut ctx: SmokeDriverCtx,
    mut exit: MessageWriter<AppExit>,
) {
    if state.is_changed() {
        info!("Smoke capture entered state {:?}", state.get());
        cfg.wait_frames = 0;
        return;
    }
    let all_loaded = !imgs.is_empty()
        && imgs.iter().all(|n| {
            n.image.path().is_none() || asset_server.is_loaded_with_dependencies(n.image.id())
        });
    let all_scenes_loaded = scenes
        .iter()
        .all(|s| asset_server.is_loaded_with_dependencies(s.0.id()));
    cfg.wait_frames += 1;
    if cfg.wait_frames.is_multiple_of(180) {
        info!(
            "Smoke wait state={:?} frames={} all_loaded={} all_scenes={}",
            state.get(),
            cfg.wait_frames,
            all_loaded,
            all_scenes_loaded
        );
    }
    match state.get() {
        AppState::LauncherSplash
            if !cfg.captured_splash && all_loaded && cfg.wait_frames >= 80 =>
        {
            cfg.captured_splash = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("01_splash.png")));
        }
        AppState::LauncherSplash if cfg.captured_splash && cfg.wait_frames >= 16 => {
            if let Some(ref mut hold) = splash_hold {
                hold.0 = false;
            }
        }
        AppState::LoginLobby if !cfg.captured_login && all_loaded && cfg.wait_frames >= 48 => {
            cfg.captured_login = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("02_login.png")));
        }
        AppState::LoginLobby
            if cfg.capture_all_tabs
                && cfg.captured_login
                && !cfg.captured_server_modal
                && !ctx.prefs.server_modal_open
                && cfg.wait_frames >= 16 =>
        {
            ctx.prefs.server_modal_open = true;
            if let Some(ref mut lu) = ctx.login_ui {
                lu.dirty = true;
            }
            cfg.wait_frames = 0;
        }
        AppState::LoginLobby
            if cfg.capture_all_tabs
                && !cfg.captured_server_modal
                && ctx.prefs.server_modal_open
                && all_loaded
                && cfg.wait_frames >= 28 =>
        {
            cfg.captured_server_modal = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("02b_server_modal.png")));
        }
        AppState::LoginLobby
            if cfg.capture_all_tabs
                && cfg.captured_server_modal
                && !cfg.captured_account_modal
                && !ctx.prefs.account_modal_open
                && cfg.wait_frames >= 16 =>
        {
            ctx.prefs.server_modal_open = false;
            ctx.prefs.account_modal_open = true;
            if let Some(ref mut lu) = ctx.login_ui {
                lu.cache_dropdown_open = true;
                lu.dirty = true;
            }
            cfg.wait_frames = 0;
        }
        AppState::LoginLobby
            if cfg.capture_all_tabs
                && !cfg.captured_account_modal
                && ctx.prefs.account_modal_open
                && all_loaded
                && cfg.wait_frames >= 28 =>
        {
            cfg.captured_account_modal = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("02c_account_modal.png")));
        }
        AppState::LoginLobby
            if cfg.captured_login
                && (!cfg.capture_all_tabs || cfg.captured_account_modal)
                && cfg.wait_frames >= 16 =>
        {
            ctx.prefs.server_modal_open = false;
            ctx.prefs.account_modal_open = false;
            next_state.set_if_different(AppState::MainLobby);
        }
        AppState::MainLobby
            if !cfg.captured_lobby
                && all_loaded
                && all_scenes_loaded
                && cfg.wait_frames >= 120 =>
        {
            cfg.captured_lobby = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("03_main_lobby.png")));
        }
        AppState::MainLobby
            if cfg.capture_all_tabs
                && cfg.captured_lobby
                && !cfg.captured_establish
                && cfg.wait_frames >= 18
                && ctx.lobby_ui.as_deref().is_some_and(|u| u.active_tab != crate::ui::main_lobby::LobbyTab::Establish) =>
        {
            if let Some(ref mut lu) = ctx.lobby_ui {
                lu.active_tab = crate::ui::main_lobby::LobbyTab::Establish;
                lu.dirty = true;
            }
            cfg.wait_frames = 0;
        }
        AppState::MainLobby
            if cfg.capture_all_tabs
                && !cfg.captured_establish
                && all_loaded
                && cfg.wait_frames >= 42
                && ctx.lobby_ui.as_deref().is_some_and(|u| u.active_tab == crate::ui::main_lobby::LobbyTab::Establish) =>
        {
            cfg.captured_establish = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("03b_establish.png")));
        }
        AppState::MainLobby
            if cfg.capture_all_tabs
                && cfg.captured_establish
                && !cfg.captured_armory
                && cfg.wait_frames >= 18
                && ctx.lobby_ui.as_deref().is_some_and(|u| u.active_tab != crate::ui::main_lobby::LobbyTab::Armory) =>
        {
            if let Some(ref mut lu) = ctx.lobby_ui {
                lu.active_tab = crate::ui::main_lobby::LobbyTab::Armory;
                lu.dirty = true;
            }
            cfg.wait_frames = 0;
        }
        AppState::MainLobby
            if cfg.capture_all_tabs
                && !cfg.captured_armory
                && all_loaded
                && cfg.wait_frames >= 36
                && ctx.lobby_ui.as_deref().is_some_and(|u| u.active_tab == crate::ui::main_lobby::LobbyTab::Armory) =>
        {
            cfg.captured_armory = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("03c_armory.png")));
        }
        AppState::MainLobby
            if cfg.capture_all_tabs
                && cfg.captured_armory
                && !cfg.captured_settings
                && cfg.wait_frames >= 18
                && ctx.lobby_ui.as_deref().is_some_and(|u| u.active_tab != crate::ui::main_lobby::LobbyTab::Settings) =>
        {
            if let Some(ref mut lu) = ctx.lobby_ui {
                lu.active_tab = crate::ui::main_lobby::LobbyTab::Settings;
                lu.dirty = true;
            }
            cfg.wait_frames = 0;
        }
        AppState::MainLobby
            if cfg.capture_all_tabs
                && !cfg.captured_settings
                && all_loaded
                && cfg.wait_frames >= 36
                && ctx.lobby_ui.as_deref().is_some_and(|u| u.active_tab == crate::ui::main_lobby::LobbyTab::Settings) =>
        {
            cfg.captured_settings = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("03d_settings.png")));
        }
        AppState::MainLobby
            if !cfg.started_ingame_connect
                && cfg.captured_lobby
                && (!cfg.capture_all_tabs || cfg.captured_settings)
                && cfg.wait_frames >= 20 =>
        {
            cfg.started_ingame_connect = true;
            cfg.wait_frames = 0;
            let ver = if ctx.configs.version.str_v.is_empty() {
                "3.14.0.0"
            } else {
                ctx.configs.version.str_v.as_str()
            };
            if let Some(srv) = ctx
                .configs
                .server_list
                .servers
                .iter()
                .find(|s| s.is_local_loopback)
            {
                let _ = ctx.client.connect_to_server_entry(
                    &mut commands,
                    srv,
                    ver,
                    &ctx.prefs.account_name,
                    ctx.l10n.selected_lang_index,
                );
            }
            next_state.set_if_different(AppState::LoadingScene);
        }
        AppState::LoadingScene
            if cfg.capture_all_tabs && !cfg.captured_loading && all_loaded && cfg.wait_frames >= 8 =>
        {
            cfg.captured_loading = true;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("04_loading_scene.png")));
        }
        AppState::InGame
            if !cfg.seeded_ingame_sample && cfg.wait_frames >= 40 =>
        {
            cfg.seeded_ingame_sample = true;
            if let Some(terrain) = ctx.terrain_opt.as_deref() {
                let gy = terrain.ground_y(0.0, -4.2);
                ctx.client.send_world_op(
                    crate::net::protocol::OP_PLACE_BUILD,
                    0,
                    crate::world_physics::BuildPieceKind::Foundation as u8,
                    0,
                    Vec3::new(0.0, gy - 1.25, -4.2),
                );
                let gx = terrain.ground_y(-4.2, -5.5);
                ctx.client.send_world_op(
                    crate::net::protocol::OP_PLACE_BUILD,
                    0,
                    crate::world_physics::BuildPieceKind::Generator as u8,
                    0,
                    Vec3::new(-4.2, gx, -5.5),
                );
                ctx.client.send_world_op(
                    crate::net::protocol::OP_PLACE_BUILD,
                    0,
                    crate::world_physics::BuildPieceKind::ElectricTurret as u8,
                    0,
                    Vec3::new(0.0, gy + 0.75, -4.2),
                );
            }
        }
        AppState::InGame
            if !cfg.captured_ingame && cfg.seeded_ingame_sample && all_scenes_loaded && cfg.wait_frames >= 135 =>
        {
            cfg.captured_ingame = true;
            cfg.wait_frames = 0;
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(cfg.out_dir.join("04_ingame.png")));
        }
        _ if cfg.captured_ingame && cfg.wait_frames >= 24 => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use bevy::app::TaskPoolPlugin;
    use bevy::state::app::StatesPlugin;
    use bevy::time::{TimePlugin, TimeUpdateStrategy};

    use crate::config::LocalizationDb;
    use crate::states::AppState;

    #[test]
    fn manifest_loads_and_referenced_assets_exist_on_disk() {
        let manifest = load_manifest_from_disk(Path::new("assets"))
            .expect("assets/manifest.json must exist alongside bevy_zero_game");

        let total_glbs = manifest.scenes.len()
            + manifest.characters.len()
            + manifest.props.len()
            + manifest.build_parts.len()
            + manifest.world_props.len();
        assert!(total_glbs > 0, "manifest must reference at least one exported .glb");
        assert!(
            !manifest.ui_panels.is_empty(),
            "manifest must reference Phase 1 exported ui_panels/*.json"
        );

        for entry in manifest
            .scenes
            .iter()
            .chain(manifest.characters.iter())
            .chain(manifest.props.iter())
            .chain(manifest.build_parts.iter())
            .chain(manifest.world_props.iter())
        {
            let full = Path::new("assets").join(&entry.asset_path);
            let bytes = std::fs::read(&full)
                .unwrap_or_else(|_| panic!("missing glb file: {}", full.display()));
            assert!(
                bytes.starts_with(b"glTF"),
                "invalid GLB header at {}",
                full.display()
            );
        }

        for media in manifest
            .audio
            .iter()
            .chain(manifest.fonts.iter())
            .chain(manifest.ui_sprites.iter())
        {
            let full = Path::new("assets").join(&media.asset_path);
            assert!(full.is_file(), "missing media file: {}", full.display());
        }

        for panel in &manifest.ui_panels {
            let full = Path::new("assets").join(&panel.asset_path);
            assert!(full.is_file(), "missing ui panel json: {}", full.display());
        }

        for audio_entry in &manifest.audio {
            let full = Path::new("assets").join(&audio_entry.asset_path);
            let bytes = std::fs::read(&full)
                .unwrap_or_else(|_| panic!("missing audio file: {}", full.display()));
            let source = bevy::audio::AudioSource {
                bytes: bytes.into(),
            };
            let mut decoder =
                <bevy::audio::AudioSource as bevy::audio::Decodable>::decoder(&source);
            assert!(
                decoder.next().is_some(),
                "exported audio asset `{}` decoded 0 samples",
                full.display()
            );
        }
    }

    #[test]
    fn boot_preload_transitions_cleanly_to_launcher_splash() {
        let mut app = App::new();
        app.add_plugins((
            TaskPoolPlugin::default(),
            TimePlugin,
            StatesPlugin,
            AppStatePlugin,
            GameConfigPlugin,
        ))
        .insert_resource(TimeUpdateStrategy::FixedTimesteps(1));

        for _ in 0..4 {
            app.update();
            if *app.world().resource::<State<AppState>>().get() == AppState::LauncherSplash {
                break;
            }
        }

        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::LauncherSplash
        );
        let configs = app.world().resource::<crate::config::GameConfigs>();
        assert!(configs.loaded_small);
        assert_eq!(configs.version.str_v, "3.14.0.0");
        assert!(!app.world().resource::<LocalizationDb>().index_entries.is_empty());

        // Drain async MultiLanguageCfg.json IoTaskPool task deterministically with strict deadline.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.world().resource::<crate::config::GameConfigs>().loaded_multilang {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for MultiLanguageCfg async load"
            );
            std::thread::yield_now();
            app.update();
        }
        assert!(app.world().resource::<LocalizationDb>().by_key.len() > 40_000);
        let configs = app.world().resource::<crate::config::GameConfigs>();
        assert!(configs.item_catalog_count > 10_000);
        assert!(!configs.weapon_catalog.is_empty());

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::MainLobby);
        app.update();
        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::MainLobby
        );
    }
}
