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

/// Size in bytes of a compact (32-bit size) MP4 box header.
pub const BOX_HEADER_LEN: usize = 8;

/// A four-character code identifying an MP4 box type, such as `moov`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(pub [u8; 4]);

/// Encodes a compact MP4 box header for a payload of `payload_len` bytes.
///
/// The header is the big-endian total box size (header included) followed by
/// the box type.
///
/// # Errors
///
/// Returns [`MuxError::BoxTooLarge`] if the total size does not fit in a `u32`.
///
/// ```
/// use blink_mux::{FourCc, box_header};
///
/// assert_eq!(
///     box_header(FourCc(*b"free"), 0).ok(),
///     Some([0, 0, 0, 8, b'f', b'r', b'e', b'e']),
/// );
/// ```
pub fn box_header(kind: FourCc, payload_len: usize) -> Result<[u8; BOX_HEADER_LEN], MuxError> {
    let total = payload_len
        .checked_add(BOX_HEADER_LEN)
        .and_then(|total| u32::try_from(total).ok())
        .ok_or(MuxError::BoxTooLarge { payload_len })?;
    let [s0, s1, s2, s3] = total.to_be_bytes();
    let [t0, t1, t2, t3] = kind.0;
    Ok([s0, s1, s2, s3, t0, t1, t2, t3])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_size_and_type() {
        let header = box_header(FourCc(*b"mdat"), 0x0102_0300).expect("fits in u32");
        assert_eq!(header, [0x01, 0x02, 0x03, 0x08, b'm', b'd', b'a', b't']);
    }

    #[test]
    fn accepts_largest_compact_box() {
        let payload_len =
            usize::try_from(u32::MAX).expect("usize is at least 32 bits") - BOX_HEADER_LEN;
        let header = box_header(FourCc(*b"mdat"), payload_len).expect("fits in u32");
        assert_eq!(header[..4], [0xff; 4]);
    }

    #[test]
    fn rejects_oversized_box() {
        let payload_len = usize::try_from(u32::MAX).expect("usize is at least 32 bits");
        assert!(matches!(
            box_header(FourCc(*b"mdat"), payload_len),
            Err(MuxError::BoxTooLarge { payload_len: len }) if len == payload_len,
        ));
    }
}
