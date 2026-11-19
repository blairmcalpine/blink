//! The shared media clock: the Query Performance Counter in 100 ns ticks.
//!
//! Video frames, WASAPI packets, and save requests are all stamped on this
//! timeline, so nothing downstream mixes clocks. [`QpcClock`] is the portable
//! conversion; [`qpc_counter`] and [`qpc_now`] read the counter on Windows.

use std::time::Duration;

use blink_ring::media::Timestamp;

/// Converts raw QPC readings into [`Timestamp`] ticks (100 ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QpcClock {
    frequency: i64,
}

impl QpcClock {
    /// Wraps a QPC frequency in counts per second. `None` unless positive.
    #[must_use]
    pub const fn new(frequency: i64) -> Option<Self> {
        if frequency > 0 {
            Some(Self { frequency })
        } else {
            None
        }
    }

    /// The system's QPC frequency, read once per process (Windows fixes it at
    /// boot).
    ///
    /// # Errors
    ///
    /// [`SysError::InvalidQpcFrequency`](crate::SysError::InvalidQpcFrequency)
    /// or [`SysError::Windows`](crate::SysError::Windows) if the frequency
    /// cannot be read, which Windows documents as impossible on XP and later.
    #[cfg(windows)]
    pub fn system() -> Result<Self, crate::SysError> {
        use std::sync::OnceLock;

        use windows::Win32::System::Performance::QueryPerformanceFrequency;

        static FREQUENCY: OnceLock<i64> = OnceLock::new();
        let frequency = *FREQUENCY.get_or_init(|| {
            let mut frequency = 0;
            // SAFETY: `frequency` is a valid, writable i64 for the call.
            let read = unsafe { QueryPerformanceFrequency(&raw mut frequency) };
            if read.is_ok() { frequency } else { 0 }
        });
        Self::new(frequency).ok_or(crate::SysError::InvalidQpcFrequency(frequency))
    }

    /// QPC counts per second.
    #[must_use]
    pub const fn frequency(self) -> i64 {
        self.frequency
    }

    /// Converts a QPC reading into ticks, rounding toward negative infinity.
    /// The product is computed in `i128`, so it cannot overflow; a result
    /// outside `i64` (thousands of years of uptime) saturates.
    #[must_use]
    pub fn to_ticks(self, qpc: i64) -> Timestamp {
        if self.frequency == Timestamp::TICKS_PER_SECOND {
            return Timestamp::from_ticks(qpc);
        }
        let scaled = (i128::from(qpc) * i128::from(Timestamp::TICKS_PER_SECOND))
            .div_euclid(i128::from(self.frequency));
        Timestamp::from_ticks(i64::try_from(scaled).unwrap_or(if scaled < 0 {
            i64::MIN
        } else {
            i64::MAX
        }))
    }

    /// Converts a span of QPC counts into a `Duration`, rounding up to the
    /// next 100 ns tick so a wait never ends before the target. Negative
    /// spans are zero.
    #[must_use]
    pub fn to_duration(self, qpc_delta: i64) -> Duration {
        let delta = i128::from(qpc_delta.max(0));
        let frequency = i128::from(self.frequency);
        let per_second = i128::from(Timestamp::TICKS_PER_SECOND);
        // Both operands are non-negative, so this is ceiling division.
        let ticks = (delta * per_second + frequency - 1) / frequency;
        Timestamp::from_ticks(i64::try_from(ticks).unwrap_or(i64::MAX)).to_duration()
    }
}

/// The raw QPC counter, in counts of [`QpcClock::system`]'s frequency.
///
/// # Errors
///
/// [`SysError::Windows`](crate::SysError::Windows) if the counter cannot be
/// read, which Windows documents as impossible on XP and later.
#[cfg(windows)]
pub fn qpc_counter() -> Result<i64, crate::SysError> {
    use windows::Win32::System::Performance::QueryPerformanceCounter;

    use crate::error::Context as _;

    let mut counter = 0;
    // SAFETY: `counter` is a valid, writable i64 for the call.
    unsafe { QueryPerformanceCounter(&raw mut counter) }.context("read the QPC counter")?;
    Ok(counter)
}

/// The current time on the shared timeline: the same clock as captured
/// frames and audio packets.
///
/// # Errors
///
/// Any error from [`QpcClock::system`] or [`qpc_counter`].
#[cfg(windows)]
pub fn qpc_now() -> Result<Timestamp, crate::SysError> {
    let clock = QpcClock::system()?;
    Ok(clock.to_ticks(qpc_counter()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKS_PER_SECOND: i64 = Timestamp::TICKS_PER_SECOND;

    fn ticks(clock: QpcClock, qpc: i64) -> i64 {
        clock.to_ticks(qpc).ticks()
    }

    #[test]
    fn rejects_non_positive_frequency() {
        assert_eq!(QpcClock::new(0), None);
        assert_eq!(QpcClock::new(-1), None);
    }

    #[test]
    fn ten_megahertz_is_identity() {
        let clock = QpcClock::new(TICKS_PER_SECOND).unwrap();
        assert_eq!(ticks(clock, 123_456_789), 123_456_789);
        assert_eq!(ticks(clock, i64::MAX), i64::MAX);
    }

    #[test]
    fn converts_common_frequencies() {
        // ACPI PM timer frequency used by older systems.
        let acpi = QpcClock::new(3_579_545).unwrap();
        assert_eq!(ticks(acpi, 3_579_545), TICKS_PER_SECOND);
        // TSC-backed QPC on some machines reports the raw TSC rate.
        let tsc = QpcClock::new(2_400_000_000).unwrap();
        assert_eq!(ticks(tsc, 2_400_000_000), TICKS_PER_SECOND);
        assert_eq!(ticks(tsc, 240), 1);
        assert_eq!(ticks(tsc, 239), 0);
    }

    #[test]
    fn large_readings_do_not_overflow() {
        let tsc = QpcClock::new(3_000_000_000).unwrap();
        // A naive `qpc * 10_000_000` overflows i64 here.
        let qpc = 3_000_000_000_i64 * 86_400 * 365;
        assert_eq!(ticks(tsc, qpc), TICKS_PER_SECOND * 86_400 * 365);
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
        assert_eq!(
            slow.to_duration(i64::MAX),
            Timestamp::from_ticks(i64::MAX).to_duration()
        );
    }

    #[test]
    fn saturates_instead_of_wrapping() {
        let slow = QpcClock::new(1).unwrap();
        assert_eq!(ticks(slow, i64::MAX), i64::MAX);
        assert_eq!(ticks(slow, i64::MIN), i64::MIN);
        assert_eq!(slow.frequency(), 1);
    }
}
