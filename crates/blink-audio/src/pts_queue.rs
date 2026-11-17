//! Matches encoder output frames to the timestamps of the input they came from.
//!
//! The AAC encoder buffers a little input before producing output, so frames
//! come out later than the 1024-sample chunks that went in. Each chunk's
//! timestamp is queued on input and popped per output frame, in order. If the
//! encoder emits more frames than it was fed (for example the flush frame of a
//! drain), the next timestamp is extrapolated from the last one. An optional
//! priming delay is subtracted from every output timestamp.

use blink_ring::media::Timestamp;

/// Timestamps that can be in flight inside the encoder. The encoder holds at
/// most a couple of frames, so reaching this means it stopped producing output.
pub(crate) const CAPACITY: usize = 16;

/// Fixed-capacity FIFO of input timestamps.
#[derive(Debug, Clone)]
pub(crate) struct PtsQueue {
    slots: [Timestamp; CAPACITY],
    head: usize,
    len: usize,
    last: Option<Timestamp>,
    delay: Timestamp,
}

impl PtsQueue {
    /// An empty queue with no delay.
    #[cfg(test)]
    pub(crate) const fn new() -> Self {
        Self::with_delay(Timestamp::ZERO)
    }

    /// An empty queue whose output timestamps are `delay` earlier than the
    /// queued input timestamps (the encoder's priming delay).
    pub(crate) const fn with_delay(delay: Timestamp) -> Self {
        Self {
            slots: [Timestamp::ZERO; CAPACITY],
            head: 0,
            len: 0,
            last: None,
            delay,
        }
    }

    /// Queues the timestamp of a chunk handed to the encoder. When full, the
    /// oldest entry is dropped and `false` is returned.
    pub(crate) fn push(&mut self, pts: Timestamp) -> bool {
        let fits = self.len < CAPACITY;
        if !fits {
            self.head = (self.head + 1) % CAPACITY;
            self.len -= 1;
        }
        if let Some(slot) = self.slots.get_mut((self.head + self.len) % CAPACITY) {
            *slot = pts;
            self.len += 1;
        }
        fits
    }

    /// Timestamp for the next output frame: the oldest queued input minus the
    /// delay, or the previous output plus `duration` when the queue is empty.
    pub(crate) fn next(&mut self, duration: Timestamp) -> Timestamp {
        let pts = if self.len == 0 {
            None
        } else {
            let pts = self.slots.get(self.head).copied();
            self.head = (self.head + 1) % CAPACITY;
            self.len -= 1;
            pts.map(|pts| pts.saturating_sub(self.delay))
        };
        let pts = pts.unwrap_or_else(|| {
            self.last
                .map_or(Timestamp::ZERO, |last| last.saturating_add(duration))
        });
        self.last = Some(pts);
        pts
    }

    /// Number of queued timestamps.
    #[cfg(test)]
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Forgets everything, including the extrapolation base. Called at the end
    /// of a segment, after the encoder has been drained.
    pub(crate) const fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Timestamp = Timestamp(10);

    #[test]
    fn pops_in_order() {
        let mut queue = PtsQueue::new();
        assert!(queue.push(Timestamp(100)));
        assert!(queue.push(Timestamp(110)));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.next(D), Timestamp(100));
        assert_eq!(queue.next(D), Timestamp(110));
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn extrapolates_when_empty() {
        let mut queue = PtsQueue::new();
        assert_eq!(queue.next(D), Timestamp::ZERO);
        queue.push(Timestamp(500));
        assert_eq!(queue.next(D), Timestamp(500));
        assert_eq!(queue.next(D), Timestamp(510));
        assert_eq!(queue.next(D), Timestamp(520));
    }

    #[test]
    fn overflow_drops_oldest() {
        let mut queue = PtsQueue::new();
        for i in 0..CAPACITY {
            assert!(queue.push(Timestamp(i64::try_from(i).unwrap())));
        }
        assert!(!queue.push(Timestamp(99)));
        assert_eq!(queue.len(), CAPACITY);
        assert_eq!(queue.next(D), Timestamp(1));
    }

    #[test]
    fn wraps_around() {
        let mut queue = PtsQueue::new();
        for round in 0..3 * CAPACITY {
            let pts = Timestamp(i64::try_from(round).unwrap());
            queue.push(pts);
            assert_eq!(queue.next(D), pts);
        }
    }

    #[test]
    fn delay_shifts_queued_and_extrapolated_output() {
        let mut queue = PtsQueue::with_delay(Timestamp(3));
        queue.push(Timestamp(100));
        assert_eq!(queue.next(D), Timestamp(97));
        assert_eq!(queue.next(D), Timestamp(107));
        queue.clear();
        queue.push(Timestamp(200));
        assert_eq!(queue.next(D), Timestamp(197));
    }

    #[test]
    fn clear_resets_extrapolation() {
        let mut queue = PtsQueue::new();
        queue.push(Timestamp(7));
        queue.push(Timestamp(8));
        queue.next(D);
        queue.clear();
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.next(D), Timestamp::ZERO);
    }
}
