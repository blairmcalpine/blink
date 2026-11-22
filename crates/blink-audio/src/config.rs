//! Audio capture settings.

/// An AAC bitrate the Media Foundation encoder accepts at 48 kHz stereo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AacBitrate {
    /// 96 kbit/s.
    Kbps96,
    /// 128 kbit/s.
    Kbps128,
    /// 160 kbit/s (the default).
    #[default]
    Kbps160,
    /// 192 kbit/s.
    Kbps192,
}

impl AacBitrate {
    /// Every supported bitrate, lowest first.
    pub const ALL: [Self; 4] = [Self::Kbps96, Self::Kbps128, Self::Kbps160, Self::Kbps192];

    /// The bitrate in kbit/s.
    #[must_use]
    pub const fn kbps(self) -> u32 {
        match self {
            Self::Kbps96 => 96,
            Self::Kbps128 => 128,
            Self::Kbps160 => 160,
            Self::Kbps192 => 192,
        }
    }

    /// The encoder's `MF_MT_AUDIO_AVG_BYTES_PER_SECOND` (`kbps * 1000 / 8`).
    #[must_use]
    pub const fn avg_bytes_per_second(self) -> u32 {
        self.kbps() * 125
    }
}

impl TryFrom<u32> for AacBitrate {
    type Error = UnsupportedBitrate;

    /// Accepts exactly the rates in [`AacBitrate::ALL`], in kbit/s.
    fn try_from(kbps: u32) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|bitrate| bitrate.kbps() == kbps)
            .ok_or(UnsupportedBitrate(kbps))
    }
}

/// A bitrate, in kbit/s, that is not one of [`AacBitrate::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unsupported AAC bitrate {0} kbit/s; expected one of {supported:?}", supported = AacBitrate::ALL.map(AacBitrate::kbps))]
pub struct UnsupportedBitrate(pub u32);

/// Settings for [`AudioCapture::start`](crate::AudioCapture::start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioConfig {
    /// AAC bitrate.
    pub bitrate: AacBitrate,
    /// Whether the default recording device is mixed in (on by default).
    pub microphone: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            bitrate: AacBitrate::default(),
            microphone: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_bitrates_round_trip() {
        let kbps = AacBitrate::ALL.map(AacBitrate::kbps);
        assert_eq!(kbps, [96, 128, 160, 192]);
        for bitrate in AacBitrate::ALL {
            assert_eq!(AacBitrate::try_from(bitrate.kbps()), Ok(bitrate));
        }
        let bytes = AacBitrate::ALL.map(AacBitrate::avg_bytes_per_second);
        assert_eq!(bytes, [12_000, 16_000, 20_000, 24_000]);
    }

    #[test]
    fn rejects_unsupported_bitrates() {
        for kbps in [0, 64, 127, 256, u32::MAX] {
            assert_eq!(AacBitrate::try_from(kbps), Err(UnsupportedBitrate(kbps)));
        }
        assert_eq!(
            UnsupportedBitrate(64).to_string(),
            "unsupported AAC bitrate 64 kbit/s; expected one of [96, 128, 160, 192]"
        );
    }

    #[test]
    fn default_is_160_with_the_microphone() {
        assert_eq!(AudioConfig::default().bitrate, AacBitrate::Kbps160);
        assert!(AudioConfig::default().microphone);
    }
}
