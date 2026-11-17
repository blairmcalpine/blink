//! Shared configuration for blink.
//!
//! Both the capture engine and the `--settings` process use these types. The
//! config lives in a TOML file that the settings process replaces with
//! [`Config::save_atomic`] and the engine reloads with [`Config::load`] off
//! the hot path. When a reload fails, `blink-app` keeps running with the
//! previous config. The settings window opens files with
//! [`Config::load_unvalidated`] so it can show [`Config::errors`] and let the
//! user fix them. This crate is portable and contains no `unsafe`.

#![forbid(unsafe_code)]

mod config;
mod hotkey;
mod paths;
mod resolution;

pub use config::{
    AUDIO_BITRATES_KBPS, CLIP_SECONDS_RANGE, Config, ConfigError, FPS_RANGE,
    STRETCH_TO_DIMENSION_RANGE, VIDEO_BITRATE_KBPS_RANGE, ValidationError,
};
pub use hotkey::{Hotkey, HotkeyParseError, Key, Modifiers};
pub use paths::{default_config_path, default_output_dir};
pub use resolution::{Resolution, ResolutionParseError};
