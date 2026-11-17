//! Query Performance Counter to 100 ns tick conversion.
//!
//! WASAPI already reports packet positions in 100 ns QPC units; this is only
//! needed for the wall-clock reads that drive silence fill. `blink-capture`
//! has the same conversion; the crates do not depend on each other.

use blink_ring::media::Timestamp;

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

    /// Converts a QPC reading into 100 ns ticks, rounding toward negative
    /// infinity. The product is computed in `i128`, so it cannot overflow; a
    /// result outside `i64` saturates.
    pub(crate) fn to_ticks(self, qpc: i64) -> i64 {
        if self.frequency == Timestamp::TICKS_PER_SECOND {
            return qpc;
        }
        let scaled = (i128::from(qpc) * i128::from(Timestamp::TICKS_PER_SECOND))
            .div_euclid(i128::from(self.frequency));
        i64::try_from(scaled).unwrap_or(if scaled < 0 { i64::MIN } else { i64::MAX })
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
        let clock = QpcClock::new(10_000_000).unwrap();
        assert_eq!(clock.to_ticks(123_456_789), 123_456_789);
    }

    #[test]
    fn converts_common_frequencies() {
        let acpi = QpcClock::new(3_579_545).unwrap();
        assert_eq!(acpi.to_ticks(3_579_545), 10_000_000);
        let tsc = QpcClock::new(2_400_000_000).unwrap();
        assert_eq!(tsc.to_ticks(2_400_000_000), 10_000_000);
        assert_eq!(tsc.to_ticks(240), 1);
        assert_eq!(tsc.to_ticks(239), 0);
    }

    #[test]
    fn saturates_instead_of_wrapping() {
        let slow = QpcClock::new(1).unwrap();
        assert_eq!(slow.to_ticks(i64::MAX), i64::MAX);
        assert_eq!(slow.to_ticks(i64::MIN), i64::MIN);
    }
}
