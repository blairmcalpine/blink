//! The GOP-aligned clip ring.

mod blocks;
mod clip;

use std::collections::VecDeque;
use std::fmt;
use std::mem::size_of;
use std::sync::Arc;

use thiserror::Error;
use tracing::warn;

use self::blocks::Blocks;
pub use self::clip::ClipBuf;
use crate::media::{AudioFormat, PacketMeta, StreamKind, Timestamp, VideoFormat};

/// Upper bound on packets per second used to size the metadata deque when
/// the caller does not give one: 240 fps video plus 48 kHz AAC (about 47
/// frames per second), with headroom.
const MAX_PACKETS_PER_SECOND: usize = 512;

/// Audio packets per second that [`ClipRing::recommended_packet_capacity`]
/// allows for on top of the video rate: 48 kHz AAC is about 47.
const AUDIO_PACKETS_PER_SECOND: u32 = 64;

/// Media kept beyond the window, which both the byte limit and the metadata deque
/// are sized for: eviction is whole-GOP, so up to one GOP (1 to 2 s) more
/// than the window is held.
const GOP_SLACK: Timestamp = Timestamp::from_secs(4);

/// Errors returned by [`ClipRing::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PushError {
    /// The payload was empty.
    #[error("packet payload is empty")]
    EmptyPayload,
    /// The payload can never fit, even in an empty ring.
    #[error("packet of {len} bytes exceeds the ring's byte capacity of {capacity}")]
    PayloadTooLarge {
        /// Payload length in bytes.
        len: usize,
        /// Byte capacity of the ring.
        capacity: usize,
    },
    /// A video packet arrived before [`ClipRing::set_video_format`].
    #[error("video packet pushed before a video format was set")]
    MissingVideoFormat,
    /// An audio packet arrived before [`ClipRing::set_audio_format`].
    #[error("audio packet pushed before an audio format was set")]
    MissingAudioFormat,
}

/// What [`ClipRing::push`] did with a packet it accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// The packet was stored.
    Stored,
    /// A non-keyframe video packet was dropped because the frame it depends on
    /// is not in the ring: the segment has not seen an IDR yet, or the current
    /// GOP was evicted to make room. Storing resumes at the next keyframe, so
    /// the encoder must force an IDR on its next frame.
    DroppedAwaitingKeyframe,
}

/// Errors returned while cutting a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ClipError {
    /// No video format is set, or no keyframe is stored before the save time.
    #[error("no keyframe is buffered before the save time")]
    NoKeyframe,
}

/// One stored packet: its metadata and where its bytes live.
#[derive(Debug, Clone, Copy)]
struct Entry {
    meta: PacketMeta,
    /// Id of the block holding the payload.
    block: u64,
    offset: usize,
    len: usize,
    seq: u64,
}

/// A stored video keyframe, i.e. the start of a GOP.
#[derive(Debug, Clone, Copy)]
struct Keyframe {
    seq: u64,
    pts: Timestamp,
}

/// Ring buffer of the last N seconds of encoded audio and video.
///
/// Payloads live in 1 MiB blocks that are allocated as media arrives and
/// freed as it is evicted, so the ring's memory follows what it holds (a few
/// megabytes on a static desktop) rather than its byte capacity, which is
/// only a limit. One freed block is kept for reuse, so a steady stream stops
/// allocating once the window is full. Packet metadata lives in
/// fixed-capacity deques allocated in the constructor. When the time window,
/// the byte capacity, or the packet capacity is exceeded, the ring evicts
/// whole video GOPs from the front, together with the audio stored before
/// the next kept keyframe, so every stored video packet can be decoded.
///
/// Packets are stored in push order. Video and audio arrive on different
/// threads with different latency, so audio stored just before a keyframe can
/// carry a PTS after it. Eviction keeps such audio, and clips select audio by
/// PTS, not by position.
///
/// # Encoder contract
///
/// The ring can only evict whole GOPs, so it relies on the encoder to:
///
/// - emit an IDR at least once per second of PTS. When no frame has been
///   encoded for 1 s because the screen is static, re-encode the last frame as
///   an IDR. Otherwise one GOP spans the whole static period, the window
///   cannot evict it, and audio piles up behind it;
/// - force an IDR on the next frame whenever [`ClipRing::push`] returns
///   [`PushOutcome::DroppedAwaitingKeyframe`], because the ring stores no
///   video until it sees one.
///
/// The ring logs a warning when its oldest GOP alone is longer than the
/// window, and when byte or packet pressure forces it to drop the current GOP.
///
/// # Saving a clip
///
/// [`ClipRing::snapshot`] cuts a clip without copying any payload: the
/// [`ClipBuf`] shares the ring's blocks, so the lock is held only while the
/// packet list is built, and the clip's media stays alive while it is
/// written even if the ring evicts it meanwhile:
///
/// ```
/// use std::sync::Mutex;
/// use blink_ring::{ClipBuf, ClipRing};
/// use blink_ring::media::{PacketMeta, StreamKind, Timestamp, VideoFormat};
///
/// let ring = Mutex::new(ClipRing::with_capacity(Timestamp::from_secs(30), 1 << 20));
/// let mut guard = ring.lock().map_err(|_| "poisoned")?;
/// guard.set_video_format(VideoFormat {
///     width: 64,
///     height: 64,
///     fps: 30,
///     sps: vec![0x67, 0x42, 0xc0, 0x0a],
///     pps: vec![0x68, 0xce, 0x3c, 0x80],
///     sample_aspect: None,
/// });
/// let meta = PacketMeta {
///     kind: StreamKind::Video,
///     pts: Timestamp::ZERO,
///     duration: Timestamp::from_millis(33),
///     keyframe: true,
/// };
/// guard.push(meta, &[0, 0, 0, 1, 0x65, 0x88])?;
/// drop(guard);
///
/// let mut clip = ClipBuf::new();
/// let now = Timestamp::from_secs(1);
/// ring.lock()
///     .map_err(|_| "poisoned")?
///     .snapshot(now, Timestamp::from_secs(10), &mut clip)?;
/// assert_eq!(clip.video_len(), 1);
/// assert_eq!(clip.end(), now);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct ClipRing {
    store: Blocks,
    entries: VecDeque<Entry>,
    keyframes: VecDeque<Keyframe>,
    packet_capacity: usize,
    window: Timestamp,
    next_seq: u64,
    newest: Option<Timestamp>,
    video_synced: bool,
    audio_from_seq: u64,
    long_gop_warned: bool,
    video_format: Option<VideoFormat>,
    audio_format: Option<AudioFormat>,
}

impl ClipRing {
    /// Creates a ring that keeps at least `window` of media in at most
    /// `byte_capacity` bytes of payload blocks.
    ///
    /// Size `byte_capacity` with [`ClipRing::recommended_byte_capacity`]; a
    /// smaller limit evicts by bytes before the window is reached. It is
    /// only a limit: blocks are allocated as media arrives. The packet
    /// capacity is derived from the window, assuming at most 512 packets per
    /// second plus 4 seconds of GOP slack, and never exceeds `byte_capacity`
    /// (every packet holds at least one byte).
    #[must_use]
    pub fn with_capacity(window: Timestamp, byte_capacity: usize) -> Self {
        Self::with_limits(
            window,
            byte_capacity,
            default_packet_capacity(window, byte_capacity),
        )
    }

    /// Creates a ring with an explicit cap on the number of stored packets,
    /// whose metadata is allocated up front. Size it with
    /// [`ClipRing::recommended_packet_capacity`].
    ///
    /// `packet_capacity` is clamped to `1..=byte_capacity` and to what the
    /// allocator can address. Exceeding it evicts GOPs the same way exceeding
    /// the byte capacity does.
    #[must_use]
    pub fn with_limits(window: Timestamp, byte_capacity: usize, packet_capacity: usize) -> Self {
        let byte_capacity = byte_capacity.min(max_allocation(1));
        let packet_capacity = packet_capacity
            .min(byte_capacity)
            .min(max_allocation(size_of::<Entry>()))
            .max(1);
        Self {
            store: Blocks::new(byte_capacity),
            entries: VecDeque::with_capacity(packet_capacity),
            keyframes: VecDeque::with_capacity(packet_capacity),
            packet_capacity,
            window,
            next_seq: 0,
            newest: None,
            video_synced: false,
            audio_from_seq: 0,
            long_gop_warned: false,
            video_format: None,
            audio_format: None,
        }
    }

    /// Byte capacity for `window` at a combined audio and video bitrate of
    /// `bits_per_second`: the window plus the ring's GOP slack (4 s), with 25%
    /// headroom for bitrate overshoot.
    #[must_use]
    pub fn recommended_byte_capacity(window: Timestamp, bits_per_second: u64) -> usize {
        let ticks = u128::from(u64::try_from(kept(window).ticks()).unwrap_or(0));
        let ticks_per_second = u128::from(Timestamp::TICKS_PER_SECOND.unsigned_abs());
        // bytes = bits / 8 * seconds * 1.25 = bits * seconds * 5 / 32
        let bytes = u128::from(bits_per_second) * ticks * 5 / 32 / ticks_per_second;
        usize::try_from(bytes).unwrap_or(usize::MAX)
    }

    /// Packet capacity for `window` of video at `fps` plus AAC audio: the
    /// window plus the ring's GOP slack (4 s), at `fps` video packets and
    /// 64 audio packets a second. Capture is paced to at most one frame per
    /// slot of `fps`, so the video rate never exceeds it.
    #[must_use]
    pub fn recommended_packet_capacity(window: Timestamp, fps: u32) -> usize {
        let per_second =
            usize::try_from(fps.saturating_add(AUDIO_PACKETS_PER_SECOND)).unwrap_or(usize::MAX);
        kept_secs(window).saturating_mul(per_second)
    }

    /// Starts a new segment with `format`, discarding every stored packet.
    ///
    /// Setting the format that is already current is a no-op, so the stored
    /// packets survive an encoder restart that did not change the stream.
    pub fn set_video_format(&mut self, format: VideoFormat) {
        if self.video_format.as_ref() != Some(&format) {
            self.clear_packets();
            self.newest = None;
            self.video_format = Some(format);
        }
    }

    /// Switches the audio format. Audio stored before the change is excluded
    /// from later clips; video is kept.
    ///
    /// Setting the format that is already current is a no-op.
    pub fn set_audio_format(&mut self, format: AudioFormat) {
        if self.audio_format.as_ref() != Some(&format) {
            self.audio_from_seq = self.next_seq;
            self.audio_format = Some(format);
        }
    }

    /// Stores one encoded packet, evicting old GOPs as needed. Allocates
    /// only to open a block when no freed one is spare, which a steady
    /// stream stops doing once the window is full.
    ///
    /// # Errors
    ///
    /// Returns [`PushError`] if the payload is empty or larger than the whole
    /// ring, or if no format has been set for the packet's stream. The ring is
    /// unchanged in that case.
    pub fn push(&mut self, meta: PacketMeta, payload: &[u8]) -> Result<PushOutcome, PushError> {
        self.check_push(meta.kind, payload)?;
        if self.is_orphan(meta) {
            return Ok(PushOutcome::DroppedAwaitingKeyframe);
        }
        self.make_room(payload.len());
        if self.is_orphan(meta) {
            return Ok(PushOutcome::DroppedAwaitingKeyframe);
        }
        let (block, offset) =
            self.store
                .push(payload)
                .ok_or_else(|| PushError::PayloadTooLarge {
                    len: payload.len(),
                    capacity: self.store.limit(),
                })?;
        self.record(meta, block, offset, payload.len());
        self.enforce_window();
        Ok(PushOutcome::Stored)
    }

    /// Cuts the clip that ends at `now` (the save request time) and covers
    /// about `duration` before it into `clip`, replacing its contents.
    ///
    /// The clip starts at the latest stored keyframe at or before
    /// `now - duration`, so it can run up to one GOP longer than `duration`.
    /// If the segment is shorter, it starts at the oldest stored keyframe and
    /// is shorter than requested; [`ClipBuf::duration`] reports the real span.
    /// It includes later video and the audio from that keyframe on (plus one
    /// AAC preroll frame before it), excluding packets with a PTS at or after
    /// `now`.
    ///
    /// No payload is copied: `clip` shares the blocks the packets live in,
    /// so this only builds the packet list, in time linear in the packets
    /// stored. The newest block, once shared, is not written again; the next
    /// push opens a fresh one. Drop `clip` once it is written, so the
    /// blocks it shares can be freed or reused.
    ///
    /// # Errors
    ///
    /// Returns [`ClipError::NoKeyframe`] if no video format is set or no
    /// keyframe is stored before `now`.
    pub fn snapshot(
        &self,
        now: Timestamp,
        duration: Timestamp,
        clip: &mut ClipBuf,
    ) -> Result<(), ClipError> {
        let video_format = self.video_format.as_ref().ok_or(ClipError::NoKeyframe)?;
        let target = now.saturating_sub(duration);
        let start = *self
            .keyframes
            .iter()
            .rev()
            .find(|keyframe| keyframe.pts <= target)
            .or_else(|| self.keyframes.front())
            .filter(|keyframe| keyframe.pts < now)
            .ok_or(ClipError::NoKeyframe)?;
        let selection = Selection {
            video_from: start.seq,
            audio_from: self.clip_audio_start(start),
            end: now,
        };
        clip.begin(video_format, self.audio_format.as_ref(), start.pts, now);
        let front_seq = self
            .entries
            .front()
            .map_or(self.next_seq, |entry| entry.seq);
        let skip = usize::try_from(
            selection
                .video_from
                .min(selection.audio_from)
                .saturating_sub(front_seq),
        )
        .unwrap_or(usize::MAX);
        let included = || {
            self.entries
                .iter()
                .skip(skip)
                .filter(|entry| selection.includes(entry))
        };
        let (video, audio) =
            included().fold((0, 0), |(video, audio), entry| match entry.meta.kind {
                StreamKind::Video => (video + 1, audio),
                StreamKind::Audio => (video, audio + 1),
            });
        clip.reserve(video, audio);
        let mut shared = None;
        for entry in included() {
            if shared != Some(entry.block) {
                let Some(block) = self.store.block(entry.block) else {
                    continue;
                };
                clip.share(Arc::clone(block));
                shared = Some(entry.block);
            }
            clip.push_shared(
                entry.meta.kind,
                entry.meta,
                entry.offset..entry.offset.saturating_add(entry.len),
            );
        }
        Ok(())
    }

    /// Number of stored packets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ring holds no packets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Payload bytes currently stored.
    #[must_use]
    pub const fn bytes_used(&self) -> usize {
        self.store.used()
    }

    /// Bytes of payload blocks the ring holds now, used or not: its
    /// payload memory, apart from at most three spare blocks.
    #[must_use]
    pub const fn bytes_reserved(&self) -> usize {
        self.store.reserved()
    }

    /// Most bytes of payload blocks the ring may hold.
    #[must_use]
    pub const fn byte_capacity(&self) -> usize {
        self.store.limit()
    }

    /// Maximum number of stored packets.
    #[must_use]
    pub const fn packet_capacity(&self) -> usize {
        self.packet_capacity
    }

    /// Minimum span of media the ring tries to keep.
    #[must_use]
    pub const fn window(&self) -> Timestamp {
        self.window
    }

    /// Latest PTS pushed in the current segment.
    #[must_use]
    pub const fn newest_pts(&self) -> Option<Timestamp> {
        self.newest
    }

    /// Video format of the current segment.
    #[must_use]
    pub const fn video_format(&self) -> Option<&VideoFormat> {
        self.video_format.as_ref()
    }

    /// Current audio format.
    #[must_use]
    pub const fn audio_format(&self) -> Option<&AudioFormat> {
        self.audio_format.as_ref()
    }

    const fn check_push(&self, kind: StreamKind, payload: &[u8]) -> Result<(), PushError> {
        if payload.is_empty() {
            return Err(PushError::EmptyPayload);
        }
        if payload.len() > self.store.limit() {
            return Err(PushError::PayloadTooLarge {
                len: payload.len(),
                capacity: self.store.limit(),
            });
        }
        match kind {
            StreamKind::Video if self.video_format.is_none() => Err(PushError::MissingVideoFormat),
            StreamKind::Audio if self.audio_format.is_none() => Err(PushError::MissingAudioFormat),
            StreamKind::Video | StreamKind::Audio => Ok(()),
        }
    }

    /// A delta video frame whose reference frame is not stored.
    const fn is_orphan(&self, meta: PacketMeta) -> bool {
        matches!(meta.kind, StreamKind::Video) && !meta.keyframe && !self.video_synced
    }

    fn record(&mut self, meta: PacketMeta, block: u64, offset: usize, len: usize) {
        let seq = self.next_seq;
        self.next_seq = seq.wrapping_add(1);
        if matches!(meta.kind, StreamKind::Video) && meta.keyframe {
            self.keyframes.push_back(Keyframe { seq, pts: meta.pts });
            self.video_synced = true;
        }
        self.entries.push_back(Entry {
            meta,
            block,
            offset,
            len,
            seq,
        });
        self.newest = Some(self.newest.map_or(meta.pts, |newest| newest.max(meta.pts)));
    }

    /// Sequence number of the first audio packet a clip starting at `start`
    /// needs: the AAC preroll frame just before it if stored, else the first
    /// audio frame at or after it. `u64::MAX` if there is none.
    fn clip_audio_start(&self, start: Keyframe) -> u64 {
        let mut preroll = None;
        let audio = self.entries.iter().filter(|entry| {
            matches!(entry.meta.kind, StreamKind::Audio) && entry.seq >= self.audio_from_seq
        });
        for entry in audio {
            if entry.meta.pts >= start.pts {
                return preroll.unwrap_or(entry.seq);
            }
            preroll = needed_by_clip_at(entry, start.pts).then_some(entry.seq);
        }
        preroll.unwrap_or(u64::MAX)
    }

    /// Evicts until `len` more bytes and one more packet fit.
    fn make_room(&mut self, len: usize) {
        while !self.store.fits(len) || self.entries.len() >= self.packet_capacity {
            if !self.evict_step() {
                break;
            }
        }
    }

    /// Frees the oldest evictable unit; returns `false` if the ring is empty.
    ///
    /// In order of preference: one packet stored before the first keyframe
    /// (audio that arrived before any video, or leftovers of an evicted GOP),
    /// the oldest whole GOP, and as a last resort the current GOP, after which
    /// delta frames are dropped until the next keyframe.
    fn evict_step(&mut self) -> bool {
        if self.front_precedes_keyframes() {
            self.pop_entry();
        } else if self.keyframes.len() >= 2 {
            self.evict_oldest_gop();
        } else if self.entries.is_empty() {
            return false;
        } else {
            warn!(
                bytes = self.store.used(),
                packets = self.entries.len(),
                "clip ring overflowed within one GOP; dropped it and waiting for an IDR"
            );
            self.clear_packets();
        }
        true
    }

    /// Drops GOPs (and stale pre-keyframe packets) that the window no longer needs.
    fn enforce_window(&mut self) {
        let Some(newest) = self.newest else {
            return;
        };
        let cutoff = newest.saturating_sub(self.window);
        while self.keyframes.get(1).is_some_and(|next| next.pts <= cutoff) {
            self.evict_oldest_gop();
        }
        let first_keyframe = self.keyframes.front().map(|first| first.pts);
        while let Some(front) = self.entries.front()
            && self.front_precedes_keyframes()
            && first_keyframe.map_or(front.meta.pts < cutoff, |pts| {
                !(matches!(front.meta.kind, StreamKind::Audio) && needed_by_clip_at(front, pts))
            })
        {
            self.pop_entry();
        }
        self.warn_on_long_gop(newest);
    }

    /// Warns once per GOP when the oldest GOP alone outlasts the window.
    fn warn_on_long_gop(&mut self, newest: Timestamp) {
        let Some(first) = self.keyframes.front() else {
            return;
        };
        let gop_end = self.keyframes.get(1).map_or(newest, |next| next.pts);
        if !self.long_gop_warned && gop_end.saturating_sub(first.pts) > self.window {
            self.long_gop_warned = true;
            warn!(
                gop_start = first.pts.ticks(),
                gop_end = gop_end.ticks(),
                window = self.window.ticks(),
                "oldest GOP is longer than the clip window; the encoder must emit an IDR at least \
                 once per second"
            );
        }
    }

    /// Whether the front packet sits before the first stored keyframe.
    fn front_precedes_keyframes(&self) -> bool {
        self.entries.front().is_some_and(|front| {
            self.keyframes
                .front()
                .is_none_or(|first| front.seq < first.seq)
        })
    }

    /// Removes the oldest GOP: every packet stored before the second keyframe,
    /// except that it stops at the first audio packet a clip starting at that
    /// keyframe needs. Video left behind sits before the first keyframe, is
    /// never part of a clip, and is reclaimed by later evictions.
    fn evict_oldest_gop(&mut self) {
        let Some(next) = self.keyframes.get(1).copied() else {
            return;
        };
        while let Some(front) = self.entries.front()
            && front.seq < next.seq
            && !(matches!(front.meta.kind, StreamKind::Audio) && needed_by_clip_at(front, next.pts))
        {
            self.pop_entry();
        }
        self.keyframes.pop_front();
        self.long_gop_warned = false;
    }

    fn pop_entry(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            let first_needed = self.entries.front().map(|front| front.block);
            self.store.release(entry.len, first_needed);
        }
    }

    fn clear_packets(&mut self) {
        self.entries.clear();
        self.keyframes.clear();
        self.store.clear();
        self.video_synced = false;
        self.long_gop_warned = false;
    }
}

/// The packets of a clip: video from one sequence number, audio from
/// another, everything before the save time.
struct Selection {
    video_from: u64,
    audio_from: u64,
    end: Timestamp,
}

impl Selection {
    /// Whether `entry` belongs to the clip.
    fn includes(&self, entry: &Entry) -> bool {
        entry.meta.pts < self.end
            && match entry.meta.kind {
                StreamKind::Video => entry.seq >= self.video_from,
                StreamKind::Audio => entry.seq >= self.audio_from,
            }
    }
}

impl fmt::Debug for ClipRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipRing")
            .field("packets", &self.entries.len())
            .field("packet_capacity", &self.packet_capacity)
            .field("bytes_used", &self.store.used())
            .field("bytes_reserved", &self.store.reserved())
            .field("byte_capacity", &self.store.limit())
            .field("window", &self.window)
            .field("newest", &self.newest)
            .field("video_format", &self.video_format)
            .field("audio_format", &self.audio_format)
            .finish_non_exhaustive()
    }
}

/// Whether an audio packet is needed by a clip starting at `keyframe_pts`: it
/// starts at or after it, or is the AAC preroll frame just before it.
fn needed_by_clip_at(entry: &Entry, keyframe_pts: Timestamp) -> bool {
    let preroll = entry.meta.duration.saturating_add(entry.meta.duration);
    keyframe_pts.saturating_sub(entry.meta.pts) < preroll || entry.meta.pts >= keyframe_pts
}

/// Largest element count of `element_size` bytes a single allocation can hold.
fn max_allocation(element_size: usize) -> usize {
    usize::try_from(isize::MAX).unwrap_or(usize::MAX) / element_size.max(1)
}

/// The most media the ring holds for `window`: the window plus [`GOP_SLACK`].
const fn kept(window: Timestamp) -> Timestamp {
    let window = if window.ticks() < 0 {
        Timestamp::ZERO
    } else {
        window
    };
    window.saturating_add(GOP_SLACK)
}

/// Whole seconds the ring keeps for `window` (see [`kept`]), rounded up.
fn kept_secs(window: Timestamp) -> usize {
    let ticks = u64::try_from(kept(window).ticks()).unwrap_or(0);
    let secs = ticks.div_ceil(Timestamp::TICKS_PER_SECOND.unsigned_abs());
    usize::try_from(secs).unwrap_or(usize::MAX)
}

fn default_packet_capacity(window: Timestamp, byte_capacity: usize) -> usize {
    kept_secs(window)
        .saturating_mul(MAX_PACKETS_PER_SECOND)
        .min(byte_capacity)
}

#[cfg(test)]
mod tests;
