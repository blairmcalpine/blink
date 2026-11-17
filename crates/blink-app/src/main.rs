//! The blink binary.
//!
//! `blink` runs the recorder: a capture-plus-encode thread feeding NVENC
//! packets into the clip ring, an audio thread, a save thread, and the tray
//! and hotkey thread that drives them (see `engine`). `blink --settings` runs
//! the settings window instead, as a separate short-lived process, so the
//! recorder never loads a UI toolkit.
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

/// The argument that runs the settings window instead of the recorder.
const SETTINGS_FLAG: &str = "--settings";

fn main() -> ExitCode {
    let mut settings = false;
    let mut unknown = Vec::new();
    for arg in std::env::args_os().skip(1) {
        if arg == OsStr::new(SETTINGS_FLAG) {
            settings = true;
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
        run_recorder()
    }
}

/// Runs the settings window on the default config file.
fn run_settings() -> ExitCode {
    let Some(path) = blink_config::default_config_path() else {
        tracing::error!("cannot locate the per-user config folder");
        return ExitCode::FAILURE;
    };
    match blink_ui::run(&path) {
        Ok(blink_ui::Outcome::Saved) => {
            tracing::info!(path = %path.display(), "settings saved");
            ExitCode::SUCCESS
        }
        Ok(blink_ui::Outcome::Cancelled) => {
            tracing::info!("settings closed without saving");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(error = error_chain(&error), "settings window failed");
            ExitCode::FAILURE
        }
    }
}

/// `error` and its sources, outermost first, joined with `: `. `UiError` is
/// not `Send` (it can hold a window-system error), so it cannot go through
/// `anyhow`.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
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
fn run_recorder() -> ExitCode {
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "blink starting");
    match engine::run() {
        Ok(code) => code,
        Err(error) => {
            let message = format!("{error:#}");
            tracing::error!(error = message, "blink stopped");
            engine::show_error(&message);
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn run_recorder() -> ExitCode {
    tracing::error!(
        "blink records with DXGI Desktop Duplication and NVENC, so it only runs on Windows; \
         `blink {SETTINGS_FLAG}` opens the settings window on this host"
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;

    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Self>>);

    impl fmt::Display for Layer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|inner| {
                let source: &(dyn std::error::Error + 'static) = inner;
                source
            })
        }
    }

    #[test]
    fn error_chain_lists_every_source() {
        let error = Layer(
            "failed to show the window",
            Some(Box::new(Layer("no OpenGL context", None))),
        );
        assert_eq!(
            error_chain(&error),
            "failed to show the window: no OpenGL context"
        );
        assert_eq!(error_chain(&Layer("alone", None)), "alone");
    }
}
