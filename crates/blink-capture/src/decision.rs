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
    /// slot has gone a whole interval without a content update to deliver
    /// it, and this frame has no image to deliver it with.
    Resync,
}

/// Decides what to do with `frame`, consuming a decimation slot when it is
/// copied, and updates `staleness`.
///
/// Only frames with new content are copied. DXGI documents a pointer-only
/// update as "the desktop image was not updated", and on the first acquire
/// of a new session that surface has never been written: copying it encodes
/// a black frame. A dropped change waiting for its slot is delivered by the
/// next content update or, once the slot is a whole interval overdue (the
/// desktop went idle), by a resync; a new session waits for its first content
/// update.
///
/// The overdue margin matters: at 144 Hz into 60 fps the next update lands
/// up to 6.9 ms after the slot opens, so resyncing as soon as the slot opened
/// would recreate the session on most frames of continuous motion.
pub(crate) fn decide(
    frame: Acquired,
    staleness: &mut Staleness,
    scheduler: &mut FrameScheduler,
) -> FrameAction {
    if !frame.content_changed {
        return if *staleness == Staleness::DroppedChange && scheduler.is_overdue(frame.qpc) {
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
    /// Sessions that ended (lost, or re-duplicated for having nothing to
    /// show) before their first content update.
    pub(crate) contentless_sessions: u64,
    /// Sessions recreated to deliver a dropped change.
    pub(crate) resyncs: u64,
    /// The slowest of those recreations.
    pub(crate) slowest_resync: Duration,
    /// The longest a new session waited for its first content update.
    pub(crate) slowest_first_content: Duration,
    /// Sessions re-duplicated because they had nothing to show and got no
    /// content update in time.
    pub(crate) empty_resyncs: u64,
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

    /// Records how long a new session waited for its first content update.
    pub(crate) fn record_first_content(&mut self, waited: Duration) {
        self.slowest_first_content = self.slowest_first_content.max(waited);
    }

    /// Records a session that ended after waiting `waited` without a
    /// content update.
    pub(crate) fn record_contentless_session(&mut self, waited: Duration) {
        self.contentless_sessions = self.contentless_sessions.saturating_add(1);
        self.slowest_first_content = self.slowest_first_content.max(waited);
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
    /// The desktop went idle after a dropped change and its slot is a whole
    /// interval overdue: recreate the session, whose first content update
    /// carries the full desktop, so the last change is not lost.
    Resync,
}

/// Decides what an acquire timeout means. `overdue` is the decimator's
/// [`is_overdue`](FrameScheduler::is_overdue) now.
pub(crate) fn on_timeout(staleness: Staleness, overdue: bool) -> TimeoutAction {
    if staleness == Staleness::DroppedChange && overdue {
        TimeoutAction::Resync
    } else {
        TimeoutAction::Report
    }
}

/// The QPC time at which an acquire that sees no update should stop waiting
/// and report a timeout, so [`on_timeout`] can resync: while a dropped change
/// is pending, the time its slot becomes overdue. `None` means wait for the
/// caller's full timeout.
pub(crate) fn resync_deadline_qpc(staleness: Staleness, scheduler: &FrameScheduler) -> Option<i64> {
    if staleness == Staleness::DroppedChange {
        scheduler.overdue_qpc()
    } else {
        None
    }
}

/// The `AcquireNextFrame` timeout in milliseconds: the caller's `remaining`
/// time, capped at `until_slot` (until the slot is overdue) while a dropped
/// change waits.
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
    fn early_change_is_dropped_then_resynced_by_a_pointer_move_once_overdue() {
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
        let overdue = s.overdue_qpc().unwrap();
        // Pointer moves before and after the slot opens, short of a whole
        // interval past it, wait for a content update.
        for qpc in [INTERVAL / 2, INTERVAL, overdue - 1] {
            assert_eq!(
                decide(pointer(qpc), &mut staleness, &mut s),
                FrameAction::Unchanged
            );
            assert_eq!(staleness, Staleness::DroppedChange);
        }
        assert_eq!(
            decide(pointer(overdue), &mut staleness, &mut s),
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
    fn early_change_then_idle_resyncs_once_the_slot_is_overdue() {
        let mut s = scheduler();
        let mut staleness = Staleness::Fresh;
        decide(changed(0), &mut staleness, &mut s);
        decide(changed(INTERVAL / 4), &mut staleness, &mut s);
        let slot = s.next_admit_qpc().unwrap();
        let overdue = s.overdue_qpc().unwrap();
        for qpc in [slot, overdue - 1] {
            assert_eq!(
                on_timeout(staleness, s.is_overdue(qpc)),
                TimeoutAction::Report
            );
        }
        assert_eq!(
            on_timeout(staleness, s.is_overdue(overdue)),
            TimeoutAction::Resync
        );
    }

    /// An acquired update in [`simulate`].
    #[derive(Clone, Copy)]
    enum Update {
        Content(i64),
        Pointer(i64),
    }

    impl Update {
        const fn qpc(self) -> i64 {
            match self {
                Self::Content(qpc) | Self::Pointer(qpc) => qpc,
            }
        }
    }

    /// Drives `decide` and `on_timeout` the way the duplicator does:
    /// `AcquireNextFrame` times out at `resync_deadline_qpc` if no update
    /// comes first. With `idle_after`, the desktop stays
    /// idle after the last update. Returns `(copies, resyncs)`.
    fn simulate(updates: &[Update], idle_after: bool) -> (usize, usize) {
        let mut s = scheduler();
        let mut staleness = Staleness::Fresh;
        let (mut copies, mut resyncs) = (0, 0);
        let mut resync = |s: &mut FrameScheduler, staleness: &mut Staleness| {
            resyncs += 1;
            s.reset();
            *staleness = Staleness::NewSession;
        };
        let mut updates = updates.iter().copied().peekable();
        loop {
            let next = updates.peek().map(|update| update.qpc());
            if next.is_none() && !idle_after {
                break;
            }
            if let Some(cap) = resync_deadline_qpc(staleness, &s)
                && next.is_none_or(|next| cap < next)
            {
                assert_eq!(
                    on_timeout(staleness, s.is_overdue(cap)),
                    TimeoutAction::Resync
                );
                resync(&mut s, &mut staleness);
                continue;
            }
            let Some(update) = updates.next() else {
                break;
            };
            let frame = match update {
                Update::Content(qpc) => changed(qpc),
                Update::Pointer(qpc) => pointer(qpc),
            };
            match decide(frame, &mut staleness, &mut s) {
                FrameAction::Copy => copies += 1,
                FrameAction::Resync => resync(&mut s, &mut staleness),
                FrameAction::Unchanged | FrameAction::Drop => {}
            }
        }
        (copies, resyncs)
    }

    const HZ_144: i64 = QPC_HZ / 144;

    fn content_at_144_hz(frames: i64) -> Vec<Update> {
        (0..frames).map(|i| Update::Content(i * HZ_144)).collect()
    }

    #[test]
    fn continuous_motion_at_144_hz_never_resyncs() {
        let (copies, resyncs) = simulate(&content_at_144_hz(1440), false);
        assert_eq!(resyncs, 0);
        assert!((590..=601).contains(&copies), "copied {copies}");

        // Pointer moves between the content updates change nothing.
        let mixed: Vec<Update> = (0..1440)
            .flat_map(|i| {
                [
                    Update::Content(i * HZ_144),
                    Update::Pointer(i * HZ_144 + HZ_144 / 2),
                ]
            })
            .collect();
        assert_eq!(simulate(&mixed, false).1, 0);
    }

    /// 144 Hz content up to and including the first dropped update after a
    /// second of motion, so the desktop goes idle with a change pending.
    fn motion_ending_in_a_dropped_change() -> Vec<Update> {
        let mut s = scheduler();
        let last = (0..=144).filter(|&i| !s.admit(i * HZ_144)).last().unwrap();
        content_at_144_hz(last + 1)
    }

    #[test]
    fn idle_after_a_dropped_change_resyncs_exactly_once() {
        let updates = motion_ending_in_a_dropped_change();
        assert_eq!(simulate(&updates, true).1, 1);

        // Same with the pointer still moving through the idle period.
        let end = updates.last().unwrap().qpc();
        let mut with_pointer = updates;
        with_pointer.extend((1..100).map(|i| Update::Pointer(end + i * HZ_144)));
        assert_eq!(simulate(&with_pointer, true).1, 1);

        // Idle after an admitted update leaves nothing to resync.
        let mut admitted = motion_ending_in_a_dropped_change();
        admitted.pop();
        assert_eq!(simulate(&admitted, true).1, 0);
    }

    #[test]
    fn acquire_waits_until_a_dropped_change_is_overdue() {
        let mut s = scheduler();
        let mut staleness = Staleness::Fresh;
        assert_eq!(resync_deadline_qpc(staleness, &s), None);
        decide(changed(0), &mut staleness, &mut s);
        decide(changed(INTERVAL / 4), &mut staleness, &mut s);
        assert_eq!(resync_deadline_qpc(staleness, &s), s.overdue_qpc());
        assert_ne!(resync_deadline_qpc(staleness, &s), s.next_admit_qpc());
        assert_eq!(resync_deadline_qpc(Staleness::NewSession, &s), None);
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
        stats.record_first_content(Duration::from_millis(40));
        stats.record_first_content(Duration::from_millis(20));
        stats.record_contentless_session(Duration::from_millis(30));
        assert_eq!(
            stats,
            CaptureStats {
                copied: 1,
                dropped: 1,
                pointer_only: 2,
                new_session_pointer_only: 1,
                no_image: 1,
                contentless_sessions: 1,
                resyncs: 2,
                slowest_resync: Duration::from_millis(3),
                slowest_first_content: Duration::from_millis(40),
                empty_resyncs: 0,
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
