//! The capture thread's decisions, kept free of Windows types so they can be
//! tested on any host.

use blink_ring::media::Timestamp;

/// The largest width or height NVENC's H.264 encoder accepts.
pub(crate) const H264_MAX_DIMENSION: u32 = 4096;

/// What a display mode change does to the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeAction {
    /// The mode fits the current session: reconfigure it in place.
    Reconfigure,
    /// Open a new session (after closing the old one) that can reconfigure
    /// up to `max` later.
    Recreate {
        /// Session maximum: the largest mode seen so far, at least this one.
        max: (u32, u32),
    },
    /// The mode is larger than H.264 allows; pause encoding until it changes.
    Unsupported,
}

/// Decides how to follow a mode change to `width` x `height`.
///
/// `current_max` is the open session's maximum, if any. `largest` is the
/// largest supported mode seen in this run, so that switching back to a
/// bigger mode (alt-tab out of a lower-resolution game) reconfigures instead
/// of reopening the session.
pub(crate) fn mode_change_action(
    current_max: Option<(u32, u32)>,
    largest: (u32, u32),
    width: u32,
    height: u32,
) -> ModeAction {
    if width > H264_MAX_DIMENSION || height > H264_MAX_DIMENSION {
        return ModeAction::Unsupported;
    }
    match current_max {
        Some((max_width, max_height)) if width <= max_width && height <= max_height => {
            ModeAction::Reconfigure
        }
        _ => ModeAction::Recreate {
            max: (largest.0.max(width), largest.1.max(height)),
        },
    }
}

/// Whether to re-encode the last frame at `now` on a static screen: the
/// encoder wants an IDR, there is a frame to encode, and `now` is after the
/// last encoded timestamp.
pub(crate) fn should_refresh(
    idr_due: bool,
    last_pts: Option<Timestamp>,
    now: Timestamp,
    has_frame: bool,
) -> bool {
    idr_due && has_frame && last_pts.is_none_or(|last| now > last)
}

/// The timestamp to encode a frame with: its capture time, nudged past the
/// last encoded one so timestamps strictly increase even if the clock
/// sources disagree by a tick.
pub(crate) fn next_pts(last_pts: Option<Timestamp>, pts: Timestamp) -> Timestamp {
    last_pts.map_or(pts, |last| {
        pts.max(last.saturating_add(Timestamp::from_ticks(1)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_mode_opens_a_session_at_that_size() {
        assert_eq!(
            mode_change_action(None, (0, 0), 1920, 1080),
            ModeAction::Recreate { max: (1920, 1080) }
        );
    }

    #[test]
    fn smaller_modes_reconfigure() {
        let current = Some((2560, 1440));
        assert_eq!(
            mode_change_action(current, (2560, 1440), 1280, 720),
            ModeAction::Reconfigure
        );
        assert_eq!(
            mode_change_action(current, (2560, 1440), 2560, 1440),
            ModeAction::Reconfigure
        );
    }

    #[test]
    fn larger_modes_recreate_at_the_largest_seen() {
        assert_eq!(
            mode_change_action(Some((1920, 1080)), (2560, 1080), 1920, 1200),
            ModeAction::Recreate { max: (2560, 1200) }
        );
        // After a failed session there is no current maximum, but the
        // largest mode seen still sizes the new one.
        assert_eq!(
            mode_change_action(None, (3840, 2160), 1280, 720),
            ModeAction::Recreate { max: (3840, 2160) }
        );
    }

    #[test]
    fn modes_beyond_h264_are_unsupported() {
        assert_eq!(
            mode_change_action(Some((4096, 4096)), (4096, 2160), 5120, 1440),
            ModeAction::Unsupported
        );
        assert_eq!(
            mode_change_action(None, (0, 0), 1440, 5120),
            ModeAction::Unsupported
        );
        assert_eq!(
            mode_change_action(None, (0, 0), 4096, 2160),
            ModeAction::Recreate { max: (4096, 2160) }
        );
    }

    #[test]
    fn refresh_only_when_due_with_a_frame_and_time_moved_on() {
        let last = Some(Timestamp::from_ticks(100));
        assert!(should_refresh(true, last, Timestamp::from_ticks(101), true));
        assert!(should_refresh(true, None, Timestamp::from_ticks(0), true));
        assert!(!should_refresh(
            false,
            last,
            Timestamp::from_ticks(101),
            true
        ));
        assert!(!should_refresh(
            true,
            last,
            Timestamp::from_ticks(101),
            false
        ));
        assert!(!should_refresh(
            true,
            last,
            Timestamp::from_ticks(100),
            true
        ));
        assert!(!should_refresh(true, last, Timestamp::from_ticks(99), true));
    }

    #[test]
    fn pts_strictly_increase() {
        assert_eq!(
            next_pts(None, Timestamp::from_ticks(50)),
            Timestamp::from_ticks(50)
        );
        assert_eq!(
            next_pts(Some(Timestamp::from_ticks(40)), Timestamp::from_ticks(50)),
            Timestamp::from_ticks(50)
        );
        assert_eq!(
            next_pts(Some(Timestamp::from_ticks(50)), Timestamp::from_ticks(50)),
            Timestamp::from_ticks(51)
        );
        assert_eq!(
            next_pts(Some(Timestamp::from_ticks(60)), Timestamp::from_ticks(50)),
            Timestamp::from_ticks(61)
        );
    }
}
