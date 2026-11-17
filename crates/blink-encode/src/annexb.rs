//! Annex B H.264 parsing: just enough to pull SPS and PPS out of NVENC's
//! sequence header.

/// NAL unit type of a sequence parameter set.
const NAL_SPS: u8 = 7;
/// NAL unit type of a picture parameter set.
const NAL_PPS: u8 = 8;

/// The sequence header lacked a parameter set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParameterSetError {
    /// No SPS NAL unit was found.
    #[error("the sequence header has no SPS")]
    MissingSps,
    /// No PPS NAL unit was found.
    #[error("the sequence header has no PPS")]
    MissingPps,
}

/// The first SPS and PPS in an Annex B stream, each as a raw NAL unit (header
/// byte included) without its start code.
pub(crate) fn parameter_sets(stream: &[u8]) -> Result<(&[u8], &[u8]), ParameterSetError> {
    let mut sps = None;
    let mut pps = None;
    for nal in nal_units(stream) {
        match nal.first().map(|header| header & 0x1F) {
            Some(NAL_SPS) if sps.is_none() => sps = Some(nal),
            Some(NAL_PPS) if pps.is_none() => pps = Some(nal),
            _ => {}
        }
    }
    let sps = sps.ok_or(ParameterSetError::MissingSps)?;
    let pps = pps.ok_or(ParameterSetError::MissingPps)?;
    Ok((sps, pps))
}

/// Iterates over the NAL units of an Annex B byte stream.
///
/// Units are delimited by the three-byte start code `00 00 01`. The leading
/// `zero_byte` of a four-byte start code and any `trailing_zero_8bits` are
/// stripped, which is safe because a NAL unit never ends in a zero byte (its
/// RBSP ends with the stop bit). Bytes before the first start code are skipped.
fn nal_units(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut next = find_start_code(stream, 0);
    core::iter::from_fn(move || {
        let start = next?;
        next = find_start_code(stream, start);
        let end = next.map_or(stream.len(), |after| after.saturating_sub(3));
        let nal = stream.get(start..end).unwrap_or_default();
        Some(trim_trailing_zeros(nal))
    })
}

/// Index of the first byte after the next `00 00 01` at or after `from`.
fn find_start_code(stream: &[u8], from: usize) -> Option<usize> {
    stream
        .get(from..)?
        .windows(3)
        .position(|window| window == [0, 0, 1])
        .map(|offset| from + offset + 3)
}

fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let len = nal
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |last| last + 1);
    nal.get(..len).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 6] = [0x67, 0x64, 0x00, 0x28, 0xac, 0x2b];
    const PPS: [u8; 4] = [0x68, 0xee, 0x3c, 0xb0];

    #[test]
    fn extracts_sps_and_pps_with_four_byte_start_codes() {
        let mut stream = vec![0, 0, 0, 1];
        stream.extend_from_slice(&SPS);
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&PPS);
        assert_eq!(parameter_sets(&stream), Ok((&SPS[..], &PPS[..])));
    }

    #[test]
    fn extracts_sps_and_pps_with_three_byte_start_codes_and_padding() {
        let mut stream = vec![0, 0, 1];
        stream.extend_from_slice(&SPS);
        stream.extend_from_slice(&[0, 0, 0, 0, 1]);
        stream.extend_from_slice(&PPS);
        stream.extend_from_slice(&[0, 0]);
        assert_eq!(parameter_sets(&stream), Ok((&SPS[..], &PPS[..])));
    }

    #[test]
    fn skips_other_nal_units_and_keeps_the_first_of_each() {
        let mut stream = vec![0, 0, 0, 1, 0x09, 0xf0];
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&SPS);
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&PPS);
        stream.extend_from_slice(&[0, 0, 0, 1, 0x67, 0xff]);
        assert_eq!(parameter_sets(&stream), Ok((&SPS[..], &PPS[..])));
    }

    #[test]
    fn reports_missing_parameter_sets() {
        let mut sequence_only = vec![0, 0, 0, 1];
        sequence_only.extend_from_slice(&SPS);
        assert_eq!(
            parameter_sets(&sequence_only),
            Err(ParameterSetError::MissingPps)
        );
        let mut picture_only = vec![0, 0, 1];
        picture_only.extend_from_slice(&PPS);
        assert_eq!(
            parameter_sets(&picture_only),
            Err(ParameterSetError::MissingSps)
        );
        assert_eq!(parameter_sets(&[]), Err(ParameterSetError::MissingSps));
        assert_eq!(
            parameter_sets(&[0x67, 0x64]),
            Err(ParameterSetError::MissingSps)
        );
    }

    #[test]
    fn splits_every_unit() {
        let stream = [0, 0, 1, 0x09, 0, 0, 0, 1, 0x67, 0x01, 0, 0, 1, 0x68];
        let units: Vec<&[u8]> = nal_units(&stream).collect();
        assert_eq!(units, [&[0x09][..], &[0x67, 0x01][..], &[0x68][..]]);
    }
}
