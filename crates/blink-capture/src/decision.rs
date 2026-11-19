//! What to do with each acquired duplication frame, as pure logic.
//!
//! The Windows duplicator gathers the facts (did content change, is there an
//! image, when) and acts on the answer; everything in between lives here so it
//! is tested on every host.

use std::time::Duration;

use blink_ring::media::Timestamp;

use crate::schedule::FrameScheduler;

/// `AcquireNextFrame` treats `u32::MAX` as INFINITE; longer waits are capped
/// one below it and re-issued by the caller's loop.
pub(crate) const MAX_WAIT_MS: u32 = u32::MAX - 1;

/// Whether the latest desktop image has been copied, and if not, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Staleness {
    /// The last copied frame is the current desktop.
    Fresh,
    /// A content change was dropped (by the decimator, or because DXGI handed
    /// over no image), so the desktop is ahead of the last copied frame.
    DroppedChange,
    /// The session is new and nothing has been copied from it yet. Its first
    /// acquire returns the whole desktop, so that frame is copied even if it
    /// reports no new content.
    NewSession,
}

/// The facts about one acquired frame that the decision needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Acquired {
    /// `LastPresentTime != 0`: the desktop image changed since the last
    /// acquire. A zero present time means only the pointer moved, whatever
    /// `AccumulatedFrames` says.
    pub(crate) content_changed: bool,
    /// DXGI returned a desktop resource with the frame.
    pub(crate) has_image: bool,
    /// The frame's QPC time: the present time if content changed, otherwise
    /// the pointer update time or the time it was observed.
    pub(crate) qpc: i64,
}

/// What to do with an acquired frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameAction {
    /// Copy the image into the pool and emit it.
    Copy,
    /// Release without copying and report `Unchanged`.
    Unchanged,
    /// Release without copying and keep waiting.
    Drop,
}

/// Decides what to do with `frame`, consuming a decimation slot when it is
/// copied, and updates `staleness`.
///
/// A pointer-only update still carries the full current desktop image, so
/// while the desktop is ahead of the last copy it can deliver that content.
pub(crate) fn decide(
    frame: Acquired,
    staleness: &mut Staleness,
    scheduler: &mut FrameScheduler,
) -> FrameAction {
    if !frame.content_changed && *staleness == Staleness::Fresh {
        return FrameAction::Unchanged;
    }
    if !scheduler.admit(frame.qpc) {
        if frame.content_changed {
            *staleness = Staleness::DroppedChange;
            return FrameAction::Drop;
        }
        return FrameAction::Unchanged;
    }
    if !frame.has_image {
        *staleness = Staleness::DroppedChange;
        return FrameAction::Drop;
    }
    *staleness = Staleness::Fresh;
    FrameAction::Copy
}

/// What to do when `AcquireNextFrame` times out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeoutAction {
    /// Report `Timeout`.
    Report,
    /// The desktop went idle after a dropped change and its slot is due:
    /// recreate the session, whose first acquire returns the full desktop, so
    /// the last change is not lost.
    Resync,
}

/// Decides what an acquire timeout means. `slot_due` is whether the decimator
/// would admit a frame now.
pub(crate) fn on_timeout(staleness: Staleness, slot_due: bool) -> TimeoutAction {
    if staleness == Staleness::DroppedChange && slot_due {
        TimeoutAction::Resync
    } else {
        TimeoutAction::Report
    }
}

/// The `AcquireNextFrame` timeout in milliseconds: the caller's `remaining`
/// time, capped at `until_slot` while a dropped change waits for its slot.
/// Rounds up so a sub-millisecond wait does not become a busy poll, and
/// never returns INFINITE.
pub(crate) fn acquire_timeout_ms(remaining: Duration, until_slot: Option<Duration>) -> u32 {
    let wait = until_slot.map_or(remaining, |slot| slot.min(remaining));
    u32::try_from(wait.as_micros().div_ceil(1000))
        .unwrap_or(MAX_WAIT_MS)
        .min(MAX_WAIT_MS)
}

/// Keeps emitted frame timestamps strictly increasing. A copy triggered by a
/// pointer update carries the pointer time, which can trail the present time
/// of the frame before it.
pub(crate) fn monotonic(last: Option<Timestamp>, timestamp: Timestamp) -> Timestamp {
    last.map_or(timestamp, |last| {
        timestamp.max(last.saturating_add(Timestamp::from_ticks(1)))
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    const QPC_HZ: i64 = 10_000_000;
    const INTERVAL: i64 = QPC_HZ / 60;

    fn scheduler() -> FrameScheduler {
        FrameScheduler::new(NonZeroU32::new(60).unwrap(), QPC_HZ)
    }

    const fn changed(qpc: i64) -> Acquired {
        Acquired {
            content_changed: true,
            has_image: true,
            qpc,
        }
    }

    const fn pointer(qpc: i64) -> Acquired {
        Acquired {
            content_changed: false,
            has_image: true,
            qpc,
        }
    }

    #[test]
    fn first_frame_of_session_is_copied_even_without_new_content() {
        let mut s = scheduler();
        let mut staleness = Staleness::NewSession;
        assert_eq!(
            decide(pointer(0), &mut staleness, &mut s),
            FrameAction::Copy
        );
        assert_eq!(staleness, Staleness::Fresh);
    }

    #[test]
    fn pointer_moves_on_a_fresh_desktop_are_unchanged() {
        let mut s = scheduler();
        let mut staleness = Staleness::Fresh;
        assert_eq!(
            decide(pointer(0), &mut staleness, &mut s),
            FrameAction::Unchanged
        );
        // No slot consumed: a change right after is still admitted.
        assert_eq!(
            decide(changed(1), &mut staleness, &mut s),
            FrameAction::Copy
        );
    }

    #[test]
    fn early_change_is_dropped_then_delivered_by_a_pointer_move() {
        let mut s = scheduler();
        let mut staleness = Staleness::Fresh;
        assert_eq!(
            decide(changed(0), &mut staleness, &mut s),
            FrameAction::Copy
        );
        assert_eq!(
            decide(changed(INTERVAL / 4), &mut staleness, &mut s),
            FrameAction::Drop
        );
        assert_eq!(staleness, Staleness::DroppedChange);
        // Pointer moves before the slot do not copy yet.
        assert_eq!(
            decide(pointer(INTERVAL / 2), &mut staleness, &mut s),
            FrameAction::Unchanged
        );
        assert_eq!(staleness, Staleness::DroppedChange);
        // At the slot, a pointer move carries the dropped content.
        assert_eq!(
            decide(pointer(INTERVAL), &mut staleness, &mut s),
            FrameAction::Copy
        );
        assert_eq!(staleness, Staleness::Fresh);
    }

    #[test]
    fn early_change_then_idle_resyncs_once_the_slot_is_due() {
        let mut s = scheduler();
        let mut staleness = Staleness::Fresh;
        decide(changed(0), &mut staleness, &mut s);
        decide(changed(INTERVAL / 4), &mut staleness, &mut s);
        let slot = s.next_admit_qpc().unwrap();
        assert_eq!(
            on_timeout(staleness, s.is_due(slot - 1)),
            TimeoutAction::Report
        );
        assert_eq!(on_timeout(staleness, s.is_due(slot)), TimeoutAction::Resync);
    }

    #[test]
    fn idle_without_a_dropped_change_just_times_out() {
        assert_eq!(on_timeout(Staleness::Fresh, true), TimeoutAction::Report);
        assert_eq!(
            on_timeout(Staleness::NewSession, true),
            TimeoutAction::Report
        );
    }

    #[test]
    fn admitted_change_without_an_image_is_not_lost() {
        let mut s = scheduler();
        let mut staleness = Staleness::Fresh;
        let missing = Acquired {
            has_image: false,
            ..changed(0)
        };
        assert_eq!(decide(missing, &mut staleness, &mut s), FrameAction::Drop);
        assert_eq!(staleness, Staleness::DroppedChange);
    }

    #[test]
    fn timeout_rounds_up_caps_and_handles_zero() {
        assert_eq!(acquire_timeout_ms(Duration::ZERO, None), 0);
        assert_eq!(acquire_timeout_ms(Duration::from_micros(1), None), 1);
        assert_eq!(acquire_timeout_ms(Duration::from_micros(1500), None), 2);
        assert_eq!(acquire_timeout_ms(Duration::from_millis(16), None), 16);
        assert_eq!(acquire_timeout_ms(Duration::MAX, None), MAX_WAIT_MS);
        let slot = Some(Duration::from_millis(3));
        assert_eq!(acquire_timeout_ms(Duration::from_millis(100), slot), 3);
        assert_eq!(acquire_timeout_ms(Duration::from_millis(1), slot), 1);
        assert_eq!(acquire_timeout_ms(Duration::MAX, Some(Duration::ZERO)), 0);
    }

    #[test]
    fn timestamps_never_go_backwards() {
        let t = Timestamp::from_ticks;
        assert_eq!(monotonic(None, t(5)), t(5));
        assert_eq!(monotonic(Some(t(5)), t(9)), t(9));
        assert_eq!(monotonic(Some(t(5)), t(5)), t(6));
        assert_eq!(monotonic(Some(t(5)), t(2)), t(6));
        assert_eq!(monotonic(Some(t(i64::MAX)), t(0)), t(i64::MAX));
    }
}
