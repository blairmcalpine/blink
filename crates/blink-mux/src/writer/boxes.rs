//! Primitives for serializing ISO BMFF boxes into a byte vector.

use crate::{FourCc, MuxError, box_header};

/// Result of writing into a box body.
pub(super) type BoxResult = Result<(), MuxError>;

/// Big-endian field writers.
pub(super) trait Put {
    /// Appends raw bytes.
    fn put(&mut self, bytes: &[u8]);

    /// Appends `count` zero bytes.
    fn zeros(&mut self, count: usize);

    /// Appends a `u16`.
    fn u16(&mut self, value: u16) {
        self.put(&value.to_be_bytes());
    }

    /// Appends a `u32`.
    fn u32(&mut self, value: u32) {
        self.put(&value.to_be_bytes());
    }

    /// Appends a `u64`.
    fn u64(&mut self, value: u64) {
        self.put(&value.to_be_bytes());
    }

    /// Appends a time or duration as `u64` for version 1 boxes, else as `u32`.
    fn versioned(&mut self, wide: bool, value: u64) -> BoxResult {
        if wide {
            self.u64(value);
        } else {
            self.u32(narrow(value, "32-bit time field")?);
        }
        Ok(())
    }
}

impl Put for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }

    fn zeros(&mut self, count: usize) {
        self.resize(self.len() + count, 0);
    }
}

/// Appends a box of type `kind` whose body is written by `body`.
pub(super) fn plain(
    out: &mut Vec<u8>,
    kind: [u8; 4],
    body: impl FnOnce(&mut Vec<u8>) -> BoxResult,
) -> BoxResult {
    let mut inner = Vec::new();
    body(&mut inner)?;
    out.put(&box_header(FourCc(kind), inner.len())?);
    out.put(&inner);
    Ok(())
}

/// Appends a full box (version and 24-bit flags) of type `kind`.
pub(super) fn full(
    out: &mut Vec<u8>,
    kind: [u8; 4],
    version: u8,
    flags: u32,
    body: impl FnOnce(&mut Vec<u8>) -> BoxResult,
) -> BoxResult {
    plain(out, kind, |inner| {
        let [_, f1, f2, f3] = flags.to_be_bytes();
        inner.put(&[version, f1, f2, f3]);
        body(inner)
    })
}

/// Converts to `u32`, naming `field` on overflow.
pub(super) fn narrow<T: TryInto<u32>>(value: T, field: &'static str) -> Result<u32, MuxError> {
    value
        .try_into()
        .map_err(|_| MuxError::ValueOutOfRange { field })
}

/// Converts a length or count to `u64`.
pub(super) fn widen(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nests_boxes_with_sizes() {
        let mut out = Vec::new();
        plain(&mut out, *b"moov", |moov| {
            full(moov, *b"mvhd", 1, 0x0a_0b0c, |mvhd| {
                mvhd.u16(0x1234);
                Ok(())
            })
        })
        .expect("small");
        assert_eq!(
            out,
            [
                0, 0, 0, 22, b'm', b'o', b'o', b'v', 0, 0, 0, 14, b'm', b'v', b'h', b'd', 1, 0x0a,
                0x0b, 0x0c, 0x12, 0x34
            ]
        );
    }

    #[test]
    fn versioned_fields_check_range() {
        let mut out = Vec::new();
        assert!(out.versioned(false, u64::from(u32::MAX) + 1).is_err());
        out.versioned(true, 1).expect("wide");
        out.versioned(false, 2).expect("narrow");
        assert_eq!(out, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2]);
    }
}
