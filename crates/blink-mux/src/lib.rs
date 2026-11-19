//! MP4 muxing for blink clips.
//!
//! [`write_mp4`] turns a [`ClipBuf`](blink_ring::ClipBuf) cut from the ring into a
//! faststart MP4 (ISO BMFF) file without re-encoding: an `avc1` video track
//! and, when present, an `mp4a` audio track. The writer is hand-written,
//! platform-agnostic, and contains no `unsafe`.

#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing)]

mod error;
mod writer;

pub use error::MuxError;
pub use writer::write_mp4;
