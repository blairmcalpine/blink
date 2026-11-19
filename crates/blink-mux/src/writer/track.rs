//! Converts a clip into tracks of timed samples grouped into interleaved chunks.

use blink_ring::ClipBuf;
use blink_ring::media::{AudioFormat, PacketMeta, Timestamp, VideoFormat};
use tracing::warn;

use super::annexb;
use super::boxes::narrow;
use crate::MuxError;

/// Timescale of the movie header and edit lists.
pub(super) const MOVIE_TIMESCALE: u32 = 90_000;

/// Video media timescale. 90 kHz is exact for 24, 25, 30, 50, 60, 120, 144,
/// and 240 fps, and the conventional H.264 clock.
pub(super) const VIDEO_TIMESCALE: u32 = 90_000;

/// Samples per AAC-LC frame, the fallback audio frame duration.
const AAC_FRAME_SAMPLES: u64 = 1024;

/// Target span of one chunk; chunks of both tracks interleave in the `mdat`.
const CHUNK_TICKS: i64 = Timestamp::TICKS_PER_SECOND / 2;

/// What a track carries.
#[derive(Debug, Clone, Copy)]
pub(super) enum Media<'a> {
    /// H.264 video; samples are Annex B and converted while writing.
    Video(&'a VideoFormat),
    /// AAC audio; samples are written as is.
    Audio(&'a AudioFormat),
}

/// One MP4 sample.
#[derive(Debug)]
pub(super) struct Sample<'a> {
    /// Source payload, borrowed from the clip.
    pub(super) data: &'a [u8],
    /// Size of the sample as written to the `mdat`.
    pub(super) size: u32,
    /// Duration in the track timescale.
    pub(super) duration: u32,
    /// Whether the sample is a sync sample.
    pub(super) keyframe: bool,
}

/// A run of consecutive samples stored contiguously in the `mdat`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Chunk {
    /// Index of the first sample.
    pub(super) first_sample: usize,
    /// Number of samples.
    pub(super) count: usize,
    /// Start time in ticks relative to the clip start, for interleaving.
    start: i64,
}

/// One `trak`.
#[derive(Debug)]
pub(super) struct Track<'a> {
    /// `track_ID`, starting at 1.
    pub(super) id: u32,
    /// Format of the samples.
    pub(super) media: Media<'a>,
    /// Media timescale in units per second.
    pub(super) timescale: u32,
    /// Samples in decode order.
    pub(super) samples: Vec<Sample<'a>>,
    /// Chunks in decode order.
    pub(super) chunks: Vec<Chunk>,
    /// Sum of sample durations, in the media timescale.
    pub(super) media_duration: u64,
    /// Empty edit before the first sample, in the movie timescale.
    pub(super) start_delay: u64,
    /// Media skipped at the start (AAC preroll), in the media timescale.
    pub(super) media_time: u64,
    /// Presented duration of the media, in the movie timescale.
    pub(super) edit_duration: u64,
}

impl Track<'_> {
    /// Samples of `chunk`.
    pub(super) fn chunk_samples(&self, chunk: Chunk) -> impl Iterator<Item = &Sample<'_>> {
        self.samples
            .iter()
            .skip(chunk.first_sample)
            .take(chunk.count)
    }

    fn chunk_len(&self, chunk: Chunk) -> u64 {
        self.chunk_samples(chunk)
            .map(|sample| u64::from(sample.size))
            .sum()
    }
}

/// All tracks of a clip and the order their chunks take in the `mdat`.
#[derive(Debug)]
pub(super) struct Movie<'a> {
    /// Video first, then audio if present.
    pub(super) tracks: Vec<Track<'a>>,
    /// Movie duration in the movie timescale.
    pub(super) duration: u64,
    /// `(track index, chunk index)` in `mdat` order.
    order: Vec<(usize, usize)>,
}

impl<'a> Movie<'a> {
    /// Plans the tracks of `clip`, presented from `clip.start()`.
    pub(super) fn plan(clip: &'a ClipBuf) -> Result<Self, MuxError> {
        let video_format = clip.video_format().ok_or(MuxError::MissingVideoFormat)?;
        let video: Vec<(PacketMeta, &[u8])> = clip.video().collect();
        if video.is_empty() {
            return Err(MuxError::NoVideo);
        }
        let audio: Vec<(PacketMeta, &[u8])> = clip.audio().collect();
        let mut tracks = vec![video_track(video_format, &video, clip.start(), clip.end())?];
        if !audio.is_empty() {
            let format = clip.audio_format().ok_or(MuxError::MissingAudioFormat)?;
            tracks.push(audio_track(format, &audio, clip.start())?);
        }
        let duration = tracks
            .iter()
            .map(|track| track.start_delay.saturating_add(track.edit_duration))
            .max()
            .unwrap_or(0);
        let order = interleave(&tracks);
        Ok(Self {
            tracks,
            duration,
            order,
        })
    }

    /// Chunks in `mdat` order, with their track.
    pub(super) fn ordered_chunks(&self) -> impl Iterator<Item = (usize, &Track<'a>, Chunk)> {
        self.order.iter().filter_map(|&(track_index, chunk_index)| {
            let track = self.tracks.get(track_index)?;
            Some((track_index, track, *track.chunks.get(chunk_index)?))
        })
    }

    /// Total sample bytes in the `mdat`.
    pub(super) fn data_len(&self) -> u64 {
        self.tracks
            .iter()
            .flat_map(|track| &track.samples)
            .map(|sample| u64::from(sample.size))
            .sum()
    }

    /// Absolute file offset of every chunk, per track, when sample data starts at `data_start`.
    pub(super) fn chunk_offsets(&self, data_start: u64) -> Vec<Vec<u64>> {
        let mut offsets: Vec<Vec<u64>> = self
            .tracks
            .iter()
            .map(|track| Vec::with_capacity(track.chunks.len()))
            .collect();
        let mut pos = data_start;
        for (track_index, track, chunk) in self.ordered_chunks() {
            if let Some(list) = offsets.get_mut(track_index) {
                list.push(pos);
            }
            pos = pos.saturating_add(track.chunk_len(chunk));
        }
        offsets
    }
}

type Packet<'a> = (PacketMeta, &'a [u8]);

fn video_track<'a>(
    format: &'a VideoFormat,
    packets: &[Packet<'a>],
    start: Timestamp,
    end: Timestamp,
) -> Result<Track<'a>, MuxError> {
    let sizes = packets
        .iter()
        .enumerate()
        .map(|(index, (_, access_unit))| {
            annexb::length_prefixed_len(access_unit).ok_or(MuxError::InvalidAccessUnit { index })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let fallback = u64::from(VIDEO_TIMESCALE) / u64::from(format.fps.max(1));
    let durations = video_durations(packets, VIDEO_TIMESCALE, end, fallback)?;
    let media = Media::Video(format);
    build_track(1, media, VIDEO_TIMESCALE, packets, &sizes, durations, start)
}

fn audio_track<'a>(
    format: &'a AudioFormat,
    packets: &[Packet<'a>],
    start: Timestamp,
) -> Result<Track<'a>, MuxError> {
    if format.sample_rate == 0 {
        return Err(MuxError::ValueOutOfRange {
            field: "audio sample rate",
        });
    }
    let sizes: Vec<usize> = packets.iter().map(|(_, frame)| frame.len()).collect();
    let durations = audio_durations(packets, format.sample_rate)?;
    let media = Media::Audio(format);
    build_track(
        2,
        media,
        format.sample_rate,
        packets,
        &sizes,
        durations,
        start,
    )
}

fn build_track<'a>(
    id: u32,
    media: Media<'a>,
    timescale: u32,
    packets: &[Packet<'a>],
    sizes: &[usize],
    durations: Vec<u32>,
    start: Timestamp,
) -> Result<Track<'a>, MuxError> {
    let samples = packets
        .iter()
        .zip(sizes)
        .zip(durations)
        .map(|(((meta, data), &size), duration)| {
            Ok(Sample {
                data,
                size: narrow(size, "sample size")?,
                duration,
                keyframe: meta.keyframe || matches!(media, Media::Audio(_)),
            })
        })
        .collect::<Result<Vec<_>, MuxError>>()?;
    let first_pts = packets.first().map_or(start, |(meta, _)| meta.pts);
    let media_duration: u64 = samples
        .iter()
        .map(|sample| u64::from(sample.duration))
        .sum();
    let (start_delay, media_time) = if first_pts >= start {
        (
            ticks_to_units(first_pts.saturating_sub(start), MOVIE_TIMESCALE),
            0,
        )
    } else {
        let skipped = ticks_to_units(start.saturating_sub(first_pts), timescale);
        (0, skipped.min(media_duration))
    };
    Ok(Track {
        id,
        media,
        timescale,
        chunks: chunks(packets, start),
        start_delay,
        media_time,
        edit_duration: rescale(media_duration - media_time, timescale, MOVIE_TIMESCALE),
        media_duration,
        samples,
    })
}

/// Video sample durations in `timescale` units.
///
/// Each frame lasts until the next one starts, which preserves variable frame
/// rate (frames are skipped while the screen is static). Start times are
/// converted from absolute PTS rather than summed, so rounding never
/// accumulates. The last frame lasts until the clip end, or for its own
/// `PacketMeta::duration` (else `fallback`) if the end is not after it.
/// Non-increasing timestamps get the minimum duration of one unit.
fn video_durations(
    packets: &[Packet<'_>],
    timescale: u32,
    end: Timestamp,
    fallback: u64,
) -> Result<Vec<u32>, MuxError> {
    let first = packets
        .first()
        .map_or(Timestamp::ZERO, |(meta, _)| meta.pts);
    let starts: Vec<u64> = packets
        .iter()
        .map(|(meta, _)| ticks_to_units(meta.pts.saturating_sub(first), timescale))
        .collect();
    let mut durations: Vec<u64> = starts
        .windows(2)
        .map(|pair| match pair {
            [start, next] => next.saturating_sub(*start).max(1),
            _ => 1,
        })
        .collect();
    if let (Some((last, _)), Some(&last_start)) = (packets.last(), starts.last()) {
        let duration = if end > last.pts {
            ticks_to_units(end.saturating_sub(first), timescale).saturating_sub(last_start)
        } else {
            Some(ticks_to_units(last.duration, timescale))
                .filter(|&own| own > 0)
                .unwrap_or(fallback)
        };
        durations.push(duration.max(1));
    }
    to_u32(durations)
}

/// Audio sample durations in `timescale` (sample rate) units.
///
/// Every AAC frame lasts its nominal duration (1024 samples), so a gap-free
/// stream gets a single `stts` entry. When a frame's PTS is off the running
/// timeline by more than half a frame (a real gap or overlap), the previous
/// frame's duration absorbs the difference to keep audio in sync, and the
/// deviation is logged.
fn audio_durations(packets: &[Packet<'_>], timescale: u32) -> Result<Vec<u32>, MuxError> {
    let first = packets
        .first()
        .map_or(Timestamp::ZERO, |(meta, _)| meta.pts);
    let nominal = packets
        .first()
        .map(|(meta, _)| ticks_to_units(meta.duration, timescale))
        .filter(|&nominal| nominal > 0)
        .unwrap_or(AAC_FRAME_SAMPLES);
    let mut durations = Vec::with_capacity(packets.len());
    let mut timeline = 0_u64;
    let mut adjusted = 0_usize;
    for (_, (next, _)) in packets.iter().zip(packets.iter().skip(1)) {
        let actual = ticks_to_units(next.pts.saturating_sub(first), timescale);
        let duration = if actual.abs_diff(timeline + nominal) > nominal / 2 {
            adjusted += 1;
            actual.saturating_sub(timeline).max(1)
        } else {
            nominal
        };
        timeline += duration;
        durations.push(duration);
    }
    if !packets.is_empty() {
        durations.push(nominal);
    }
    if adjusted > 0 {
        warn!(
            adjusted,
            frames = packets.len(),
            "audio timestamps deviate from the AAC frame rate; adjusted sample durations"
        );
    }
    to_u32(durations)
}

fn to_u32(durations: Vec<u64>) -> Result<Vec<u32>, MuxError> {
    durations
        .into_iter()
        .map(|duration| narrow(duration, "sample duration"))
        .collect()
}

/// Groups consecutive packets into chunks spanning about [`CHUNK_TICKS`].
fn chunks(packets: &[Packet<'_>], base: Timestamp) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    for (index, (meta, _)) in packets.iter().enumerate() {
        let start = meta.pts.saturating_sub(base).ticks();
        match chunks.last_mut() {
            Some(chunk) if start.saturating_sub(chunk.start) < CHUNK_TICKS => chunk.count += 1,
            _ => chunks.push(Chunk {
                first_sample: index,
                count: 1,
                start,
            }),
        }
    }
    chunks
}

/// Orders every chunk by start time; video first on ties.
fn interleave(tracks: &[Track<'_>]) -> Vec<(usize, usize)> {
    let mut order: Vec<(i64, usize, usize)> = tracks
        .iter()
        .enumerate()
        .flat_map(|(track_index, track)| {
            track
                .chunks
                .iter()
                .enumerate()
                .map(move |(chunk_index, chunk)| (chunk.start, track_index, chunk_index))
        })
        .collect();
    order.sort_unstable();
    order
        .into_iter()
        .map(|(_, track_index, chunk_index)| (track_index, chunk_index))
        .collect()
}

/// Converts clock ticks to `timescale` units, rounding to nearest. Negative
/// spans become zero.
pub(super) fn ticks_to_units(span: Timestamp, timescale: u32) -> u64 {
    let ticks = u64::try_from(span.ticks()).unwrap_or(0);
    rescale_u128(
        ticks,
        u128::from(Timestamp::TICKS_PER_SECOND.unsigned_abs()),
        timescale,
    )
}

/// Converts `value` from timescale `from` to timescale `to`, rounding to nearest.
pub(super) fn rescale(value: u64, from: u32, to: u32) -> u64 {
    rescale_u128(value, u128::from(from.max(1)), to)
}

fn rescale_u128(value: u64, from: u128, to: u32) -> u64 {
    let scaled = (u128::from(value) * u128::from(to) + from / 2) / from;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// Total number of samples as a `u32` box field.
pub(super) fn sample_count(track: &Track<'_>) -> Result<u32, MuxError> {
    narrow(track.samples.len(), "sample count")
}

#[cfg(test)]
mod tests {
    use blink_ring::media::StreamKind;

    use super::*;

    const PAYLOAD: &[u8] = &[0, 0, 1, 0x65, 1];

    fn packet(kind: StreamKind, pts: i64, duration: i64) -> Packet<'static> {
        let meta = PacketMeta {
            kind,
            pts: Timestamp::from_ticks(pts),
            duration: Timestamp::from_ticks(duration),
            keyframe: true,
        };
        (meta, PAYLOAD)
    }

    fn videos(pts: &[i64]) -> Vec<Packet<'static>> {
        pts.iter()
            .map(|&pts| packet(StreamKind::Video, pts, 333_333))
            .collect()
    }

    #[test]
    fn converts_ticks_without_drift() {
        assert_eq!(
            ticks_to_units(Timestamp::from_ticks(333_333), VIDEO_TIMESCALE),
            3000
        );
        assert_eq!(
            ticks_to_units(Timestamp::from_ticks(-5), VIDEO_TIMESCALE),
            0
        );
        assert_eq!(
            ticks_to_units(Timestamp::from_ticks(i64::MAX), u32::MAX),
            u64::MAX
        );
        assert_eq!(rescale(48_000, 48_000, MOVIE_TIMESCALE), 90_000);
        assert_eq!(rescale(5, 0, 10), 50);
    }

    #[test]
    fn video_durations_follow_timestamps_and_clip_end() {
        // 30 fps with QPC-rounded timestamps, then a 100 ms stall.
        let packets = videos(&[0, 333_333, 666_667, 1_000_000, 2_000_000]);
        let end = Timestamp::from_ticks(2_000_000);
        let durations = video_durations(&packets, VIDEO_TIMESCALE, end, 3000).expect("fits");
        assert_eq!(
            durations,
            [3000, 3000, 3000, 9000, 3000],
            "end not after last: own duration"
        );
        // A static screen after the last frame: it lasts until the save time.
        let end = Timestamp::from_ticks(32_000_000);
        let durations = video_durations(&packets, VIDEO_TIMESCALE, end, 3000).expect("fits");
        assert_eq!(durations.last(), Some(&(3 * 90_000)));
        let single = videos(&[5]);
        let durations = video_durations(&single, VIDEO_TIMESCALE, Timestamp::from_ticks(0), 1500);
        assert_eq!(durations.expect("fits"), [3000]);
    }

    #[test]
    fn video_durations_handle_bad_timestamps() {
        let backwards = vec![
            packet(StreamKind::Video, 10, 0),
            packet(StreamKind::Video, 5, 0),
        ];
        let durations =
            video_durations(&backwards, 90_000, Timestamp::from_ticks(0), 7).expect("fits");
        assert_eq!(durations, [1, 7]);
        assert_eq!(
            video_durations(&[], 90_000, Timestamp::from_ticks(0), 0).expect("fits"),
            Vec::<u32>::new()
        );
        let huge = vec![packet(StreamKind::Video, 0, 0)];
        assert!(matches!(
            video_durations(
                &huge,
                VIDEO_TIMESCALE,
                Timestamp::from_ticks(i64::MAX),
                3000
            ),
            Err(MuxError::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn audio_durations_are_constant_and_absorb_gaps() {
        // QPC-rounded 1024-sample frames, then a 100 ms gap.
        let pts = [0, 213_333, 426_667, 640_000, 1_853_333];
        let packets: Vec<Packet<'_>> = pts
            .iter()
            .map(|&pts| packet(StreamKind::Audio, pts, 213_333))
            .collect();
        let durations = audio_durations(&packets, 48_000).expect("fits");
        assert_eq!(durations, [1024, 1024, 1024, 5824, 1024]);
        let jitter: Vec<Packet<'_>> = [0, 213_000, 427_000]
            .iter()
            .map(|&pts| packet(StreamKind::Audio, pts, 0))
            .collect();
        assert_eq!(audio_durations(&jitter, 48_000).expect("fits"), [1024; 3]);
        assert_eq!(
            audio_durations(&[], 48_000).expect("fits"),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn groups_half_second_chunks() {
        let packets: Vec<Packet<'_>> = (0..25)
            .map(|index| packet(StreamKind::Video, index * 1_000_000, 0))
            .collect();
        let chunks = chunks(&packets, Timestamp::ZERO);
        let counts: Vec<usize> = chunks.iter().map(|chunk| chunk.count).collect();
        assert_eq!(counts, [5, 5, 5, 5, 5]);
        assert_eq!(chunks.get(1).map(|chunk| chunk.first_sample), Some(5));
    }
}
