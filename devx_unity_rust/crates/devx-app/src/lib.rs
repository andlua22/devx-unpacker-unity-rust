//! Desktop `egui`/`eframe` workbench library for DevX Unity Rust.

pub mod app;
pub mod audio_player;
pub mod gl_viewport;
pub mod scene_preview;
pub mod state;
pub mod views;

pub use app::DevXApp;

use std::path::PathBuf;

/// Launches the native desktop workbench window, optionally opening `initial_path`.
pub fn run_native_app(initial_path: Option<PathBuf>) -> anyhow::Result<()> {
    let native_options = eframe::NativeOptions {
        depth_buffer: 24,
        viewport: egui::ViewportBuilder::default()
            .with_title("DevX Unity Unpacker — Rust Edition")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 620.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "devx_unity_rust",
        native_options,
        Box::new(move |cc| Ok(Box::new(DevXApp::new(&cc.egui_ctx, initial_path.clone())))),
    )
    .map_err(|err| anyhow::anyhow!("eframe exited with error: {err}"))?;

    Ok(())
}
