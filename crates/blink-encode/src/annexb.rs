//! Annex B H.264 parsing: just enough to pull SPS and PPS out of NVENC's
//! sequence header.

use blink_ring::h264::{NAL_PPS, NAL_SPS, nal_type, nal_units};

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
        match nal_type(nal) {
            Some(NAL_SPS) if sps.is_none() => sps = Some(nal),
            Some(NAL_PPS) if pps.is_none() => pps = Some(nal),
            _ => {}
        }
    }
    let sps = sps.ok_or(ParameterSetError::MissingSps)?;
    let pps = pps.ok_or(ParameterSetError::MissingPps)?;
    Ok((sps, pps))
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
}
