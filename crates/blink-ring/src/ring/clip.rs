//! Reusable buffer that a clip is copied into on the save thread.

use std::ops::Range;

use crate::media::{AudioFormat, PacketMeta, StreamKind, Timestamp, VideoFormat};

/// The packets of one clip, owned by the save thread and reused across saves.
///
/// Filled by [`ClipRing::begin_clip`](crate::ClipRing::begin_clip) and
/// [`ClipRing::copy_clip`](crate::ClipRing::copy_clip), or by hand with
/// [`ClipBuf::begin`] and the `push_*` methods. All payloads live in one byte
/// vector, so a reused buffer stops allocating once it has grown to the
/// largest clip saved.
///
/// The clip is presented from [`start`](Self::start) to [`end`](Self::end).
/// Video starts at a keyframe at `start`. Audio may include one frame before
/// `start` as AAC decoder preroll; the muxer trims it with an edit list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipBuf {
    video_format: Option<VideoFormat>,
    audio_format: Option<AudioFormat>,
    start: Timestamp,
    end: Timestamp,
    data: Vec<u8>,
    video: Vec<(PacketMeta, Range<usize>)>,
    audio: Vec<(PacketMeta, Range<usize>)>,
}

impl ClipBuf {
    /// Creates an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty buffer with room for `bytes` of payload and `packets` packets.
    #[must_use]
    pub fn with_capacity(bytes: usize, packets: usize) -> Self {
        Self {
            data: Vec::with_capacity(bytes),
            video: Vec::with_capacity(packets),
            audio: Vec::with_capacity(packets / 4),
            ..Self::default()
        }
    }

    /// Clears the buffer for a new clip, keeping its allocations.
    pub fn begin(
        &mut self,
        video_format: &VideoFormat,
        audio_format: Option<&AudioFormat>,
        start: Timestamp,
        end: Timestamp,
    ) {
        self.video_format = Some(video_format.clone());
        self.audio_format = audio_format.cloned();
        self.start = start;
        self.end = end;
        self.data.clear();
        self.video.clear();
        self.audio.clear();
    }

    /// Empties the buffer and makes room for a clip of up to `bytes` of
    /// payload and `packets` packets of each stream, touching the payload
    /// pages so that copying into them later neither allocates nor
    /// page-faults.
    ///
    /// Call it before [`ClipRing::begin_clip`](crate::ClipRing::begin_clip)
    /// without holding the ring's lock, sized from
    /// [`ClipRing::bytes_used`](crate::ClipRing::bytes_used) and
    /// [`ClipRing::len`](crate::ClipRing::len), so the copy under the lock
    /// only moves bytes.
    pub fn prepare(&mut self, bytes: usize, packets: usize) {
        self.data.clear();
        self.video.clear();
        self.audio.clear();
        self.data.resize(bytes, 0);
        self.data.clear();
        self.video.reserve(packets);
        self.audio.reserve(packets);
    }

    /// Reserves room for `bytes` more payload and `packets` more packets.
    pub fn reserve(&mut self, bytes: usize, packets: usize) {
        self.data.reserve(bytes);
        self.video.reserve(packets);
    }

    /// Appends a video access unit (Annex B, as pushed into the ring).
    pub fn push_video(&mut self, meta: PacketMeta, payload: &[u8]) {
        self.push_parts(StreamKind::Video, meta, payload, &[]);
    }

    /// Appends a raw AAC frame.
    pub fn push_audio(&mut self, meta: PacketMeta, payload: &[u8]) {
        self.push_parts(StreamKind::Audio, meta, payload, &[]);
    }

    /// Appends a packet whose payload is split in two, as at the arena's end.
    pub(super) fn push_parts(
        &mut self,
        kind: StreamKind,
        meta: PacketMeta,
        first: &[u8],
        second: &[u8],
    ) {
        let begin = self.data.len();
        self.data.extend_from_slice(first);
        self.data.extend_from_slice(second);
        let entry = (meta, begin..self.data.len());
        match kind {
            StreamKind::Video => self.video.push(entry),
            StreamKind::Audio => self.audio.push(entry),
        }
    }

    /// Video format of the clip; `None` until [`begin`](Self::begin).
    #[must_use]
    pub const fn video_format(&self) -> Option<&VideoFormat> {
        self.video_format.as_ref()
    }

    /// Audio format of the clip, if audio was recorded.
    #[must_use]
    pub const fn audio_format(&self) -> Option<&AudioFormat> {
        self.audio_format.as_ref()
    }

    /// Presentation start: the PTS of the first video keyframe.
    #[must_use]
    pub const fn start(&self) -> Timestamp {
        self.start
    }

    /// Presentation end: the save request time. Packets at or after it are excluded.
    #[must_use]
    pub const fn end(&self) -> Timestamp {
        self.end
    }

    /// Presented length of the clip, `end - start`.
    #[must_use]
    pub const fn duration(&self) -> Timestamp {
        self.end.saturating_sub(self.start)
    }

    /// Video packets in decode order.
    pub fn video(&self) -> impl Iterator<Item = (PacketMeta, &[u8])> {
        self.packets(&self.video)
    }

    /// Audio packets in order.
    pub fn audio(&self) -> impl Iterator<Item = (PacketMeta, &[u8])> {
        self.packets(&self.audio)
    }

    /// Number of video packets.
    #[must_use]
    pub const fn video_len(&self) -> usize {
        self.video.len()
    }

    /// Number of audio packets.
    #[must_use]
    pub const fn audio_len(&self) -> usize {
        self.audio.len()
    }

    /// Total payload bytes.
    #[must_use]
    pub const fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Payload bytes the buffer can hold without allocating.
    #[must_use]
    pub const fn data_capacity(&self) -> usize {
        self.data.capacity()
    }

    fn packets<'a>(
        &'a self,
        list: &'a [(PacketMeta, Range<usize>)],
    ) -> impl Iterator<Item = (PacketMeta, &'a [u8])> {
        list.iter()
            .filter_map(|(meta, range)| Some((*meta, self.data.get(range.clone())?)))
    }
}
