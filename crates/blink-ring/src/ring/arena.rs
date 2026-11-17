//! Fixed-size circular byte FIFO backing the clip ring.

/// A contiguous circular byte buffer allocated once at construction.
///
/// Bytes are appended at the tail and released from the head, strictly in
/// FIFO order. A payload that reaches the end of the buffer is split and
/// continues at the start, so no space is wasted on padding and the only
/// requirement for a write is enough free bytes in total.
pub(super) struct Arena {
    buf: Box<[u8]>,
    head: usize,
    len: usize,
}

impl Arena {
    /// Allocates an arena of exactly `capacity` bytes.
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: vec![0; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    /// Total size in bytes.
    pub(super) const fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Bytes currently held.
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// Bytes that can be appended before the arena is full.
    pub(super) const fn free(&self) -> usize {
        self.capacity() - self.len
    }

    /// Appends `data` and returns its start offset, or `None` if it does not fit.
    pub(super) fn push(&mut self, data: &[u8]) -> Option<usize> {
        if data.len() > self.free() {
            return None;
        }
        let offset = self.wrap(self.head + self.len);
        let (front, back) = self.buf.split_at_mut_checked(offset)?;
        let written = copy_prefix(back, data);
        copy_prefix(front, data.get(written..).unwrap_or_default());
        self.len += data.len();
        Some(offset)
    }

    /// Releases `count` bytes from the head.
    pub(super) fn pop_front(&mut self, count: usize) {
        let count = count.min(self.len);
        self.head = self.wrap(self.head + count);
        self.len -= count;
    }

    /// Releases every byte without touching the allocation.
    pub(super) const fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// The `len` bytes starting at `offset`, as the part before the arena's
    /// end and the part that wrapped to its start (often empty).
    pub(super) fn parts(&self, offset: usize, len: usize) -> Option<(&[u8], &[u8])> {
        let (front, back) = self.buf.split_at_checked(offset)?;
        let first = back.get(..len.min(back.len()))?;
        let second = front.get(..len - first.len())?;
        Some((first, second))
    }

    /// Address of the backing allocation, to prove it never moves.
    #[cfg(test)]
    pub(super) fn as_ptr(&self) -> *const u8 {
        self.buf.as_ptr()
    }

    /// Maps a position in `0..2 * capacity` back into `0..capacity`.
    const fn wrap(&self, pos: usize) -> usize {
        if pos >= self.capacity() {
            pos - self.capacity()
        } else {
            pos
        }
    }
}

/// Copies as much of `src` into the start of `dst` as fits; returns the count.
fn copy_prefix(dst: &mut [u8], src: &[u8]) -> usize {
    let count = dst.len().min(src.len());
    if let (Some(dst), Some(src)) = (dst.get_mut(..count), src.get(..count)) {
        dst.copy_from_slice(src);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Arena {
        fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
            self.parts(offset, len)
                .map(|parts| <[&[u8]; 2]>::from(parts).concat())
        }
    }

    #[test]
    fn splits_payload_across_the_end() {
        let mut arena = Arena::with_capacity(8);
        assert_eq!(arena.push(&[1, 2, 3, 4, 5]), Some(0));
        arena.pop_front(5);
        assert_eq!(arena.push(&[6, 7, 8, 9, 10, 11]), Some(5));
        assert_eq!(arena.read(5, 6), Some(vec![6, 7, 8, 9, 10, 11]));
        assert_eq!(arena.buf.as_ref(), &[9, 10, 11, 4, 5, 6, 7, 8]);
        assert_eq!(arena.len(), 6);
    }

    #[test]
    fn rejects_payload_larger_than_free_space() {
        let mut arena = Arena::with_capacity(4);
        assert_eq!(arena.push(&[1, 2, 3]), Some(0));
        assert_eq!(arena.push(&[4, 5]), None);
        assert_eq!(arena.free(), 1);
    }

    #[test]
    fn fills_exactly_to_capacity() {
        let mut arena = Arena::with_capacity(4);
        assert_eq!(arena.push(&[1, 2]), Some(0));
        arena.pop_front(1);
        assert_eq!(arena.push(&[3, 4, 5]), Some(2));
        assert_eq!(arena.free(), 0);
        assert_eq!(arena.read(1, 4), Some(vec![2, 3, 4, 5]));
    }

    #[test]
    fn zero_capacity_never_panics() {
        let mut arena = Arena::with_capacity(0);
        assert_eq!(arena.push(&[1]), None);
        arena.pop_front(3);
        assert_eq!(arena.read(0, 0), Some(Vec::new()));
    }
}
