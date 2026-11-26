//! The packets of one saved clip.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use super::blocks::Block;
use crate::media::{AudioFormat, PacketMeta, StreamKind, Timestamp, VideoFormat};

/// The packets of one clip, filled by
/// [`ClipRing::snapshot`](crate::ClipRing::snapshot) or by hand with
/// [`ClipBuf::begin`] and the `push_*` methods.
///
/// A clip cut from the ring copies no payload: it shares the ring's blocks,
/// which stay alive until the clip is dropped even if the ring evicts them
/// meanwhile. Drop the clip once it is written, so those blocks are freed.
///
/// The clip is presented from [`start`](Self::start) to [`end`](Self::end).
/// Video starts at a keyframe at `start`. Audio may include one frame before
/// `start` as AAC decoder preroll; the muxer trims it with an edit list.
#[derive(Clone, Default)]
pub struct ClipBuf {
    video_format: Option<VideoFormat>,
    audio_format: Option<AudioFormat>,
    start: Timestamp,
    end: Timestamp,
    blocks: Vec<Block>,
    video: Vec<Packet>,
    audio: Vec<Packet>,
    data_len: usize,
}

/// Where one packet's payload lives in a clip's blocks.
#[derive(Clone)]
struct Packet {
    meta: PacketMeta,
    block: usize,
    range: Range<usize>,
}

impl ClipBuf {
    /// Creates an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears the buffer for a new clip.
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
        self.blocks.clear();
        self.video.clear();
        self.audio.clear();
        self.data_len = 0;
    }

    /// Makes room for `video` and `audio` more packets.
    pub(super) fn reserve(&mut self, video: usize, audio: usize) {
        self.video.reserve(video);
        self.audio.reserve(audio);
    }

    /// Appends a video access unit (Annex B, as pushed into the ring),
    /// copying it.
    pub fn push_video(&mut self, meta: PacketMeta, payload: &[u8]) {
        self.push_owned(StreamKind::Video, meta, payload);
    }

    /// Appends a raw AAC frame, copying it.
    pub fn push_audio(&mut self, meta: PacketMeta, payload: &[u8]) {
        self.push_owned(StreamKind::Audio, meta, payload);
    }

    fn push_owned(&mut self, kind: StreamKind, meta: PacketMeta, payload: &[u8]) {
        self.share(Arc::new(payload.to_vec()));
        self.push_shared(kind, meta, 0..payload.len());
    }

    /// Adds a block that the following [`push_shared`](Self::push_shared)
    /// packets live in.
    pub(super) fn share(&mut self, block: Block) {
        self.blocks.push(block);
    }

    /// Appends a packet whose payload is `range` of the last shared block.
    pub(super) fn push_shared(&mut self, kind: StreamKind, meta: PacketMeta, range: Range<usize>) {
        self.data_len = self.data_len.saturating_add(range.len());
        let packet = Packet {
            meta,
            block: self.blocks.len().saturating_sub(1),
            range,
        };
        match kind {
            StreamKind::Video => self.video.push(packet),
            StreamKind::Audio => self.audio.push(packet),
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
        self.data_len
    }

    fn packets<'a>(&'a self, list: &'a [Packet]) -> impl Iterator<Item = (PacketMeta, &'a [u8])> {
        list.iter().filter_map(|packet| {
            let block = self.blocks.get(packet.block)?;
            Some((packet.meta, block.get(packet.range.clone())?))
        })
    }
}

impl fmt::Debug for ClipBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipBuf")
            .field("video_format", &self.video_format)
            .field("audio_format", &self.audio_format)
            .field("start", &self.start)
            .field("end", &self.end)
            .field("video_packets", &self.video.len())
            .field("audio_packets", &self.audio.len())
            .field("blocks", &self.blocks.len())
            .field("data_len", &self.data_len)
            .finish()
    }
}
