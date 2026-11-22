//! The recorder engine and its threads.
//!
//! - **Tray thread** (the main thread, below-normal priority): owns the tray
//!   icon and the global hotkey, pumps their window messages, and supervises
//!   everything else: it sends save requests, restarts audio after failures,
//!   spawns the settings process, applies the reloaded config, and plays
//!   the save sound ([`sound`]) when a save finishes.
//! - **Capture thread** (below normal, [`capture`]): Desktop Duplication into
//!   NVENC into the ring. Never waits on I/O or on a save.
//! - **Audio thread** (below normal, owned by `blink-audio`): WASAPI loopback
//!   into AAC into the ring.
//! - **Save threads** ([`save`]): a cutter copies each clip out of the ring
//!   in chunks at below-normal priority as soon as it can, and a writer
//!   writes the MP4s in background mode.
//! - A short-lived thread per settings window waits for the child process,
//!   and one per warning dialog, so the tray thread keeps pumping messages.
//!
//! The ring is the only state the threads share, behind one mutex held for
//! one packet or one chunk at a time. Workers report back to the tray thread
//! over a channel, and it shows their state in the tray tooltip.

mod audio;
mod capture;
mod save;
mod signal;
mod sound;
mod tray;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use blink_config::Config;
use blink_ring::media::Timestamp;
use blink_sys::{ErrorChain, SingleInstance, ThreadPriority};

use self::audio::AudioSupervisor;
use self::save::{SaveRequest, Saver, local_time};
use self::signal::StopSignal;
use self::tray::{MenuAction, SaveHotkey, Tray};
use crate::mapping::{RingSize, audio_config};
use crate::reload::ReloadPlan;
use crate::shared::SharedRing;
use crate::status::{
    CaptureState, SaveResult, Status, clip_end, escalate, tooltip, unsupported_message,
};

/// How long the tray thread sleeps between checks of its own channels.
/// Window messages (hotkey presses, menu clicks) wake it immediately.
const TICK: Duration = Duration::from_millis(250);

/// How often a hotkey that could not be registered is tried again.
const HOTKEY_RETRY: Duration = Duration::from_secs(10);

/// The per-session mutex that keeps a second recorder from starting.
const INSTANCE_MUTEX: &str = "Local\\blink-engine";

/// Messages from worker threads to the tray thread.
pub(super) enum EngineEvent {
    /// Capture can never work here; show the message and exit.
    CaptureFatal(String),
    /// The capture thread's state changed.
    Capture(CaptureState),
    /// A save finished.
    Saved(SaveResult),
    /// The settings process exited.
    SettingsClosed(std::io::Result<ExitStatus>),
}

/// Shows an error in a native message box. Release builds have no console,
/// so this is how a startup failure reaches the user.
pub(crate) fn show_error(message: &str) {
    message_box(rfd::MessageLevel::Error, message);
}

/// Shows a blocking message box with an OK button.
fn message_box(level: rfd::MessageLevel, message: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(level)
        .set_title("blink")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Shows a warning without blocking the calling thread, which for the tray
/// thread has to keep pumping messages.
fn warn_in_background(message: String) {
    report_in_background(rfd::MessageLevel::Warning, message);
}

/// Shows a message box at `level` on its own thread.
fn report_in_background(level: rfd::MessageLevel, message: String) {
    let shown = thread::Builder::new()
        .name("blink-dialog".to_owned())
        .spawn(move || message_box(level, &message));
    if let Err(error) = shown {
        tracing::warn!(error = %ErrorChain(&error), "cannot show a warning dialog");
    }
}

/// Runs the recorder until Quit. Startup failures are returned; the caller
/// logs and shows them.
pub(crate) fn run() -> anyhow::Result<ExitCode> {
    if let Err(error) = blink_sys::set_current_thread_priority(ThreadPriority::BelowNormal) {
        tracing::warn!(error = %ErrorChain(&error), "could not lower the tray thread priority");
    }
    let Some(_instance) = SingleInstance::acquire(INSTANCE_MUTEX)
        .context("cannot check whether blink is already running")?
    else {
        tracing::info!("blink is already running; exiting");
        message_box(
            rfd::MessageLevel::Info,
            "blink is already running. Find it in the notification area.",
        );
        return Ok(ExitCode::FAILURE);
    };
    let config_path = blink_config::default_config_path()
        .context("cannot locate the per-user config folder (%APPDATA%)")?;
    let config = load_config(&config_path).unwrap_or_else(|| {
        tracing::warn!(path = %config_path.display(), "using the default settings");
        Config::default()
    });
    tracing::info!(?config, path = %config_path.display(), "settings loaded");

    let mut engine = Engine::start(config_path, config)?;
    let code = engine.run();
    engine.shutdown();
    Ok(code)
}

/// Loads the config, logging why when it cannot be used. The file is never
/// overwritten here, and no dialog is shown: the settings window already
/// tells the user when it cannot parse the file.
fn load_config(path: &Path) -> Option<Config> {
    match Config::load(path) {
        Ok(config) => Some(config),
        Err(error) => {
            tracing::error!(
                error = %ErrorChain(&error),
                "cannot use the config file"
            );
            None
        }
    }
}

/// The capture thread of the running pipeline.
struct Capture {
    stop: Arc<StopSignal>,
    thread: JoinHandle<()>,
}

struct Engine {
    config_path: PathBuf,
    config: Config,
    ring: Arc<SharedRing>,
    capture: Option<Capture>,
    audio: AudioSupervisor,
    saver: Saver,
    tray: Tray,
    hotkey: SaveHotkey,
    /// When to try registering the configured hotkey again after it failed.
    hotkey_retry_at: Option<Instant>,
    events: Sender<EngineEvent>,
    received: Receiver<EngineEvent>,
    settings_open: bool,
    capture_state: CaptureState,
    /// When video paused for an unsupported mode, until frames return.
    paused_since: Option<Timestamp>,
    last_save: Option<SaveResult>,
    /// The unsupported display mode last warned about.
    warned_unsupported: Option<(u32, u32)>,
}

impl Engine {
    fn start(config_path: PathBuf, config: Config) -> anyhow::Result<Self> {
        sound::prepare();
        if config.save_sound {
            sound::warm_up();
        }
        let ring = Arc::new(SharedRing::new(RingSize::for_config(&config)));
        let (events, received) = mpsc::channel();
        let saver = Saver::spawn(Arc::clone(&ring), events.clone(), config.output_dir.clone())?;
        let tray = Tray::new("blink")?;
        let hotkey = SaveHotkey::new()?;
        let mut engine = Self {
            config_path,
            config,
            ring,
            capture: None,
            audio: AudioSupervisor::default(),
            saver,
            tray,
            hotkey,
            hotkey_retry_at: None,
            events,
            received,
            settings_open: false,
            capture_state: CaptureState::Starting,
            paused_since: None,
            last_save: None,
            warned_unsupported: None,
        };
        engine.bind_hotkey();
        engine.start_pipeline()?;
        Ok(engine)
    }

    /// The tray thread's loop. Returns the process exit code.
    fn run(&mut self) -> ExitCode {
        loop {
            if !blink_sys::pump_messages(TICK) {
                tracing::info!("window messages ended; quitting");
                return ExitCode::SUCCESS;
            }
            if self.hotkey.pressed() {
                self.request_save();
            }
            while let Some(action) = self.tray.next_action() {
                match action {
                    MenuAction::SaveClip => self.request_save(),
                    MenuAction::OpenClipsFolder => self.open_clips_folder(),
                    MenuAction::OpenLogFolder => open_log_folder(),
                    MenuAction::Settings => self.open_settings(),
                    MenuAction::Quit => {
                        tracing::info!("quit from the tray menu");
                        return ExitCode::SUCCESS;
                    }
                }
            }
            while let Ok(event) = self.received.try_recv() {
                match event {
                    EngineEvent::CaptureFatal(message) => {
                        show_error(&message);
                        return ExitCode::FAILURE;
                    }
                    EngineEvent::Capture(state) => self.capture_changed(state),
                    EngineEvent::Saved(result) => {
                        if self.config.save_sound {
                            sound::play(&result);
                        }
                        self.last_save = Some(result);
                        self.update_tooltip();
                    }
                    EngineEvent::SettingsClosed(status) => self.settings_closed(status),
                }
            }
            self.audio.poll(&self.ring);
            if !self.settings_open && self.hotkey_retry_at.is_some_and(|at| Instant::now() >= at) {
                self.bind_hotkey();
            }
        }
    }

    fn shutdown(&mut self) {
        self.stop_pipeline();
        self.saver.shutdown();
        tracing::info!("blink stopped");
    }

    fn capture_changed(&mut self, state: CaptureState) {
        match &state {
            CaptureState::Retrying { failures, error } if escalate(*failures) => {
                warn_in_background(format!(
                    "blink is not recording: capture failed {failures} times in a row and keeps \
                     retrying.\n\nLatest error: {error}"
                ));
            }
            CaptureState::Unsupported { width, height, .. }
                if self.warned_unsupported != Some((*width, *height)) =>
            {
                self.warned_unsupported = Some((*width, *height));
                warn_in_background(unsupported_message(*width, *height));
            }
            CaptureState::Recording => self.warned_unsupported = None,
            CaptureState::Starting
            | CaptureState::Retrying { .. }
            | CaptureState::Unsupported { .. } => {}
        }
        self.paused_since = state.paused_since(self.paused_since);
        self.capture_state = state;
        self.update_tooltip();
    }

    fn start_pipeline(&mut self) -> anyhow::Result<()> {
        // A restart comes with a new ring, which holds nothing paused.
        self.capture_state = CaptureState::Starting;
        self.paused_since = None;
        let stop = Arc::new(StopSignal::default());
        let thread = capture::spawn(
            &self.config,
            Arc::clone(&self.ring),
            Arc::clone(&stop),
            self.events.clone(),
        )?;
        self.capture = Some(Capture { stop, thread });
        if self.config.audio_enabled {
            match audio_config(&self.config) {
                Ok(config) => self.audio.start(config, &self.ring),
                Err(error) => tracing::error!(error = %ErrorChain(&error), "audio disabled"),
            }
        }
        Ok(())
    }

    /// Stops audio, then capture. Neither holds the ring lock while joining.
    fn stop_pipeline(&mut self) {
        self.audio.stop();
        if let Some(capture) = self.capture.take() {
            capture.stop.stop();
            if capture.thread.join().is_err() {
                tracing::error!("the capture thread panicked");
            }
        }
    }

    fn request_save(&self) {
        let now = match blink_sys::qpc_now() {
            Ok(now) => now,
            Err(error) => {
                tracing::error!(error = %ErrorChain(&error), "cannot read the clock; clip not saved");
                return;
            }
        };
        let end = clip_end(self.paused_since, now);
        if end < now {
            tracing::info!("recording is paused; the clip ends where it paused");
        }
        tracing::info!(seconds = self.config.clip_seconds, "saving a clip");
        self.saver.request(SaveRequest {
            now: end,
            time: local_time(),
            clip_seconds: self.config.clip_seconds,
            output_dir: self.config.output_dir.clone(),
        });
    }

    fn open_clips_folder(&self) {
        let dir = &self.config.output_dir;
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %ErrorChain(&error), dir = %dir.display(), "cannot create the clips folder");
        }
        if let Err(error) = Command::new("explorer.exe").arg(dir).spawn() {
            tracing::warn!(error = %ErrorChain(&error), "cannot open the clips folder");
        }
    }

    /// Spawns `blink --settings` and a thread that reports when it exits.
    /// Ignored while a settings window is already open. The save hotkey is
    /// suspended meanwhile, so the window can record any combination.
    fn open_settings(&mut self) {
        if self.settings_open {
            tracing::debug!("settings already open");
            return;
        }
        let child = std::env::current_exe()
            .context("cannot locate blink.exe")
            .and_then(|exe| {
                Command::new(exe)
                    .arg(crate::SETTINGS_FLAG)
                    .spawn()
                    .context("cannot start the settings window")
            });
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                tracing::error!(error = %ErrorChain(error.as_ref()), "settings unavailable");
                return;
            }
        };
        let events = self.events.clone();
        let waiter = thread::Builder::new()
            .name("blink-settings-wait".to_owned())
            .spawn(move || {
                let _ = events.send(EngineEvent::SettingsClosed(child.wait()));
            });
        match waiter {
            Ok(_detached) => {
                self.settings_open = true;
                self.hotkey.unbind();
            }
            Err(error) => {
                tracing::error!(error = %ErrorChain(&error), "cannot watch the settings window");
            }
        }
    }

    /// Reloads the config whatever the window's outcome: an unchanged save
    /// does not write the file, and hand edits are picked up too.
    fn settings_closed(&mut self, status: std::io::Result<ExitStatus>) {
        self.settings_open = false;
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!(%status, "settings window exited with an error"),
            Err(error) => {
                tracing::warn!(error = %ErrorChain(&error), "lost track of the settings window");
            }
        }
        if let Some(config) = load_config(&self.config_path) {
            self.apply(config);
        }
        self.bind_hotkey();
    }

    fn apply(&mut self, new: Config) {
        let plan = ReloadPlan::between(&self.config, &new);
        if plan.is_noop() {
            tracing::debug!("settings unchanged");
            return;
        }
        tracing::info!(?plan, config = ?new, "applying new settings");
        self.config = new;
        if plan.rebind_hotkey {
            // Registered by the caller once the settings window is gone.
            tracing::info!(hotkey = %self.config.hotkey, "save hotkey changed");
        }
        if plan.restart_pipeline {
            self.stop_pipeline();
            self.ring.replace(RingSize::for_config(&self.config));
            if let Err(error) = self.start_pipeline() {
                let message = ErrorChain(error.as_ref()).to_string();
                tracing::error!(error = message, "cannot restart capture");
                report_in_background(rfd::MessageLevel::Error, message);
            }
        } else if plan.replace_ring {
            self.ring.replace(RingSize::for_config(&self.config));
        }
        if plan.save_sound_changed && self.config.save_sound {
            sound::warm_up();
        }
        self.update_tooltip();
    }

    /// Registers the configured hotkey. On failure the previous one stays in
    /// effect, and it is retried every [`HOTKEY_RETRY`] (the program holding
    /// it may exit); only the first failure is logged as an error.
    fn bind_hotkey(&mut self) {
        let retrying = self.hotkey_retry_at.is_some();
        match self.hotkey.bind(self.config.hotkey) {
            Ok(()) => self.hotkey_retry_at = None,
            Err(error) => {
                let error = ErrorChain(error.as_ref()).to_string();
                if retrying {
                    tracing::debug!(error, "save hotkey still unavailable");
                } else {
                    tracing::error!(error, "save hotkey unavailable; retrying");
                }
                self.hotkey_retry_at = Instant::now().checked_add(HOTKEY_RETRY);
            }
        }
        self.update_tooltip();
    }

    fn update_tooltip(&self) {
        self.tray.set_tooltip(&tooltip(&Status {
            capture: self.capture_state.clone(),
            last_save: self.last_save.clone(),
            requested_hotkey: self.config.hotkey,
            hotkey_registered: self.hotkey.current() == Some(self.config.hotkey),
            clip_seconds: self.config.clip_seconds,
        }));
    }
}

fn open_log_folder() {
    let Some(dir) = crate::logging::log_dir() else {
        tracing::warn!("no log folder: the per-user local data folder is unknown");
        return;
    };
    if let Err(error) = Command::new("explorer.exe").arg(&dir).spawn() {
        tracing::warn!(error = %ErrorChain(&error), dir = %dir.display(), "cannot open the log folder");
    }
}
