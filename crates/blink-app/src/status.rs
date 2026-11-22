//! What the tray tooltip says about the recorder, so a release build (with
//! no console) still shows when capture or a save is failing.

use blink_audio::MicState;
use blink_config::Hotkey;
use blink_ring::media::Timestamp;

use crate::clip::Seconds;

/// Windows truncates tray tooltips at 127 UTF-16 units.
const TOOLTIP_UNITS: usize = 127;

/// Consecutive capture failures after which the user is told with a dialog,
/// not only the tooltip.
const ESCALATE_AFTER: u32 = 5;

/// Whether the capture failure numbered `failures` in a row should be shown
/// in a dialog: once per streak, when it has clearly not recovered.
pub(crate) const fn escalate(failures: u32) -> bool {
    failures == ESCALATE_AFTER
}

/// State of the capture thread, as last reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureState {
    /// The device and encoder are being set up.
    Starting,
    /// Frames are being encoded into the ring.
    Recording,
    /// The last session failed; it is rebuilt after a backoff delay.
    Retrying {
        /// Failures since capture last encoded a frame.
        failures: u32,
        /// The latest failure.
        error: String,
    },
    /// The display mode is larger than H.264 allows; paused until it changes.
    Unsupported {
        /// Display width in pixels.
        width: u32,
        /// Display height in pixels.
        height: u32,
        /// When video stopped; nothing after it is encoded.
        since: Timestamp,
    },
}

impl CaptureState {
    /// When video paused, after this state follows one in which it had
    /// paused at `previous`. A pause lasts until frames are recorded again,
    /// including through capture failures and restarts in between.
    pub(crate) const fn paused_since(&self, previous: Option<Timestamp>) -> Option<Timestamp> {
        match self {
            Self::Unsupported { since, .. } => Some(*since),
            Self::Recording => None,
            Self::Starting | Self::Retrying { .. } => previous,
        }
    }
}

/// Where a clip saved at `now` should end. While video is paused for an
/// unsupported mode the ring holds none after the pause, so the clip ends
/// there instead of running on as audio over a frozen tail.
pub(crate) fn clip_end(paused_since: Option<Timestamp>, now: Timestamp) -> Timestamp {
    paused_since.map_or(now, |since| now.min(since))
}

/// How the last save went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SaveResult {
    /// Written, with the clip's length.
    Saved(Seconds),
    /// Not written, and why.
    Failed(String),
}

/// Everything the tooltip reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Status {
    /// The capture thread's state.
    pub(crate) capture: CaptureState,
    /// The last save since start, if any.
    pub(crate) last_save: Option<SaveResult>,
    /// The hotkey the config asks for.
    pub(crate) requested_hotkey: Hotkey,
    /// Whether that hotkey is registered.
    pub(crate) hotkey_registered: bool,
    /// Clip length in seconds.
    pub(crate) clip_seconds: u32,
    /// What the microphone is doing.
    pub(crate) microphone: MicState,
}

/// The tooltip line for a microphone that should be recording but is not.
const fn mic_line(state: MicState) -> Option<&'static str> {
    match state {
        MicState::Off | MicState::Starting | MicState::Recording => None,
        MicState::NotFound => Some("No microphone found; recording desktop audio only"),
        MicState::Denied => Some("Microphone blocked by Windows privacy settings"),
        MicState::Unavailable => Some("Microphone unavailable; retrying"),
        MicState::BluetoothSkipped => {
            Some("Bluetooth headset mic skipped (it would lower headset audio quality)")
        }
    }
}

/// The message for an unsupported display mode.
pub(crate) fn unsupported_message(width: u32, height: u32) -> String {
    format!(
        "The display is {width}x{height}, larger than the {max} pixels per side that NVENC's \
         H.264 encoder supports. blink is paused and resumes when the resolution changes.",
        max = crate::decide::H264_MAX_DIMENSION
    )
}

/// The tray tooltip for `status`, one line per concern, cut to fit.
pub(crate) fn tooltip(status: &Status) -> String {
    let mut lines = Vec::with_capacity(4);
    lines.push(if status.hotkey_registered {
        format!(
            "blink: {} saves the last {} s",
            status.requested_hotkey, status.clip_seconds
        )
    } else {
        format!(
            "blink: hotkey {} unavailable; save from this menu",
            status.requested_hotkey
        )
    });
    match &status.capture {
        CaptureState::Starting => lines.push("Starting capture".to_owned()),
        CaptureState::Recording => {}
        CaptureState::Retrying { failures, error } => {
            lines.push(format!("Not recording ({failures} failures): {error}"));
        }
        CaptureState::Unsupported { width, height, .. } => {
            lines.push(format!("Paused: {width}x{height} is too large for H.264"));
        }
    }
    match &status.last_save {
        None => {}
        Some(SaveResult::Saved(duration)) => lines.push(format!("Last clip saved ({duration})")),
        Some(SaveResult::Failed(error)) => lines.push(format!("Last save failed: {error}")),
    }
    // Last, so a long tooltip loses it before a save failure.
    if let Some(line) = mic_line(status.microphone) {
        lines.push(line.to_owned());
    }
    truncate(&lines.join("\n"))
}

fn truncate(text: &str) -> String {
    if text.encode_utf16().count() <= TOOLTIP_UNITS {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut units = 0;
    for ch in text.chars() {
        // Leave room for the ellipsis.
        if units + ch.len_utf16() > TOOLTIP_UNITS - 1 {
            break;
        }
        units += ch.len_utf16();
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use blink_config::Config;

    use super::*;

    fn status() -> Status {
        Status {
            capture: CaptureState::Recording,
            last_save: None,
            requested_hotkey: Config::default().hotkey,
            hotkey_registered: true,
            clip_seconds: 30,
            microphone: MicState::Recording,
        }
    }

    #[test]
    fn only_a_failing_microphone_gets_a_line() {
        for state in [MicState::Off, MicState::Starting, MicState::Recording] {
            let text = tooltip(&Status {
                microphone: state,
                ..status()
            });
            assert_eq!(text.lines().count(), 1, "{state:?}: {text}");
        }
        let denied = tooltip(&Status {
            capture: CaptureState::Starting,
            microphone: MicState::Denied,
            last_save: Some(SaveResult::Failed("disk full".to_owned())),
            ..status()
        });
        let lines: Vec<_> = denied.lines().collect();
        assert_eq!(lines.len(), 4, "{denied}");
        assert_eq!(lines[2], "Last save failed: disk full");
        assert!(lines[3].contains("privacy"));
        assert!(mic_line(MicState::NotFound).is_some());
        assert!(mic_line(MicState::Unavailable).is_some());
    }

    #[test]
    fn a_skipped_bluetooth_mic_fits_next_to_the_hotkey_line() {
        let text = tooltip(&Status {
            microphone: MicState::BluetoothSkipped,
            ..status()
        });
        assert!(text.encode_utf16().count() < TOOLTIP_UNITS, "{text}");
        assert!(!text.ends_with('…'), "{text}");
        assert!(
            text.ends_with("Bluetooth headset mic skipped (it would lower headset audio quality)")
        );
    }

    #[test]
    fn recording_shows_only_the_hotkey() {
        let tooltip = tooltip(&status());
        assert_eq!(
            tooltip,
            format!("blink: {} saves the last 30 s", Config::default().hotkey)
        );
    }

    #[test]
    fn failures_and_saves_get_their_own_lines() {
        let text = tooltip(&Status {
            capture: CaptureState::Retrying {
                failures: 3,
                error: "access lost".to_owned(),
            },
            last_save: Some(SaveResult::Failed("disk full".to_owned())),
            hotkey_registered: false,
            ..status()
        });
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].contains("unavailable"));
        assert!(lines[1].contains("3 failures") && lines[1].contains("access lost"));
        assert_eq!(lines[2], "Last save failed: disk full");

        let saved = tooltip(&Status {
            capture: CaptureState::Unsupported {
                width: 5120,
                height: 1440,
                since: Timestamp::from_ticks(0),
            },
            last_save: Some(SaveResult::Saved(Seconds(Timestamp::from_secs(30)))),
            ..status()
        });
        assert!(saved.contains("5120x1440"));
        assert!(saved.ends_with("Last clip saved (30.000 s)"));
    }

    #[test]
    fn long_tooltips_are_cut_to_what_windows_shows() {
        let text = tooltip(&Status {
            capture: CaptureState::Retrying {
                failures: 1,
                error: "é".repeat(300),
            },
            ..status()
        });
        assert_eq!(text.encode_utf16().count(), TOOLTIP_UNITS);
        assert!(text.ends_with('…'));
        assert_eq!(truncate("short"), "short");
    }

    #[test]
    fn starting_says_so_and_escalation_waits_for_repeats() {
        let text = tooltip(&Status {
            capture: CaptureState::Starting,
            ..status()
        });
        assert!(text.ends_with("\nStarting capture"));
        assert!(!escalate(1), "a single failure only shows in the tooltip");
        let warned: Vec<u32> = (1..=20).filter(|&failures| escalate(failures)).collect();
        assert_eq!(warned, [ESCALATE_AFTER], "one dialog per streak");
    }

    #[test]
    fn a_paused_capture_ends_clips_at_the_pause() {
        let paused = CaptureState::Unsupported {
            width: 5120,
            height: 1440,
            since: Timestamp::from_ticks(500),
        };
        let since = paused.paused_since(None);
        assert_eq!(since, Some(Timestamp::from_ticks(500)));
        assert_eq!(
            clip_end(since, Timestamp::from_ticks(900)),
            Timestamp::from_ticks(500)
        );
        assert_eq!(
            clip_end(since, Timestamp::from_ticks(400)),
            Timestamp::from_ticks(400)
        );
        assert_eq!(
            clip_end(None, Timestamp::from_ticks(900)),
            Timestamp::from_ticks(900)
        );
    }

    #[test]
    fn a_pause_survives_failures_until_frames_are_recorded() {
        let since = Some(Timestamp::from_ticks(500));
        let retrying = CaptureState::Retrying {
            failures: 1,
            error: "access lost".to_owned(),
        };
        assert_eq!(retrying.paused_since(since), since);
        assert_eq!(CaptureState::Starting.paused_since(since), since);
        assert_eq!(CaptureState::Recording.paused_since(since), None);
        assert_eq!(retrying.paused_since(None), None);
    }

    #[test]
    fn unsupported_message_names_the_limit() {
        let message = unsupported_message(5120, 1440);
        assert!(message.contains("5120x1440") && message.contains("4096"));
    }
}
