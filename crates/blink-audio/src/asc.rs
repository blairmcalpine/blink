//! MPEG-4 `AudioSpecificConfig` (ISO/IEC 14496-3, 1.6.2.1) for AAC-LC.
//!
//! The MP4 `esds` box carries this descriptor. The Media Foundation encoder
//! reports it in `MF_MT_USER_DATA`; [`from_user_data`] extracts it there and
//! [`aac_lc`] builds the expected value so the two can be cross-checked.

/// MPEG-4 audio object type for AAC-LC.
const AOT_AAC_LC: u8 = 2;

/// Sampling frequencies by `samplingFrequencyIndex`.
const SAMPLING_FREQUENCIES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// Length of the `HEAACWAVEINFO` fields that precede the `AudioSpecificConfig`
/// in `MF_MT_USER_DATA`: `wPayloadType`, `wAudioProfileLevelIndication`,
/// `wStructType` (all `u16`), `wReserved1` (`u16`), and `dwReserved2` (`u32`).
/// The user data is the `HEAACWAVEINFO` structure minus its leading
/// `WAVEFORMATEX`, followed by the descriptor.
pub(crate) const HEAAC_INFO_LEN: usize = 12;

/// The two-byte `AudioSpecificConfig` for AAC-LC at `sample_rate` with
/// `channels` channels, or `None` if either has no MPEG-4 code.
///
/// Layout: 5 bits object type, 4 bits frequency index, 4 bits channel
/// configuration, then `GASpecificConfig` with `frameLengthFlag = 0` (1024
/// samples), `dependsOnCoreCoder = 0`, and `extensionFlag = 0`.
pub(crate) fn aac_lc(sample_rate: u32, channels: u16) -> Option<[u8; 2]> {
    let index = SAMPLING_FREQUENCIES
        .iter()
        .position(|&rate| rate == sample_rate)?;
    let index = u16::try_from(index).ok()?;
    if !(1..=7).contains(&channels) {
        return None;
    }
    let bits = (u16::from(AOT_AAC_LC) << 11) | (index << 7) | (channels << 3);
    Some(bits.to_be_bytes())
}

/// Fields of a parsed AAC `AudioSpecificConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AscInfo {
    /// Audio object type (2 for AAC-LC).
    pub(crate) object_type: u8,
    /// Sampling frequency in hertz.
    pub(crate) sample_rate: u32,
    /// Channel configuration (equal to the channel count for 1 to 6).
    pub(crate) channels: u16,
}

/// Parses the leading fields of an `AudioSpecificConfig`. Escaped object
/// types (31) and explicit frequencies (index 15) are rejected because the
/// encoder never emits them for AAC-LC.
pub(crate) fn parse(asc: &[u8]) -> Option<AscInfo> {
    let (&first, rest) = asc.split_first()?;
    let &second = rest.first()?;
    let bits = u16::from_be_bytes([first, second]);
    let object_type = u8::try_from(bits >> 11).ok()?;
    let index = usize::from((bits >> 7) & 0x0f);
    let channels = (bits >> 3) & 0x0f;
    if object_type == 31 {
        return None;
    }
    let sample_rate = *SAMPLING_FREQUENCIES.get(index)?;
    Some(AscInfo {
        object_type,
        sample_rate,
        channels,
    })
}

/// Extracts the `AudioSpecificConfig` from an AAC encoder's `MF_MT_USER_DATA`
/// blob. `None` if the blob is too short or does not describe raw AAC
/// (`wPayloadType` 0).
pub(crate) fn from_user_data(user_data: &[u8]) -> Option<&[u8]> {
    let payload_type = user_data.get(..2)?;
    if payload_type != [0, 0] {
        return None;
    }
    let asc = user_data.get(HEAAC_INFO_LEN..)?;
    if asc.is_empty() { None } else { Some(asc) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_aac_lc_48k_stereo() {
        assert_eq!(aac_lc(48_000, 2), Some([0x11, 0x90]));
    }

    #[test]
    fn builds_other_common_configs() {
        assert_eq!(aac_lc(44_100, 2), Some([0x12, 0x10]));
        assert_eq!(aac_lc(48_000, 1), Some([0x11, 0x88]));
        assert_eq!(aac_lc(48_000, 6), Some([0x11, 0xb0]));
    }

    #[test]
    fn rejects_unknown_rates_and_channel_counts() {
        assert_eq!(aac_lc(47_999, 2), None);
        assert_eq!(aac_lc(48_000, 0), None);
        assert_eq!(aac_lc(48_000, 8), None);
    }

    #[test]
    fn parse_round_trips() {
        let asc = aac_lc(48_000, 2).unwrap();
        assert_eq!(
            parse(&asc),
            Some(AscInfo {
                object_type: 2,
                sample_rate: 48_000,
                channels: 2,
            })
        );
    }

    #[test]
    fn parse_rejects_short_and_escaped() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0x11]), None);
        // Object type 31 (escape).
        assert_eq!(parse(&[0xf9, 0x90]), None);
        // Frequency index 15 (explicit 24-bit frequency).
        assert_eq!(parse(&[0x17, 0x90]), None);
    }

    #[test]
    fn extracts_config_from_heaac_user_data() {
        // wPayloadType = 0 (raw), wAudioProfileLevelIndication = 0x29,
        // wStructType = 0, wReserved1 = 0, dwReserved2 = 0, then the ASC.
        let user_data = [0, 0, 0x29, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x90];
        assert_eq!(from_user_data(&user_data), Some([0x11, 0x90].as_slice()));
    }

    #[test]
    fn rejects_bad_user_data() {
        assert_eq!(from_user_data(&[0; HEAAC_INFO_LEN]), None);
        assert_eq!(from_user_data(&[0; 4]), None);
        // ADTS payload type.
        let adts = [1, 0, 0x29, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x90];
        assert_eq!(from_user_data(&adts), None);
    }
}
