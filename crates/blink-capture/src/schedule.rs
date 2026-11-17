//! Frame-rate decimation on QPC present times.
//!
//! Desktop Duplication delivers a frame per composed desktop update, which on a
//! high-refresh monitor running a game is the refresh rate. The scheduler
//! admits at most `target_fps` of them per second so the rest are released
//! without being copied or encoded.

use std::num::NonZeroU32;

/// Decides which present times to keep so the output rate stays at or below a
/// target frame rate.
///
/// Admitted times sit on a grid with one slot per target frame interval. A
/// frame is admitted when it reaches its slot, give or take an eighth of an
/// interval of tolerance so jitter in a refresh rate that is an exact multiple of the
/// target (240 Hz into 60 fps) does not push every pick one source frame late.
/// Each admitted frame consumes one slot, so the long-run rate never exceeds
/// the target. If a frame arrives after the following slot has already passed
/// (a pause in desktop updates), the grid restarts from that frame instead of
/// admitting a burst to catch up.
///
/// All arithmetic is in QPC counts scaled by `target_fps`, which makes the
/// interval exactly `qpc_frequency` and avoids accumulating rounding error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameScheduler {
    fps: i128,
    interval: i128,
    tolerance: i128,
    next_due: Option<i128>,
}

impl FrameScheduler {
    /// Builds a scheduler for `target_fps` on a QPC clock running at
    /// `qpc_frequency` counts per second (must be positive).
    pub(crate) fn new(target_fps: NonZeroU32, qpc_frequency: i64) -> Self {
        let interval = i128::from(qpc_frequency.max(1));
        Self {
            fps: i128::from(target_fps.get()),
            interval,
            tolerance: interval / 8,
            next_due: None,
        }
    }

    /// Returns whether a frame presented at QPC time `qpc` should be kept, and
    /// if so, consumes its slot.
    pub(crate) fn admit(&mut self, qpc: i64) -> bool {
        let now = i128::from(qpc) * self.fps;
        let due = self.live_due(now);
        if due.is_some_and(|due| now < due - self.tolerance) {
            return false;
        }
        let next = due
            .map(|due| due + self.interval)
            .filter(|&next| next > now)
            .unwrap_or(now + self.interval);
        self.next_due = Some(next);
        true
    }

    /// Whether [`admit`](Self::admit) would keep a frame at `qpc`, without
    /// consuming a slot.
    pub(crate) fn is_due(&self, qpc: i64) -> bool {
        let now = i128::from(qpc) * self.fps;
        self.live_due(now)
            .is_none_or(|due| now >= due - self.tolerance)
    }

    /// The earliest QPC time [`admit`](Self::admit) would keep a frame at, or
    /// `None` if it would keep one at any time.
    pub(crate) fn next_admit_qpc(&self) -> Option<i64> {
        let earliest = self.next_due? - self.tolerance;
        // Ceiling division: the first whole QPC count at or after the slot.
        let qpc = -(-earliest).div_euclid(self.fps);
        Some(i64::try_from(qpc).unwrap_or(if qpc < 0 { i64::MIN } else { i64::MAX }))
    }

    /// The pending slot, unless it is implausibly far ahead of `now` (scaled).
    /// That means the clock went backwards, which QPC should never do; the
    /// grid then resynchronizes instead of dropping frames until it catches
    /// up.
    fn live_due(&self, now: i128) -> Option<i128> {
        self.next_due.filter(|&due| due - now <= 2 * self.interval)
    }

    /// Forgets the grid, so the next frame is admitted unconditionally. Called
    /// when a duplication session is recreated.
    pub(crate) const fn reset(&mut self) {
        self.next_due = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QPC_HZ: i64 = 10_000_000;

    fn scheduler(fps: u32) -> FrameScheduler {
        FrameScheduler::new(NonZeroU32::new(fps).unwrap(), QPC_HZ)
    }

    /// Feeds `count` frames at `source_hz` and returns the admitted times.
    fn run(scheduler: &mut FrameScheduler, source_hz: i64, count: i64) -> Vec<i64> {
        (0..count)
            .map(|i| i * QPC_HZ / source_hz)
            .filter(|&t| scheduler.admit(t))
            .collect()
    }

    #[test]
    fn first_frame_is_always_admitted() {
        let mut s = scheduler(60);
        assert!(s.admit(123_456));
    }

    #[test]
    fn exact_multiple_keeps_every_nth_frame() {
        let mut s = scheduler(60);
        let admitted = run(&mut s, 240, 240);
        assert_eq!(admitted.len(), 60);
        let expected: Vec<i64> = (0..60).map(|i| i * 4 * QPC_HZ / 240).collect();
        assert_eq!(admitted, expected);
    }

    #[test]
    fn non_multiple_source_never_exceeds_target() {
        for source in [75, 120, 144, 165, 170, 360] {
            for fps in [24, 30, 60] {
                let mut s = scheduler(fps);
                let admitted = run(&mut s, source, source * 10);
                let count = i64::try_from(admitted.len()).unwrap();
                let target = i64::from(fps) * 10;
                assert!(
                    count <= target + 1,
                    "{source} Hz -> {fps} fps admitted {count}"
                );
                assert!(
                    count >= target * 9 / 10,
                    "{source} Hz -> {fps} fps admitted {count}"
                );
                let min_gap = QPC_HZ / i64::from(fps) / 2;
                for pair in admitted.windows(2) {
                    assert!(
                        pair[1] - pair[0] >= min_gap,
                        "{source} Hz -> {fps} fps bunched"
                    );
                }
            }
        }
    }

    #[test]
    fn slower_source_keeps_everything() {
        let mut s = scheduler(60);
        assert_eq!(run(&mut s, 30, 300).len(), 300);
    }

    #[test]
    fn jitter_just_before_the_slot_is_tolerated() {
        let mut s = scheduler(60);
        let interval = QPC_HZ / 60;
        assert!(s.admit(0));
        assert!(s.admit(interval - interval / 10));
        assert!(!s.admit(interval + interval / 2));
    }

    #[test]
    fn resumes_after_a_pause_without_a_burst() {
        let mut s = scheduler(60);
        let interval = QPC_HZ / 60;
        assert!(s.admit(0));
        // Nothing for a second, then 240 Hz again.
        let resume = QPC_HZ;
        assert!(s.admit(resume));
        assert!(!s.admit(resume + interval / 4));
        assert!(!s.admit(resume + interval / 2));
    }

    #[test]
    fn backwards_clock_resynchronizes() {
        let mut s = scheduler(60);
        assert!(s.admit(QPC_HZ * 100));
        assert!(s.admit(QPC_HZ));
        assert!(!s.admit(QPC_HZ + 1));
    }

    #[test]
    fn next_admit_qpc_matches_admit() {
        let mut s = scheduler(60);
        assert_eq!(s.next_admit_qpc(), None);
        assert!(s.is_due(0));
        assert!(s.admit(0));
        let earliest = s.next_admit_qpc().unwrap();
        // Slot at 1/60 s minus an eighth of an interval, rounded up.
        assert_eq!(earliest, 145_834);
        assert!(!s.is_due(earliest - 1));
        assert!(s.is_due(earliest));
        assert!(!s.admit(earliest - 1));
        assert!(s.admit(earliest));
    }

    #[test]
    fn is_due_does_not_consume_the_slot() {
        let mut s = scheduler(60);
        assert!(s.admit(0));
        let earliest = s.next_admit_qpc().unwrap();
        assert!(s.is_due(earliest));
        assert!(s.is_due(earliest));
        assert!(s.admit(earliest));
    }

    #[test]
    fn reset_admits_next_frame() {
        let mut s = scheduler(60);
        assert!(s.admit(0));
        assert!(!s.admit(1));
        s.reset();
        assert!(s.admit(2));
    }
}
