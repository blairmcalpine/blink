//! Bounded exponential backoff for recreating a lost duplication session.

use std::time::Duration;

/// Delay before the first retry after a failed recreation.
pub(crate) const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Longest delay between retries. Recovery never gives up, because the
/// secure desktop (UAC, lock screen) can block duplication for minutes, so the
/// cap keeps polling cheap rather than ending it.
pub(crate) const MAX_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Doubling retry delay, capped at a maximum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Backoff {
    initial: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    /// Starts at `initial` and never exceeds `max`.
    pub(crate) fn new(initial: Duration, max: Duration) -> Self {
        let initial = initial.min(max);
        Self {
            initial,
            max,
            next: initial,
        }
    }

    /// Returns the delay to wait before the next attempt and doubles it.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = delay.saturating_mul(2).min(self.max);
        delay
    }

    /// Restarts from the initial delay after a successful attempt.
    pub(crate) const fn reset(&mut self) {
        self.next = self.initial;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(INITIAL_RETRY_DELAY, MAX_RETRY_DELAY)
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
        let mut backoff = Backoff::default();
        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), INITIAL_RETRY_DELAY);
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
