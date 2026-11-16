//! Binary entry point for `devx-app`.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let initial_path = std::env::args_os().nth(1).map(PathBuf::from);
    devx_app::run_native_app(initial_path)
}
