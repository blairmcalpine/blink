use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Hotkey, Key, Modifiers, Resolution, default_output_dir};

/// Allowed clip lengths, in seconds.
pub const CLIP_SECONDS_RANGE: RangeInclusive<u32> = 5..=600;
/// Allowed capture frame rates.
pub const FPS_RANGE: RangeInclusive<u32> = 1..=240;
/// Allowed video bitrates, in kbit/s.
pub const VIDEO_BITRATE_KBPS_RANGE: RangeInclusive<u32> = 1_000..=200_000;
/// Allowed audio bitrates, in kbit/s: the rates the Media Foundation AAC
/// encoder supports.
pub const AUDIO_BITRATES_KBPS: [u32; 4] = [96, 128, 160, 192];
/// Allowed width and height of [`Config::stretch_to`], in pixels.
pub const STRETCH_TO_DIMENSION_RANGE: RangeInclusive<u32> = 1..=16_384;

/// User settings, stored as TOML.
///
/// Missing keys take their [`Default`] values; unknown keys are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Absolute folder that saved clips are written to. Defaults to
    /// [`default_output_dir`], or an empty path (which fails validation)
    /// when that is unknown.
    pub output_dir: PathBuf,
    /// Length of a saved clip, in seconds. See [`CLIP_SECONDS_RANGE`].
    pub clip_seconds: u32,
    /// Global hotkey that saves a clip.
    pub hotkey: Hotkey,
    /// Capture frame rate. See [`FPS_RANGE`].
    pub fps: u32,
    /// Video bitrate in kbit/s. See [`VIDEO_BITRATE_KBPS_RANGE`].
    pub video_bitrate_kbps: u32,
    /// Whether system audio is recorded.
    pub audio_enabled: bool,
    /// Audio bitrate in kbit/s. One of [`AUDIO_BITRATES_KBPS`].
    pub audio_bitrate_kbps: u32,
    /// Display size that players stretch frames to, via the sample aspect
    /// ratio. `None` keeps square pixels. See [`STRETCH_TO_DIMENSION_RANGE`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stretch_to: Option<Resolution>,
}

/// A config value outside its allowed range.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidationError {
    /// `output_dir` is empty or relative.
    #[error("output_dir {} must be an absolute path", .0.display())]
    RelativeOutputDir(PathBuf),
    /// `clip_seconds` is outside [`CLIP_SECONDS_RANGE`].
    #[error("clip_seconds {0} is outside {CLIP_SECONDS_RANGE:?}")]
    ClipSeconds(u32),
    /// `hotkey` would swallow normal typing, or is the reserved bare F12.
    /// Letters, digits, Space, Enter, Tab, Escape, Backspace, and arrows need
    /// Ctrl, Alt, or Win.
    #[error("hotkey {0} needs Ctrl, Alt, or Win so it does not swallow normal input")]
    Hotkey(Hotkey),
    /// `fps` is outside [`FPS_RANGE`].
    #[error("fps {0} is outside {FPS_RANGE:?}")]
    Fps(u32),
    /// `video_bitrate_kbps` is outside [`VIDEO_BITRATE_KBPS_RANGE`].
    #[error("video_bitrate_kbps {0} is outside {VIDEO_BITRATE_KBPS_RANGE:?}")]
    VideoBitrate(u32),
    /// `audio_bitrate_kbps` is not one of [`AUDIO_BITRATES_KBPS`].
    #[error("audio_bitrate_kbps {0} is not one of {AUDIO_BITRATES_KBPS:?}")]
    AudioBitrate(u32),
    /// A `stretch_to` dimension is outside [`STRETCH_TO_DIMENSION_RANGE`].
    #[error("stretch_to {0} has a dimension outside {STRETCH_TO_DIMENSION_RANGE:?}")]
    StretchTo(Resolution),
}

/// Errors produced while loading or saving a [`Config`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The config file exists but could not be read.
    #[error("failed to read config {}", path.display())]
    Read {
        /// Path of the config file.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The config file is not valid UTF-8 TOML for a [`Config`].
    #[error("failed to parse config {}", path.display())]
    Parse {
        /// Path of the config file.
        path: PathBuf,
        /// Underlying TOML error.
        source: toml::de::Error,
    },
    /// The config parsed but has an out-of-range value.
    #[error("invalid config {}", path.display())]
    Invalid {
        /// Path of the config file being loaded or saved.
        path: PathBuf,
        /// The first value that failed validation.
        source: ValidationError,
    },
    /// The save path does not name a file.
    #[error("config path {} does not name a file", path.display())]
    NoFileName {
        /// The rejected path.
        path: PathBuf,
    },
    /// The config could not be serialized to TOML.
    #[error("failed to serialize config")]
    Serialize(#[from] toml::ser::Error),
    /// The config file could not be written.
    #[error("failed to write config {}", path.display())]
    Write {
        /// Path that was being written.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_dir: default_output_dir().unwrap_or_default(),
            clip_seconds: 30,
            hotkey: Hotkey {
                modifiers: Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
                key: Key::F10,
            },
            fps: 60,
            video_bitrate_kbps: 20_000,
            audio_enabled: true,
            audio_bitrate_kbps: 160,
            stretch_to: None,
        }
    }
}

impl Config {
    /// Returns every value that fails validation, in field order. Empty when
    /// the config is valid.
    #[must_use]
    pub fn errors(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        if !self.output_dir.is_absolute() {
            errors.push(ValidationError::RelativeOutputDir(self.output_dir.clone()));
        }
        if !CLIP_SECONDS_RANGE.contains(&self.clip_seconds) {
            errors.push(ValidationError::ClipSeconds(self.clip_seconds));
        }
        if !self.hotkey.is_allowed() {
            errors.push(ValidationError::Hotkey(self.hotkey));
        }
        if !FPS_RANGE.contains(&self.fps) {
            errors.push(ValidationError::Fps(self.fps));
        }
        if !VIDEO_BITRATE_KBPS_RANGE.contains(&self.video_bitrate_kbps) {
            errors.push(ValidationError::VideoBitrate(self.video_bitrate_kbps));
        }
        if !AUDIO_BITRATES_KBPS.contains(&self.audio_bitrate_kbps) {
            errors.push(ValidationError::AudioBitrate(self.audio_bitrate_kbps));
        }
        if let Some(stretch) = self.stretch_to
            && !(STRETCH_TO_DIMENSION_RANGE.contains(&stretch.width)
                && STRETCH_TO_DIMENSION_RANGE.contains(&stretch.height))
        {
            errors.push(ValidationError::StretchTo(stretch));
        }
        errors
    }

    /// Checks every value against its allowed range.
    ///
    /// # Errors
    ///
    /// Returns the first of [`Config::errors`].
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.errors().into_iter().next().map_or(Ok(()), Err)
    }

    /// Loads and validates the config at `path`. A missing file yields the
    /// defaults.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the file cannot be read or parsed, or
    /// fails [`Config::validate`].
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let config = Self::load_unvalidated(path)?;
        config.validate().map_err(|source| ConfigError::Invalid {
            path: path.to_owned(),
            source,
        })?;
        Ok(config)
    }

    /// Loads the config at `path` without validating it, so an editor can
    /// open an out-of-range file and fix it. A missing file yields the
    /// defaults.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Read`] or [`ConfigError::Parse`].
    pub fn load_unvalidated(path: &Path) -> Result<Self, ConfigError> {
        match fs::read(path) {
            Ok(bytes) => toml::from_slice(&bytes).map_err(|source| ConfigError::Parse {
                path: path.to_owned(),
                source,
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Validates the config and writes it to `path` atomically: parent
    /// directories are created, the TOML is written and synced to a uniquely
    /// named temp file in the same directory, and the temp file is renamed
    /// over `path`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when validation, serialization, or any file
    /// operation fails. On failure `path` is left untouched.
    pub fn save_atomic(&self, path: &Path) -> Result<(), ConfigError> {
        let write_error = |source| ConfigError::Write {
            path: path.to_owned(),
            source,
        };
        self.validate().map_err(|source| ConfigError::Invalid {
            path: path.to_owned(),
            source,
        })?;
        let file_name = path.file_name().ok_or_else(|| ConfigError::NoFileName {
            path: path.to_owned(),
        })?;
        let text = toml::to_string_pretty(self)?;
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(write_error)?;
        let (temp, file) = create_temp(dir, file_name).map_err(write_error)?;
        let result = write_synced(file, text.as_bytes())
            .and_then(|()| blink_sys::rename_replacing(&temp, path));
        result.map_err(|source| {
            // Best effort: the original error matters more than a stray temp file.
            let _ = fs::remove_file(&temp);
            write_error(source)
        })
    }
}

/// Creates `.{file_name}.{pid}.{n}.tmp` in `dir`, never reusing a name that
/// another save (in this or a crashed process) may still hold.
fn create_temp(dir: &Path, file_name: &std::ffi::OsStr) -> io::Result<(PathBuf, File)> {
    const ATTEMPTS: u32 = 16;
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    for _ in 0..ATTEMPTS {
        let mut name = OsString::from(".");
        name.push(file_name);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        name.push(format!(".{}.{n}.tmp", std::process::id()));
        let temp = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

fn write_synced(mut file: File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("blink-config-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn entries(&self) -> Vec<OsString> {
            let mut names: Vec<_> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn clips_dir() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\clips" } else { "/clips" })
    }

    fn sample() -> Config {
        Config {
            output_dir: clips_dir(),
            clip_seconds: 45,
            hotkey: "Ctrl+Shift+F9".parse().unwrap(),
            fps: 144,
            video_bitrate_kbps: 50_000,
            audio_enabled: false,
            audio_bitrate_kbps: 192,
            stretch_to: Some("1920x1080".parse().unwrap()),
        }
    }

    fn with_hotkey(hotkey: &str) -> Config {
        Config {
            hotkey: hotkey.parse().unwrap(),
            ..sample()
        }
    }

    #[test]
    fn defaults_match_spec() {
        let config = Config::default();
        assert_eq!(config.clip_seconds, 30);
        assert_eq!(config.hotkey.to_string(), "Alt+F10");
        assert_eq!(config.fps, 60);
        assert_eq!(config.video_bitrate_kbps, 20_000);
        assert!(config.audio_enabled);
        assert_eq!(config.audio_bitrate_kbps, 160);
        assert_eq!(config.stretch_to, None);
        assert_eq!(config.output_dir, default_output_dir().unwrap_or_default());
    }

    #[test]
    fn defaults_validate_apart_from_output_dir() {
        let config = Config {
            output_dir: clips_dir(),
            ..Config::default()
        };
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn validation_bounds() {
        type Case = (fn(&mut Config, u32), u32, u32, fn(u32) -> ValidationError);
        let cases: [Case; 3] = [
            (
                |c, v| c.clip_seconds = v,
                5,
                600,
                ValidationError::ClipSeconds,
            ),
            (|c, v| c.fps = v, 1, 240, ValidationError::Fps),
            (
                |c, v| c.video_bitrate_kbps = v,
                1_000,
                200_000,
                ValidationError::VideoBitrate,
            ),
        ];
        for (set, min, max, error) in cases {
            for (value, ok) in [(min - 1, false), (min, true), (max, true), (max + 1, false)] {
                let mut config = sample();
                set(&mut config, value);
                let expected = if ok { Ok(()) } else { Err(error(value)) };
                assert_eq!(config.validate(), expected, "value {value}");
            }
        }
    }

    #[test]
    fn audio_bitrate_must_be_supported() {
        for kbps in AUDIO_BITRATES_KBPS {
            let config = Config {
                audio_bitrate_kbps: kbps,
                ..sample()
            };
            assert_eq!(config.validate(), Ok(()));
        }
        for kbps in [0, 64, 112, 150, 256, 320] {
            let config = Config {
                audio_bitrate_kbps: kbps,
                ..sample()
            };
            assert_eq!(config.validate(), Err(ValidationError::AudioBitrate(kbps)));
        }
    }

    #[test]
    fn output_dir_must_be_absolute() {
        for dir in ["", "clips", "./clips"] {
            let config = Config {
                output_dir: PathBuf::from(dir),
                ..sample()
            };
            assert_eq!(
                config.validate(),
                Err(ValidationError::RelativeOutputDir(PathBuf::from(dir)))
            );
        }
    }

    #[test]
    fn stretch_to_dimensions_are_bounded() {
        let cases = [
            ((1, 1), true),
            ((16_384, 16_384), true),
            ((0, 1080), false),
            ((1920, 0), false),
            ((16_385, 1080), false),
            ((1920, 70_000), false),
        ];
        for ((width, height), ok) in cases {
            let stretch = Resolution { width, height };
            let config = Config {
                stretch_to: Some(stretch),
                ..sample()
            };
            let expected = if ok {
                Ok(())
            } else {
                Err(ValidationError::StretchTo(stretch))
            };
            assert_eq!(config.validate(), expected, "{stretch}");
        }
    }

    #[test]
    fn hotkey_policy() {
        let cases = [
            ("F9", true),
            ("F24", true),
            ("Insert", true),
            ("Delete", true),
            ("Home", true),
            ("End", true),
            ("PageUp", true),
            ("PageDown", true),
            ("PrintScreen", true),
            ("ScrollLock", true),
            ("Pause", true),
            ("F12", false),
            ("Shift+F12", true),
            ("Ctrl+F12", true),
            ("A", false),
            ("Shift+A", false),
            ("Ctrl+A", true),
            ("Alt+A", true),
            ("Win+A", true),
            ("Ctrl+Shift+A", true),
            ("5", false),
            ("Shift+5", false),
            ("Alt+5", true),
            ("Space", false),
            ("Enter", false),
            ("Tab", false),
            ("Backspace", false),
            ("Escape", false),
            ("Shift+Escape", false),
            ("Ctrl+Escape", true),
            ("Up", false),
            ("Shift+Left", false),
            ("Alt+Right", true),
        ];
        for (hotkey, ok) in cases {
            let config = with_hotkey(hotkey);
            let expected = if ok {
                Ok(())
            } else {
                Err(ValidationError::Hotkey(config.hotkey))
            };
            assert_eq!(config.validate(), expected, "{hotkey}");
        }
    }

    #[test]
    fn errors_lists_every_problem_in_field_order() {
        let config = Config {
            output_dir: PathBuf::from("clips"),
            fps: 0,
            audio_bitrate_kbps: 100,
            ..with_hotkey("A")
        };
        assert_eq!(
            config.errors(),
            [
                ValidationError::RelativeOutputDir(PathBuf::from("clips")),
                ValidationError::Hotkey(config.hotkey),
                ValidationError::Fps(0),
                ValidationError::AudioBitrate(100),
            ]
        );
        assert_eq!(sample().errors(), []);
    }

    #[cfg(not(windows))]
    #[test]
    fn serialized_format_is_stable() {
        let expected = r#"output_dir = "/clips"
clip_seconds = 45
hotkey = "Ctrl+Shift+F9"
fps = 144
video_bitrate_kbps = 50000
audio_enabled = false
audio_bitrate_kbps = 192
stretch_to = "1920x1080"
"#;
        assert_eq!(toml::to_string_pretty(&sample()).unwrap(), expected);
        assert_eq!(toml::from_str::<Config>(expected).unwrap(), sample());
    }

    #[test]
    fn none_stretch_is_omitted() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!text.contains("stretch_to"), "{text}");
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let config: Config = toml::from_str("fps = 120\nhotkey = \"ctrl+f9\"\n").unwrap();
        assert_eq!(config.fps, 120);
        assert_eq!(config.hotkey.to_string(), "Ctrl+F9");
        assert_eq!(config.clip_seconds, 30);
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        assert!(toml::from_str::<Config>("bogus = 1\n").is_err());
        assert!(toml::from_str::<Config>("hotkey = \"Ctrl+Nope\"\n").is_err());
        assert!(toml::from_str::<Config>("stretch_to = \"wide\"\n").is_err());
    }

    #[test]
    fn load_missing_file_yields_defaults() {
        let dir = TempDir::new("missing");
        let path = dir.0.join("config.toml");
        assert_eq!(Config::load_unvalidated(&path).unwrap(), Config::default());
        if default_output_dir().is_some() {
            assert_eq!(Config::load(&path).unwrap(), Config::default());
        }
    }

    #[test]
    fn save_then_load_round_trips_and_creates_dirs() {
        let dir = TempDir::new("round-trip");
        let path = dir.0.join("nested").join("config.toml");
        let config = sample();
        config.save_atomic(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);

        let updated = Config { fps: 30, ..config };
        updated.save_atomic(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), updated);

        let nested = TempDir(path.parent().unwrap().to_owned());
        assert_eq!(nested.entries(), ["config.toml"], "temp file left behind");
    }

    #[test]
    fn concurrent_saves_do_not_collide() {
        let dir = TempDir::new("concurrent");
        let path = dir.0.join("config.toml");
        std::thread::scope(|scope| {
            for fps in 1..=8 {
                let path = &path;
                scope.spawn(move || Config { fps, ..sample() }.save_atomic(path).unwrap());
            }
        });
        assert!((1..=8).contains(&Config::load(&path).unwrap().fps));
        assert_eq!(dir.entries(), ["config.toml"]);
    }

    #[test]
    fn failed_save_leaves_destination_and_no_temp_file() {
        let dir = TempDir::new("failed-save");
        let path = dir.0.join("config.toml");
        fs::create_dir_all(path.join("keep")).unwrap();
        assert!(matches!(
            sample().save_atomic(&path),
            Err(ConfigError::Write { .. })
        ));
        assert!(path.join("keep").is_dir());
        assert_eq!(dir.entries(), ["config.toml"]);
    }

    #[test]
    fn save_rejects_path_without_file_name() {
        let dir = TempDir::new("no-file-name");
        let path = dir.0.join("..");
        assert!(matches!(
            sample().save_atomic(&path),
            Err(ConfigError::NoFileName { .. })
        ));
    }

    #[test]
    fn save_rejects_invalid_config_without_writing() {
        let dir = TempDir::new("invalid-save");
        let path = dir.0.join("config.toml");
        let config = Config { fps: 0, ..sample() };
        assert!(matches!(
            config.save_atomic(&path),
            Err(ConfigError::Invalid {
                source: ValidationError::Fps(0),
                ..
            })
        ));
        assert!(!path.exists());
    }

    #[test]
    fn load_unvalidated_opens_out_of_range_files() {
        let dir = TempDir::new("unvalidated");
        fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("config.toml");
        let invalid = Config {
            clip_seconds: 1,
            ..sample()
        };
        fs::write(&path, toml::to_string(&invalid).unwrap()).unwrap();

        assert_eq!(Config::load_unvalidated(&path).unwrap(), invalid);
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Invalid { path: p, source: ValidationError::ClipSeconds(1) }) if p == path
        ));
    }

    #[test]
    fn load_reports_parse_errors_including_bad_utf8() {
        let dir = TempDir::new("parse-errors");
        fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("config.toml");

        fs::write(&path, "clip_seconds = \"long\"\n").unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Parse { .. })
        ));

        fs::write(&path, b"fps = 60 # \xff\n").unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn load_reports_read_errors() {
        let dir = TempDir::new("read-error");
        fs::create_dir_all(&dir.0).unwrap();
        assert!(matches!(
            Config::load(&dir.0),
            Err(ConfigError::Read { .. })
        ));
    }
}
