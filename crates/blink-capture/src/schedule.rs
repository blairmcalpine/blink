//! Frame pacing on the QPC clock.
//!
//! Desktop Duplication delivers a frame per composed desktop update, which on a
//! high-refresh monitor running a game is the refresh rate. Rather than
//! acquiring every update and dropping most of them, the capture thread
//! acquires at most once per slot of the target frame rate and lets DXGI
//! accumulate the updates in between: an acquire returns the latest desktop
//! image, so nothing that changed is lost and nothing needs re-delivering.

use std::num::NonZeroU32;

use blink_ring::media::Timestamp;

/// Divides the QPC timeline into slots of one target frame interval and
/// decides when the next acquire may start and which slot a frame takes.
///
/// Slots tile the timeline from QPC zero: slot `n` starts at
/// `n / target_fps` seconds. A copied frame takes the slot containing the
/// time it was acquired, and is stamped with that slot's start, so frames
/// during continuous motion are exactly one interval apart and a stamp is
/// never later than the moment its image was taken. After a frame takes a
/// slot, the next acquire waits for the following slot to start; if it starts
/// later than that (the encode overran, or the thread was preempted), it takes
/// the slot it lands in, skipping the ones that passed rather than catching up
/// with a burst.
///
/// Arithmetic is in `i128` on QPC counts scaled by `target_fps`, so slot
/// boundaries are exact and rounding never accumulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FramePacer {
    fps: i128,
    frequency: i128,
    /// The earliest slot the next acquire may use; `None` before the first.
    next: Option<i64>,
}

impl FramePacer {
    /// Builds a pacer for `target_fps` on a QPC clock running at
    /// `qpc_frequency` counts per second (must be positive).
    pub(crate) fn new(target_fps: NonZeroU32, qpc_frequency: i64) -> Self {
        Self {
            fps: i128::from(target_fps.get()),
            frequency: i128::from(qpc_frequency.max(1)),
            next: None,
        }
    }

    /// The slot containing QPC time `qpc`.
    pub(crate) fn slot_at(&self, qpc: i64) -> i64 {
        saturate((i128::from(qpc) * self.fps).div_euclid(self.frequency))
    }

    /// The first whole QPC count in `slot`.
    pub(crate) fn slot_start(&self, slot: i64) -> i64 {
        let scaled = i128::from(slot) * self.frequency;
        saturate(-(-scaled).div_euclid(self.fps))
    }

    /// The start of `slot` on the shared 100 ns timeline, rounded down, so it
    /// is never after any QPC time inside the slot.
    pub(crate) fn timestamp(&self, slot: i64) -> Timestamp {
        let ticks = i128::from(slot) * i128::from(Timestamp::TICKS_PER_SECOND);
        Timestamp::from_ticks(saturate(ticks.div_euclid(self.fps)))
    }

    /// When the next acquire may start, if that is after `now`. Never more
    /// than one slot boundary away, so a pending slot implausibly far ahead
    /// (the clock went backwards, which QPC should never do) cannot stall
    /// capture.
    pub(crate) fn wait_until(&self, now: i64) -> Option<i64> {
        let start = self.slot_start(self.next?);
        let cap = self.slot_start(self.slot_at(now).saturating_add(1));
        (start > now).then(|| start.min(cap))
    }

    /// How many slots ended before an acquire starting at `now` could use
    /// them: the encode or the thread ran late.
    pub(crate) fn late_slots(&self, now: i64) -> u64 {
        self.next.map_or(0, |next| {
            u64::try_from(self.slot_at(now).saturating_sub(next)).unwrap_or(0)
        })
    }

    /// Takes the slot for a frame acquired at `qpc` and returns its
    /// timestamp: the slot containing `qpc`, or the next unclaimed one if
    /// that is later, so timestamps strictly increase.
    pub(crate) fn claim(&mut self, qpc: i64) -> Timestamp {
        let at = self.slot_at(qpc);
        let slot = self.next.map_or(at, |next| next.max(at));
        self.next = Some(slot.saturating_add(1));
        self.timestamp(slot)
    }

    /// Holds the next acquire until the slot after the one containing `qpc`,
    /// without emitting a frame: the update acquired at `qpc` had nothing to
    /// copy, and whatever changes next accumulates until then.
    pub(crate) fn defer(&mut self, qpc: i64) {
        self.raise(self.slot_at(qpc).saturating_add(1));
    }

    /// Records that an acquire waited until `qpc` without an update. The slot
    /// containing `qpc` is still open, and the ones before it were not missed
    /// by running late.
    pub(crate) fn idle(&mut self, qpc: i64) {
        self.raise(self.slot_at(qpc));
    }

    fn raise(&mut self, slot: i64) {
        self.next = Some(self.next.map_or(slot, |next| next.max(slot)));
    }
}

fn saturate(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}

#[cfg(test)]
mod tests {
    use blink_sys::QpcClock;

    use super::*;

    const QPC_HZ: i64 = 10_000_000;
    const INTERVAL: i64 = QPC_HZ / 60;

    fn pacer(fps: u32, frequency: i64) -> FramePacer {
        FramePacer::new(NonZeroU32::new(fps).unwrap(), frequency)
    }

    #[test]
    fn slots_tile_the_timeline_exactly() {
        let p = pacer(60, QPC_HZ);
        assert_eq!(p.slot_at(0), 0);
        assert_eq!(p.slot_at(INTERVAL), 0);
        assert_eq!(p.slot_at(INTERVAL + 1), 1);
        assert_eq!(p.slot_start(1), INTERVAL + 1);
        assert_eq!(p.slot_start(60), QPC_HZ);
        assert_eq!(p.slot_at(-1), -1);
        assert_eq!(p.timestamp(1), Timestamp::from_ticks(166_666));
        assert_eq!(p.timestamp(60), Timestamp::from_ticks(QPC_HZ));
        assert_eq!(p.timestamp(6_000), Timestamp::from_secs(100));
    }

    #[test]
    fn slot_boundaries_and_stamps_hold_for_common_frequencies() {
        for frequency in [QPC_HZ, 3_579_545, 2_400_000_000] {
            let clock = QpcClock::new(frequency).unwrap();
            for fps in [1, 24, 30, 60, 144, 240] {
                let p = pacer(fps, frequency);
                for qpc in (0..2_000).map(|i| i * 7_919_993 + frequency * 86_400) {
                    let slot = p.slot_at(qpc);
                    assert!(p.slot_start(slot) <= qpc, "{frequency} Hz, {fps} fps");
                    assert!(p.slot_start(slot + 1) > qpc, "{frequency} Hz, {fps} fps");
                    // A stamp is never after the moment the frame was taken.
                    assert!(p.timestamp(slot) <= clock.to_ticks(qpc));
                    assert!(p.timestamp(slot + 1) > p.timestamp(slot));
                }
            }
        }
    }

    #[test]
    fn first_claim_takes_the_current_slot_then_waits_for_the_next() {
        let mut p = pacer(60, QPC_HZ);
        assert_eq!(p.wait_until(12_345), None);
        assert_eq!(p.late_slots(12_345), 0);
        let taken = p.slot_start(10) + 5;
        assert_eq!(p.claim(taken), p.timestamp(10));
        assert_eq!(p.wait_until(taken + 1), Some(p.slot_start(11)));
        assert_eq!(p.wait_until(p.slot_start(11)), None);
        assert_eq!(p.claim(p.slot_start(11) + 100), p.timestamp(11));
    }

    #[test]
    fn a_late_acquire_skips_the_passed_slots() {
        let mut p = pacer(60, QPC_HZ);
        p.claim(0);
        let late = p.slot_start(4) + 10;
        assert_eq!(p.late_slots(late), 3);
        assert_eq!(p.wait_until(late), None);
        assert_eq!(p.claim(late), p.timestamp(4));
        assert_eq!(p.wait_until(late), Some(p.slot_start(5)));
    }

    #[test]
    fn deferring_holds_the_acquire_without_taking_a_slot() {
        let mut p = pacer(60, QPC_HZ);
        p.claim(0);
        let pointer = p.slot_start(3) + 50;
        p.defer(pointer);
        assert_eq!(p.wait_until(pointer), Some(p.slot_start(4)));
        assert_eq!(p.claim(p.slot_start(4)), p.timestamp(4));
        // Deferring to an earlier slot never moves the next one back.
        p.defer(0);
        assert_eq!(p.wait_until(0), Some(p.slot_start(1)));
    }

    #[test]
    fn idle_waits_are_not_late() {
        let mut p = pacer(60, QPC_HZ);
        p.claim(0);
        let woke = p.slot_start(30) + 7;
        p.idle(woke);
        assert_eq!(p.late_slots(woke), 0);
        assert_eq!(p.wait_until(woke), None);
        assert_eq!(p.claim(woke + 1), p.timestamp(30));
    }

    #[test]
    fn a_backwards_clock_neither_stalls_nor_repeats_a_stamp() {
        let mut p = pacer(60, QPC_HZ);
        let ahead = p.claim(QPC_HZ * 100);
        let now = QPC_HZ;
        assert_eq!(p.wait_until(now), Some(p.slot_start(p.slot_at(now) + 1)));
        assert!(p.claim(now) > ahead);
    }
}
