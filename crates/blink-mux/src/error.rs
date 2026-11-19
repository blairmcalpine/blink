//! Errors produced while muxing.

use std::io;

use thiserror::Error;

/// Errors produced while muxing.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MuxError {
    /// The box is too large for a compact 32-bit size field.
    #[error("box payload of {payload_len} bytes does not fit a 32-bit box size")]
    BoxTooLarge {
        /// Payload length that was requested, in bytes.
        payload_len: usize,
    },
    /// Writing to the output failed.
    #[error("failed to write the MP4 output")]
    Io(#[from] io::Error),
    /// The clip has no video format.
    #[error("clip has no video format")]
    MissingVideoFormat,
    /// The clip has no video packets.
    #[error("clip has no video packets")]
    NoVideo,
    /// The clip has audio packets but no audio format.
    #[error("clip has audio packets but no audio format")]
    MissingAudioFormat,
    /// The video format's SPS cannot be used to build an `avcC` box.
    #[error("invalid H.264 SPS: {reason}")]
    InvalidSps {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The video format's PPS is empty or not a PPS NAL unit.
    #[error("invalid H.264 PPS")]
    InvalidPps,
    /// A video packet has no start code, or no NAL units besides SPS, PPS, and AUD.
    #[error("video packet {index} is not a usable Annex B access unit")]
    InvalidAccessUnit {
        /// Index of the packet in the clip's video list.
        index: usize,
    },
    /// A value does not fit the field the MP4 format stores it in.
    #[error("{field} is out of range for MP4")]
    ValueOutOfRange {
        /// Name of the field.
        field: &'static str,
    },
}
