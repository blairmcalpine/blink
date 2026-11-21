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
    /// Release without copying and recreate the session: a dropped change's
    /// slot is due, but this frame has no image to deliver it with.
    Resync,
}

/// Decides what to do with `frame`, consuming a decimation slot when it is
/// copied, and updates `staleness`.
///
/// Only frames with new content are copied. DXGI documents a pointer-only
/// update as "the desktop image was not updated", and on the first acquire
/// of a new session that surface has never been written: copying it encodes
/// a black frame. A dropped change waiting for its slot is delivered by the
/// next content update or, once the slot is due, by a resync; a new session
/// waits for its first content update.
pub(crate) fn decide(
    frame: Acquired,
    staleness: &mut Staleness,
    scheduler: &mut FrameScheduler,
) -> FrameAction {
    if !frame.content_changed {
        return if *staleness == Staleness::DroppedChange && scheduler.is_due(frame.qpc) {
            FrameAction::Resync
        } else {
            FrameAction::Unchanged
        };
    }
    if !scheduler.admit(frame.qpc) {
        *staleness = Staleness::DroppedChange;
        return FrameAction::Drop;
    }
    if !frame.has_image {
        *staleness = Staleness::DroppedChange;
        return FrameAction::Drop;
    }
    *staleness = Staleness::Fresh;
    FrameAction::Copy
}

/// Frame counters for the duplicator's periodic summary log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CaptureStats {
    /// Frames copied into the pool and emitted.
    pub(crate) copied: u64,
    /// Content updates dropped by the decimator.
    pub(crate) dropped: u64,
    /// Updates where only the pointer moved.
    pub(crate) pointer_only: u64,
    /// Pointer-only updates on a session with nothing copied yet; earlier
    /// versions copied these and encoded black frames.
    pub(crate) new_session_pointer_only: u64,
    /// Content updates that came without a desktop resource.
    pub(crate) no_image: u64,
    /// Sessions recreated to deliver a dropped change.
    pub(crate) resyncs: u64,
    /// The slowest of those recreations.
    pub(crate) slowest_resync: Duration,
}

impl CaptureStats {
    /// Counts one acquired frame, given the staleness before [`decide`] ran
    /// and its answer.
    pub(crate) fn record(&mut self, frame: Acquired, before: Staleness, action: FrameAction) {
        let counter = if !frame.content_changed {
            if before == Staleness::NewSession {
                self.new_session_pointer_only = self.new_session_pointer_only.saturating_add(1);
            }
            &mut self.pointer_only
        } else if !frame.has_image {
            &mut self.no_image
        } else if action == FrameAction::Copy {
            &mut self.copied
        } else {
            &mut self.dropped
        };
        *counter = counter.saturating_add(1);
    }

    /// Counts one resync that took `elapsed`.
    pub(crate) fn record_resync(&mut self, elapsed: Duration) {
        self.resyncs = self.resyncs.saturating_add(1);
        self.slowest_resync = self.slowest_resync.max(elapsed);
    }
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
    fn new_session_waits_for_content_before_copying() {
        let mut s = scheduler();
        let mut staleness = Staleness::NewSession;
        assert_eq!(
            decide(pointer(0), &mut staleness, &mut s),
            FrameAction::Unchanged
        );
        assert_eq!(staleness, Staleness::NewSession);
        // Nothing to resync for: the session never dropped a change.
        assert_eq!(
            decide(pointer(INTERVAL * 5), &mut staleness, &mut s),
            FrameAction::Unchanged
        );
        assert_eq!(
            on_timeout(staleness, s.is_due(INTERVAL * 5)),
            TimeoutAction::Report
        );
        assert_eq!(
            decide(changed(INTERVAL * 6), &mut staleness, &mut s),
            FrameAction::Copy
        );
        assert_eq!(staleness, Staleness::Fresh);
    }

    #[test]
    fn pointer_only_frames_are_never_copied() {
        for start in [
            Staleness::Fresh,
            Staleness::NewSession,
            Staleness::DroppedChange,
        ] {
            for qpc in [0, INTERVAL / 2, INTERVAL, INTERVAL * 10] {
                let mut s = scheduler();
                let mut staleness = start;
                assert_ne!(
                    decide(pointer(qpc), &mut staleness, &mut s),
                    FrameAction::Copy
                );
            }
        }
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
    fn early_change_is_dropped_then_resynced_by_a_pointer_move_at_its_slot() {
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
        // At the slot a pointer move cannot carry the dropped content, so
        // the session is resynced.
        assert_eq!(
            decide(pointer(INTERVAL), &mut staleness, &mut s),
            FrameAction::Resync
        );
        // A content update at the slot delivers it directly.
        assert_eq!(
            decide(changed(INTERVAL), &mut staleness, &mut s),
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
    fn stats_count_each_kind_of_frame() {
        let mut stats = CaptureStats::default();
        stats.record(changed(0), Staleness::Fresh, FrameAction::Copy);
        stats.record(changed(1), Staleness::Fresh, FrameAction::Drop);
        stats.record(pointer(2), Staleness::Fresh, FrameAction::Unchanged);
        stats.record(pointer(3), Staleness::NewSession, FrameAction::Unchanged);
        let missing = Acquired {
            has_image: false,
            ..changed(4)
        };
        stats.record(missing, Staleness::Fresh, FrameAction::Drop);
        stats.record_resync(Duration::from_millis(3));
        stats.record_resync(Duration::from_millis(1));
        assert_eq!(
            stats,
            CaptureStats {
                copied: 1,
                dropped: 1,
                pointer_only: 2,
                new_session_pointer_only: 1,
                no_image: 1,
                resyncs: 2,
                slowest_resync: Duration::from_millis(3),
            }
        );
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
