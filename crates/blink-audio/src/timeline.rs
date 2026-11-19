//! The audio sample clock and silence gap-fill.
//!
//! WASAPI loopback delivers nothing while nothing plays, may drop data when
//! the capture thread is starved, and its device clock drifts against QPC.
//! [`Timeline`] keeps the AAC stream continuous and on the shared 100 ns QPC
//! timeline anyway:
//!
//! - A segment is anchored at a QPC time. Frame `n` of the segment is due at
//!   `anchor + n / 48 kHz`, computed exactly from the frame count, so rounding
//!   never accumulates.
//! - Each captured packet carries the QPC time of its first frame; its drift is
//!   that time minus the next frame's due time.
//! - **Onsets are exact.** The first packet of a segment, after a silence
//!   fill, or flagged `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY` is placed on its
//!   timestamp: a late packet gets zero frames inserted before it (within
//!   [`RESYNC_TOLERANCE`]). An early packet after a fill overlaps silence that
//!   was already emitted, so it is appended unless it is more than
//!   [`DRIFT_TOLERANCE`] early; trimming real audio to fix our own fill would
//!   cut the onset.
//! - **Device-clock drift is slewed.** In continuous audio, drift beyond
//!   [`DRIFT_TOLERANCE`] starts a correction that repeats or drops at most one
//!   frame per hundred per packet (with a minimum of one), until the drift is
//!   back under [`SLEW_TARGET`]. At a typical 100 ppm that is a frame or two
//!   every few seconds instead of an audible 20 ms jump every few minutes.
//! - **Real gaps are filled.** Drift beyond [`HARD_LIMIT`] in continuous audio
//!   (lost data without a discontinuity flag) is corrected at once with
//!   silence or by dropping the overlap.
//! - When no packets arrive, [`Timeline::fill_silence`] writes silence up to
//!   the wall clock minus [`FILL_LAG`], leaving room for a packet that is
//!   already in flight to land without being trimmed. The caller must read
//!   that wall clock *before* draining the WASAPI buffer, so the fill never
//!   runs ahead of audio that was still queued.
//! - A gap longer than [`MAX_GAP`] (system sleep, a stalled thread) is not
//!   filled; the segment ends and a new one is anchored at the new time.
//!
//! Frames are assembled into 1024-frame chunks of interleaved 16-bit PCM, one
//! AAC access unit each, and handed to a sink with the chunk's timestamp.

use blink_ring::media::Timestamp;

use crate::pcm::{self, SampleFormat};

/// Interleaved samples in one chunk: [`FRAMES_PER_PACKET`](crate::FRAMES_PER_PACKET)
/// frames of [`CHANNELS`](crate::CHANNELS) samples.
pub(crate) const CHUNK_SAMPLES: usize = 2048;

/// Frames in one chunk.
const CHUNK_FRAMES: u64 = 1024;

/// Samples per frame.
const CHANNELS: usize = 2;

/// Frames per second.
const SAMPLE_RATE: i128 = 48_000;

/// Drift in continuous audio that starts a slewed correction: 20 ms, about two
/// WASAPI periods, so timestamp jitter never triggers one.
pub(crate) const DRIFT_TOLERANCE: i64 = Timestamp::from_millis(20).ticks();

/// Drift a slewed correction stops at: 5 ms.
pub(crate) const SLEW_TARGET: i64 = Timestamp::from_millis(5).ticks();

/// A slewed correction changes at most one frame in this many per packet.
const SLEW_DIVISOR: u64 = 100;

/// Drift in continuous audio that is corrected at once instead of slewed:
/// 50 ms. Device-clock drift never gets here because slewing starts at
/// [`DRIFT_TOLERANCE`]; only lost or duplicated data does.
pub(crate) const HARD_LIMIT: i64 = Timestamp::from_millis(50).ticks();

/// How exactly an onset is placed: 1 ms.
pub(crate) const RESYNC_TOLERANCE: i64 = Timestamp::from_millis(1).ticks();

/// How far behind the wall clock silence fill stops: 60 ms. A packet's
/// timestamp is the capture time of its first frame, which precedes its
/// delivery by up to a period (10 ms) plus scheduling delay.
pub(crate) const FILL_LAG: i64 = Timestamp::from_millis(60).ticks();

/// Longest gap filled with silence: 60 s (about 2,800 cheap silent AAC
/// frames). Longer gaps start a new segment with a timestamp jump.
pub(crate) const MAX_GAP: i64 = Timestamp::from_secs(60).ticks();

/// Converts a frame count to 100 ns ticks, rounding down and saturating.
pub(crate) fn frames_to_ticks(frames: u64) -> i64 {
    let ticks = i128::from(frames) * i128::from(Timestamp::TICKS_PER_SECOND) / SAMPLE_RATE;
    i64::try_from(ticks).unwrap_or(i64::MAX)
}

/// Converts a non-negative tick span to the nearest whole frame count.
/// Negative spans are zero frames.
pub(crate) fn ticks_to_frames(ticks: i64) -> u64 {
    if ticks <= 0 {
        return 0;
    }
    let per_second = i128::from(Timestamp::TICKS_PER_SECOND);
    let frames = (i128::from(ticks) * SAMPLE_RATE + per_second / 2) / per_second;
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// One captured WASAPI packet.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Packet<'a> {
    /// `AUDCLNT_BUFFERFLAGS_SILENT`: treat the packet as `frames` zero frames.
    Silent {
        /// Frames in the packet.
        frames: u32,
    },
    /// Interleaved stereo PCM, little-endian. A trailing partial frame is
    /// ignored.
    Pcm {
        /// Sample encoding.
        format: SampleFormat,
        /// Raw packet bytes.
        data: &'a [u8],
    },
}

impl Packet<'_> {
    /// Whole frames in the packet.
    pub(crate) fn frames(&self) -> u64 {
        match *self {
            Self::Silent { frames } => u64::from(frames),
            Self::Pcm { format, data } => {
                let frames = data.len() / (format.bytes_per_sample() * CHANNELS);
                u64::try_from(frames).unwrap_or(u64::MAX)
            }
        }
    }

    /// The packet's first frame as 16-bit samples (silence if empty).
    fn first_frame(&self) -> [i16; CHANNELS] {
        let mut frame = [0; CHANNELS];
        if let Self::Pcm { format, data } = *self {
            let bytes = data.get(..format.bytes_per_sample() * CHANNELS);
            if let Some(bytes) = bytes {
                pcm::convert(format, bytes, &mut frame);
            }
        }
        frame
    }
}

/// A complete 1024-frame chunk, ready for the encoder.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Chunk<'a> {
    /// [`CHUNK_SAMPLES`] interleaved 16-bit samples.
    pub(crate) pcm: &'a [i16],
    /// Timestamp of the chunk's first frame.
    pub(crate) pts: Timestamp,
}

/// What the timeline hands to its sink.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Event<'a> {
    /// A complete chunk.
    Chunk(Chunk<'a>),
    /// The segment ended after the previous chunk: drain the encoder so every
    /// frame of the old segment is emitted before the next chunk, which starts
    /// a new segment at a discontinuous timestamp.
    SegmentEnd,
}

/// How a packet or a fill moved the clock, for logging, statistics, and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Correction {
    /// Appended without correction.
    None,
    /// This many zero frames were inserted first.
    Silence(u64),
    /// This many frames at the head of the packet were dropped at once.
    Skipped(u64),
    /// Slewing: the packet's first frame was repeated this many extra times.
    Repeated(u64),
    /// Slewing: this many frames at the head of the packet were dropped.
    Dropped(u64),
    /// The gap exceeded [`MAX_GAP`]; a new segment started.
    Reanchored,
}

/// Sample clock plus chunk assembler. See the module documentation.
#[derive(Debug, Clone)]
pub(crate) struct Timeline {
    anchor: i64,
    chunks: u64,
    filled: usize,
    /// The next packet is an onset: nothing real has been written since the
    /// segment started or since the last silence fill.
    onset: bool,
    /// A slewed drift correction is in progress.
    slewing: bool,
    buf: [i16; CHUNK_SAMPLES],
}

impl Timeline {
    /// Starts a segment whose first frame is due at `anchor` (100 ns ticks).
    pub(crate) const fn new(anchor: i64) -> Self {
        Self {
            anchor,
            chunks: 0,
            filled: 0,
            onset: true,
            slewing: false,
            buf: [0; CHUNK_SAMPLES],
        }
    }

    /// Frames written to the current segment, including the partial chunk.
    pub(crate) fn frames_written(&self) -> u64 {
        let partial = u64::try_from(self.filled / CHANNELS).unwrap_or(u64::MAX);
        self.chunks
            .saturating_mul(CHUNK_FRAMES)
            .saturating_add(partial)
    }

    /// Due time of the next frame.
    pub(crate) fn expected(&self) -> i64 {
        self.anchor
            .saturating_add(frames_to_ticks(self.frames_written()))
    }

    /// Appends a captured packet whose first frame was captured at
    /// `timestamp`, correcting the clock first if needed (see the module
    /// documentation).
    ///
    /// `timestamp` is `None` when WASAPI flagged it invalid
    /// (`AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`); the packet is then assumed to
    /// be contiguous.
    pub(crate) fn push_packet<E>(
        &mut self,
        packet: Packet<'_>,
        timestamp: Option<i64>,
        discontinuity: bool,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<Correction, E> {
        let correction = match timestamp {
            None => Correction::None,
            Some(timestamp) => {
                let drift = timestamp.saturating_sub(self.expected());
                if drift.unsigned_abs() > MAX_GAP.unsigned_abs() {
                    self.end_segment(timestamp, sink)?;
                    Correction::Reanchored
                } else if discontinuity || self.onset {
                    self.slewing = false;
                    let early_tolerance = if discontinuity {
                        RESYNC_TOLERANCE
                    } else {
                        DRIFT_TOLERANCE
                    };
                    self.correct_at_once(drift, RESYNC_TOLERANCE, early_tolerance, &packet, sink)?
                } else if drift.unsigned_abs() > HARD_LIMIT.unsigned_abs() {
                    self.slewing = false;
                    self.correct_at_once(drift, HARD_LIMIT, HARD_LIMIT, &packet, sink)?
                } else {
                    self.slew(drift, &packet)
                }
            }
        };
        let skip = match correction {
            Correction::Skipped(frames) | Correction::Dropped(frames) => frames,
            Correction::Repeated(frames) => {
                self.write_frame(packet.first_frame(), frames, sink)?;
                0
            }
            _ => 0,
        };
        self.write(packet, skip, sink)?;
        if packet.frames() > skip {
            self.onset = false;
        }
        Ok(correction)
    }

    /// Writes silence up to `now - FILL_LAG`, for when no packets arrive.
    ///
    /// `now` must be read before the WASAPI buffer is drained in the same
    /// iteration; see the module documentation.
    pub(crate) fn fill_silence<E>(
        &mut self,
        now: i64,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<Correction, E> {
        let target = now.saturating_sub(FILL_LAG);
        let gap = target.saturating_sub(self.expected());
        if gap > MAX_GAP {
            self.end_segment(target, sink)?;
            return Ok(Correction::Reanchored);
        }
        let frames = ticks_to_frames(gap);
        if frames == 0 {
            return Ok(Correction::None);
        }
        self.write_frame([0; CHANNELS], frames, sink)?;
        self.onset = true;
        self.slewing = false;
        Ok(Correction::Silence(frames))
    }

    /// Pads the partial chunk with silence, emits it, and ends the segment.
    /// Used on shutdown so the final frames reach the sink.
    pub(crate) fn finish<E>(
        &mut self,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.flush_padded(sink)?;
        sink(Event::SegmentEnd)
    }

    /// Inserts silence for a late packet (beyond `late`) or schedules dropping
    /// the overlap of an early one (beyond `early`).
    fn correct_at_once<E>(
        &mut self,
        drift: i64,
        late: i64,
        early: i64,
        packet: &Packet<'_>,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<Correction, E> {
        if drift > late {
            let frames = ticks_to_frames(drift);
            self.write_frame([0; CHANNELS], frames, sink)?;
            Ok(Correction::Silence(frames))
        } else if drift < -early {
            Ok(Correction::Skipped(
                ticks_to_frames(-drift).min(packet.frames()),
            ))
        } else {
            Ok(Correction::None)
        }
    }

    /// One step of a slewed correction for continuous audio.
    fn slew(&mut self, drift: i64, packet: &Packet<'_>) -> Correction {
        if drift.unsigned_abs() > DRIFT_TOLERANCE.unsigned_abs() {
            self.slewing = true;
        }
        let excess =
            ticks_to_frames(drift.saturating_abs()).saturating_sub(ticks_to_frames(SLEW_TARGET));
        if !self.slewing || excess == 0 {
            self.slewing = false;
            return Correction::None;
        }
        let step = (packet.frames() / SLEW_DIVISOR).max(1).min(excess);
        if drift > 0 {
            Correction::Repeated(step)
        } else {
            Correction::Dropped(step.min(packet.frames()))
        }
    }

    fn end_segment<E>(
        &mut self,
        anchor: i64,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.finish(sink)?;
        self.anchor = anchor;
        self.chunks = 0;
        self.filled = 0;
        self.onset = true;
        self.slewing = false;
        Ok(())
    }

    fn flush_padded<E>(
        &mut self,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.filled == 0 {
            return Ok(());
        }
        let missing = u64::try_from((CHUNK_SAMPLES - self.filled) / CHANNELS).unwrap_or(0);
        self.write_frame([0; CHANNELS], missing, sink)
    }

    /// Writes `frame` `count` times.
    fn write_frame<E>(
        &mut self,
        frame: [i16; CHANNELS],
        count: u64,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut remaining = count;
        while remaining > 0 {
            let space = (CHUNK_SAMPLES - self.filled) / CHANNELS;
            let frames = usize::try_from(remaining).unwrap_or(usize::MAX).min(space);
            let end = self.filled + frames * CHANNELS;
            if let Some(dst) = self.buf.get_mut(self.filled..end) {
                for out in dst.as_chunks_mut::<CHANNELS>().0 {
                    *out = frame;
                }
            }
            self.filled = end;
            remaining = remaining.saturating_sub(u64::try_from(frames).unwrap_or(u64::MAX));
            self.emit_if_full(sink)?;
        }
        Ok(())
    }

    fn write<E>(
        &mut self,
        packet: Packet<'_>,
        skip_frames: u64,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        match packet {
            Packet::Silent { frames } => self.write_frame(
                [0; CHANNELS],
                u64::from(frames).saturating_sub(skip_frames),
                sink,
            ),
            Packet::Pcm { format, data } => {
                let frame_bytes = format.bytes_per_sample() * CHANNELS;
                let whole = data.len() - data.len() % frame_bytes;
                let start = usize::try_from(skip_frames)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(frame_bytes)
                    .min(whole);
                let mut src = data.get(start..whole).unwrap_or_default();
                while !src.is_empty() {
                    let dst = self.buf.get_mut(self.filled..).unwrap_or_default();
                    let count = pcm::convert(format, src, dst);
                    if count == 0 {
                        break;
                    }
                    self.filled += count;
                    src = src
                        .get(count * format.bytes_per_sample()..)
                        .unwrap_or_default();
                    self.emit_if_full(sink)?;
                }
                Ok(())
            }
        }
    }

    fn emit_if_full<E>(
        &mut self,
        sink: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.filled < CHUNK_SAMPLES {
            return Ok(());
        }
        let offset = frames_to_ticks(self.chunks.saturating_mul(CHUNK_FRAMES));
        let pts = Timestamp::from_ticks(self.anchor.saturating_add(offset));
        self.filled = 0;
        self.chunks += 1;
        sink(Event::Chunk(Chunk {
            pcm: &self.buf,
            pts,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CHANNELS as PUB_CHANNELS, FRAMES_PER_PACKET, PACKET_DURATION, SAMPLE_RATE as PUB_RATE,
    };

    /// What a test sink saw.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Seen {
        Chunk { pts: i64, first: i16, last: i16 },
        End,
    }

    #[derive(Default)]
    struct Recorder(Vec<Seen>);

    impl Recorder {
        fn sink(&mut self) -> impl FnMut(Event<'_>) -> Result<(), ()> + '_ {
            |event: Event<'_>| {
                self.0.push(match event {
                    Event::Chunk(chunk) => {
                        assert_eq!(chunk.pcm.len(), CHUNK_SAMPLES);
                        Seen::Chunk {
                            pts: chunk.pts.ticks(),
                            first: chunk.pcm[0],
                            last: chunk.pcm[CHUNK_SAMPLES - 1],
                        }
                    }
                    Event::SegmentEnd => Seen::End,
                });
                Ok(())
            }
        }

        fn chunk_pts(&self) -> Vec<i64> {
            self.0
                .iter()
                .filter_map(|seen| match seen {
                    Seen::Chunk { pts, .. } => Some(*pts),
                    Seen::End => None,
                })
                .collect()
        }
    }

    fn s16_packet(frames: usize, value: i16) -> Vec<u8> {
        std::iter::repeat_n(value.to_le_bytes(), frames * CHANNELS)
            .flatten()
            .collect()
    }

    fn pcm(data: &[u8]) -> Packet<'_> {
        Packet::Pcm {
            format: SampleFormat::S16,
            data,
        }
    }

    const ANCHOR: i64 = 1_000_000_000;

    #[test]
    fn constants_agree_with_the_public_ones() {
        assert_eq!(
            CHUNK_SAMPLES,
            usize::try_from(FRAMES_PER_PACKET).unwrap() * usize::from(PUB_CHANNELS)
        );
        assert_eq!(CHUNK_FRAMES, u64::from(FRAMES_PER_PACKET));
        assert_eq!(SAMPLE_RATE, i128::from(PUB_RATE));
        assert_eq!(frames_to_ticks(CHUNK_FRAMES), PACKET_DURATION.ticks());
    }

    #[test]
    fn tick_conversions() {
        assert_eq!(frames_to_ticks(48_000), 10_000_000);
        assert_eq!(frames_to_ticks(1), 208);
        assert_eq!(frames_to_ticks(3), 625);
        assert_eq!(frames_to_ticks(u64::MAX), i64::MAX);
        assert_eq!(ticks_to_frames(10_000_000), 48_000);
        assert_eq!(ticks_to_frames(208), 1);
        assert_eq!(ticks_to_frames(104), 0);
        assert_eq!(ticks_to_frames(105), 1);
        assert_eq!(ticks_to_frames(0), 0);
        assert_eq!(ticks_to_frames(-1_000), 0);
    }

    #[test]
    fn contiguous_packets_need_no_correction() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let data = s16_packet(480, 5);
        for i in 0..10_u64 {
            let ts = ANCHOR + frames_to_ticks(i * 480);
            let correction = timeline
                .push_packet(pcm(&data), Some(ts), false, &mut rec.sink())
                .unwrap();
            assert_eq!(correction, Correction::None);
        }
        assert_eq!(timeline.frames_written(), 4_800);
        assert_eq!(
            rec.chunk_pts(),
            [0, 1, 2, 3].map(|k| ANCHOR + frames_to_ticks(k * 1024))
        );
        assert!(rec.0.iter().all(|seen| matches!(
            seen,
            Seen::Chunk {
                first: 5,
                last: 5,
                ..
            }
        )));
    }

    #[test]
    fn exact_timestamps_never_drift() {
        let mut timeline = Timeline::new(0);
        let data = s16_packet(441, 1);
        let mut chunks = 0_u64;
        let mut sink = |event: Event<'_>| {
            if let Event::Chunk(chunk) = event {
                assert_eq!(chunk.pts.ticks(), frames_to_ticks(chunks * 1024));
                chunks += 1;
            }
            Ok::<(), ()>(())
        };
        let packets = 120 * 48_000 / 441;
        for i in 0..packets {
            let ts = frames_to_ticks(i * 441);
            let correction = timeline
                .push_packet(pcm(&data), Some(ts), false, &mut sink)
                .unwrap();
            assert_eq!(correction, Correction::None);
        }
        assert_eq!(chunks, packets * 441 / 1024);
    }

    #[test]
    fn jitter_within_tolerance_is_ignored() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let data = s16_packet(480, 1);
        timeline
            .push_packet(pcm(&data), Some(ANCHOR), false, &mut rec.sink())
            .unwrap();
        let jitter = DRIFT_TOLERANCE - 1;
        let late_ts = timeline.expected() + jitter;
        let late = timeline
            .push_packet(pcm(&data), Some(late_ts), false, &mut rec.sink())
            .unwrap();
        let early_ts = timeline.expected() - jitter;
        let early = timeline
            .push_packet(pcm(&data), Some(early_ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!((late, early), (Correction::None, Correction::None));
        assert_eq!(timeline.frames_written(), 1_440);
    }

    #[test]
    fn late_packet_inserts_silence() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let data = s16_packet(1024, 9);
        // 2048 frames of silence before the first packet.
        let ts = ANCHOR + frames_to_ticks(2048);
        let correction = timeline
            .push_packet(pcm(&data), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::Silence(2048));
        assert_eq!(
            rec.0,
            [
                Seen::Chunk {
                    pts: ANCHOR,
                    first: 0,
                    last: 0
                },
                Seen::Chunk {
                    pts: ANCHOR + frames_to_ticks(1024),
                    first: 0,
                    last: 0
                },
                Seen::Chunk {
                    pts: ts,
                    first: 9,
                    last: 9
                },
            ]
        );
    }

    #[test]
    fn far_early_packet_drops_overlapping_frames() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let first = s16_packet(3000, 1);
        timeline
            .push_packet(pcm(&first), Some(ANCHOR), false, &mut rec.sink())
            .unwrap();
        // The next packet claims to start 2880 frames (60 ms) before the clock.
        let ts = ANCHOR + frames_to_ticks(120);
        let second = s16_packet(3000, 2);
        let correction = timeline
            .push_packet(pcm(&second), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::Skipped(2880));
        assert_eq!(timeline.frames_written(), 3_120);
        assert_eq!(
            rec.0,
            [
                Seen::Chunk {
                    pts: ANCHOR,
                    first: 1,
                    last: 1
                },
                Seen::Chunk {
                    pts: ANCHOR + frames_to_ticks(1024),
                    first: 1,
                    last: 1
                },
                Seen::Chunk {
                    pts: ANCHOR + frames_to_ticks(2048),
                    first: 1,
                    last: 2
                },
            ]
        );
    }

    #[test]
    fn far_late_continuous_packet_inserts_silence() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline
            .push_packet(
                pcm(&s16_packet(480, 1)),
                Some(ANCHOR),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        let ts = timeline.expected() + HARD_LIMIT + 10_000;
        let correction = timeline
            .push_packet(pcm(&s16_packet(480, 1)), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!(
            correction,
            Correction::Silence(ticks_to_frames(HARD_LIMIT + 10_000))
        );
    }

    #[test]
    fn moderate_drift_is_slewed_not_jumped() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline
            .push_packet(
                pcm(&s16_packet(480, 1)),
                Some(ANCHOR),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        // 25 ms late: repeat the first frame, at most 480 / 100 frames.
        let late = timeline.expected() + 250_000;
        let correction = timeline
            .push_packet(pcm(&s16_packet(480, 9)), Some(late), false, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::Repeated(4));
        assert_eq!(timeline.frames_written(), 964);
        // Still slewing while above the target, even inside the tolerance.
        let still_late = timeline.expected() + 100_000;
        let correction = timeline
            .push_packet(
                pcm(&s16_packet(480, 9)),
                Some(still_late),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(correction, Correction::Repeated(4));
        // Back under the target: done.
        let close = timeline.expected() + SLEW_TARGET - 1_000;
        let correction = timeline
            .push_packet(
                pcm(&s16_packet(480, 9)),
                Some(close),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(correction, Correction::None);
        // Early drift drops frames instead.
        let early = timeline.expected() - 250_000;
        let correction = timeline
            .push_packet(
                pcm(&s16_packet(480, 9)),
                Some(early),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(correction, Correction::Dropped(4));
        assert_eq!(
            rec.0.first(),
            Some(&Seen::Chunk {
                pts: ANCHOR,
                first: 1,
                last: 9
            })
        );
    }

    /// Simulates a device clock `ppm` parts per million off QPC for `seconds`
    /// and returns the corrections and the final drift.
    fn simulate_drift(ppm: i64, seconds: u64) -> (Vec<Correction>, i64) {
        let mut timeline = Timeline::new(0);
        let mut sink = |_: Event<'_>| Ok::<(), ()>(());
        let data = s16_packet(480, 1);
        let mut corrections = Vec::new();
        let mut last_ts = 0;
        for i in 0..seconds * 100 {
            let nominal = frames_to_ticks(i * 480);
            last_ts = nominal + nominal * ppm / 1_000_000;
            let correction = timeline
                .push_packet(pcm(&data), Some(last_ts), false, &mut sink)
                .unwrap();
            if correction != Correction::None {
                corrections.push(correction);
            }
        }
        (
            corrections,
            last_ts - (timeline.expected() - frames_to_ticks(480)),
        )
    }

    #[test]
    fn slow_device_clock_is_slewed_without_jumps() {
        let (corrections, drift) = simulate_drift(100, 600);
        assert!(corrections.len() > 10, "{} corrections", corrections.len());
        assert!(
            corrections
                .iter()
                .all(|c| matches!(c, Correction::Repeated(1..=4)))
        );
        assert!(drift.abs() <= DRIFT_TOLERANCE + 10_000, "drift {drift}");
    }

    #[test]
    fn fast_device_clock_is_slewed_without_jumps() {
        let (corrections, drift) = simulate_drift(-100, 600);
        assert!(corrections.len() > 10, "{} corrections", corrections.len());
        assert!(
            corrections
                .iter()
                .all(|c| matches!(c, Correction::Dropped(1..=4)))
        );
        assert!(drift.abs() <= DRIFT_TOLERANCE + 10_000, "drift {drift}");
    }

    #[test]
    fn onset_after_fill_is_exact() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline
            .push_packet(
                pcm(&s16_packet(480, 1)),
                Some(ANCHOR),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        let now = ANCHOR + 20_000_000;
        timeline.fill_silence(now, &mut rec.sink()).unwrap();
        // 5 ms late without a discontinuity flag: still placed exactly.
        let ts = timeline.expected() + 50_000;
        let correction = timeline
            .push_packet(pcm(&s16_packet(480, 2)), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::Silence(240));
        // The next packet is continuous audio again: jitter is tolerated.
        let jitter = timeline.expected() + 50_000;
        let correction = timeline
            .push_packet(
                pcm(&s16_packet(480, 2)),
                Some(jitter),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(correction, Correction::None);
    }

    #[test]
    fn packet_in_flight_at_the_clock_read_is_not_trimmed() {
        // The loop reads `now`, drains, then fills to `now - FILL_LAG`. A
        // packet captured just before `now` lands after an earlier fill.
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let now = ANCHOR + 20_000_000;
        timeline.fill_silence(now, &mut rec.sink()).unwrap();
        let ts = now - 100_000;
        let correction = timeline
            .push_packet(pcm(&s16_packet(480, 3)), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert!(matches!(correction, Correction::Silence(_)));
    }

    #[test]
    fn early_onset_overlapping_fill_is_kept() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline
            .fill_silence(ANCHOR + 20_000_000, &mut rec.sink())
            .unwrap();
        let ts = timeline.expected() - 150_000;
        let correction = timeline
            .push_packet(pcm(&s16_packet(480, 3)), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::None);
        // Far beyond the tolerance the overlap is still dropped.
        timeline
            .fill_silence(ANCHOR + 40_000_000, &mut rec.sink())
            .unwrap();
        let ts = timeline.expected() - 400_000;
        let correction = timeline
            .push_packet(pcm(&s16_packet(4800, 3)), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::Skipped(1920));
    }

    #[test]
    fn skip_is_capped_at_the_packet() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline
            .push_packet(
                pcm(&s16_packet(4096, 1)),
                Some(ANCHOR),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        let correction = timeline
            .push_packet(
                pcm(&s16_packet(100, 2)),
                Some(ANCHOR),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(correction, Correction::Skipped(100));
        assert_eq!(timeline.frames_written(), 4096);
    }

    #[test]
    fn discontinuity_resyncs_tightly() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let data = s16_packet(480, 1);
        let ts = ANCHOR + 50_000;
        let correction = timeline
            .push_packet(pcm(&data), Some(ts), true, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::Silence(240));
        let within = timeline.expected() + RESYNC_TOLERANCE;
        let correction = timeline
            .push_packet(pcm(&data), Some(within), true, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::None);
    }

    #[test]
    fn invalid_timestamp_appends_contiguously() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let correction = timeline
            .push_packet(pcm(&s16_packet(2048, 3)), None, true, &mut rec.sink())
            .unwrap();
        assert_eq!(correction, Correction::None);
        assert_eq!(rec.chunk_pts(), [ANCHOR, ANCHOR + frames_to_ticks(1024)]);
    }

    #[test]
    fn silent_packets_write_zeros() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline
            .push_packet(
                pcm(&s16_packet(24, 7)),
                Some(ANCHOR),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        timeline
            .push_packet(
                Packet::Silent { frames: 1000 },
                None,
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(
            rec.0,
            [Seen::Chunk {
                pts: ANCHOR,
                first: 7,
                last: 0
            }]
        );
    }

    #[test]
    fn float_packets_are_converted() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let data: Vec<u8> = std::iter::repeat_n(0.5_f32.to_le_bytes(), CHUNK_SAMPLES)
            .flatten()
            .collect();
        let packet = Packet::Pcm {
            format: SampleFormat::F32,
            data: &data,
        };
        assert_eq!(packet.frames(), 1024);
        timeline
            .push_packet(packet, Some(ANCHOR), false, &mut rec.sink())
            .unwrap();
        assert_eq!(
            rec.0,
            [Seen::Chunk {
                pts: ANCHOR,
                first: 16_384,
                last: 16_384
            }]
        );
    }

    #[test]
    fn partial_trailing_frame_is_ignored() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let mut data = s16_packet(10, 1);
        data.extend_from_slice(&[1, 2, 3]);
        let packet = pcm(&data);
        assert_eq!(packet.frames(), 10);
        timeline
            .push_packet(packet, Some(ANCHOR), false, &mut rec.sink())
            .unwrap();
        assert_eq!(timeline.frames_written(), 10);
    }

    #[test]
    fn fill_silence_lags_the_wall_clock() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        assert_eq!(
            timeline.fill_silence(ANCHOR, &mut rec.sink()).unwrap(),
            Correction::None
        );
        let now = ANCHOR + FILL_LAG + 10_000_000;
        assert_eq!(
            timeline.fill_silence(now, &mut rec.sink()).unwrap(),
            Correction::Silence(48_000)
        );
        assert_eq!(timeline.expected(), now - FILL_LAG);
        assert_eq!(rec.chunk_pts().len(), 46);
        assert!(rec.0.iter().all(|seen| matches!(
            seen,
            Seen::Chunk {
                first: 0,
                last: 0,
                ..
            }
        )));
    }

    #[test]
    fn audio_after_silence_lands_on_its_timestamp() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        // Silence up to wall clock `now`, then a packet captured 10 ms ago.
        let now = ANCHOR + 20_000_000;
        timeline.fill_silence(now, &mut rec.sink()).unwrap();
        let ts = now - 100_000;
        let correction = timeline
            .push_packet(pcm(&s16_packet(480, 4)), Some(ts), false, &mut rec.sink())
            .unwrap();
        assert_eq!(
            correction,
            Correction::Silence(ticks_to_frames(FILL_LAG - 100_000))
        );
        let due = timeline.expected() - frames_to_ticks(480);
        assert!((due - ts).abs() <= frames_to_ticks(1));
    }

    #[test]
    fn huge_gap_starts_a_new_segment() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline
            .push_packet(
                pcm(&s16_packet(100, 1)),
                Some(ANCHOR),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        let later = ANCHOR + MAX_GAP + 1_000_000;
        let correction = timeline
            .push_packet(
                pcm(&s16_packet(1024, 2)),
                Some(later),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(correction, Correction::Reanchored);
        assert_eq!(
            rec.0,
            [
                Seen::Chunk {
                    pts: ANCHOR,
                    first: 1,
                    last: 0
                },
                Seen::End,
                Seen::Chunk {
                    pts: later,
                    first: 2,
                    last: 2
                },
            ]
        );
    }

    #[test]
    fn huge_backwards_jump_starts_a_new_segment() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let earlier = ANCHOR - MAX_GAP - 1;
        let correction = timeline
            .push_packet(
                pcm(&s16_packet(1024, 2)),
                Some(earlier),
                false,
                &mut rec.sink(),
            )
            .unwrap();
        assert_eq!(correction, Correction::Reanchored);
        assert_eq!(
            rec.0,
            [
                Seen::End,
                Seen::Chunk {
                    pts: earlier,
                    first: 2,
                    last: 2
                }
            ]
        );
    }

    #[test]
    fn fill_after_sleep_starts_a_new_segment() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        let now = ANCHOR + FILL_LAG + MAX_GAP + 1;
        assert_eq!(
            timeline.fill_silence(now, &mut rec.sink()).unwrap(),
            Correction::Reanchored
        );
        assert_eq!(rec.0, [Seen::End]);
        assert_eq!(timeline.expected(), now - FILL_LAG);
        assert_eq!(timeline.frames_written(), 0);
    }

    #[test]
    fn finish_pads_and_ends() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut rec = Recorder::default();
        timeline.finish(&mut rec.sink()).unwrap();
        assert_eq!(rec.0, [Seen::End]);
        timeline
            .push_packet(pcm(&s16_packet(1, 6)), None, false, &mut rec.sink())
            .unwrap();
        timeline.finish(&mut rec.sink()).unwrap();
        assert_eq!(
            rec.0,
            [
                Seen::End,
                Seen::Chunk {
                    pts: ANCHOR,
                    first: 6,
                    last: 0
                },
                Seen::End
            ]
        );
    }

    #[test]
    fn sink_errors_propagate() {
        let mut timeline = Timeline::new(ANCHOR);
        let mut failing = |_: Event<'_>| Err("encoder failed");
        assert_eq!(
            timeline.push_packet(pcm(&s16_packet(1024, 1)), None, false, &mut failing),
            Err("encoder failed")
        );
    }
}
