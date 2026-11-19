//! Opens the settings window for manual testing, without `blink-app`:
//!
//! ```sh
//! cargo run -p blink-ui --example settings -- [CONFIG_PATH]
//! ```
//!
//! `CONFIG_PATH` defaults to the user's config file.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt().init();
    let Some(path) = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(blink_config::default_config_path)
    else {
        tracing::error!("no config path given and the default is unknown");
        return ExitCode::FAILURE;
    };
    match blink_ui::run(&path) {
        Ok(outcome) => {
            tracing::info!(?outcome, path = %path.display(), "done");
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!(error = %blink_sys::ErrorChain(&err), "settings failed");
            ExitCode::FAILURE
        }
    }
}
