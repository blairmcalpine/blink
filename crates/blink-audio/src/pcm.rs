//! Captured sample formats and conversion to the encoder's 16-bit PCM.
//!
//! WASAPI buffers are read as little-endian bytes rather than reinterpreted as
//! `f32` or `i16` slices, so nothing here depends on the buffer's alignment.

/// Sample encoding of a captured WASAPI packet. Both formats are interleaved
/// stereo at 48 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleFormat {
    /// 32-bit IEEE float, nominal range `[-1.0, 1.0]`.
    F32,
    /// 16-bit signed integer PCM.
    S16,
}

impl SampleFormat {
    /// Bytes per sample (one channel).
    pub(crate) const fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::S16 => 2,
        }
    }

    /// Container bits per sample.
    pub(crate) const fn bits(self) -> u16 {
        match self {
            Self::F32 => 32,
            Self::S16 => 16,
        }
    }
}

/// Converts one float sample to 16-bit PCM: scales by 32768, rounds to
/// nearest, and saturates to the `i16` range. NaN becomes silence.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "f32 -> i16 has no From/TryFrom; `as` saturates and maps NaN to 0, the intended conversion"
)]
pub(crate) fn f32_to_s16(sample: f32) -> i16 {
    (sample * 32_768.0).round() as i16
}

/// Converts interleaved little-endian samples in `src` to `i16` in `dst`.
///
/// Converts as many whole samples as fit in both buffers and returns how many
/// were written. Trailing bytes that do not form a whole sample are ignored.
pub(crate) fn convert(format: SampleFormat, src: &[u8], dst: &mut [i16]) -> usize {
    match format {
        SampleFormat::F32 => src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.iter_mut())
            .map(|(&bytes, out)| *out = f32_to_s16(f32::from_le_bytes(bytes)))
            .count(),
        SampleFormat::S16 => src
            .as_chunks::<2>()
            .0
            .iter()
            .zip(dst.iter_mut())
            .map(|(&bytes, out)| *out = i16::from_le_bytes(bytes))
            .count(),
    }
}

/// Writes `src` into `dst` as little-endian bytes, the layout Media
/// Foundation expects for `MFAudioFormat_PCM`. Returns the bytes written.
pub(crate) fn write_le(src: &[i16], dst: &mut [u8]) -> usize {
    src.iter()
        .zip(dst.as_chunks_mut::<2>().0.iter_mut())
        .map(|(sample, out)| *out = sample.to_le_bytes())
        .count()
        * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_conversion_scales_rounds_and_clamps() {
        assert_eq!(f32_to_s16(0.0), 0);
        assert_eq!(f32_to_s16(-0.0), 0);
        assert_eq!(f32_to_s16(0.5), 16_384);
        assert_eq!(f32_to_s16(-0.5), -16_384);
        assert_eq!(f32_to_s16(-1.0), i16::MIN);
        assert_eq!(f32_to_s16(1.0), i16::MAX);
        assert_eq!(f32_to_s16(1.5), i16::MAX);
        assert_eq!(f32_to_s16(-7.0), i16::MIN);
        assert_eq!(f32_to_s16(1.0 / 32_768.0), 1);
        assert_eq!(f32_to_s16(0.4 / 32_768.0), 0);
    }

    #[test]
    fn float_conversion_handles_non_finite() {
        assert_eq!(f32_to_s16(f32::NAN), 0);
        assert_eq!(f32_to_s16(f32::INFINITY), i16::MAX);
        assert_eq!(f32_to_s16(f32::NEG_INFINITY), i16::MIN);
    }

    #[test]
    fn converts_float_bytes() {
        let src: Vec<u8> = [0.5_f32, -0.25, 2.0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut dst = [7_i16; 4];
        assert_eq!(convert(SampleFormat::F32, &src, &mut dst), 3);
        assert_eq!(dst, [16_384, -8_192, i16::MAX, 7]);
    }

    #[test]
    fn converts_s16_bytes() {
        let src: Vec<u8> = [1_i16, -2, i16::MIN]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut dst = [0_i16; 3];
        assert_eq!(convert(SampleFormat::S16, &src, &mut dst), 3);
        assert_eq!(dst, [1, -2, i16::MIN]);
    }

    #[test]
    fn conversion_stops_at_the_shorter_buffer() {
        let src = [0_u8; 13];
        let mut dst = [1_i16; 8];
        assert_eq!(convert(SampleFormat::F32, &src, &mut dst), 3);
        assert_eq!(dst, [0, 0, 0, 1, 1, 1, 1, 1]);
        let mut short = [1_i16; 2];
        assert_eq!(convert(SampleFormat::S16, &src, &mut short), 2);
    }

    #[test]
    fn writes_little_endian() {
        let mut dst = [0_u8; 5];
        assert_eq!(write_le(&[0x0102, -1], &mut dst), 4);
        assert_eq!(dst, [0x02, 0x01, 0xff, 0xff, 0]);
    }

    #[test]
    fn format_sizes() {
        assert_eq!(SampleFormat::F32.bytes_per_sample(), 4);
        assert_eq!(SampleFormat::S16.bytes_per_sample(), 2);
        assert_eq!(SampleFormat::F32.bits(), 32);
        assert_eq!(SampleFormat::S16.bits(), 16);
    }
}
