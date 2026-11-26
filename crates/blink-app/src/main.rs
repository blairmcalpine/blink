//! The blink binary.
//!
//! `blink` runs the recorder: a capture-plus-encode thread feeding NVENC
//! packets into the clip ring, an audio thread, a save thread, and the tray
//! and hotkey thread that drives them (see `engine`). `blink --settings` runs
//! the settings window instead, as a separate short-lived process, so the
//! recorder never loads a UI toolkit. `blink --open-settings` runs the
//! recorder and has it open the settings window once started, as the
//! installer's "Launch blink" and the Start menu shortcut do. Starting blink while it already runs asks
//! the running recorder to open its settings window instead.
//!
//! The recorder only runs on Windows; elsewhere the binary still builds, and
//! `--settings` works for UI development.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

// The modules below are direct children of the crate root, so restricted
// visibility on their items (`pub(crate)`) trips `clippy::redundant_pub_crate`,
// while plain `pub` trips the denied `unreachable_pub`. `expect` documents
// that conflict. The pure modules are only used by the Windows engine and by
// tests.
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod chime;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod clip;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod cut;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod decide;
#[cfg(windows)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod engine;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod icon;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod keys;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod logging;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod mapping;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod pipeline;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod reload;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod shared;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod status;

use std::ffi::OsStr;
use std::process::ExitCode;

use blink_sys::ErrorChain;

/// The argument that runs the settings window instead of the recorder.
const SETTINGS_FLAG: &str = "--settings";

/// The argument that runs the recorder and opens the settings window.
const OPEN_SETTINGS_FLAG: &str = "--open-settings";

fn main() -> ExitCode {
    let mut settings = false;
    let mut open_settings = false;
    let mut unknown = Vec::new();
    for arg in std::env::args_os().skip(1) {
        if arg == OsStr::new(SETTINGS_FLAG) {
            settings = true;
        } else if arg == OsStr::new(OPEN_SETTINGS_FLAG) {
            open_settings = true;
        } else {
            unknown.push(arg);
        }
    }

    let _log = logging::init(if settings { "blink-settings" } else { "blink" });
    logging::install_panic_hook(notify_crash);
    if !unknown.is_empty() {
        tracing::warn!(?unknown, "ignoring unknown arguments");
    }
    if settings {
        run_settings()
    } else {
        run_recorder(open_settings)
    }
}

/// Runs the settings window on the default config file.
fn run_settings() -> ExitCode {
    let Some(path) = blink_config::default_config_path() else {
        tracing::error!("cannot locate the per-user config folder");
        return ExitCode::FAILURE;
    };
    #[cfg(windows)]
    let run_key = RunKey::for_this_exe();
    #[cfg(windows)]
    let autostart = run_key
        .as_ref()
        .map(|key| -> &dyn blink_ui::AutostartSetting { key });
    // Starting at sign-in is a Windows feature; elsewhere there is no row.
    #[cfg(not(windows))]
    let autostart = None;
    match blink_ui::run(&path, autostart) {
        Ok(blink_ui::Outcome::Saved) => {
            tracing::info!(path = %path.display(), "settings saved");
            ExitCode::SUCCESS
        }
        Ok(blink_ui::Outcome::Cancelled) => {
            tracing::info!("settings closed without saving");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(error = %ErrorChain(&error), "settings window failed");
            ExitCode::FAILURE
        }
    }
}

/// Starting this blink.exe at sign-in, through the per-user Run key.
#[cfg(windows)]
struct RunKey {
    exe: std::path::PathBuf,
}

#[cfg(windows)]
impl RunKey {
    /// `None`, with a warning, when the running executable's path is
    /// unknown; the settings window then has no startup row.
    fn for_this_exe() -> Option<Self> {
        std::env::current_exe()
            .inspect_err(|error| {
                tracing::warn!(error = %ErrorChain(error), "cannot locate blink.exe for the startup setting");
            })
            .ok()
            .map(|exe| Self { exe })
    }
}

#[cfg(windows)]
impl blink_ui::AutostartSetting for RunKey {
    fn read(&self) -> Result<blink_sys::Autostart, Box<dyn std::error::Error + Send + Sync>> {
        Ok(blink_sys::read_autostart(&self.exe)?)
    }

    fn write(&self, enabled: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(blink_sys::write_autostart(&self.exe, enabled)?)
    }
}
/// Tells the user the process crashed; release builds have no console.
#[cfg(windows)]
fn notify_crash(message: &str) {
    engine::show_error(message);
}

/// Other hosts have the panic on stderr already.
#[cfg(not(windows))]
const fn notify_crash(_message: &str) {}

#[cfg(windows)]
fn run_recorder(open_settings: bool) -> ExitCode {
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "blink starting");
    match engine::run(open_settings) {
        Ok(code) => code,
        Err(error) => {
            let message = ErrorChain(error.as_ref()).to_string();
            tracing::error!(error = message, "blink stopped");
            engine::show_error(&message);
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn run_recorder(_open_settings: bool) -> ExitCode {
    tracing::error!(
        "blink records with DXGI Desktop Duplication and NVENC, so it only runs on Windows; \
         `blink {SETTINGS_FLAG}` opens the settings window on this host"
    );
    ExitCode::FAILURE
}
