//! WASAPI loopback and microphone audio capture for blink.
//!
//! [`AudioCapture`] owns one audio thread. It captures what the default render
//! endpoint plays through WASAPI loopback and, unless turned off, the default
//! recording endpoint (the microphone), keeps a sample clock for each anchored
//! to the shared QPC timeline (100 ns ticks), fills silent gaps with digital
//! silence so the stream never stalls, mixes the microphone into the desktop
//! audio, and encodes one AAC-LC track (48 kHz, stereo, raw frames without
//! ADTS) with the Media Foundation encoder that ships with Windows.
//! On Windows, `AudioCapture::mic_state` reports the [`MicState`]. Every encoded frame reaches an [`AudioSink`] together with its
//! [`PacketMeta`](blink_ring::media::PacketMeta); the stream's
//! [`AudioFormat`](blink_ring::media::AudioFormat) is available from
//! [`AudioCapture::audio_format`] as soon as [`AudioCapture::start`] returns.
//!
//! Windows only. The Windows API is gated behind `#[cfg(windows)]`; on other
//! hosts [`AudioCapture::start`] returns `AudioError::Unsupported`, and the
//! portable timeline, mixer, PCM, and `AudioSpecificConfig` helpers are compiled only
//! for their tests. The QPC clock and thread priority come from `blink-sys`.
//! Every `unsafe` block needs a `// SAFETY:` comment and may contain only one
//! unsafe operation.

use blink_ring::media::Timestamp;

#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod asc;
mod config;
mod error;
mod mic;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod mix;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod pcm;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod pts_queue;
mod sink;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
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
pub use mic::MicState;
pub use sink::AudioSink;
#[cfg(not(windows))]
pub use stub::AudioCapture;

/// Sample rate of the captured PCM and of the AAC stream, in hertz.
pub const SAMPLE_RATE: u32 = 48_000;

/// Channel count of the captured PCM and of the AAC stream (stereo).
pub const CHANNELS: u16 = 2;

/// PCM frames (one sample per channel) in one AAC-LC access unit.
pub const FRAMES_PER_PACKET: u32 = blink_ring::media::AAC_FRAME_SAMPLES;

/// Nominal duration of one AAC packet, in whole 100 ns ticks (21.3333 ms).
///
/// 1024 frames at 48 kHz, truncated. Packet timestamps are computed from the
/// exact sample count, so the truncation never accumulates.
pub const PACKET_DURATION: Timestamp = Timestamp::from_ticks(213_333);
