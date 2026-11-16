//! Pure-Bevy UI subsystem and menu-camera management.

pub mod ingame_hud;
pub mod launcher;
pub mod loading_panel;
pub mod login;
pub mod main_lobby;
pub mod panel_loader;
pub mod widgets;

use bevy::prelude::*;

use crate::states::{AppState, MenuCamera2d, despawn_roots_with};
use self::ingame_hud::InGameHudPlugin;
use self::launcher::LauncherSplashPlugin;
use self::login::LoginUiPlugin;
use self::main_lobby::MainLobbyUiPlugin;
use self::panel_loader::{
    BoundUiClick, UiPreviewInteractionState, drive_ui_panel_animations, resize_ui_panel_frames,
    style_palette_buttons,
};
use self::widgets::UiWidgetsRuntimePlugin;

#[derive(Resource, Default)]
struct WarmMenuTextures(Vec<Handle<Image>>);

/// Coordinates all 2D menu screens, HUDs, shared button hover/press styling, and UI camera.
pub struct GameUiPlugin;

impl Plugin for GameUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiPreviewInteractionState>()
            .init_resource::<WarmMenuTextures>()
            .add_message::<BoundUiClick>()
            .add_plugins((
                UiWidgetsRuntimePlugin,
                LauncherSplashPlugin,
                LoginUiPlugin,
                MainLobbyUiPlugin,
                InGameHudPlugin,
            ))
            .add_systems(Startup, warm_key_menu_textures)
            .add_systems(OnEnter(AppState::MainLobby), despawn_roots_with::<MenuCamera2d>)
            .add_systems(OnEnter(AppState::InGame), despawn_roots_with::<MenuCamera2d>)
            .add_systems(OnEnter(AppState::DevSandbox), despawn_roots_with::<MenuCamera2d>)
            .add_systems(
                Update,
                (
                    sync_menu_camera_for_state,
                    refresh_early_fonts,
                    style_palette_buttons,
                    resize_ui_panel_frames,
                    drive_ui_panel_animations,
                ),
            );
    }
}

fn warm_key_menu_textures(asset_server: Res<AssetServer>, mut warm: ResMut<WarmMenuTextures>) {
    for rel in [
        "ui/Loading_7.png",
        "ui/Loading_Topic2.png",
        "ui/Loading_bg_2.png",
        "ui/logo_haiwai.png",
        "ui/loginPanel_ks.png",
        "ui/Loading_5.png",
        "ui/NewMainPanel_dibian1.png",
        "ui/BagAndBuildPanel_bg_1.png",
        "ui/LoginServerPanel_ditu.png",
        "ui/1BattlePanel_zuo_shang_infoBG_1.png",
        "ui/1BattlePanel_map_di_2_1.png",
        "ui/1BattlePanel_buttle_diban.png",
        "ui/1BattlePanel_xuan8.png",
        "ui/qbz97.png",
        "ui/tiechui.png",
        "ui/tb_junyongbishou_da_1.png",
    ] {
        warm.0.push(asset_server.load(rel));
    }
}

/// Ensures any UI `Text` spawned while `system_font_discovery` is still initializing finishes
/// re-layout smoothly once system sans-serif fonts finish scanning.
fn refresh_early_fonts(
    time: Res<Time>,
    mut stage: Local<u8>,
    mut fonts_q: Query<&mut TextFont>,
) {
    let elapsed = time.elapsed_secs();
    let target_stage = if elapsed >= 2.2 {
        3
    } else if elapsed >= 1.1 {
        2
    } else if elapsed >= 0.35 {
        1
    } else {
        0
    };
    if target_stage > *stage {
        *stage = target_stage;
        for mut tf in &mut fonts_q {
            tf.set_changed();
        }
    }
}

fn sync_menu_camera_for_state(
    mut commands: Commands,
    state: Res<State<AppState>>,
    menu_cam_q: Query<Entity, With<MenuCamera2d>>,
) {
    let needs_menu_cam = matches!(
        state.get(),
        AppState::Boot
            | AppState::PreloadConfigs
            | AppState::LauncherSplash
            | AppState::LoginLobby
            | AppState::LoadingScene
    );

    if needs_menu_cam && menu_cam_q.is_empty() {
        commands.spawn((MenuCamera2d, Camera2d));
    } else if !needs_menu_cam {
        for cam in &menu_cam_q {
            commands.entity(cam).try_despawn();
        }
    }
}
