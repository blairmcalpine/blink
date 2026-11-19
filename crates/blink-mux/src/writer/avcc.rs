//! `AVCDecoderConfigurationRecord` (the `avcC` box payload, ISO/IEC 14496-15).

use blink_ring::h264::{NAL_PPS, NAL_SPS, nal_type};

use crate::MuxError;

const TRUNCATED: MuxError = MuxError::InvalidSps {
    reason: "truncated",
};

/// Profiles whose SPS carries `chroma_format_idc` and the bit depths, and
/// whose `avcC` therefore gets the chroma format and bit depth extension.
/// This is every profile but Baseline (66), Main (77), and Extended (88),
/// matching what `ffmpeg` writes; 144 is the withdrawn High 4:4:4 profile.
const CHROMA_PROFILES: [u8; 13] = [100, 110, 122, 144, 244, 44, 83, 86, 118, 128, 138, 139, 134];

/// Builds the `avcC` payload from one SPS and one PPS NAL unit.
///
/// Profile, compatibility flags, and level come from SPS bytes 1 to 3. NAL
/// lengths are 4 bytes. High profiles also get the chroma format and bit
/// depths parsed from the SPS (every profile but 66, 77, and 88).
pub(super) fn decoder_config(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, MuxError> {
    let &[_, profile, compatibility, level, ..] = sps else {
        return Err(MuxError::InvalidSps {
            reason: "shorter than 4 bytes",
        });
    };
    if nal_type(sps) != Some(NAL_SPS) {
        return Err(MuxError::InvalidSps {
            reason: "not an SPS NAL unit",
        });
    }
    if nal_type(pps) != Some(NAL_PPS) {
        return Err(MuxError::InvalidPps);
    }
    let sps_len =
        u16::try_from(sps.len()).map_err(|_| MuxError::InvalidSps { reason: "too long" })?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| MuxError::InvalidPps)?;

    let mut out = Vec::with_capacity(sps.len() + pps.len() + 15);
    // Version 1, profile, compatibility, level, 4-byte lengths, one SPS.
    out.extend_from_slice(&[1, profile, compatibility, level, 0xff, 0xe1]);
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    if CHROMA_PROFILES.contains(&profile) {
        let chroma = parse_chroma(sps.get(4..).unwrap_or_default())?;
        out.extend_from_slice(&[
            0xfc | chroma.format,
            0xf8 | chroma.luma_depth_minus8,
            0xf8 | chroma.chroma_depth_minus8,
            0,
        ]);
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
struct SpsChroma {
    format: u8,
    luma_depth_minus8: u8,
    chroma_depth_minus8: u8,
}

/// Parses `chroma_format_idc` and the bit depths from the SPS RBSP that
/// follows `level_idc`.
fn parse_chroma(after_level: &[u8]) -> Result<SpsChroma, MuxError> {
    let rbsp = unescape(after_level);
    let mut bits = BitReader::new(&rbsp);
    let _seq_parameter_set_id = bits.exp_golomb().ok_or(TRUNCATED)?;
    let format = bits.exp_golomb().ok_or(TRUNCATED)?;
    if format == 3 {
        bits.bit().ok_or(TRUNCATED)?;
    }
    let luma = bits.exp_golomb().ok_or(TRUNCATED)?;
    let chroma = bits.exp_golomb().ok_or(TRUNCATED)?;
    let field = |value: u32, max: u32| {
        u8::try_from(value)
            .ok()
            .filter(|_| value <= max)
            .ok_or(MuxError::InvalidSps {
                reason: "field out of range",
            })
    };
    Ok(SpsChroma {
        format: field(format, 3)?,
        luma_depth_minus8: field(luma, 6)?,
        chroma_depth_minus8: field(chroma, 6)?,
    })
}

/// Removes emulation-prevention bytes (`00 00 03` becomes `00 00`).
fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0_u8;
    for &byte in data {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 {
            zeros.saturating_add(1)
        } else {
            0
        };
        out.push(byte);
    }
    out
}

/// Big-endian bit reader with Exp-Golomb decoding.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = self.data.get(self.pos / 8)?;
        let shift = 7 - u32::try_from(self.pos % 8).ok()?;
        self.pos += 1;
        Some(u32::from(byte >> shift) & 1)
    }

    /// Reads an unsigned Exp-Golomb code of at most 32 bits.
    fn exp_golomb(&mut self) -> Option<u32> {
        let mut leading_zeros = 0_u32;
        while self.bit()? == 0 {
            leading_zeros += 1;
            if leading_zeros > 31 {
                return None;
            }
        }
        let mut suffix = 0_u32;
        for _ in 0..leading_zeros {
            suffix = (suffix << 1) | self.bit()?;
        }
        Some((1_u32 << leading_zeros) - 1 + suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASELINE_SPS: [u8; 4] = [0x67, 0x42, 0xc0, 0x0a];
    const PPS: [u8; 4] = [0x68, 0xce, 0x3c, 0x80];
    /// SPS from `tests/fixtures/video_high.h264` (High, 4:2:0, 8-bit), with an
    /// emulation-prevention byte.
    const HIGH_SPS: [u8; 22] = [
        0x67, 0x64, 0x00, 0x0a, 0xac, 0xb4, 0x23, 0xd8, 0x08, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80,
        0x00, 0x00, 0x1e, 0x07, 0x89, 0x13, 0x50,
    ];

    #[test]
    fn baseline_record_has_no_extension() {
        let record = decoder_config(&BASELINE_SPS, &PPS).expect("valid");
        assert_eq!(
            record,
            [
                1, 0x42, 0xc0, 0x0a, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0xc0, 0x0a, 1, 0, 4, 0x68, 0xce,
                0x3c, 0x80
            ]
        );
    }

    #[test]
    fn high_record_has_chroma_extension() {
        let record = decoder_config(&HIGH_SPS, &PPS).expect("valid");
        assert_eq!(record.get(..4), Some(&[1, 0x64, 0x00, 0x0a][..]));
        assert_eq!(
            record.get(record.len() - 4..),
            Some(&[0xfd, 0xf8, 0xf8, 0][..])
        );
    }

    #[test]
    fn parses_high_444_10_bit() {
        // seq_parameter_set_id 0 (1), chroma 3 (00100), separate planes 0 (0),
        // luma depth 2 (011), chroma depth 2 (011), padding.
        let chroma = parse_chroma(&[0b1001_0000, 0b1101_1000]).expect("valid");
        assert_eq!(
            chroma,
            SpsChroma {
                format: 3,
                luma_depth_minus8: 2,
                chroma_depth_minus8: 2
            }
        );
    }

    #[test]
    fn rejects_bad_parameter_sets() {
        assert!(matches!(
            decoder_config(&[0x67, 0x42], &PPS),
            Err(MuxError::InvalidSps { .. })
        ));
        assert!(matches!(
            decoder_config(&PPS, &PPS),
            Err(MuxError::InvalidSps { .. })
        ));
        assert!(matches!(
            decoder_config(&BASELINE_SPS, &[]),
            Err(MuxError::InvalidPps)
        ));
        assert!(matches!(
            decoder_config(&[0x67, 100, 0, 10], &PPS),
            Err(MuxError::InvalidSps {
                reason: "truncated"
            })
        ));
        assert!(matches!(
            parse_chroma(&[0x80, 0x01]),
            Err(MuxError::InvalidSps { .. })
        ));
    }

    #[test]
    fn unescapes_emulation_prevention() {
        assert_eq!(
            unescape(&[0, 0, 3, 1, 0, 0, 3, 0, 3]),
            [0, 0, 1, 0, 0, 0, 3]
        );
    }

    #[test]
    fn reads_exp_golomb() {
        // 1 -> 0, 010 -> 1, 011 -> 2, 00100 -> 3.
        let data = [0b1010_0110, 0b0100_0000];
        let mut bits = BitReader::new(&data);
        assert_eq!(bits.exp_golomb(), Some(0));
        assert_eq!(bits.exp_golomb(), Some(1));
        assert_eq!(bits.exp_golomb(), Some(2));
        assert_eq!(bits.exp_golomb(), Some(3));
        assert_eq!(BitReader::new(&[0, 0, 0, 0, 0]).exp_golomb(), None);
    }
}
