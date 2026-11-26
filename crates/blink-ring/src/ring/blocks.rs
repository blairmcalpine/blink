//! Payload storage for the clip ring: a FIFO of byte blocks that are
//! allocated as media arrives and freed as it is evicted.

use std::collections::VecDeque;
use std::sync::Arc;

/// A block of payload bytes. Clips share the blocks they were cut from, so a
/// save references the buffered media instead of copying it.
pub(super) type Block = Arc<Vec<u8>>;

/// Size of a standard block. Well above the Windows heap's direct-allocation
/// threshold (1016 KiB on x64), so a freed block goes straight back to the
/// OS instead of staying committed in a heap segment. Payloads are never
/// split, so a payload that does not fit the rest of the newest block wastes
/// that rest: about `len² / 2` divided by the block size per payload, which
/// at 4 MiB stays near 1% for 600 KB keyframes and 5% for 400 KB frames.
const BLOCK_SIZE: usize = 4 << 20;

/// A ring's byte limit is split into at least this many blocks, so a small
/// ring still evicts GOP by GOP rather than all at once. Below a 64 MiB
/// limit blocks are smaller than [`BLOCK_SIZE`], and below 16 MiB they fall
/// under the heap threshold, so their memory may stay in the heap.
const MIN_BLOCKS: usize = 16;

/// Most block slots preallocated in the queue; a ring that somehow needs
/// more grows it.
const MAX_PREALLOCATED_BLOCKS: usize = 4096;

/// Why [`Blocks::push`] stored nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StoreError {
    /// The payload does not fit under the limit without releasing blocks.
    Full,
    /// The allocator could not provide a new block.
    OutOfMemory,
}

/// Byte storage whose footprint follows the media it holds.
///
/// Payloads are appended to the newest block while it has room and nobody
/// else holds it. Otherwise a new block is opened: a standard block, or one
/// exactly as large as a payload that does not fit in one, so every payload
/// is contiguous. Blocks are released from the front once no stored payload
/// lives in them. Released blocks no clip holds are kept as spares, up to
/// about a second of media at the rate the limit was sized for, and reused
/// before anything is allocated, so a steady stream stops allocating; the
/// rest are freed. A released block a clip still holds is freed by the clip.
pub(super) struct Blocks {
    live: VecDeque<Block>,
    /// Id of `live`'s front block. Ids increase by one per block opened.
    front_id: u64,
    /// Empty blocks no clip holds, ready to reuse.
    spares: Vec<Block>,
    max_spares: usize,
    block_size: usize,
    /// Most bytes the live blocks may reserve.
    limit: usize,
    /// Bytes the live blocks reserve (their capacities).
    reserved: usize,
    /// Payload bytes stored.
    used: usize,
    /// Blocks allocated (not taken from a spare) so far.
    #[cfg(test)]
    pub(super) allocations: usize,
}

impl Blocks {
    /// Storage that reserves at most `limit` bytes at once and keeps about
    /// `spare_bytes` of released blocks for reuse: what evicting the oldest
    /// GOP releases at once, so the blocks opened over the next GOP need no
    /// allocation. Allocates nothing up front but the queue of block slots.
    pub(super) fn new(limit: usize, spare_bytes: usize) -> Self {
        let block_size = (limit / MIN_BLOCKS).clamp(1, BLOCK_SIZE);
        let slots = (limit / block_size)
            .saturating_add(1)
            .min(MAX_PREALLOCATED_BLOCKS);
        let max_spares = spare_bytes
            .div_ceil(block_size)
            .saturating_add(1)
            .min(slots);
        Self {
            live: VecDeque::with_capacity(slots),
            front_id: 0,
            spares: Vec::with_capacity(max_spares),
            max_spares,
            block_size,
            limit,
            reserved: 0,
            used: 0,
            #[cfg(test)]
            allocations: 0,
        }
    }

    /// Most bytes the storage may reserve, and so the largest payload it
    /// can hold.
    pub(super) const fn limit(&self) -> usize {
        self.limit
    }

    /// Payload bytes stored.
    pub(super) const fn used(&self) -> usize {
        self.used
    }

    /// Bytes reserved by the live blocks, used or not.
    pub(super) const fn reserved(&self) -> usize {
        self.reserved
    }

    /// Bytes held by spare blocks, on top of [`reserved`](Self::reserved).
    pub(super) fn spare(&self) -> usize {
        self.spares.iter().map(|spare| spare.capacity()).sum()
    }

    /// Whether `len` more bytes fit without releasing anything. Takes
    /// `&mut self` because telling whether the newest block is shared with a
    /// clip needs [`Arc::get_mut`].
    pub(super) fn fits(&mut self, len: usize) -> bool {
        self.tail_room() >= len || self.new_block_fits(len)
    }

    /// Appends `data` and returns the id of the block that holds it and its
    /// offset there.
    pub(super) fn push(&mut self, data: &[u8]) -> Result<(u64, usize), StoreError> {
        if let Some(tail) = self.live.back_mut().and_then(Arc::get_mut)
            && tail.capacity() - tail.len() >= data.len()
        {
            let offset = tail.len();
            tail.extend_from_slice(data);
            self.used += data.len();
            return Ok((self.tail_id(), offset));
        }
        if !self.new_block_fits(data.len()) {
            return Err(StoreError::Full);
        }
        let mut block = self.open(data.len().max(self.block_size))?;
        let Some(bytes) = Arc::get_mut(&mut block) else {
            return Err(StoreError::Full);
        };
        bytes.extend_from_slice(data);
        self.reserved += bytes.capacity();
        self.used += data.len();
        self.live.push_back(block);
        Ok((self.tail_id(), 0))
    }

    /// Forgets `len` payload bytes and releases the blocks before
    /// `first_needed`, the block of the oldest payload still stored (`None`
    /// when nothing is).
    pub(super) fn release(&mut self, len: usize, first_needed: Option<u64>) {
        self.used = self.used.saturating_sub(len);
        let keep_from = first_needed.unwrap_or(u64::MAX);
        while self.front_id < keep_from {
            let Some(block) = self.live.pop_front() else {
                break;
            };
            self.front_id += 1;
            self.reserved = self.reserved.saturating_sub(block.capacity());
            self.recycle(block);
        }
    }

    /// Releases every block.
    pub(super) fn clear(&mut self) {
        self.release(self.used, None);
    }

    /// The block with id `id`, if it is live.
    pub(super) fn block(&self, id: u64) -> Option<&Block> {
        let index = usize::try_from(id.checked_sub(self.front_id)?).ok()?;
        self.live.get(index)
    }

    /// The `len` bytes at `offset` in block `id`.
    #[cfg(test)]
    pub(super) fn get(&self, id: u64, offset: usize, len: usize) -> Option<&[u8]> {
        self.block(id)?.get(offset..offset.checked_add(len)?)
    }

    /// Free bytes at the end of the newest block, or zero when it is shared
    /// with a clip (and so must not change) or there is none.
    fn tail_room(&mut self) -> usize {
        self.live
            .back_mut()
            .and_then(Arc::get_mut)
            .map_or(0, |tail| tail.capacity() - tail.len())
    }

    const fn room(&self) -> usize {
        self.limit.saturating_sub(self.reserved)
    }

    fn new_block_fits(&self, len: usize) -> bool {
        len.max(self.block_size) <= self.room()
    }

    fn tail_id(&self) -> u64 {
        let len = u64::try_from(self.live.len()).unwrap_or(u64::MAX);
        self.front_id.saturating_add(len).saturating_sub(1)
    }

    /// An empty block with room for `size` bytes that fits under the limit:
    /// a spare if one is large enough, else a new allocation.
    fn open(&mut self, size: usize) -> Result<Block, StoreError> {
        let room = self.room();
        if let Some(index) = self
            .spares
            .iter()
            .position(|spare| (size..=room).contains(&spare.capacity()))
        {
            return Ok(self.spares.swap_remove(index));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| StoreError::OutOfMemory)?;
        #[cfg(test)]
        {
            self.allocations += 1;
        }
        Ok(Arc::new(bytes))
    }

    /// Keeps a released block as a spare if there is room and no clip holds
    /// it; drops it otherwise.
    fn recycle(&mut self, mut block: Block) {
        if self.spares.len() >= self.max_spares {
            return;
        }
        if let Some(bytes) = Arc::get_mut(&mut block) {
            bytes.clear();
            self.spares.push(block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_until_a_block_is_full() {
        let mut blocks = Blocks::new(64, 0);
        assert_eq!(blocks.block_size, 4);
        assert_eq!(blocks.push(&[1, 2]), Ok((0, 0)));
        assert_eq!(blocks.push(&[3, 4]), Ok((0, 2)));
        assert_eq!(blocks.push(&[5]), Ok((1, 0)));
        assert_eq!(blocks.get(0, 0, 4), Some(&[1, 2, 3, 4][..]));
        assert_eq!((blocks.used(), blocks.reserved()), (5, 8));
    }

    #[test]
    fn a_large_payload_gets_its_own_block() {
        let mut blocks = Blocks::new(64, 0);
        assert_eq!(blocks.push(&[1]), Ok((0, 0)));
        assert_eq!(blocks.push(&[2; 10]), Ok((1, 0)));
        assert_eq!(blocks.get(1, 0, 10), Some(&[2; 10][..]));
        assert_eq!(blocks.reserved(), 14);
        assert_eq!(blocks.push(&[3]), Ok((2, 0)), "the large block is full");
    }

    #[test]
    fn respects_the_limit() {
        let mut blocks = Blocks::new(16, 0);
        assert_eq!(blocks.block_size, 1);
        assert!(blocks.fits(16));
        assert_eq!(blocks.push(&[0; 12]), Ok((0, 0)));
        assert!(!blocks.fits(5));
        assert_eq!(blocks.push(&[0; 5]), Err(StoreError::Full));
        assert_eq!(blocks.push(&[0; 4]), Ok((1, 0)));
        assert_eq!(blocks.reserved(), 16);
    }

    #[test]
    fn releases_blocks_before_the_first_needed() {
        let mut blocks = Blocks::new(64, 0);
        for byte in 0..12 {
            assert!(blocks.push(&[byte; 2]).is_ok());
        }
        assert_eq!(blocks.reserved(), 24);
        blocks.release(8, Some(2));
        assert_eq!((blocks.used(), blocks.reserved()), (16, 16));
        assert!(blocks.block(1).is_none());
        assert_eq!(blocks.get(2, 0, 4), Some(&[4, 4, 5, 5][..]));
        blocks.clear();
        assert_eq!((blocks.used(), blocks.reserved()), (0, 0));
        assert_eq!(blocks.push(&[9]), Ok((6, 0)), "ids keep increasing");
    }

    #[test]
    fn a_released_block_is_reused() {
        let mut blocks = Blocks::new(64, 0);
        for _ in 0..100 {
            assert!(blocks.push(&[1; 4]).is_ok());
            blocks.release(4, None);
        }
        assert_eq!(blocks.allocations, 1);
    }

    #[test]
    fn spares_cover_the_spare_bytes() {
        let mut blocks = Blocks::new(64, 10);
        assert_eq!(blocks.max_spares, 4, "three blocks for ten bytes, plus one");
        for byte in 0..6 {
            assert!(blocks.push(&[byte; 4]).is_ok());
        }
        blocks.clear();
        assert_eq!(blocks.spare(), 16);
        for byte in 0..4 {
            assert!(blocks.push(&[byte; 4]).is_ok());
        }
        assert_eq!(blocks.allocations, 6, "the next four blocks were spares");
    }

    #[test]
    fn an_oversized_spare_is_reused_for_what_fits() {
        let mut blocks = Blocks::new(64, 0);
        assert!(blocks.push(&[1; 10]).is_ok());
        blocks.clear();
        assert!(blocks.push(&[2; 8]).is_ok());
        assert!(blocks.push(&[3; 2]).is_ok());
        assert_eq!(blocks.allocations, 1);
        assert_eq!(blocks.get(0, 0, 0), None);
        assert_eq!(blocks.get(1, 8, 2), Some(&[3; 2][..]));
    }

    #[test]
    fn a_shared_block_is_never_written_or_reused() {
        let mut blocks = Blocks::new(64, 0);
        assert!(blocks.push(&[1]).is_ok());
        let held = blocks.block(0).map(Arc::clone);
        assert_eq!(blocks.push(&[2]), Ok((1, 0)), "the shared tail is sealed");
        blocks.clear();
        assert_eq!(held.as_deref().map(Vec::as_slice), Some(&[1][..]));
        assert!(blocks.push(&[3; 4]).is_ok());
        assert_eq!(held.as_deref().map(Vec::as_slice), Some(&[1][..]));
        assert_eq!(
            blocks.allocations, 2,
            "the spare came from the second block"
        );
        assert_eq!(blocks.spares.len(), 0);
    }

    #[test]
    fn zero_limit_holds_nothing() {
        let mut blocks = Blocks::new(0, 0);
        assert!(!blocks.fits(1));
        assert_eq!(blocks.push(&[1]), Err(StoreError::Full));
        assert!(blocks.fits(0));
    }
}
