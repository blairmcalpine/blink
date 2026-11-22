//! What a config reload has to do to the running engine.

use blink_config::Config;

use crate::mapping::RingSize;

/// The work needed to move the engine from one config to another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent piece of reload work"
)]
pub(crate) struct ReloadPlan {
    /// Unregister the old hotkey and register the new one.
    pub(crate) rebind_hotkey: bool,
    /// Stop capture, encode, and audio, then start them with the new config.
    /// The ring is recreated as part of the restart.
    pub(crate) restart_pipeline: bool,
    /// Replace the ring with one sized for the new config while capture keeps
    /// running. Only set when the pipeline is not restarted anyway.
    pub(crate) replace_ring: bool,
    /// Later saves go to a new folder. Saves read the folder from the config,
    /// so this needs no work beyond storing the new config.
    pub(crate) output_dir_changed: bool,
    /// The save sound was turned on or off. The tray thread reads it from
    /// the config when a save finishes; turning it on also warms up
    /// playback.
    pub(crate) save_sound_changed: bool,
}

impl ReloadPlan {
    /// Compares every field, so a new [`Config`] field fails to compile here
    /// until its reload behavior is decided.
    pub(crate) fn between(old: &Config, new: &Config) -> Self {
        let Config {
            output_dir,
            clip_seconds: _,
            hotkey,
            fps,
            video_bitrate_kbps,
            audio_enabled,
            audio_bitrate_kbps,
            stretch_to,
            save_sound,
        } = new;
        let restart_pipeline = *fps != old.fps
            || *video_bitrate_kbps != old.video_bitrate_kbps
            || *audio_enabled != old.audio_enabled
            || *audio_bitrate_kbps != old.audio_bitrate_kbps
            || *stretch_to != old.stretch_to;
        Self {
            rebind_hotkey: *hotkey != old.hotkey,
            restart_pipeline,
            replace_ring: !restart_pipeline
                && RingSize::for_config(new) != RingSize::for_config(old),
            output_dir_changed: *output_dir != old.output_dir,
            save_sound_changed: *save_sound != old.save_sound,
        }
    }

    /// Whether the new config changes nothing.
    pub(crate) fn is_noop(self) -> bool {
        self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn plan(change: impl FnOnce(&mut Config)) -> ReloadPlan {
        let old = Config::default();
        let mut new = old.clone();
        change(&mut new);
        ReloadPlan::between(&old, &new)
    }

    const RESTART: ReloadPlan = ReloadPlan {
        rebind_hotkey: false,
        restart_pipeline: true,
        replace_ring: false,
        output_dir_changed: false,
        save_sound_changed: false,
    };

    #[test]
    fn identical_configs_need_nothing() {
        assert!(plan(|_| {}).is_noop());
    }

    #[test]
    fn capture_and_encode_settings_restart_the_pipeline() {
        assert_eq!(plan(|c| c.fps = 144), RESTART);
        assert_eq!(plan(|c| c.video_bitrate_kbps = 50_000), RESTART);
        assert_eq!(plan(|c| c.audio_enabled = !c.audio_enabled), RESTART);
        assert_eq!(plan(|c| c.audio_bitrate_kbps = 192), RESTART);
        assert_eq!(
            plan(|c| c.stretch_to = Some("1920x1080".parse().unwrap())),
            RESTART
        );
    }

    #[test]
    fn clip_length_replaces_the_ring_live() {
        assert_eq!(
            plan(|c| c.clip_seconds = 90),
            ReloadPlan {
                replace_ring: true,
                ..ReloadPlan::default()
            }
        );
    }

    #[test]
    fn a_restart_subsumes_the_ring_replacement() {
        let both = plan(|c| {
            c.clip_seconds = 90;
            c.fps = 30;
        });
        assert_eq!(both, RESTART);
    }

    #[test]
    fn hotkey_and_output_dir_apply_live() {
        assert_eq!(
            plan(|c| c.hotkey = "Ctrl+Shift+F9".parse().unwrap()),
            ReloadPlan {
                rebind_hotkey: true,
                ..ReloadPlan::default()
            }
        );
        assert_eq!(
            plan(|c| c.output_dir = PathBuf::from("/elsewhere")),
            ReloadPlan {
                output_dir_changed: true,
                ..ReloadPlan::default()
            }
        );
    }

    #[test]
    fn save_sound_toggle_applies_live() {
        assert_eq!(
            plan(|c| c.save_sound = !c.save_sound),
            ReloadPlan {
                save_sound_changed: true,
                ..ReloadPlan::default()
            }
        );
    }
}
