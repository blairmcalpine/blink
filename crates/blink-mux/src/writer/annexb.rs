//! Annex B to length-prefixed (AVCC) conversion.

use std::io::{self, Write};

use blink_ring::h264::{NAL_AUD, NAL_PPS, NAL_SPS, nal_type, nal_units};

/// NAL units of an access unit that belong in an MP4 sample: everything but
/// SPS, PPS, and AUD.
fn sample_nal_units(access_unit: &[u8]) -> impl Iterator<Item = &[u8]> {
    nal_units(access_unit).filter(|nal| !matches!(nal_type(nal), Some(NAL_SPS | NAL_PPS | NAL_AUD)))
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
