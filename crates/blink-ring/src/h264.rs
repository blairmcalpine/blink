//! Annex B H.264 byte-stream splitting, shared by the encoder (which pulls
//! SPS and PPS out of NVENC's sequence header) and the muxer (which rewrites
//! access units as length-prefixed samples).

/// NAL unit type of a sequence parameter set.
pub const NAL_SPS: u8 = 7;
/// NAL unit type of a picture parameter set.
pub const NAL_PPS: u8 = 8;
/// NAL unit type of an access unit delimiter.
pub const NAL_AUD: u8 = 9;

/// The three-byte start code. A four-byte start code is a zero byte followed
/// by this one, and that zero is trimmed as trailing padding of the previous
/// NAL unit.
const START_CODE: [u8; 3] = [0, 0, 1];

/// Iterator over the NAL units of an Annex B byte stream, without start
/// codes. Created by [`nal_units`].
#[derive(Debug, Clone)]
pub struct NalUnits<'a> {
    rest: &'a [u8],
}

/// Splits `data` at 3- and 4-byte start codes.
///
/// Bytes before the first start code are ignored, as are empty NAL units and
/// trailing zero bytes. Stripping trailing zeros is safe because a NAL unit
/// never ends in a zero byte: its RBSP ends with the stop bit.
#[must_use]
pub fn nal_units(data: &[u8]) -> NalUnits<'_> {
    let rest = find_start_code(data)
        .and_then(|pos| data.get(pos.saturating_add(START_CODE.len())..))
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
                    self.rest
                        .get(pos.saturating_add(START_CODE.len())..)
                        .unwrap_or_default(),
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

/// The `nal_unit_type` of a NAL unit, or `None` if it is empty.
#[must_use]
pub fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|header| header & 0x1f)
}

fn find_start_code(data: &[u8]) -> Option<usize> {
    data.windows(START_CODE.len())
        .position(|window| window == START_CODE)
}

fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let end = nal
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |last| last.saturating_add(1));
    nal.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn reads_the_nal_type_from_the_header() {
        assert_eq!(nal_type(&[0x67, 0x42]), Some(NAL_SPS));
        assert_eq!(nal_type(&[0x68]), Some(NAL_PPS));
        assert_eq!(nal_type(&[0x09, 0xf0]), Some(NAL_AUD));
        assert_eq!(nal_type(&[]), None);
    }
}
