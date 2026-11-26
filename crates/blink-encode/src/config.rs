//! Encoder configuration and the values derived from it.

use blink_ring::media::Timestamp;

/// NVENC preset. Lower numbers are faster and lower quality.
///
/// Only P1 to P3 are offered: `docs/ARCHITECTURE.md` limits blink to fast
/// presets. The preset work runs on the NVENC block rather than the shader
/// cores, but slower presets take longer per frame, and the encode thread
/// waits for each frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Preset {
    /// The default: the fastest preset, so each frame holds the NVENC block
    /// (and the waiting capture thread) for the least time. The quality
    /// lost to P2 is small at blink's bitrates.
    #[default]
    P1,
    /// A little better quality than P1 at more encoder time.
    P2,
    /// Slightly better quality than P2 at a little more encoder time.
    P3,
}

/// Settings for `Encoder::new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EncoderConfig {
    /// Initial encode width in pixels.
    pub width: u32,
    /// Initial encode height in pixels.
    pub height: u32,
    /// Largest width a later `reconfigure` may use, normally the output's
    /// native width. Fixed for the life of the session; a larger size needs a
    /// new `Encoder`.
    pub max_width: u32,
    /// Largest height a later `reconfigure` may use. Fixed for the life of the
    /// session; a larger size needs a new `Encoder`.
    pub max_height: u32,
    /// Nominal frame rate: the rate-control frame budget and the nominal
    /// packet duration. IDR frames are scheduled by timestamp (at least one
    /// per second of PTS), not by frame count, so variable-rate input is fine.
    pub fps: u32,
    /// Constant bitrate in kilobits (1000 bits) per second.
    pub bitrate_kbps: u32,
    /// Display size to stretch every frame to, as `(width, height)` (for
    /// example `(1920, 1080)` for a 4:3 game mode on a 16:9 panel). Only its
    /// ratio matters. The sample aspect ratio is derived from it for each
    /// encode size, so it stays correct across `reconfigure`. `None` means
    /// square pixels.
    pub stretch_to: Option<(u32, u32)>,
    /// NVENC preset.
    pub preset: Preset,
}

/// A rejected [`EncoderConfig`] or reconfigure size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EncoderConfigError {
    /// A width or height was zero.
    #[error("frame size {width}x{height} has a zero dimension")]
    ZeroSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The frame is larger than the session maximum.
    #[error("frame size {width}x{height} exceeds the session maximum {max_width}x{max_height}")]
    ExceedsMax {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
        /// Session maximum width.
        max_width: u32,
        /// Session maximum height.
        max_height: u32,
    },
    /// The frame rate was zero.
    #[error("frame rate must be at least 1 fps")]
    ZeroFps,
    /// The bitrate was zero or does not fit NVENC's 32-bit bits-per-second field.
    #[error("bitrate {0} kbps is outside 1..=4294967 kbps")]
    Bitrate(u32),
    /// A `stretch_to` dimension was zero.
    #[error("stretch_to {0}x{1} has a zero dimension")]
    ZeroStretch(u32, u32),
    /// The sample aspect ratio for this size and `stretch_to` does not fit in
    /// 32 bits even after reduction.
    #[error(
        "sample aspect ratio for {width}x{height} stretched to {stretch_width}x{stretch_height} overflows"
    )]
    AspectOverflow {
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
        /// `stretch_to` width.
        stretch_width: u32,
        /// `stretch_to` height.
        stretch_height: u32,
    },
}

impl EncoderConfig {
    /// A configuration whose maximum size is the initial size, with square
    /// pixels and the default preset.
    #[must_use]
    pub const fn new(width: u32, height: u32, fps: u32, bitrate_kbps: u32) -> Self {
        Self {
            width,
            height,
            max_width: width,
            max_height: height,
            fps,
            bitrate_kbps,
            stretch_to: None,
            preset: Preset::P1,
        }
    }

    /// Checks every field.
    ///
    /// # Errors
    ///
    /// The first [`EncoderConfigError`] found.
    pub fn validate(&self) -> Result<(), EncoderConfigError> {
        check_size(self.width, self.height, self.max_width, self.max_height)?;
        if self.fps == 0 {
            return Err(EncoderConfigError::ZeroFps);
        }
        bitrate_bps(self.bitrate_kbps)?;
        self.sample_aspect()?;
        Ok(())
    }

    /// Length of one frame at the nominal frame rate, in 100 ns ticks,
    /// truncated (166 666 ticks at 60 fps).
    pub(crate) fn frame_duration(&self) -> Timestamp {
        Timestamp::from_ticks(Timestamp::TICKS_PER_SECOND / i64::from(self.fps.max(1)))
    }

    /// The display aspect ratio for `NV_ENC_INITIALIZE_PARAMS::darWidth` and
    /// `darHeight`: `stretch_to`, or the frame's own shape, reduced.
    ///
    /// The SDK 12.1 VUI struct has no aspect-ratio fields; NVENC derives the
    /// SPS sample aspect ratio from the display aspect ratio and the coded size
    /// (`SAR = DAR * height / width`). The frame's own shape signals square
    /// pixels (the same choice `FFmpeg`'s `nvenc.c` makes).
    pub(crate) fn display_aspect(&self) -> Result<(u32, u32), EncoderConfigError> {
        let (width, height) = self.stretch()?.unwrap_or((self.width, self.height));
        // Reducing never grows a value, so the conversions back cannot fail.
        let zero = EncoderConfigError::ZeroSize { width, height };
        let (width, height) = reduce(u64::from(width), u64::from(height)).ok_or(zero)?;
        Ok((
            u32::try_from(width).map_err(|_| zero)?,
            u32::try_from(height).map_err(|_| zero)?,
        ))
    }

    /// The sample aspect ratio for `VideoFormat::sample_aspect` (and the MP4
    /// `pasp` box): `stretch_w * height : stretch_h * width`, reduced, or
    /// `None` for square pixels.
    pub(crate) fn sample_aspect(&self) -> Result<Option<(u32, u32)>, EncoderConfigError> {
        let Some((stretch_width, stretch_height)) = self.stretch()? else {
            return Ok(None);
        };
        let (width, height) = (self.width, self.height);
        let overflow = EncoderConfigError::AspectOverflow {
            width,
            height,
            stretch_width,
            stretch_height,
        };
        let horizontal = u64::from(stretch_width) * u64::from(height);
        let vertical = u64::from(stretch_height) * u64::from(width);
        let (horizontal, vertical) =
            reduce(horizontal, vertical).ok_or(EncoderConfigError::ZeroSize { width, height })?;
        let sample = (
            u32::try_from(horizontal).map_err(|_| overflow)?,
            u32::try_from(vertical).map_err(|_| overflow)?,
        );
        Ok((sample.0 != sample.1).then_some(sample))
    }

    const fn stretch(&self) -> Result<Option<(u32, u32)>, EncoderConfigError> {
        match self.stretch_to {
            Some((width, height)) if width == 0 || height == 0 => {
                Err(EncoderConfigError::ZeroStretch(width, height))
            }
            stretch => Ok(stretch),
        }
    }
}

/// Checks a frame size against the session maximum.
const fn check_size(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> Result<(), EncoderConfigError> {
    if width == 0 || height == 0 {
        return Err(EncoderConfigError::ZeroSize { width, height });
    }
    if width > max_width || height > max_height {
        return Err(EncoderConfigError::ExceedsMax {
            width,
            height,
            max_width,
            max_height,
        });
    }
    Ok(())
}

/// The bitrate in bits per second, as NVENC's `averageBitRate` wants it.
pub(crate) const fn bitrate_bps(kbps: u32) -> Result<u32, EncoderConfigError> {
    match kbps.checked_mul(1000) {
        Some(bps) if bps > 0 => Ok(bps),
        _ => Err(EncoderConfigError::Bitrate(kbps)),
    }
}

/// `a : b` in lowest terms, or `None` if either is zero.
const fn reduce(a: u64, b: u64) -> Option<(u64, u64)> {
    if a == 0 || b == 0 {
        return None;
    }
    let divisor = gcd(a, b);
    Some((a / divisor, b / divisor))
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let rest = a % b;
        a = b;
        b = rest;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> EncoderConfig {
        EncoderConfig {
            max_width: 2560,
            max_height: 1440,
            ..EncoderConfig::new(1920, 1080, 60, 20_000)
        }
    }

    fn sized(width: u32, height: u32, stretch_to: Option<(u32, u32)>) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            stretch_to,
            ..valid()
        }
    }

    #[test]
    fn new_uses_the_initial_size_as_maximum() {
        let config = EncoderConfig::new(1280, 720, 30, 8000);
        assert_eq!((config.max_width, config.max_height), (1280, 720));
        assert_eq!(config.preset, Preset::P1);
        assert_eq!(config.stretch_to, None);
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn validate_accepts_a_sane_config() {
        assert_eq!(valid().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_zero_and_oversized_frames() {
        let zero = EncoderConfig {
            width: 0,
            ..valid()
        };
        assert_eq!(
            zero.validate(),
            Err(EncoderConfigError::ZeroSize {
                width: 0,
                height: 1080
            })
        );
        let too_big = EncoderConfig {
            height: 1600,
            ..valid()
        };
        assert!(matches!(
            too_big.validate(),
            Err(EncoderConfigError::ExceedsMax { height: 1600, .. })
        ));
    }

    #[test]
    fn validate_rejects_bad_rates() {
        let no_fps = EncoderConfig { fps: 0, ..valid() };
        assert_eq!(no_fps.validate(), Err(EncoderConfigError::ZeroFps));
        let no_bitrate = EncoderConfig {
            bitrate_kbps: 0,
            ..valid()
        };
        assert_eq!(no_bitrate.validate(), Err(EncoderConfigError::Bitrate(0)));
        let huge = EncoderConfig {
            bitrate_kbps: 4_294_968,
            ..valid()
        };
        assert_eq!(huge.validate(), Err(EncoderConfigError::Bitrate(4_294_968)));
    }

    #[test]
    fn validate_rejects_a_zero_stretch() {
        let config = sized(1920, 1080, Some((1920, 0)));
        assert_eq!(
            config.validate(),
            Err(EncoderConfigError::ZeroStretch(1920, 0))
        );
    }

    #[test]
    fn bitrate_converts_to_bits_per_second() {
        assert_eq!(bitrate_bps(20_000), Ok(20_000_000));
        assert_eq!(bitrate_bps(4_294_967), Ok(4_294_967_000));
    }

    #[test]
    fn frame_duration_is_one_frame_in_ticks() {
        assert_eq!(valid().frame_duration(), Timestamp::from_ticks(166_666));
        let thirty = EncoderConfig { fps: 30, ..valid() };
        assert_eq!(thirty.frame_duration(), Timestamp::from_ticks(333_333));
    }

    #[test]
    fn square_pixels_give_the_frame_shape() {
        assert_eq!(sized(1920, 1080, None).display_aspect(), Ok((16, 9)));
        assert_eq!(sized(1280, 1024, None).display_aspect(), Ok((5, 4)));
        assert_eq!(sized(1920, 1080, None).sample_aspect(), Ok(None));
    }

    #[test]
    fn stretch_to_sets_the_display_aspect_directly() {
        let stretched = sized(1280, 960, Some((1920, 1080)));
        assert_eq!(stretched.display_aspect(), Ok((16, 9)));
        assert_eq!(stretched.sample_aspect(), Ok(Some((4, 3))));
    }

    #[test]
    fn sample_aspect_follows_the_encode_size() {
        // The same stretch_to across a mode change: 4:3 game, then the native
        // desktop. Native needs no stretch, so it must report square pixels,
        // not the 4:3 game's SAR (which would play as 2560x1080).
        let stretch = Some((1920, 1080));
        assert_eq!(sized(1280, 960, stretch).sample_aspect(), Ok(Some((4, 3))));
        assert_eq!(sized(1920, 1080, stretch).sample_aspect(), Ok(None));
        assert_eq!(sized(1920, 1080, stretch).display_aspect(), Ok((16, 9)));
        // 5:4 game mode on the same panel.
        assert_eq!(
            sized(1280, 1024, stretch).sample_aspect(),
            Ok(Some((64, 45)))
        );
    }

    #[test]
    fn sample_aspect_reports_overflow() {
        let config = EncoderConfig {
            width: 4093,
            height: 4091,
            max_width: 4096,
            max_height: 4096,
            stretch_to: Some((u32::MAX, 1)),
            ..valid()
        };
        assert!(matches!(
            config.sample_aspect(),
            Err(EncoderConfigError::AspectOverflow { .. })
        ));
    }

    #[test]
    fn check_size_allows_the_maximum() {
        assert_eq!(check_size(2560, 1440, 2560, 1440), Ok(()));
        assert!(check_size(2561, 1440, 2560, 1440).is_err());
    }
}
