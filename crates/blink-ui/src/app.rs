//! The settings window: an [`eframe::App`] that edits a [`Draft`] in place
//! and validates it every frame.

use blink_sys::ErrorChain;
use std::cell::Cell;
use std::path::PathBuf;

use blink_config::{
    AUDIO_BITRATES_KBPS, CLIP_SECONDS_RANGE, Config, STRETCH_TO_DIMENSION_RANGE,
    VIDEO_BITRATE_KBPS_RANGE,
};
use eframe::egui;

use crate::Outcome;
use crate::draft::{Draft, Errors, SaveDecision, decide, fps_choices};
use crate::keys::{Recorder, Step};

/// Whether this platform has a native folder picker for Browse.
const CAN_BROWSE: bool = cfg!(any(windows, target_os = "macos"));

/// The window's width, in points. Its height follows the form.
const WINDOW_WIDTH: f32 = 420.0;

/// The size the window opens at, before the first frame measures the form.
pub(crate) const INITIAL_SIZE: [f32; 2] = [WINDOW_WIDTH, 270.0];

/// Something the window asks of its host after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    /// Show the folder picker for the output folder.
    Browse,
}

pub(crate) struct SettingsApp<'a> {
    config_path: PathBuf,
    /// The config as opened, so an unchanged Save skips the write.
    loaded: Config,
    draft: Draft,
    fps_options: Vec<u32>,
    recorder: Recorder,
    /// A problem with the draft that validation cannot see, such as a failed
    /// write, and the draft it was for. Shown until the draft changes.
    status: Option<(Draft, String)>,
    outcome: &'a Cell<Outcome>,
}

impl<'a> SettingsApp<'a> {
    pub(crate) fn new(config_path: PathBuf, loaded: Config, outcome: &'a Cell<Outcome>) -> Self {
        Self {
            config_path,
            draft: Draft::from_config(&loaded),
            fps_options: fps_choices(loaded.fps),
            loaded,
            recorder: Recorder::default(),
            status: None,
            outcome,
        }
    }

    /// Draws one frame.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        self.record_keys(ui.ctx());
        let errors = Errors::of(&self.draft.to_config());
        let mut request = None;

        let actions = egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            let status = self
                .status
                .as_ref()
                .filter(|(draft, _)| *draft == self.draft)
                .map(|(_, message)| message);
            for message in errors.other.iter().chain(status) {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
            let buttons = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
            let layout = egui::Layout::right_to_left(egui::Align::Center);
            ui.allocate_ui_with_layout(buttons, layout, |ui| {
                let save = ui.add_enabled(errors.is_empty(), egui::Button::new("Save"));
                if save.clicked() {
                    self.save(ui.ctx());
                }
                if ui.button("Cancel").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.add_space(6.0);
        });

        let frame = egui::Frame::central_panel(ui.style());
        let margin = frame.inner_margin.bottomf();
        let form = egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    if self.fields(ui, &errors) {
                        request = Some(Request::Browse);
                    }
                })
                .response
                .rect
        });

        fit_height(
            ui.ctx(),
            form.inner.bottom() + margin + actions.response.rect.height(),
        );
        request
    }

    /// Lays out the form rows. Returns whether Browse was clicked.
    fn fields(&mut self, ui: &mut egui::Ui, errors: &Errors) -> bool {
        let draft = &mut self.draft;
        let mut browse = false;

        row(ui, "Output folder", errors.output_dir.as_deref(), |ui| {
            let layout = egui::Layout::right_to_left(egui::Align::Center);
            ui.with_layout(layout, |ui| {
                browse = CAN_BROWSE && ui.button("Browse…").clicked();
                ui.add(
                    egui::TextEdit::singleline(&mut draft.output_dir)
                        .desired_width(ui.available_width()),
                );
            });
        });

        row(ui, "Clip length", errors.clip_seconds.as_deref(), |ui| {
            ui.add(number(&mut draft.clip_seconds, &CLIP_SECONDS_RANGE, 1.0).suffix(" s"));
        });

        let problem = self.recorder.problem().map(crate::keys::Problem::message);
        row(ui, "Hotkey", problem.or(errors.hotkey.as_deref()), |ui| {
            if self.recorder.is_recording() {
                ui.strong("Press keys… (Esc cancels)");
                if ui.button("Stop").clicked() {
                    self.recorder.stop();
                }
            } else {
                ui.strong(draft.hotkey.to_string());
                if ui.button("Record").clicked() {
                    ui.memory_mut(egui::Memory::stop_text_input);
                    self.recorder.start();
                }
            }
        });

        row(ui, "Frame rate", errors.fps.as_deref(), |ui| {
            menu(ui, "fps", &mut draft.fps, &self.fps_options, "fps");
        });

        row(ui, "Video bitrate", errors.video_bitrate.as_deref(), |ui| {
            let range = &VIDEO_BITRATE_KBPS_RANGE;
            ui.add(number(&mut draft.video_bitrate_kbps, range, 100.0).suffix(" kbit/s"));
        });

        row(ui, "Audio", errors.audio_bitrate.as_deref(), |ui| {
            ui.checkbox(&mut draft.audio_enabled, "Record system audio");
            ui.add_enabled_ui(draft.audio_enabled, |ui| {
                let bitrate = &mut draft.audio_bitrate_kbps;
                menu(ui, "audio_bitrate", bitrate, &AUDIO_BITRATES_KBPS, "kbit/s");
            });
        });

        row(ui, "Stretch to", errors.stretch_to.as_deref(), |ui| {
            ui.checkbox(&mut draft.stretch_enabled, "On");
            ui.add_enabled_ui(draft.stretch_enabled, |ui| {
                let range = &STRETCH_TO_DIMENSION_RANGE;
                ui.add(number(&mut draft.stretch.width, range, 1.0));
                ui.label("×");
                ui.add(number(&mut draft.stretch.height, range, 1.0));
            });
        });

        browse
    }

    /// Routes this frame's key events to the recorder while it records, so
    /// widgets never see them.
    fn record_keys(&mut self, ctx: &egui::Context) {
        if !self.recorder.is_recording() {
            return;
        }
        let draft = &mut self.draft;
        let recorder = &mut self.recorder;
        ctx.input_mut(|input| {
            input.events.retain(|event| match recorder.on_event(event) {
                Step::Pass => true,
                Step::Consumed => false,
                Step::Recorded(hotkey) => {
                    draft.hotkey = hotkey;
                    false
                }
            });
        });
    }

    fn save(&mut self, ctx: &egui::Context) {
        match decide(&self.draft, &self.loaded) {
            SaveDecision::Invalid => {}
            SaveDecision::Unchanged => {
                tracing::info!("settings unchanged; not rewriting the config");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            SaveDecision::Write(config) => match config.save_atomic(&self.config_path) {
                Ok(()) => {
                    tracing::info!(path = %self.config_path.display(), "saved settings");
                    self.outcome.set(Outcome::Saved);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(err) => {
                    let message = ErrorChain(&err).to_string();
                    tracing::error!(error = %message, "failed to save settings");
                    self.status = Some((self.draft.clone(), format!("Could not save: {message}")));
                }
            },
        }
    }

    /// Uses a folder from the picker. The output folder is edited as text, so
    /// a path that is not valid Unicode is refused rather than mangled.
    fn pick(&mut self, folder: PathBuf) {
        match folder.into_os_string().into_string() {
            Ok(folder) => self.draft.output_dir = folder,
            Err(folder) => {
                tracing::warn!(folder = ?folder, "picked folder is not valid Unicode");
                self.status = Some((
                    self.draft.clone(),
                    "That folder's name cannot be used. Choose another folder.".to_owned(),
                ));
            }
        }
    }

    /// Shows the native folder picker, which blocks until it closes.
    #[cfg(any(windows, target_os = "macos"))]
    fn browse(&mut self, frame: &eframe::Frame) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Choose the clip folder")
            .set_parent(frame);
        let current = PathBuf::from(self.draft.output_dir.trim());
        if current.is_dir() {
            dialog = dialog.set_directory(current);
        }
        if let Some(folder) = dialog.pick_folder() {
            self.pick(folder);
        }
    }

    /// No native picker here; the Browse button is hidden.
    #[cfg(not(any(windows, target_os = "macos")))]
    const fn browse(&mut self, _frame: &eframe::Frame) {}
}

impl eframe::App for SettingsApp<'_> {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.show(ui) == Some(Request::Browse) {
            self.browse(frame);
        }
    }
}

/// Resizes the window to `height` points when the form grows or shrinks,
/// such as when an error row appears.
fn fit_height(ctx: &egui::Context, height: f32) {
    let height = height.ceil();
    if (ctx.viewport_rect().height() - height).abs() >= 1.0 {
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            WINDOW_WIDTH,
            height,
        )));
    }
}

/// One labelled grid row, with the field's error on a row of its own below.
fn row(ui: &mut egui::Ui, label: &str, error: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
    ui.label(label);
    ui.horizontal(add);
    ui.end_row();
    if let Some(error) = error {
        ui.label("");
        ui.colored_label(ui.visuals().error_fg_color, error);
        ui.end_row();
    }
}

/// A number field limited to `range` when edited, but showing an out-of-range
/// value from the file as is, so its error is visible.
fn number<'v>(
    value: &'v mut u32,
    range: &std::ops::RangeInclusive<u32>,
    speed: f64,
) -> egui::DragValue<'v> {
    egui::DragValue::new(value)
        .range(range.clone())
        .clamp_existing_to_range(false)
        .speed(speed)
}

/// A drop-down menu over `options`. A current value missing from `options`
/// is still shown as the selection.
fn menu(ui: &mut egui::Ui, id: &str, value: &mut u32, options: &[u32], unit: &str) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(format!("{value} {unit}"))
        .show_ui(ui, |ui| {
            for &option in options {
                ui.selectable_value(value, option, format!("{option} {unit}"));
            }
        });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A unique scratch directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("blink-ui-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn config(&self) -> PathBuf {
            self.0.join("config.toml")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample() -> Config {
        Config {
            output_dir: PathBuf::from(if cfg!(windows) { r"C:\clips" } else { "/clips" }),
            clip_seconds: 45,
            hotkey: "Ctrl+Shift+F9".parse().unwrap(),
            fps: 144,
            video_bitrate_kbps: 50_000,
            audio_enabled: false,
            audio_bitrate_kbps: 192,
            stretch_to: Some("1280x960".parse().unwrap()),
        }
    }

    /// Runs one headless egui frame with `events`, without a window or GPU.
    fn frame(
        ctx: &egui::Context,
        app: &mut SettingsApp<'_>,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            events,
            ..egui::RawInput::default()
        };
        discard_textures(ctx.run_ui(input, |ui| {
            app.show(ui);
        }))
    }

    /// Presses Save during one headless frame.
    fn save(app: &mut SettingsApp<'_>) -> egui::FullOutput {
        let ctx = egui::Context::default();
        discard_textures(ctx.run_ui(egui::RawInput::default(), |ui| app.save(ui.ctx())))
    }

    /// There is no renderer to upload the font atlas to.
    fn discard_textures(mut output: egui::FullOutput) -> egui::FullOutput {
        output.textures_delta.clear();
        output
    }

    fn closed(output: &egui::FullOutput) -> bool {
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|viewport| viewport.commands.contains(&egui::ViewportCommand::Close))
    }

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn recording_through_egui_input_sets_the_hotkey() {
        let dir = TempDir::new("record");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, Vec::new());

        app.recorder.start();
        let ctrl = egui::Modifiers::CTRL;
        frame(
            &ctx,
            &mut app,
            vec![
                press(egui::Key::ControlLeft, ctrl),
                egui::Event::Text("x".to_owned()),
                press(egui::Key::Minus, ctrl),
            ],
        );
        assert!(app.recorder.is_recording());
        assert_eq!(
            app.recorder.problem(),
            Some(crate::keys::Problem::Unsupported)
        );

        frame(&ctx, &mut app, vec![press(egui::Key::F7, ctrl)]);
        assert!(!app.recorder.is_recording());
        assert_eq!(app.draft.hotkey, "Ctrl+F7".parse().unwrap());
    }

    #[test]
    fn escape_while_recording_keeps_the_old_hotkey() {
        let dir = TempDir::new("escape");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        let ctx = egui::Context::default();
        app.recorder.start();
        frame(
            &ctx,
            &mut app,
            vec![press(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(!app.recorder.is_recording());
        assert_eq!(app.draft.hotkey, sample().hotkey);
    }

    #[test]
    fn save_writes_edits_and_reports_saved() {
        let dir = TempDir::new("save");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        app.draft.fps = 60;
        app.draft.stretch_enabled = false;
        let output = save(&mut app);
        assert!(closed(&output));

        assert_eq!(outcome.get(), Outcome::Saved);
        assert_eq!(
            Config::load(&dir.config()).unwrap(),
            Config {
                fps: 60,
                stretch_to: None,
                ..sample()
            }
        );
    }

    #[test]
    fn unchanged_save_closes_without_writing() {
        let dir = TempDir::new("unchanged");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        let output = save(&mut app);
        assert!(closed(&output));
        assert_eq!(outcome.get(), Outcome::Cancelled);
        assert!(!dir.config().exists());
    }

    #[test]
    fn invalid_settings_are_not_saved() {
        let dir = TempDir::new("invalid");
        let outcome = Cell::new(Outcome::Cancelled);
        let loaded = Config {
            clip_seconds: 1,
            ..sample()
        };
        let mut app = SettingsApp::new(dir.config(), loaded, &outcome);
        assert_eq!(app.draft.clip_seconds, 1);
        let output = save(&mut app);
        assert!(!closed(&output));
        assert_eq!(outcome.get(), Outcome::Cancelled);
        assert!(!dir.config().exists());
    }

    #[test]
    fn menus_offer_only_valid_values() {
        let dir = TempDir::new("menus");
        let outcome = Cell::new(Outcome::Cancelled);
        let loaded = Config {
            fps: 1_000,
            ..sample()
        };
        let app = SettingsApp::new(dir.config(), loaded, &outcome);
        assert_eq!(app.fps_options, [30, 60, 120, 144, 240]);
        assert_eq!(app.draft.fps, 1_000);
    }

    #[test]
    fn picked_folder_replaces_the_output_dir() {
        let dir = TempDir::new("pick");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        app.pick(dir.0.clone());
        assert_eq!(app.draft.output_dir, dir.0.display().to_string());
        assert!(app.status.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_folder_is_refused_not_mangled() {
        use std::os::unix::ffi::OsStringExt;

        let dir = TempDir::new("pick-bad");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        let before = app.draft.output_dir.clone();
        app.pick(PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'/', 0xff,
        ])));
        assert_eq!(app.draft.output_dir, before);
        let (draft, message) = app.status.as_ref().unwrap();
        assert_eq!(*draft, app.draft);
        assert!(message.contains("cannot be used"), "{message}");
    }

    #[test]
    fn save_failure_keeps_the_window_open_until_the_draft_changes() {
        let dir = TempDir::new("write-fails");
        fs::create_dir_all(dir.config()).unwrap();
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        app.draft.fps = 30;
        let output = save(&mut app);
        assert!(!closed(&output));
        assert_eq!(outcome.get(), Outcome::Cancelled);
        let (draft, message) = app.status.as_ref().unwrap();
        assert_eq!(*draft, app.draft);
        assert!(
            message.starts_with("Could not save: failed to write config"),
            "{message}"
        );
    }

    /// Runs headless frames like a window would: each requested inner size
    /// becomes the next frame's screen size. Returns the settled size and
    /// the last frame's output.
    fn settle(ctx: &egui::Context, app: &mut SettingsApp<'_>) -> (egui::Vec2, egui::FullOutput) {
        let mut size = egui::Vec2::from(INITIAL_SIZE);
        let mut output = None;
        for _ in 0..8 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..egui::RawInput::default()
            };
            let frame = discard_textures(ctx.run_ui(input, |ui| {
                app.show(ui);
            }));
            let resize = frame
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .and_then(|viewport| {
                    viewport.commands.iter().find_map(|command| match command {
                        egui::ViewportCommand::InnerSize(size) => Some(*size),
                        _ => None,
                    })
                });
            if let Some(resize) = resize {
                size = resize;
            }
            output = Some(frame);
        }
        (size, output.unwrap())
    }

    #[test]
    fn frames_settle_without_requesting_repaints() {
        let dir = TempDir::new("render");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        let ctx = egui::Context::default();
        let (_, output) = settle(&ctx, &mut app);
        assert!(output.shapes.len() > 10, "{}", output.shapes.len());
        assert!(!closed(&output));
        let viewport = output.viewport_output.get(&egui::ViewportId::ROOT).unwrap();
        assert_eq!(viewport.repaint_delay, std::time::Duration::MAX);
        assert!(viewport.commands.is_empty(), "{:?}", viewport.commands);
    }

    #[test]
    fn window_height_follows_the_form() {
        let dir = TempDir::new("fit");
        let outcome = Cell::new(Outcome::Cancelled);
        let mut app = SettingsApp::new(dir.config(), sample(), &outcome);
        let ctx = egui::Context::default();
        let (valid, _) = settle(&ctx, &mut app);
        assert!((valid.x - WINDOW_WIDTH).abs() < f32::EPSILON);
        assert!((150.0..400.0).contains(&valid.y), "{valid:?}");

        app.draft.clip_seconds = 1;
        let (invalid, _) = settle(&ctx, &mut app);
        assert!(invalid.y > valid.y, "{invalid:?} vs {valid:?}");
    }
}
