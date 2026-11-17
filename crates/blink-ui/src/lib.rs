//! The blink settings window.
//!
//! `blink-app` calls [`run`] when started as `blink.exe --settings`, a
//! separate short-lived child process, so the engine process links eframe but
//! never initializes it. The window edits the config file and exits; the
//! engine reloads the config when that child process exits. There is no
//! other IPC.
//!
//! The window is a small egui form on eframe's OpenGL (glow) renderer. It
//! repaints only on input, never continuously; the text cursor does not
//! blink, so a focused text field stays idle too. It opens the file with
//! [`Config::load_unvalidated`] so out-of-range values show inline errors
//! instead of failing, validates the draft every frame with
//! [`Config::errors`], disables Save while any error remains, and writes with
//! [`Config::save_atomic`].

#![forbid(unsafe_code)]

#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod app;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod draft;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod keys;

use std::cell::Cell;
use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;

use blink_config::{Config, ConfigError};
use eframe::egui;
use thiserror::Error;

/// How the settings window closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The user saved a changed, valid config to the file.
    Saved,
    /// The window closed without writing the file: the user cancelled,
    /// closed the window, or saved without changing anything.
    Cancelled,
}

/// Errors that keep the settings window from running.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UiError {
    /// The config file exists but could not be read or parsed. The settings
    /// window does not open, so a file it cannot represent is never
    /// overwritten; an error dialog tells the user instead.
    #[error("failed to open the config for editing")]
    Load(#[from] ConfigError),
    /// eframe could not create the window or its OpenGL context.
    #[error("failed to show the window")]
    Window(#[from] eframe::Error),
}

/// Shows the settings window for the config at `config_path` and blocks
/// until it closes. A missing file opens with the defaults and is created on
/// save. This is `blink.exe --settings`.
///
/// Call this at most once per process, from the main thread: it starts the
/// windowing event loop, which macOS requires on the main thread and winit
/// allows only once.
///
/// # Errors
///
/// Returns [`UiError::Load`] when the file cannot be read or parsed, after
/// showing a native error dialog where one is available (Windows, macOS).
/// Returns [`UiError::Window`] when the window cannot be shown. A failed
/// save is not an error: the window stays open and shows it.
pub fn run(config_path: &Path) -> Result<Outcome, UiError> {
    let config = match Config::load_unvalidated(config_path) {
        Ok(config) => config,
        Err(err) => {
            show_load_error(config_path, &err);
            return Err(err.into());
        }
    };
    let outcome = Cell::new(Outcome::Cancelled);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("blink settings")
            .with_inner_size(app::INITIAL_SIZE)
            .with_resizable(false)
            .with_maximize_button(false),
        renderer: eframe::Renderer::Glow,
        centered: true,
        ..eframe::NativeOptions::default()
    };
    let app = app::SettingsApp::new(config_path.to_owned(), config, &outcome);
    let create: eframe::AppCreator<'_> = Box::new(|cc| {
        configure(&cc.egui_ctx);
        Ok(Box::new(app))
    });
    eframe::run_native("blink settings", options, create)?;
    let outcome = outcome.get();
    tracing::info!(?outcome, "settings window closed");
    Ok(outcome)
}

/// Turns off egui behaviour that does not fit a small fixed-width form.
fn configure(ctx: &egui::Context) {
    ctx.options_mut(|options| {
        // Ctrl+plus and Ctrl+minus zoom would clip the window's fixed width.
        options.zoom_with_keyboard = false;
        // Ctrl+Q (Cmd+Q on macOS) would close the window and silently
        // discard edits, and could not be recorded as a hotkey.
        options.quit_shortcuts.clear();
    });
    // A blinking cursor repaints twice a second while a text field has focus.
    ctx.all_styles_mut(|style| style.visuals.text_cursor.blink = false);
}

fn show_load_error(config_path: &Path, err: &ConfigError) {
    let message = describe(err);
    tracing::error!(error = %message, "cannot open the config for editing");
    #[cfg(any(windows, target_os = "macos"))]
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("blink settings")
        .set_description(format!(
            "blink cannot open its settings file:\n{}\n\n{message}\n\nFix or delete the file, \
             then open Settings again.",
            config_path.display()
        ))
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = config_path;
}

/// Formats an error with its chain of sources.
fn describe(err: &dyn Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        let _ = write!(message, ": {err}");
        source = err.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_disables_zoom_quit_and_cursor_blink() {
        let ctx = egui::Context::default();
        configure(&ctx);
        ctx.options(|options| {
            assert!(!options.zoom_with_keyboard);
            assert_eq!(options.quit_shortcuts, []);
        });
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            assert!(!ctx.style_of(theme).visuals.text_cursor.blink);
        }
    }

    #[test]
    fn describe_includes_the_source_chain() {
        let err = Config::load_unvalidated(Path::new("/")).unwrap_err();
        let message = describe(&err);
        assert!(message.starts_with("failed to read config /"), "{message}");
        assert!(message.contains(": "), "{message}");
    }
}
