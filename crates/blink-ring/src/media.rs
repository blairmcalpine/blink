//! Media types shared by the capture, encode, audio, ring, and mux crates.

use std::time::Duration;

/// Nanoseconds in one [`Timestamp`] tick.
const NANOS_PER_TICK: u64 = 100;

/// A point or span on blink's single media clock, in 100 ns ticks.
///
/// Both video and audio timestamps come from the Windows Query Performance
/// Counter, normalized to 10 MHz, so one tick is 100 ns and
/// [`Timestamp::TICKS_PER_SECOND`] ticks make a second. The same type is used
/// for durations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// Ticks in one second.
    pub const TICKS_PER_SECOND: i64 = 10_000_000;

    /// The zero timestamp.
    pub const ZERO: Self = Self(0);

    /// Creates a timestamp from a raw tick count.
    #[must_use]
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    /// Converts a span to a `Duration`. Negative spans are zero.
    #[must_use]
    pub const fn to_duration(self) -> Duration {
        if self.0 <= 0 {
            return Duration::ZERO;
        }
        let ticks = self.0.unsigned_abs();
        let per_second = Self::TICKS_PER_SECOND.unsigned_abs();
        // The remainder is below 10^7 ticks, so its nanoseconds fit easily.
        Duration::from_secs(ticks / per_second)
            .saturating_add(Duration::from_nanos((ticks % per_second) * NANOS_PER_TICK))
    }

    /// Creates a timestamp from a whole number of seconds, saturating on overflow.
    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(Self::TICKS_PER_SECOND))
    }

    /// Creates a timestamp from milliseconds, saturating on overflow.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis.saturating_mul(Self::TICKS_PER_SECOND / 1000))
    }

    /// Returns the raw tick count.
    #[must_use]
    pub const fn ticks(self) -> i64 {
        self.0
    }

    /// Adds two timestamps, saturating at the numeric bounds.
    #[must_use]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    /// Subtracts `rhs`, saturating at the numeric bounds.
    #[must_use]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

/// Which elementary stream a packet belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// H.264 video.
    Video,
    /// AAC audio.
    Audio,
}

/// Format of the H.264 video stream in the current segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VideoFormat {
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Nominal capture frame rate in frames per second.
    pub fps: u32,
    /// Sequence parameter set as a raw NAL unit (header byte included), without a start code.
    pub sps: Vec<u8>,
    /// Picture parameter set as a raw NAL unit (header byte included), without a start code.
    pub pps: Vec<u8>,
    /// Sample (pixel) aspect ratio as `(horizontal, vertical)` spacing, for `stretch_to`.
    pub sample_aspect: Option<(u32, u32)>,
}

/// Format of the AAC audio stream in the current segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// MPEG-4 `AudioSpecificConfig` bytes, as written into the MP4 `esds` box.
    pub audio_specific_config: Vec<u8>,
}

/// Metadata stored alongside every encoded packet.
///
/// Video payloads are Annex B H.264 access units exactly as NVENC emits them.
/// Audio payloads are raw AAC frames without an ADTS header. The encoder never
/// emits B-frames, so the presentation timestamp is also the decode timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketMeta {
    /// Stream the packet belongs to.
    pub kind: StreamKind,
    /// Presentation timestamp.
    pub pts: Timestamp,
    /// Nominal duration of the packet.
    pub duration: Timestamp,
    /// Whether the packet is an IDR access unit. Ignored for audio.
    pub keyframe: bool,
}
