//! Query Performance Counter to 100 ns tick conversion.
//!
//! `blink-ring` will own the shared `Timestamp(i64)` type. Until then this crate
//! hands out the raw `i64` tick count so it does not need that dependency.

use std::time::Duration;

/// 100 ns ticks per second: the timeline every blink component uses.
pub(crate) const TICKS_PER_SECOND: i64 = 10_000_000;

/// Converts raw QPC readings into 100 ns ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QpcClock {
    frequency: i64,
}

impl QpcClock {
    /// Wraps a QPC frequency in counts per second. `None` unless positive.
    pub(crate) const fn new(frequency: i64) -> Option<Self> {
        if frequency > 0 {
            Some(Self { frequency })
        } else {
            None
        }
    }

    /// QPC counts per second.
    pub(crate) const fn frequency(self) -> i64 {
        self.frequency
    }

    /// Converts a QPC reading into 100 ns ticks, rounding toward negative
    /// infinity. The product is computed in `i128`, so it cannot overflow;
    /// a result outside `i64` (thousands of years of uptime) saturates.
    pub(crate) fn to_ticks(self, qpc: i64) -> i64 {
        if self.frequency == TICKS_PER_SECOND {
            return qpc;
        }
        let scaled =
            (i128::from(qpc) * i128::from(TICKS_PER_SECOND)).div_euclid(i128::from(self.frequency));
        i64::try_from(scaled).unwrap_or(if scaled < 0 { i64::MIN } else { i64::MAX })
    }

    /// Converts a span of QPC counts into a `Duration`, rounding up to the
    /// next 100 ns tick so a wait never ends before the target. Negative
    /// spans are zero.
    pub(crate) fn to_duration(self, qpc_delta: i64) -> Duration {
        let delta = i128::from(qpc_delta.max(0));
        let frequency = i128::from(self.frequency);
        // Both operands are non-negative, so this is ceiling division.
        let ticks = (delta * i128::from(TICKS_PER_SECOND) + frequency - 1) / frequency;
        let nanos = u64::try_from(ticks.saturating_mul(100)).unwrap_or(u64::MAX);
        Duration::from_nanos(nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_positive_frequency() {
        assert_eq!(QpcClock::new(0), None);
        assert_eq!(QpcClock::new(-1), None);
    }

    #[test]
    fn ten_megahertz_is_identity() {
        let clock = QpcClock::new(TICKS_PER_SECOND).unwrap();
        assert_eq!(clock.to_ticks(123_456_789), 123_456_789);
        assert_eq!(clock.to_ticks(i64::MAX), i64::MAX);
    }

    #[test]
    fn converts_common_frequencies() {
        // ACPI PM timer frequency used by older systems.
        let acpi = QpcClock::new(3_579_545).unwrap();
        assert_eq!(acpi.to_ticks(3_579_545), TICKS_PER_SECOND);
        // TSC-backed QPC on some machines reports the raw TSC rate.
        let tsc = QpcClock::new(2_400_000_000).unwrap();
        assert_eq!(tsc.to_ticks(2_400_000_000), TICKS_PER_SECOND);
        assert_eq!(tsc.to_ticks(240), 1);
        assert_eq!(tsc.to_ticks(239), 0);
    }

    #[test]
    fn large_readings_do_not_overflow() {
        let tsc = QpcClock::new(3_000_000_000).unwrap();
        // A naive `qpc * 10_000_000` overflows i64 here.
        let qpc = 3_000_000_000_i64 * 86_400 * 365;
        assert_eq!(tsc.to_ticks(qpc), TICKS_PER_SECOND * 86_400 * 365);
    }

    #[test]
    fn durations_round_up_and_clamp_negative() {
        let tsc = QpcClock::new(3_000_000_000).unwrap();
        assert_eq!(tsc.to_duration(3_000_000_000), Duration::from_secs(1));
        // One count is a third of a nanosecond: rounds up to one 100 ns tick.
        assert_eq!(tsc.to_duration(1), Duration::from_nanos(100));
        assert_eq!(tsc.to_duration(0), Duration::ZERO);
        assert_eq!(tsc.to_duration(-5), Duration::ZERO);
        let slow = QpcClock::new(1).unwrap();
        assert_eq!(slow.to_duration(i64::MAX), Duration::from_nanos(u64::MAX));
    }

    #[test]
    fn saturates_instead_of_wrapping() {
        let slow = QpcClock::new(1).unwrap();
        assert_eq!(slow.to_ticks(i64::MAX), i64::MAX);
        assert_eq!(slow.to_ticks(i64::MIN), i64::MIN);
        assert_eq!(slow.frequency(), 1);
    }
}
