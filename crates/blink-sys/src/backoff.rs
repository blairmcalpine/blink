//! Bounded exponential backoff for retrying a lost device, stream, or file.

use std::time::Duration;

/// Doubling retry delay, capped at a maximum.
///
/// Every retry loop in blink (a lost duplication session, a lost audio
/// endpoint, a failed capture or audio thread) waits [`next_delay`] between
/// attempts and calls [`reset`] once an attempt succeeds. None of them give
/// up: the cap keeps polling cheap rather than ending it.
///
/// [`next_delay`]: Self::next_delay
/// [`reset`]: Self::reset
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    /// Starts at `initial` and never exceeds `max`.
    #[must_use]
    pub const fn new(initial: Duration, max: Duration) -> Self {
        let initial = if initial.as_nanos() > max.as_nanos() {
            max
        } else {
            initial
        };
        Self {
            initial,
            max,
            next: initial,
        }
    }

    /// Returns the delay to wait before the next attempt and doubles it.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = delay.saturating_mul(2).min(self.max);
        delay
    }

    /// Restarts from the initial delay after a successful attempt.
    pub const fn reset(&mut self) {
        self.next = self.initial;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn doubles_until_capped() {
        let mut backoff = Backoff::new(ms(10), ms(50));
        let delays: Vec<_> = (0..5).map(|_| backoff.next_delay()).collect();
        assert_eq!(delays, [ms(10), ms(20), ms(40), ms(50), ms(50)]);
    }

    #[test]
    fn reset_restarts_from_initial() {
        let mut backoff = Backoff::new(ms(10), ms(500));
        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), ms(10));
    }

    #[test]
    fn initial_is_clamped_to_max() {
        let mut backoff = Backoff::new(ms(100), ms(30));
        assert_eq!(backoff.next_delay(), ms(30));
        assert_eq!(backoff.next_delay(), ms(30));
    }

    #[test]
    fn huge_delays_saturate() {
        let mut backoff = Backoff::new(Duration::MAX, Duration::MAX);
        assert_eq!(backoff.next_delay(), Duration::MAX);
        assert_eq!(backoff.next_delay(), Duration::MAX);
    }
}
