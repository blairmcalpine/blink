//! Annex B to length-prefixed (AVCC) conversion.

use std::io::{self, Write};

/// The three-byte start code. A four-byte start code is a zero byte followed
/// by this one, and that zero is trimmed as trailing padding of the previous
/// NAL unit.
const START_CODE: [u8; 3] = [0, 0, 1];

/// NAL unit types that `avcC` carries or that MP4 samples must not contain.
const SPS: u8 = 7;
const PPS: u8 = 8;
const AUD: u8 = 9;

/// Iterator over the NAL units of an Annex B byte stream, without start codes.
pub(super) struct NalUnits<'a> {
    rest: &'a [u8],
}

/// Splits `data` at 3- and 4-byte start codes. Bytes before the first start
/// code are ignored, as are empty NAL units and trailing zero bytes.
pub(super) fn nal_units(data: &[u8]) -> NalUnits<'_> {
    let rest = find_start_code(data)
        .and_then(|pos| data.get(pos + START_CODE.len()..))
        .unwrap_or_default();
    NalUnits { rest }
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        while !self.rest.is_empty() {
            let (nal, rest) = match find_start_code(self.rest) {
                Some(pos) => (
                    self.rest.get(..pos).unwrap_or_default(),
                    self.rest.get(pos + START_CODE.len()..).unwrap_or_default(),
                ),
                None => (self.rest, &[][..]),
            };
            self.rest = rest;
            let nal = trim_trailing_zeros(nal);
            if !nal.is_empty() {
                return Some(nal);
            }
        }
        None
    }
}

/// Returns the `nal_unit_type` of a NAL unit.
pub(super) fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|header| header & 0x1f)
}

/// NAL units of an access unit that belong in an MP4 sample: everything but
/// SPS, PPS, and AUD.
fn sample_nal_units(access_unit: &[u8]) -> impl Iterator<Item = &[u8]> {
    nal_units(access_unit).filter(|nal| !matches!(nal_type(nal), Some(SPS | PPS | AUD)))
}

/// Size of `access_unit` as 4-byte length-prefixed NAL units without SPS,
/// PPS, and AUD.
///
/// Returns `None` if no NAL unit is left, or if one exceeds `u32::MAX` bytes.
pub(super) fn length_prefixed_len(access_unit: &[u8]) -> Option<usize> {
    let mut total = 0_usize;
    for nal in sample_nal_units(access_unit) {
        u32::try_from(nal.len()).ok()?;
        total = total.checked_add(4)?.checked_add(nal.len())?;
    }
    (total > 0).then_some(total)
}

/// Writes `access_unit` as length-prefixed NAL units, borrowing each NAL from
/// the source. Call [`length_prefixed_len`] first to validate it.
pub(super) fn write_length_prefixed(out: &mut impl Write, access_unit: &[u8]) -> io::Result<()> {
    for nal in sample_nal_units(access_unit) {
        let len = u32::try_from(nal.len()).map_err(io::Error::other)?;
        out.write_all(&len.to_be_bytes())?;
        out.write_all(nal)?;
    }
    Ok(())
}

fn find_start_code(data: &[u8]) -> Option<usize> {
    data.windows(START_CODE.len())
        .position(|window| window == START_CODE)
}

fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let end = nal
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |last| last + 1);
    nal.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(access_unit: &[u8]) -> Option<Vec<u8>> {
        let len = length_prefixed_len(access_unit)?;
        let mut out = Vec::new();
        write_length_prefixed(&mut out, access_unit).expect("vec write");
        assert_eq!(out.len(), len);
        Some(out)
    }

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x09, 0xf0, // AUD, 4-byte start code
            0, 0, 1, 0x67, 0x42, // SPS, 3-byte
            0, 0, 0, 1, 0x65, 0x88, 0x00, // IDR slice with trailing zero
            0, 0, 1, 0x41, 0x9a, // non-IDR slice
        ];
        let nals: Vec<&[u8]> = nal_units(&data).collect();
        assert_eq!(
            nals,
            [
                &[0x09, 0xf0][..],
                &[0x67, 0x42],
                &[0x65, 0x88],
                &[0x41, 0x9a]
            ]
        );
    }

    #[test]
    fn ignores_leading_garbage_and_empty_units() {
        let data = [0xff, 0, 0, 1, 0, 0, 1, 0x65, 1, 0, 0, 1];
        let nals: Vec<&[u8]> = nal_units(&data).collect();
        assert_eq!(nals, [&[0x65, 1][..]]);
        assert_eq!(nal_units(&[0x65, 1, 2]).count(), 0);
        assert_eq!(nal_units(&[]).count(), 0);
    }

    #[test]
    fn drops_parameter_sets_and_delimiters() {
        let data = [
            0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x06,
            0x05, 0, 0, 1, 0x65, 0x88, 0x84,
        ];
        assert_eq!(
            convert(&data),
            Some(vec![0, 0, 0, 2, 0x06, 0x05, 0, 0, 0, 3, 0x65, 0x88, 0x84])
        );
    }

    #[test]
    fn rejects_access_unit_without_slices() {
        assert_eq!(convert(&[0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x67]), None);
        assert_eq!(convert(&[0x65, 0x88]), None);
    }

    #[test]
    fn keeps_emulation_prevention_bytes() {
        let data = [0, 0, 1, 0x65, 0, 0, 3, 0, 1];
        assert_eq!(convert(&data), Some(vec![0, 0, 0, 6, 0x65, 0, 0, 3, 0, 1]));
    }
}
