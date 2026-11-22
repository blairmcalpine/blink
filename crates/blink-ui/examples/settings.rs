//! Opens the settings window for manual testing, without `blink-app`:
//!
//! ```sh
//! cargo run -p blink-ui --example settings -- [CONFIG_PATH]
//! ```
//!
//! `CONFIG_PATH` defaults to the user's config file. The startup row is
//! backed by memory here, so toggling it only logs.

use std::cell::Cell;
use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use blink_sys::Autostart;

/// A start-at-sign-in setting that lives only in this process.
struct InMemory(Cell<Autostart>);

impl blink_ui::AutostartSetting for InMemory {
    fn read(&self) -> Result<Autostart, Box<dyn Error + Send + Sync>> {
        Ok(self.0.get())
    }

    fn write(&self, enabled: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
        tracing::info!(enabled, "would change whether blink starts at sign-in");
        self.0.set(if enabled {
            Autostart::On
        } else {
            Autostart::Off
        });
        Ok(())
    }
}

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
    let autostart = InMemory(Cell::new(Autostart::On));
    match blink_ui::run(&path, Some(&autostart)) {
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
