//! The settings being edited, as typed values, and the rules that turn them
//! back into a [`Config`]: normalization, per-field error messages, and the
//! save decision. Nothing here touches egui.

use std::path::PathBuf;

use blink_config::{
    AUDIO_BITRATES_KBPS, CLIP_SECONDS_RANGE, Config, FPS_RANGE, Hotkey, Key, Resolution,
    STRETCH_TO_DIMENSION_RANGE, VIDEO_BITRATE_KBPS_RANGE, ValidationError,
};

/// Frame rates offered in the frame-rate menu.
const FPS_PRESETS: [u32; 5] = [30, 60, 120, 144, 240];

/// The stretch size shown while stretching is off.
const DEFAULT_STRETCH: Resolution = Resolution {
    width: 1920,
    height: 1080,
};

/// The editable copy of a [`Config`], shaped for the widgets that edit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    /// Edited as text; trimmed when converted.
    pub(crate) output_dir: String,
    pub(crate) clip_seconds: u32,
    pub(crate) hotkey: Hotkey,
    pub(crate) fps: u32,
    pub(crate) video_bitrate_kbps: u32,
    pub(crate) audio_enabled: bool,
    pub(crate) audio_bitrate_kbps: u32,
    pub(crate) stretch_enabled: bool,
    /// Kept while stretching is off, so toggling it back restores the size.
    pub(crate) stretch: Resolution,
}

impl Draft {
    pub(crate) fn from_config(config: &Config) -> Self {
        Self {
            output_dir: config.output_dir.display().to_string(),
            clip_seconds: config.clip_seconds,
            hotkey: config.hotkey,
            fps: config.fps,
            video_bitrate_kbps: config.video_bitrate_kbps,
            audio_enabled: config.audio_enabled,
            audio_bitrate_kbps: audio_bitrate_to_save(
                config.audio_enabled,
                config.audio_bitrate_kbps,
            ),
            stretch_enabled: config.stretch_to.is_some(),
            stretch: config.stretch_to.unwrap_or(DEFAULT_STRETCH),
        }
    }

    /// The config this draft would save. It may still be invalid; see
    /// [`Errors::of`].
    pub(crate) fn to_config(&self) -> Config {
        Config {
            output_dir: PathBuf::from(self.output_dir.trim()),
            clip_seconds: self.clip_seconds,
            hotkey: self.hotkey,
            fps: self.fps,
            video_bitrate_kbps: self.video_bitrate_kbps,
            audio_enabled: self.audio_enabled,
            audio_bitrate_kbps: audio_bitrate_to_save(self.audio_enabled, self.audio_bitrate_kbps),
            stretch_to: self.stretch_enabled.then_some(self.stretch),
        }
    }
}

/// While audio is off its bitrate menu is disabled, so an unusable value
/// there falls back to the default instead of blocking Save on a field the
/// user cannot edit.
fn audio_bitrate_to_save(audio_enabled: bool, kbps: u32) -> u32 {
    if audio_enabled || AUDIO_BITRATES_KBPS.contains(&kbps) {
        kbps
    } else {
        Config::default().audio_bitrate_kbps
    }
}

/// The frame rates the menu offers: the presets plus `current`, ascending,
/// so a valid but unusual value from the file stays selectable. An invalid
/// value is left out; the menu still shows it as the selection, with its
/// error, until the user picks another.
pub(crate) fn fps_choices(current: u32) -> Vec<u32> {
    let mut options = FPS_PRESETS.to_vec();
    if FPS_RANGE.contains(&current) && !options.contains(&current) {
        options.push(current);
        options.sort_unstable();
    }
    options
}

/// Per-field error messages for a config. `None` means the field is valid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Errors {
    pub(crate) output_dir: Option<String>,
    pub(crate) clip_seconds: Option<String>,
    pub(crate) hotkey: Option<String>,
    pub(crate) fps: Option<String>,
    pub(crate) video_bitrate: Option<String>,
    pub(crate) audio_bitrate: Option<String>,
    pub(crate) stretch_to: Option<String>,
    /// Errors that belong to no single field, for the status line.
    pub(crate) other: Vec<String>,
}

impl Errors {
    /// Everything that keeps `config` from being saved.
    pub(crate) fn of(config: &Config) -> Self {
        let mut out = Self::default();
        for error in config.errors() {
            let (slot, message) = match &error {
                ValidationError::RelativeOutputDir(dir) if dir.as_os_str().is_empty() => {
                    (&mut out.output_dir, "Choose a folder.".to_owned())
                }
                ValidationError::RelativeOutputDir(_) => {
                    (&mut out.output_dir, "Use a full path.".to_owned())
                }
                ValidationError::ClipSeconds(_) => (
                    &mut out.clip_seconds,
                    range_message(&CLIP_SECONDS_RANGE, "seconds"),
                ),
                ValidationError::Hotkey(hotkey) if hotkey.key == Key::F12 => (
                    &mut out.hotkey,
                    "F12 on its own is reserved for debuggers. Add a modifier.".to_owned(),
                ),
                ValidationError::Hotkey(_) => (
                    &mut out.hotkey,
                    "Add Ctrl, Alt, or Win so normal typing keeps working.".to_owned(),
                ),
                ValidationError::Fps(_) => (&mut out.fps, range_message(&FPS_RANGE, "fps")),
                ValidationError::VideoBitrate(_) => (
                    &mut out.video_bitrate,
                    range_message(&VIDEO_BITRATE_KBPS_RANGE, "kbit/s"),
                ),
                ValidationError::AudioBitrate(_) => (
                    &mut out.audio_bitrate,
                    "Choose one of the listed bitrates.".to_owned(),
                ),
                ValidationError::StretchTo(_) => (
                    &mut out.stretch_to,
                    format!(
                        "Width and height must each be {} to {} pixels.",
                        STRETCH_TO_DIMENSION_RANGE.start(),
                        STRETCH_TO_DIMENSION_RANGE.end()
                    ),
                ),
                other => {
                    out.other.push(other.to_string());
                    continue;
                }
            };
            slot.get_or_insert(message);
        }
        out
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.output_dir.is_none()
            && self.clip_seconds.is_none()
            && self.hotkey.is_none()
            && self.fps.is_none()
            && self.video_bitrate.is_none()
            && self.audio_bitrate.is_none()
            && self.stretch_to.is_none()
            && self.other.is_empty()
    }
}

fn range_message(range: &std::ops::RangeInclusive<u32>, unit: &str) -> String {
    format!("Must be {} to {} {unit}.", range.start(), range.end())
}

/// What pressing Save should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SaveDecision {
    /// The draft has errors; nothing is written.
    Invalid,
    /// The draft matches the file as opened; close without rewriting it.
    Unchanged,
    /// Write this config.
    Write(Config),
}

/// Decides what Save does for `draft`, given the config as it was opened.
pub(crate) fn decide(draft: &Draft, loaded: &Config) -> SaveDecision {
    let config = draft.to_config();
    if !Errors::of(&config).is_empty() {
        SaveDecision::Invalid
    } else if config == *loaded {
        SaveDecision::Unchanged
    } else {
        SaveDecision::Write(config)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn clips_dir() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\clips" } else { "/clips" })
    }

    /// A valid config with every field away from its default.
    pub(crate) fn sample() -> Config {
        Config {
            output_dir: clips_dir(),
            clip_seconds: 45,
            hotkey: "Ctrl+Shift+F9".parse().unwrap(),
            fps: 144,
            video_bitrate_kbps: 50_000,
            audio_enabled: false,
            audio_bitrate_kbps: 192,
            stretch_to: Some("1280x960".parse().unwrap()),
        }
    }

    #[test]
    fn draft_shows_every_config_field() {
        assert_eq!(
            Draft::from_config(&sample()),
            Draft {
                output_dir: clips_dir().display().to_string(),
                clip_seconds: 45,
                hotkey: "Ctrl+Shift+F9".parse().unwrap(),
                fps: 144,
                video_bitrate_kbps: 50_000,
                audio_enabled: false,
                audio_bitrate_kbps: 192,
                stretch_enabled: true,
                stretch: Resolution {
                    width: 1280,
                    height: 960
                },
            }
        );
    }

    #[test]
    fn config_round_trips_through_the_draft() {
        let no_stretch = Config {
            stretch_to: None,
            audio_enabled: true,
            ..sample()
        };
        let odd_fps = Config {
            fps: 75,
            ..sample()
        };
        for config in [sample(), no_stretch, odd_fps] {
            assert_eq!(Draft::from_config(&config).to_config(), config);
        }
    }

    #[test]
    fn stretch_off_keeps_a_default_size_and_saves_none() {
        let config = Config {
            stretch_to: None,
            ..sample()
        };
        let mut draft = Draft::from_config(&config);
        assert!(!draft.stretch_enabled);
        assert_eq!(draft.stretch, DEFAULT_STRETCH);

        draft.stretch.width = 0;
        assert_eq!(draft.to_config().stretch_to, None);
        assert!(Errors::of(&draft.to_config()).is_empty());
    }

    #[test]
    fn output_dir_is_trimmed() {
        let draft = Draft {
            output_dir: format!("  {}  ", clips_dir().display()),
            ..Draft::from_config(&sample())
        };
        assert_eq!(draft.to_config().output_dir, clips_dir());
    }

    #[test]
    fn valid_unusual_frame_rates_join_the_choices_in_order() {
        assert_eq!(fps_choices(75), [30, 60, 75, 120, 144, 240]);
        assert_eq!(fps_choices(144), FPS_PRESETS);
        for invalid in [0, 241, 1_000] {
            assert_eq!(fps_choices(invalid), FPS_PRESETS, "{invalid}");
        }
    }

    #[test]
    fn disabled_audio_opens_with_the_bitrate_it_would_save() {
        for kbps in [0, 100, 320] {
            let config = Config {
                audio_enabled: false,
                audio_bitrate_kbps: kbps,
                ..sample()
            };
            assert_eq!(Draft::from_config(&config).audio_bitrate_kbps, 160);
        }
        let enabled = Config {
            audio_enabled: true,
            audio_bitrate_kbps: 320,
            ..sample()
        };
        assert_eq!(Draft::from_config(&enabled).audio_bitrate_kbps, 320);
    }

    /// The fields that have an error, by name, with their messages.
    fn failing(errors: &Errors) -> Vec<(&'static str, &str)> {
        let mut out: Vec<_> = [
            ("output_dir", &errors.output_dir),
            ("clip_seconds", &errors.clip_seconds),
            ("hotkey", &errors.hotkey),
            ("fps", &errors.fps),
            ("video_bitrate", &errors.video_bitrate),
            ("audio_bitrate", &errors.audio_bitrate),
            ("stretch_to", &errors.stretch_to),
        ]
        .into_iter()
        .filter_map(|(name, message)| Some((name, message.as_deref()?)))
        .collect();
        out.extend(
            errors
                .other
                .iter()
                .map(|message| ("other", message.as_str())),
        );
        out
    }

    fn errors_for(draft: &Draft) -> Errors {
        Errors::of(&draft.to_config())
    }

    #[test]
    fn each_invalid_field_maps_to_its_own_error() {
        type BreakField = fn(&mut Draft);
        let valid = Draft::from_config(&sample());
        let cases: [(BreakField, &str); 10] = [
            (|d| d.output_dir = String::new(), "output_dir"),
            (|d| d.output_dir = "clips".to_owned(), "output_dir"),
            (|d| d.clip_seconds = 4, "clip_seconds"),
            (|d| d.hotkey = "A".parse().unwrap(), "hotkey"),
            (|d| d.hotkey = "F12".parse().unwrap(), "hotkey"),
            (|d| d.fps = 241, "fps"),
            (|d| d.video_bitrate_kbps = 999, "video_bitrate"),
            (
                |d| {
                    d.audio_enabled = true;
                    d.audio_bitrate_kbps = 100;
                },
                "audio_bitrate",
            ),
            (|d| d.stretch.width = 0, "stretch_to"),
            (|d| d.stretch.height = 16_385, "stretch_to"),
        ];
        for (break_field, field) in cases {
            let mut draft = valid.clone();
            break_field(&mut draft);
            let names: Vec<_> = failing(&errors_for(&draft))
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            assert_eq!(names, [field], "{draft:?}");
        }
    }

    #[test]
    fn error_messages() {
        type BreakField = fn(&mut Draft);
        let valid = Draft::from_config(&sample());
        let cases: [(BreakField, &str); 8] = [
            (|d| d.output_dir = "   ".to_owned(), "Choose a folder."),
            (|d| d.output_dir = "clips".to_owned(), "Use a full path."),
            (|d| d.clip_seconds = 601, "Must be 5 to 600 seconds."),
            (
                |d| d.hotkey = "Shift+A".parse().unwrap(),
                "Add Ctrl, Alt, or Win so normal typing keeps working.",
            ),
            (
                |d| d.hotkey = "F12".parse().unwrap(),
                "F12 on its own is reserved for debuggers. Add a modifier.",
            ),
            (|d| d.fps = 0, "Must be 1 to 240 fps."),
            (
                |d| d.video_bitrate_kbps = 200_001,
                "Must be 1000 to 200000 kbit/s.",
            ),
            (
                |d| d.stretch.height = 0,
                "Width and height must each be 1 to 16384 pixels.",
            ),
        ];
        for (break_field, message) in cases {
            let mut draft = valid.clone();
            break_field(&mut draft);
            let messages: Vec<_> = failing(&errors_for(&draft))
                .into_iter()
                .map(|(_, message)| message.to_owned())
                .collect();
            assert_eq!(messages, [message], "{draft:?}");
        }
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let draft = Draft {
            output_dir: String::new(),
            clip_seconds: 0,
            hotkey: "A".parse().unwrap(),
            fps: 0,
            video_bitrate_kbps: 0,
            audio_enabled: true,
            audio_bitrate_kbps: 1,
            stretch_enabled: true,
            stretch: Resolution {
                width: 0,
                height: 0,
            },
        };
        let names: Vec<_> = failing(&errors_for(&draft))
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            [
                "output_dir",
                "clip_seconds",
                "hotkey",
                "fps",
                "video_bitrate",
                "audio_bitrate",
                "stretch_to"
            ]
        );
    }

    #[test]
    fn out_of_range_file_values_survive_into_the_draft() {
        let config = Config {
            clip_seconds: 1,
            fps: 1_000,
            video_bitrate_kbps: u32::MAX,
            audio_enabled: true,
            audio_bitrate_kbps: 320,
            stretch_to: Some(Resolution {
                width: 0,
                height: 70_000,
            }),
            ..sample()
        };
        let draft = Draft::from_config(&config);
        assert_eq!(draft.to_config(), config);
        let names: Vec<_> = failing(&errors_for(&draft))
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            [
                "clip_seconds",
                "fps",
                "video_bitrate",
                "audio_bitrate",
                "stretch_to"
            ]
        );
    }

    #[test]
    fn disabled_audio_never_blocks_save() {
        let off = Draft {
            audio_enabled: false,
            ..Draft::from_config(&sample())
        };
        for kbps in [0, 100, 320] {
            let draft = Draft {
                audio_bitrate_kbps: kbps,
                ..off.clone()
            };
            assert_eq!(draft.to_config().audio_bitrate_kbps, 160, "{kbps}");
            assert!(errors_for(&draft).is_empty(), "{kbps}");
        }
        let kept = Draft {
            audio_bitrate_kbps: 96,
            ..off
        };
        assert_eq!(kept.to_config().audio_bitrate_kbps, 96);
    }

    #[test]
    fn enabled_audio_keeps_an_invalid_bitrate_invalid() {
        let draft = Draft {
            audio_enabled: true,
            audio_bitrate_kbps: 100,
            ..Draft::from_config(&sample())
        };
        assert_eq!(draft.to_config().audio_bitrate_kbps, 100);
        assert_eq!(
            errors_for(&draft).audio_bitrate.as_deref(),
            Some("Choose one of the listed bitrates.")
        );
    }

    #[test]
    fn errors_is_empty_only_without_messages() {
        assert!(Errors::default().is_empty());
        let errors = Errors {
            other: vec!["x".to_owned()],
            ..Errors::default()
        };
        assert!(!errors.is_empty());
    }

    #[test]
    fn save_decision() {
        let loaded = sample();
        let unchanged = Draft::from_config(&loaded);
        assert_eq!(decide(&unchanged, &loaded), SaveDecision::Unchanged);

        let padded = Draft {
            output_dir: format!(" {} ", unchanged.output_dir),
            ..unchanged
        };
        assert_eq!(decide(&padded, &loaded), SaveDecision::Unchanged);

        let edited = Draft {
            fps: 60,
            ..unchanged.clone()
        };
        assert_eq!(
            decide(&edited, &loaded),
            SaveDecision::Write(Config {
                fps: 60,
                ..sample()
            })
        );

        let invalid = Draft {
            clip_seconds: 1,
            ..unchanged
        };
        assert_eq!(decide(&invalid, &loaded), SaveDecision::Invalid);
    }

    #[test]
    fn an_invalid_file_opened_unchanged_still_cannot_be_saved() {
        let loaded = Config {
            clip_seconds: 1,
            ..sample()
        };
        let draft = Draft::from_config(&loaded);
        assert_eq!(decide(&draft, &loaded), SaveDecision::Invalid);
    }

    #[test]
    fn normalizing_disabled_audio_counts_as_a_change() {
        let loaded = Config {
            audio_bitrate_kbps: 100,
            ..sample()
        };
        let draft = Draft::from_config(&loaded);
        assert_eq!(
            decide(&draft, &loaded),
            SaveDecision::Write(Config {
                audio_bitrate_kbps: 160,
                ..sample()
            })
        );
    }
}
