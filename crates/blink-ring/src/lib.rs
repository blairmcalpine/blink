//! Encoded-packet ring buffer for blink.
//!
//! The recorder keeps the last N seconds of encoded H.264 and AAC in memory.
//! [`ClipRing`] holds those packets in one preallocated circular byte arena,
//! evicts whole GOPs as time passes, and copies a GOP-aligned clip into a
//! reusable [`ClipBuf`] when the user saves. [`media`] holds the media types shared by every crate in the
//! pipeline.
//!
//! The crate is platform-agnostic, contains no `unsafe`, and its tests run
//! under Miri.

#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing)]

pub mod media;
mod ring;

pub use ring::{ClipBuf, ClipCursor, ClipError, ClipRing, CopyProgress, PushError, PushOutcome};
