//! What to do with each acquired duplication frame, as pure logic.
//!
//! The Windows duplicator gathers the facts (did content change, is there an
//! image) and acts on the answer; everything in between lives here, in
//! [`Pacing`], and in [`FramePacer`], so it is tested on every host by
//! driving the same calls the duplicator makes.

use std::num::NonZeroU32;
use std::time::Duration;

use blink_ring::media::Timestamp;

use crate::schedule::FramePacer;

/// How long a session with nothing to show waits for a content update before
/// it is re-duplicated, doubling up to [`MAX_EMPTY_SESSION_WAIT`].
pub(crate) const EMPTY_SESSION_WAIT: Duration = Duration::from_secs(1);
pub(crate) const MAX_EMPTY_SESSION_WAIT: Duration = Duration::from_secs(8);

/// `AcquireNextFrame` treats `u32::MAX` as INFINITE; longer waits are capped
/// one below it and re-issued by the caller's loop.
pub(crate) const MAX_WAIT_MS: u32 = u32::MAX - 1;

/// Whether the latest desktop image has been copied, and if not, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Staleness {
    /// The last copied frame holds every content update acquired so far.
    Fresh,
    /// A content update was acquired without a desktop image, so the desktop
    /// is ahead of the last copied frame.
    Missed,
    /// The session is new and nothing has been copied from it yet.
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
}

/// What to do with an acquired frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameAction {
    /// Copy the image into the pool and emit it, stamped with its slot.
    Copy,
    /// Release without copying and report `Unchanged`: only the pointer
    /// moved.
    Unchanged,
    /// Release without copying and look again at the next slot: content
    /// changed, but DXGI handed over no image to copy.
    Missed,
    /// Release without copying and recreate the session: a change came
    /// without an image, and the slot after it brought no content update to
    /// deliver it with.
    Resync,
}

/// Decides what to do with `frame` and updates `staleness`.
///
/// Only frames with new content are copied. DXGI documents a pointer-only
/// update as "the desktop image was not updated", and on the first acquire
/// of a new session that surface has never been written: copying it encodes
/// a black frame. A new session therefore waits for its first content update.
///
/// Acquires are paced (see [`FramePacer`](crate::schedule::FramePacer)), and
/// DXGI accumulates updates while no frame is held, so a content update
/// always carries the latest desktop and no change is ever dropped. The one
/// exception is a content update without an image: the next slot then looks
/// again without waiting, and if it finds no content (a timeout or a pointer
/// move) the desktop has gone quiet and only a new session, whose first
/// content update carries the whole desktop, can deliver the change.
pub(crate) fn decide(frame: Acquired, staleness: &mut Staleness) -> FrameAction {
    if !frame.content_changed {
        return if *staleness == Staleness::Missed {
            FrameAction::Resync
        } else {
            FrameAction::Unchanged
        };
    }
    if !frame.has_image {
        *staleness = Staleness::Missed;
        return FrameAction::Missed;
    }
    *staleness = Staleness::Fresh;
    FrameAction::Copy
}

/// What to do when `AcquireNextFrame` times out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeoutAction {
    /// Report `Timeout`.
    Report,
    /// A change came without an image and a whole slot passed without
    /// another: recreate the session to deliver it.
    Resync,
}

/// Decides what an acquire timeout means.
pub(crate) fn on_timeout(staleness: Staleness) -> TimeoutAction {
    if staleness == Staleness::Missed {
        TimeoutAction::Resync
    } else {
        TimeoutAction::Report
    }
}

/// The `AcquireNextFrame` timeout in milliseconds: the caller's `remaining`
/// time, or zero after a change without an image (anything that would deliver
/// it has accumulated by the slot this acquire runs at).
/// Rounds up so a sub-millisecond wait does not become a busy poll, and
/// never returns INFINITE.
pub(crate) fn acquire_timeout_ms(remaining: Duration, staleness: Staleness) -> u32 {
    if staleness == Staleness::Missed {
        return 0;
    }
    u32::try_from(remaining.as_micros().div_ceil(1000))
        .unwrap_or(MAX_WAIT_MS)
        .min(MAX_WAIT_MS)
}

/// What one `AcquireNextFrame` call produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Acquire {
    /// `DXGI_ERROR_WAIT_TIMEOUT`.
    Timeout,
    /// A frame, which folded `accumulated` desktop updates.
    Frame { facts: Acquired, accumulated: u32 },
}

/// What the duplicator does after an acquire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Copy the image and emit it with `timestamp`. `first_of_session` when
    /// it is the session's first copied frame.
    Copy {
        timestamp: Timestamp,
        first_of_session: bool,
    },
    /// Report `CaptureEvent::Unchanged`.
    Unchanged,
    /// Report `CaptureEvent::Timeout`.
    Timeout,
    /// Keep waiting; the next slot looks again.
    Missed,
    /// Recreate the session.
    Resync,
}

/// The duplicator's pure state: pacing, staleness, and counters. The Windows
/// duplicator and the tests' simulation make the same calls in the same
/// order: [`before_acquire`](Self::before_acquire), then
/// [`acquire_timeout_ms`](Self::acquire_timeout_ms) and
/// [`after_acquire`](Self::after_acquire) around `AcquireNextFrame`, and
/// [`new_session`](Self::new_session) whenever a session is created.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pacing {
    pub(crate) pacer: FramePacer,
    pub(crate) staleness: Staleness,
    pub(crate) stats: CaptureStats,
}

impl Pacing {
    /// Pacing for `target_fps` on a QPC clock at `qpc_frequency`.
    pub(crate) fn new(target_fps: NonZeroU32, qpc_frequency: i64) -> Self {
        Self {
            pacer: FramePacer::new(target_fps, qpc_frequency),
            staleness: Staleness::NewSession,
            stats: CaptureStats::default(),
        }
    }

    /// Called before acquiring at QPC time `now`: the QPC time to sleep until
    /// first, or `None` to acquire now (counting any slots that passed).
    pub(crate) fn before_acquire(&mut self, now: i64) -> Option<i64> {
        let wait = self.pacer.wait_until(now);
        if wait.is_none() {
            self.stats.record_late(self.pacer.late_slots(now));
        }
        wait
    }

    /// The `AcquireNextFrame` timeout; see [`acquire_timeout_ms`].
    pub(crate) fn acquire_timeout_ms(&self, remaining: Duration) -> u32 {
        acquire_timeout_ms(remaining, self.staleness)
    }

    /// Decides what an acquire that returned at QPC time `now` becomes.
    pub(crate) fn after_acquire(&mut self, acquired: Acquire, now: i64) -> Outcome {
        let (facts, accumulated) = match acquired {
            Acquire::Timeout => {
                self.stats.record_timeout();
                return match on_timeout(self.staleness) {
                    TimeoutAction::Resync => Outcome::Resync,
                    TimeoutAction::Report => {
                        self.pacer.defer(now);
                        Outcome::Timeout
                    }
                };
            }
            Acquire::Frame { facts, accumulated } => (facts, accumulated),
        };
        let before = self.staleness;
        let action = decide(facts, &mut self.staleness);
        self.stats.record(facts, before, accumulated);
        match action {
            FrameAction::Copy => Outcome::Copy {
                timestamp: self.pacer.claim(now),
                first_of_session: before == Staleness::NewSession,
            },
            FrameAction::Unchanged => {
                self.pacer.defer(now);
                Outcome::Unchanged
            }
            FrameAction::Missed => {
                self.pacer.defer(now);
                Outcome::Missed
            }
            FrameAction::Resync => Outcome::Resync,
        }
    }

    /// Records a session created at QPC time `now`. The engine may have
    /// refreshed a keyframe at the current time while there was no session,
    /// so the slot containing `now` is not used.
    pub(crate) fn new_session(&mut self, now: i64) {
        self.staleness = Staleness::NewSession;
        self.pacer.defer(now);
    }

    /// Whether a session with nothing to show (`has_frame` is false: nothing
    /// of the current size has been copied) has waited `waited`, at least
    /// `limit`, without a content update and should be re-duplicated. A
    /// pointer-only frame is never copied instead, since its surface can be
    /// black.
    pub(crate) fn empty_session_overdue(
        &self,
        has_frame: bool,
        waited: Duration,
        limit: Duration,
    ) -> bool {
        self.staleness == Staleness::NewSession && !has_frame && waited >= limit
    }
}

/// The wait for the next empty session after one ran out of `wait`.
pub(crate) fn next_empty_wait(wait: Duration) -> Duration {
    wait.saturating_mul(2).min(MAX_EMPTY_SESSION_WAIT)
}

/// Frame counters for the duplicator's periodic summary log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CaptureStats {
    /// Frames copied into the pool and emitted.
    pub(crate) copied: u64,
    /// Desktop updates DXGI folded into the copied frames beyond one each
    /// (`AccumulatedFrames - 1`): what pacing saved acquiring separately.
    pub(crate) coalesced: u64,
    /// Acquires that ended without new content: a timeout or a pointer move.
    pub(crate) unchanged_slots: u64,
    /// Of those, updates where only the pointer moved.
    pub(crate) pointer_only: u64,
    /// Pointer-only updates on a session with nothing copied yet; earlier
    /// versions copied these and encoded black frames.
    pub(crate) new_session_pointer_only: u64,
    /// Slots that passed before the acquire meant for them started, because
    /// the encode or the thread ran late.
    pub(crate) late_slots: u64,
    /// Content updates that came without a desktop resource.
    pub(crate) no_image: u64,
    /// Sessions that ended (lost, or re-duplicated for having nothing to
    /// show) before their first content update.
    pub(crate) contentless_sessions: u64,
    /// Sessions recreated to deliver a change that came without an image.
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
    /// Counts one acquired frame that folded `accumulated` desktop updates,
    /// given the staleness before [`decide`] ran.
    pub(crate) fn record(&mut self, frame: Acquired, before: Staleness, accumulated: u32) {
        if !frame.content_changed {
            if before == Staleness::NewSession {
                self.new_session_pointer_only = self.new_session_pointer_only.saturating_add(1);
            }
            self.pointer_only = self.pointer_only.saturating_add(1);
            self.unchanged_slots = self.unchanged_slots.saturating_add(1);
        } else if !frame.has_image {
            self.no_image = self.no_image.saturating_add(1);
        } else {
            self.copied = self.copied.saturating_add(1);
            self.coalesced = self
                .coalesced
                .saturating_add(u64::from(accumulated.saturating_sub(1)));
        }
    }

    /// Counts an acquire that timed out.
    pub(crate) const fn record_timeout(&mut self) {
        self.unchanged_slots = self.unchanged_slots.saturating_add(1);
    }

    /// Counts slots skipped by a late acquire.
    pub(crate) const fn record_late(&mut self, slots: u64) {
        self.late_slots = self.late_slots.saturating_add(slots);
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use blink_ring::media::Timestamp;
    use blink_sys::QpcClock;

    use super::*;
    use crate::schedule::FramePacer;

    const QPC_HZ: i64 = 10_000_000;
    const POLL: Duration = Duration::from_millis(100);

    const fn changed() -> Acquired {
        Acquired {
            content_changed: true,
            has_image: true,
        }
    }

    const fn pointer() -> Acquired {
        Acquired {
            content_changed: false,
            has_image: true,
        }
    }

    const fn blank() -> Acquired {
        Acquired {
            content_changed: true,
            has_image: false,
        }
    }

    #[test]
    fn new_session_waits_for_content_before_copying() {
        let mut staleness = Staleness::NewSession;
        assert_eq!(decide(pointer(), &mut staleness), FrameAction::Unchanged);
        assert_eq!(staleness, Staleness::NewSession);
        assert_eq!(on_timeout(staleness), TimeoutAction::Report);
        assert_eq!(decide(changed(), &mut staleness), FrameAction::Copy);
        assert_eq!(staleness, Staleness::Fresh);
    }

    #[test]
    fn pointer_only_frames_are_never_copied() {
        for start in [Staleness::Fresh, Staleness::NewSession, Staleness::Missed] {
            let mut staleness = start;
            assert_ne!(decide(pointer(), &mut staleness), FrameAction::Copy);
        }
    }

    #[test]
    fn a_change_without_an_image_is_looked_for_again_then_resynced() {
        let mut staleness = Staleness::Fresh;
        assert_eq!(decide(blank(), &mut staleness), FrameAction::Missed);
        assert_eq!(staleness, Staleness::Missed);
        assert_eq!(acquire_timeout_ms(POLL, staleness), 0);
        assert_eq!(on_timeout(staleness), TimeoutAction::Resync);
        assert_eq!(decide(pointer(), &mut staleness), FrameAction::Resync);
        assert_eq!(decide(changed(), &mut staleness), FrameAction::Copy);
        assert_eq!(on_timeout(staleness), TimeoutAction::Report);
    }

    #[test]
    fn idle_without_a_missed_change_just_times_out() {
        assert_eq!(on_timeout(Staleness::Fresh), TimeoutAction::Report);
        assert_eq!(on_timeout(Staleness::NewSession), TimeoutAction::Report);
    }

    #[test]
    fn timeout_rounds_up_caps_and_handles_zero() {
        let fresh = Staleness::Fresh;
        assert_eq!(acquire_timeout_ms(Duration::ZERO, fresh), 0);
        assert_eq!(acquire_timeout_ms(Duration::from_micros(1), fresh), 1);
        assert_eq!(acquire_timeout_ms(Duration::from_micros(1500), fresh), 2);
        assert_eq!(acquire_timeout_ms(Duration::from_millis(16), fresh), 16);
        assert_eq!(acquire_timeout_ms(Duration::MAX, fresh), MAX_WAIT_MS);
        assert_eq!(
            acquire_timeout_ms(Duration::MAX, Staleness::NewSession),
            MAX_WAIT_MS
        );
        assert_eq!(acquire_timeout_ms(Duration::MAX, Staleness::Missed), 0);
    }

    #[test]
    fn stats_count_each_kind_of_frame() {
        let mut stats = CaptureStats::default();
        stats.record(changed(), Staleness::Fresh, 3);
        stats.record(changed(), Staleness::NewSession, 1);
        stats.record(pointer(), Staleness::Fresh, 1);
        stats.record(pointer(), Staleness::NewSession, 2);
        stats.record(blank(), Staleness::Fresh, 1);
        stats.record_timeout();
        stats.record_late(2);
        stats.record_late(1);
        stats.record_resync(Duration::from_millis(3));
        stats.record_resync(Duration::from_millis(1));
        stats.record_first_content(Duration::from_millis(40));
        stats.record_first_content(Duration::from_millis(20));
        stats.record_contentless_session(Duration::from_millis(30));
        assert_eq!(
            stats,
            CaptureStats {
                copied: 2,
                coalesced: 2,
                unchanged_slots: 3,
                pointer_only: 2,
                new_session_pointer_only: 1,
                late_slots: 3,
                no_image: 1,
                contentless_sessions: 1,
                resyncs: 2,
                slowest_resync: Duration::from_millis(3),
                slowest_first_content: Duration::from_millis(40),
                empty_resyncs: 0,
            }
        );
    }

    /// A desktop update in [`Sim`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        Content,
        Pointer,
        /// A content update DXGI hands over without an image.
        Blank,
    }

    /// Drives [`Pacing`] with the calls the duplicator makes, in the same
    /// order, against a model of DXGI: updates accumulate while no frame is
    /// held, an acquire returns at once if any are pending and otherwise
    /// blocks until the next one or its timeout, and each new session's first
    /// acquire returns `first_frames[n]` (the whole desktop past the end of
    /// the list). It also models the engine, which on every `Timeout` and
    /// `Unchanged` re-encodes its last frame stamped with the current time,
    /// as a static-screen keyframe refresh does. Lost sessions, their retry
    /// backoff, and resizes are not modelled.
    struct Sim {
        fps: u32,
        /// Desktop updates as (QPC time, kind), sorted by time.
        updates: Vec<(i64, Kind)>,
        /// Simulated QPC time to stop at.
        end: i64,
        /// How long encoding a copied frame keeps the thread busy.
        encode: i64,
        /// How late a sleep until a slot wakes up.
        overshoot: i64,
        /// What each new session's first acquire holds, by session number;
        /// `None` is nothing.
        first_frames: Vec<Option<Kind>>,
    }

    #[derive(Debug, Default)]
    struct Run {
        /// Emitted frame timestamps.
        stamps: Vec<Timestamp>,
        /// QPC time each emitted frame was acquired at.
        copied_at: Vec<i64>,
        /// Every PTS the engine encoded: frames and keyframe refreshes.
        encoded: Vec<Timestamp>,
        /// QPC time of each acquire timeout.
        timeouts: Vec<i64>,
        /// `AcquireNextFrame` calls, including timeouts.
        acquires: usize,
        resyncs: usize,
        empty_resyncs: usize,
        sessions: usize,
        late: u64,
    }

    /// What DXGI has accumulated since the last acquire.
    fn accumulate(pending: &mut Option<Acquired>, kind: Kind) {
        let frame = pending.get_or_insert(pointer());
        match kind {
            Kind::Content => *frame = changed(),
            Kind::Blank => *frame = blank(),
            Kind::Pointer => {}
        }
    }

    impl Sim {
        fn new(fps: u32, updates: Vec<(i64, Kind)>, end: i64) -> Self {
            let mut updates = updates;
            updates.sort_by_key(|&(at, _)| at);
            Self {
                fps,
                updates,
                end,
                encode: 0,
                overshoot: 0,
                first_frames: Vec::new(),
            }
        }

        /// Creates a session at `now`, as `recreate` does.
        fn new_session(&self, p: &mut Pacing, out: &mut Run, now: i64) -> Option<Acquired> {
            p.new_session(now);
            let first = self
                .first_frames
                .get(out.sessions)
                .copied()
                .unwrap_or(Some(Kind::Content));
            out.sessions += 1;
            let mut pending = None;
            if let Some(kind) = first {
                accumulate(&mut pending, kind);
            }
            pending
        }

        fn run(&self) -> Run {
            let mut p = Pacing::new(NonZeroU32::new(self.fps).unwrap(), QPC_HZ);
            let clock = QpcClock::new(QPC_HZ).unwrap();
            let mut out = Run::default();
            let mut now = 0;
            let mut next = 0;
            let mut pending = self.new_session(&mut p, &mut out, now);
            let mut session_started = now;
            let mut has_frame = false;
            let mut empty_wait = EMPTY_SESSION_WAIT;
            while now < self.end {
                if let Some(slot) = p.before_acquire(now) {
                    now = slot + self.overshoot;
                    continue;
                }
                let wait = i64::from(p.acquire_timeout_ms(POLL)) * QPC_HZ / 1000;
                if pending.is_none()
                    && let Some(&(at, _)) = self.updates.get(next)
                    && at > now
                    && at <= now + wait
                {
                    now = at;
                }
                while let Some(&(at, kind)) = self.updates.get(next)
                    && at <= now
                {
                    accumulate(&mut pending, kind);
                    next += 1;
                }
                out.acquires += 1;
                let acquired = if let Some(facts) = pending.take() {
                    Acquire::Frame {
                        facts,
                        accumulated: 1,
                    }
                } else {
                    now += wait;
                    out.timeouts.push(now);
                    Acquire::Timeout
                };
                match p.after_acquire(acquired, now) {
                    Outcome::Copy { timestamp, .. } => {
                        out.stamps.push(timestamp);
                        out.copied_at.push(now);
                        out.encoded.push(timestamp);
                        has_frame = true;
                        empty_wait = EMPTY_SESSION_WAIT;
                        now += self.encode;
                    }
                    Outcome::Unchanged | Outcome::Timeout => {
                        let refresh = clock.to_ticks(now);
                        if has_frame && out.encoded.last().is_none_or(|&last| refresh > last) {
                            out.encoded.push(refresh);
                        }
                        let waited = clock.to_duration(now - session_started);
                        if p.empty_session_overdue(has_frame, waited, empty_wait) {
                            out.empty_resyncs += 1;
                            empty_wait = next_empty_wait(empty_wait);
                            pending = self.new_session(&mut p, &mut out, now);
                            session_started = now;
                        }
                    }
                    Outcome::Missed => {}
                    Outcome::Resync => {
                        out.resyncs += 1;
                        pending = self.new_session(&mut p, &mut out, now);
                        session_started = now;
                    }
                }
            }
            out.late = p.stats.late_slots;
            self.check_grid(&out);
            out
        }

        /// Every stamp is the start of a slot, within half an interval of the
        /// moment its image was acquired, and strictly after everything the
        /// engine encoded before it, keyframe refreshes included, so the
        /// engine never has to nudge a frame off the grid.
        fn check_grid(&self, out: &Run) {
            let pacer = FramePacer::new(NonZeroU32::new(self.fps).unwrap(), QPC_HZ);
            let clock = QpcClock::new(QPC_HZ).unwrap();
            let half = Timestamp::TICKS_PER_SECOND / i64::from(self.fps) / 2 + 1;
            for (&stamp, &at) in out.stamps.iter().zip(&out.copied_at) {
                assert_eq!(stamp, pacer.timestamp(self.slot(stamp)), "off the grid");
                let error = stamp.ticks() - clock.to_ticks(at).ticks();
                assert!(error.abs() <= half, "stamped {error} ticks from capture");
            }
            assert!(
                out.encoded.windows(2).all(|pair| pair[0] < pair[1]),
                "encoded timestamps must strictly increase: {:?}",
                out.encoded
            );
        }

        /// The slot a timestamp from [`FramePacer::timestamp`] starts.
        fn slot(&self, stamp: Timestamp) -> i64 {
            (stamp.ticks() * i64::from(self.fps) + QPC_HZ - 1) / QPC_HZ
        }

        /// The gaps between consecutive frames, in slots.
        fn gaps(&self, out: &Run) -> Vec<i64> {
            out.stamps
                .windows(2)
                .map(|pair| self.slot(pair[1]) - self.slot(pair[0]))
                .collect()
        }
    }

    /// Updates of `kind` at `hz` from `from` (inclusive) to `to` (exclusive).
    fn at_hz(hz: i64, from: i64, to: i64, kind: Kind) -> Vec<(i64, Kind)> {
        (0..=(to - from) * hz / QPC_HZ)
            .map(|i| from + i * QPC_HZ / hz)
            .filter(|&at| at < to)
            .map(|at| (at, kind))
            .collect()
    }

    const SECOND: i64 = QPC_HZ;

    #[test]
    fn motion_faster_than_the_frame_rate_fills_every_slot() {
        for (hz, fps) in [
            (144, 60),
            (240, 60),
            (165, 60),
            (120, 60),
            (144, 30),
            (240, 144),
        ] {
            for overshoot in [0, 300, 4_000] {
                let mut sim = Sim::new(fps, at_hz(hz, 0, 2 * SECOND, Kind::Content), 2 * SECOND);
                sim.overshoot = overshoot;
                let out = sim.run();
                assert_eq!(out.resyncs, 0, "{hz} Hz -> {fps} fps");
                assert_eq!(out.late, 0, "{hz} Hz -> {fps} fps");
                assert!(
                    sim.gaps(&out).iter().all(|&gap| gap == 1),
                    "{hz} Hz -> {fps} fps, overshoot {overshoot}: {:?}",
                    sim.gaps(&out)
                );
                let expected = usize::try_from(2 * fps).unwrap();
                assert!(
                    out.stamps.len().abs_diff(expected) <= 1,
                    "{hz} Hz -> {fps} fps copied {}",
                    out.stamps.len()
                );
                // One acquire per slot: the updates in between accumulate.
                assert!(out.acquires <= expected + 1, "{} acquires", out.acquires);
            }
        }
    }

    #[test]
    fn same_rate_desktop_fills_every_slot_at_any_phase_and_jitter() {
        let interval = SECOND / 60;
        for phase in [0, 1, interval / 4, interval / 2, interval - 1] {
            for jitter in [0, interval / 20] {
                let updates = (0..120)
                    .map(|i| {
                        let wobble = if i % 2 == 0 { jitter } else { -jitter };
                        (i * SECOND / 60 + phase + wobble + interval, Kind::Content)
                    })
                    .collect();
                let sim = Sim::new(60, updates, 2 * SECOND + interval);
                let out = sim.run();
                assert_eq!(out.resyncs, 0);
                // After the session's initial frame, which takes slot 0.
                assert!(
                    sim.gaps(&out).iter().skip(1).all(|&gap| gap == 1),
                    "phase {phase}, jitter {jitter}: {:?}",
                    sim.gaps(&out)
                );
            }
        }
    }

    #[test]
    fn slower_desktop_keeps_every_update_on_the_grid() {
        // Phases away from a slot boundary: updates that straddle one by a
        // count of jitter alternate between the slots either side of it.
        for (hz, gap) in [(30, 2), (20, 3)] {
            for phase in [1_000, SECOND / 120, SECOND / 61] {
                let updates = at_hz(hz, SECOND / 2 + phase, 3 * SECOND, Kind::Content);
                let count = updates.len();
                let sim = Sim::new(60, updates, 3 * SECOND);
                let out = sim.run();
                // Every update, after the session's initial frame.
                assert_eq!(out.stamps.len(), count + 1, "{hz} Hz phase {phase}");
                assert!(
                    sim.gaps(&out).iter().skip(1).all(|&g| g == gap),
                    "{hz} Hz phase {phase}: {:?}",
                    sim.gaps(&out)
                );
                assert_eq!(out.resyncs, 0);
            }
        }
        // 24 Hz into 60 fps alternates two and three slots.
        let sim = Sim::new(60, at_hz(24, 0, 2 * SECOND, Kind::Content), 2 * SECOND);
        let out = sim.run();
        assert_eq!(out.stamps.len(), 48);
        assert!(
            sim.gaps(&out).iter().skip(1).all(|&g| g == 2 || g == 3),
            "{:?}",
            sim.gaps(&out)
        );
    }

    #[test]
    fn a_change_right_before_the_desktop_goes_idle_lands_at_the_next_slot() {
        let interval = SECOND / 60;
        // Sweep the last change across a whole slot.
        for offset in (0..interval).step_by(997) {
            let last = SECOND + offset;
            let mut updates = at_hz(144, 0, SECOND, Kind::Content);
            updates.push((last, Kind::Content));
            let sim = Sim::new(60, updates, 3 * SECOND);
            let out = sim.run();
            assert_eq!(out.resyncs, 0, "offset {offset}");
            let &copied = out.copied_at.last().unwrap();
            assert!(
                copied >= last,
                "offset {offset}: the change was never copied"
            );
            assert!(copied <= last + interval, "offset {offset}: copied late");
        }
    }

    #[test]
    fn a_static_screen_copies_nothing() {
        let sim = Sim::new(60, Vec::new(), 5 * SECOND);
        let out = sim.run();
        assert_eq!(out.stamps.len(), 1, "only the session's first frame");
        assert_eq!(out.resyncs, 0);
        // One acquire per poll timeout, not one per slot.
        assert!(out.acquires <= 52, "{} acquires", out.acquires);
    }

    #[test]
    fn pointer_motion_copies_nothing_and_does_not_outpace_the_slots() {
        let sim = Sim::new(60, at_hz(1000, 0, 2 * SECOND, Kind::Pointer), 2 * SECOND);
        let out = sim.run();
        assert_eq!(out.stamps.len(), 1);
        assert!(out.acquires <= 121, "{} acquires", out.acquires);
    }

    #[test]
    fn pointer_motion_does_not_hold_back_a_content_change() {
        let interval = SECOND / 60;
        for offset in (0..interval).step_by(1_231) {
            let change = SECOND + offset;
            let mut updates = at_hz(1000, 0, 2 * SECOND, Kind::Pointer);
            updates.push((change, Kind::Content));
            let sim = Sim::new(60, updates, 2 * SECOND);
            let out = sim.run();
            assert_eq!(out.stamps.len(), 2, "offset {offset}");
            let copied = out.copied_at[1];
            assert!(
                copied >= change && copied <= change + interval,
                "offset {offset}"
            );
        }
    }

    #[test]
    fn an_overrun_skips_slots_instead_of_bursting() {
        let interval = SECOND / 60;
        for encode in [interval * 3 / 2, interval * 5 / 2, interval * 4] {
            let mut sim = Sim::new(60, at_hz(144, 0, 2 * SECOND, Kind::Content), 2 * SECOND);
            sim.encode = encode;
            let out = sim.run();
            assert!(out.late > 0, "encode {encode}");
            // `check_grid` holds each stamp within half an interval of its
            // capture: no catching up on slots that already passed.
            for pair in out.copied_at.windows(2) {
                assert!(pair[1] - pair[0] >= encode, "encode {encode}: burst");
            }
            let max_gap = (encode + interval - 1) / interval + 1;
            assert!(sim.gaps(&out).iter().all(|&gap| gap >= 1 && gap <= max_gap));
        }
    }

    #[test]
    fn a_change_after_a_timeout_lands_after_the_keyframe_refresh() {
        let interval = SECOND / 60;
        let idle = Sim::new(60, Vec::new(), 2 * SECOND).run();
        // The engine refreshes a keyframe at each timeout, mid-slot.
        for &timeout in idle.timeouts.iter().take(5) {
            for after in [
                1,
                100,
                interval / 4,
                interval / 2 - 1,
                interval / 2 + 1,
                interval - 1,
            ] {
                let change = timeout + after;
                let sim = Sim::new(60, vec![(change, Kind::Content)], 2 * SECOND);
                // `check_grid` fails if the frame is stamped at or before
                // the refresh.
                let out = sim.run();
                assert_eq!(out.stamps.len(), 2);
                let copied = out.copied_at[1];
                assert!(copied >= change && copied <= change + interval);
            }
        }
    }

    #[test]
    fn a_new_session_whose_first_frame_is_pointer_only_is_reduplicated() {
        // PR #6 hardware logs: fresh sessions often start with a
        // pointer-only frame, whose surface can be black.
        let mut sim = Sim::new(60, Vec::new(), 10 * SECOND);
        sim.first_frames = vec![Some(Kind::Pointer), None, Some(Kind::Content)];
        let out = sim.run();
        assert_eq!(out.empty_resyncs, 2);
        assert_eq!(out.stamps.len(), 1, "pointer-only frames are never copied");
        // Waits of one then two seconds, at poll granularity.
        let copied = out.copied_at[0];
        assert!(
            (3 * SECOND..=3 * SECOND + SECOND / 2).contains(&copied),
            "{copied}"
        );

        // A session that never gets content backs off to every 8 s.
        let mut sim = Sim::new(60, Vec::new(), 30 * SECOND);
        sim.first_frames = vec![Some(Kind::Pointer); 100];
        let out = sim.run();
        assert_eq!(out.stamps.len(), 0);
        assert_eq!(out.empty_resyncs, 5, "1 + 2 + 4 + 8 + 8 s");

        // Content before the wait runs out needs no re-duplication.
        let mut sim = Sim::new(60, vec![(SECOND / 2, Kind::Content)], 3 * SECOND);
        sim.first_frames = vec![Some(Kind::Pointer)];
        let out = sim.run();
        assert_eq!(out.empty_resyncs, 0);
        assert_eq!(out.stamps.len(), 1);
    }

    #[test]
    fn a_change_without_an_image_then_idle_resyncs_once() {
        let mut updates = at_hz(144, 0, SECOND, Kind::Content);
        updates.push((SECOND, Kind::Blank));
        let sim = Sim::new(60, updates.clone(), 3 * SECOND);
        let out = sim.run();
        assert_eq!(out.resyncs, 1);
        let &copied = out.copied_at.last().unwrap();
        // The next slot looks again, then the new session skips one slot.
        assert!(copied > SECOND && copied <= SECOND + SECOND / 20);

        // With the pointer moving through the idle period.
        let mut with_pointer = updates.clone();
        with_pointer.extend(at_hz(500, SECOND + 1, 3 * SECOND, Kind::Pointer));
        assert_eq!(Sim::new(60, with_pointer, 3 * SECOND).run().resyncs, 1);

        // Motion right after it delivers the change without a resync.
        let mut motion = updates;
        motion.extend(at_hz(144, SECOND + 1, 2 * SECOND, Kind::Content));
        assert_eq!(Sim::new(60, motion, 3 * SECOND).run().resyncs, 0);
    }
}
