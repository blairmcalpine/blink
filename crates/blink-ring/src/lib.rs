//! Fixed-capacity ring buffer for blink's encoded packets.
//!
//! The recorder keeps the last N seconds of encoded audio and video in memory
//! and evicts the oldest entries as new ones arrive. This crate holds that
//! buffer. It is platform-agnostic, contains no `unsafe`, and its tests run
//! under Miri.

#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing)]

use std::collections::VecDeque;
use std::num::NonZeroUsize;

/// A ring buffer that holds at most `capacity` items.
///
/// Storage is allocated once in [`Ring::new`]. Pushing into a full ring evicts
/// the oldest item instead of growing, so steady-state use never allocates.
///
/// ```
/// use std::num::NonZeroUsize;
/// use blink_ring::Ring;
///
/// let Some(capacity) = NonZeroUsize::new(2) else { return };
/// let mut ring = Ring::new(capacity);
/// assert_eq!(ring.push(1), None);
/// assert_eq!(ring.push(2), None);
/// assert_eq!(ring.push(3), Some(1));
/// assert!(ring.iter().copied().eq([2, 3]));
/// ```
#[derive(Debug, Clone)]
pub struct Ring<T> {
    items: VecDeque<T>,
    capacity: NonZeroUsize,
}

impl<T> Ring<T> {
    /// Creates an empty ring that holds at most `capacity` items.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity.get()),
            capacity,
        }
    }

    /// Appends `item`, returning the evicted oldest item if the ring was full.
    pub fn push(&mut self, item: T) -> Option<T> {
        let evicted = if self.items.len() == self.capacity.get() {
            self.items.pop_front()
        } else {
            None
        };
        self.items.push_back(item);
        evicted
    }

    /// Iterates over the items from oldest to newest.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> + DoubleEndedIterator {
        self.items.iter()
    }

    /// Returns the number of items currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` if the ring holds no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns the maximum number of items the ring holds.
    #[must_use]
    pub const fn capacity(&self) -> NonZeroUsize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring<T>(capacity: usize) -> Ring<T> {
        Ring::new(NonZeroUsize::new(capacity).expect("test capacity is non-zero"))
    }

    #[test]
    fn starts_empty() {
        let ring: Ring<u8> = ring(4);
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.capacity().get(), 4);
    }

    #[test]
    fn fills_without_evicting() {
        let mut ring = ring(3);
        assert_eq!(ring.push('a'), None);
        assert_eq!(ring.push('b'), None);
        assert_eq!(ring.push('c'), None);
        assert_eq!(ring.len(), 3);
        assert!(ring.iter().copied().eq(['a', 'b', 'c']));
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut ring = ring(2);
        ring.push(1);
        ring.push(2);
        assert_eq!(ring.push(3), Some(1));
        assert_eq!(ring.push(4), Some(2));
        assert_eq!(ring.len(), 2);
        assert!(ring.iter().copied().eq([3, 4]));
    }

    #[test]
    fn never_grows_past_capacity() {
        let mut ring = ring(8);
        for value in 0..1000 {
            ring.push(value);
        }
        assert_eq!(ring.len(), 8);
        assert!(ring.iter().copied().eq(992..1000));
    }

    #[test]
    fn owns_heap_values() {
        let mut ring = ring(1);
        ring.push(String::from("first"));
        assert_eq!(ring.push(String::from("second")).as_deref(), Some("first"));
        assert_eq!(ring.iter().next().map(String::as_str), Some("second"));
    }
}
