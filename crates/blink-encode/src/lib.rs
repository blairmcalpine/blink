//! NVENC H.264 encoding for blink.
//!
//! `Encoder` loads `nvEncodeAPI64.dll` from the installed NVIDIA driver at
//! runtime, opens an NVENC session on the D3D11 device that capture uses,
//! registers each captured BGRA texture once, and encodes it on the GPU into an
//! Annex B access unit plus [`PacketMeta`](blink_ring::media::PacketMeta) for
//! the ring. Nothing is read back to the CPU except the compressed bitstream.
//! `Encoder::video_format` gives the SPS/PPS for
//! `ClipRing::set_video_format`.
//!
//! The stream is H.264 High, CBR, P-frames only, preset P1 to P3 with
//! ultra-low-latency tuning, and no lookahead, adaptive quantization, or
//! multipass, so NVENC never runs work on the CUDA cores
//! (`docs/ARCHITECTURE.md`). NVENC's own GOP is infinite; the encoder forces
//! an IDR at least once per second of PTS (and on request), because capture
//! is variable-rate and a frame-count GOP would stretch on an idle screen.
//! `stretch_to` sets the display aspect ratio, and the sample aspect ratio is
//! derived from it for every encode size.
//!
//! The NVENC bindings are hand-written from the Video Codec SDK 12.1 header
//! (`src/ffi/README.md`), so the driver must be 531.61 or newer.
//!
//! Windows only. On other hosts only the portable types
//! ([`EncoderConfig`], [`Preset`], [`NvencStatus`], and the error enums they
//! use) exist, so the workspace still builds and their logic is unit tested.
//! Every `unsafe` block needs a `// SAFETY:` comment and may contain only one
//! unsafe operation.

// The helper modules below are direct children of the crate root, so any
// restricted visibility on their items (`pub(crate)`) trips
// `clippy::redundant_pub_crate`, while plain `pub` trips the denied
// `unreachable_pub`. `expect` documents that conflict. Their crate-internal
// items are only used by the Windows encoder, hence the `dead_code`
// expectations on other hosts.
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used only by the Windows encoder")
)]
mod annexb;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used only by the Windows encoder")
)]
mod config;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod ffi;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used only by the Windows encoder")
)]
mod idr;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used only by the Windows encoder")
)]
mod params;
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used only by the Windows encoder")
)]
mod status;

#[cfg(windows)]
mod encoder;
#[cfg(windows)]
mod error;

pub use annexb::ParameterSetError;
pub use config::{EncoderConfig, EncoderConfigError, Preset};
#[cfg(windows)]
pub use encoder::{EncodedFrame, Encoder, MAX_REGISTERED_TEXTURES};
#[cfg(windows)]
pub use error::EncodeError;
pub use status::NvencStatus;
