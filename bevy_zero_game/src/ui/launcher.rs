//! Pure-Bevy Launcher Splash screen delegating to [`crate::ui::loading_panel`] (`SC.UI.LoadingPanel`).

use bevy::prelude::*;

use crate::config::{GameConfigs, LocalizationDb};
use crate::states::AppState;
pub use crate::ui::loading_panel::SplashCaptureHold;
use crate::ui::loading_panel::{spawn_loading_panel_screen, update_loading_panel_controller};

/// Registers the `LauncherSplash` UI lifecycle and retained `LoadingPanel` controller.
pub struct LauncherSplashPlugin;

impl Plugin for LauncherSplashPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::LauncherSplash), spawn_launcher_splash)
            .add_systems(
                Update,
                update_loading_panel_controller.run_if(in_state(AppState::LauncherSplash)),
            );
    }
}

fn spawn_launcher_splash(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    configs: Res<GameConfigs>,
    l10n: Res<LocalizationDb>,
    windows: Query<&Window>,
    capture_hold: Option<Res<SplashCaptureHold>>,
) {
    let win_size = windows
        .iter()
        .next()
        .map(|w| Vec2::new(w.width(), w.height()))
        .unwrap_or(Vec2::new(1280.0, 720.0));
    let deterministic_bg = capture_hold.as_deref().is_some_and(|h| h.0);

    spawn_loading_panel_screen(
        &mut commands,
        &asset_server,
        &configs,
        &l10n,
        win_size,
        deterministic_bg,
    );
}
