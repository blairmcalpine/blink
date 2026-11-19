//! WASAPI loopback audio capture for blink.
//!
//! [`AudioCapture`] owns one audio thread. It captures what the default render
//! endpoint plays through WASAPI loopback, keeps a sample clock anchored to the
//! shared QPC timeline (100 ns ticks), fills silent gaps with digital silence so
//! the stream never stalls, and encodes AAC-LC (48 kHz, stereo, raw frames
//! without ADTS) with the Media Foundation encoder that ships with Windows.
//! Every encoded frame reaches an [`AudioSink`] together with its
//! [`PacketMeta`](blink_ring::media::PacketMeta); the stream's
//! [`AudioFormat`](blink_ring::media::AudioFormat) is available from
//! [`AudioCapture::audio_format`] as soon as [`AudioCapture::start`] returns.
//!
//! Windows only. The Windows API is gated behind `#[cfg(windows)]`; on other
//! hosts [`AudioCapture::start`] returns [`AudioError::Unsupported`], and the
//! portable timeline, PCM, and `AudioSpecificConfig` helpers are compiled only
//! for their tests. The QPC clock and thread priority come from `blink-sys`. Every `unsafe` block needs a `// SAFETY:` comment and may
//! contain only one unsafe operation.

use blink_ring::media::Timestamp;

#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` here would trip the denied `unreachable_pub`"
)]
mod asc;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` here would trip the denied `unreachable_pub`"
)]
mod backoff;
mod config;
mod error;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` here would trip the denied `unreachable_pub`"
)]
mod pcm;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` here would trip the denied `unreachable_pub`"
)]
mod pts_queue;
mod sink;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` here would trip the denied `unreachable_pub`"
)]
mod timeline;

#[cfg(windows)]
mod imp;
#[cfg(not(windows))]
mod stub;

pub use config::{AacBitrate, AudioConfig, UnsupportedBitrate};
pub use error::AudioError;
#[cfg(windows)]
pub use imp::AudioCapture;
pub use sink::AudioSink;
#[cfg(not(windows))]
pub use stub::AudioCapture;

/// Sample rate of the captured PCM and of the AAC stream, in hertz.
pub const SAMPLE_RATE: u32 = 48_000;

/// Channel count of the captured PCM and of the AAC stream (stereo).
pub const CHANNELS: u16 = 2;

/// PCM frames (one sample per channel) in one AAC-LC access unit.
pub const FRAMES_PER_PACKET: u32 = 1024;

/// Nominal duration of one AAC packet, in whole 100 ns ticks (21.3333 ms).
///
/// 1024 frames at 48 kHz, truncated. Packet timestamps are computed from the
/// exact sample count, so the truncation never accumulates.
pub const PACKET_DURATION: Timestamp = Timestamp(213_333);
