//! How a [`Config`] turns into settings for the encoder, the audio capture,
//! and the clip ring.

use blink_audio::{AacBitrate, AudioConfig, UnsupportedBitrate};
use blink_config::Config;
use blink_encode::{EncoderConfig, Preset};
use blink_ring::ClipRing;
use blink_ring::media::Timestamp;

/// The encoder settings for a `width` x `height` capture in a session that
/// can later reconfigure up to `max` (at least the capture size) without a
/// new encoder.
pub(crate) fn encoder_config(
    config: &Config,
    width: u32,
    height: u32,
    max: (u32, u32),
) -> EncoderConfig {
    EncoderConfig {
        width,
        height,
        max_width: max.0.max(width),
        max_height: max.1.max(height),
        fps: config.fps,
        bitrate_kbps: config.video_bitrate_kbps,
        stretch_to: config
            .stretch_to
            .map(|stretch| (stretch.width, stretch.height)),
        preset: Preset::default(),
    }
}

/// The audio capture settings. Fails only for a bitrate that
/// [`Config::validate`] would also reject.
pub(crate) fn audio_config(config: &Config) -> Result<AudioConfig, UnsupportedBitrate> {
    Ok(AudioConfig {
        bitrate: AacBitrate::try_from(config.audio_bitrate_kbps)?,
    })
}

/// Media the ring keeps beyond the clip length. A save waits for the
/// encoder to drain and then copies while capture keeps pushing; without
/// slack, those newer packets would evict the start of the clip mid-copy.
pub(crate) const RING_SLACK: Timestamp = Timestamp::from_secs(3);

/// Size of the clip ring for a config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RingSize {
    /// Media kept: one clip length plus [`RING_SLACK`].
    pub(crate) window: Timestamp,
    /// Payload arena size, from [`ClipRing::recommended_byte_capacity`].
    pub(crate) byte_capacity: usize,
}

impl RingSize {
    /// The ring for `config`: a window of `clip_seconds` plus
    /// [`RING_SLACK`], with room for that much video and audio at their
    /// configured bitrates.
    pub(crate) fn for_config(config: &Config) -> Self {
        let window =
            Timestamp::from_secs(i64::from(config.clip_seconds)).saturating_add(RING_SLACK);
        let kbps = u64::from(config.video_bitrate_kbps) + u64::from(config.audio_bitrate_kbps);
        Self {
            window,
            byte_capacity: ClipRing::recommended_byte_capacity(window, kbps * 1000),
        }
    }

    /// Allocates a ring of this size.
    pub(crate) fn build(self) -> ClipRing {
        ClipRing::with_capacity(self.window, self.byte_capacity)
    }
}

#[cfg(test)]
mod tests {
    use blink_config::{AUDIO_BITRATES_KBPS, Resolution};

    use super::*;

    #[test]
    fn encoder_config_follows_the_config() {
        let config = Config {
            fps: 144,
            video_bitrate_kbps: 35_000,
            stretch_to: Some(Resolution {
                width: 1920,
                height: 1080,
            }),
            ..Config::default()
        };
        let encoder = encoder_config(&config, 1280, 960, (0, 0));
        assert_eq!(
            encoder,
            EncoderConfig {
                stretch_to: Some((1920, 1080)),
                ..EncoderConfig::new(1280, 960, 144, 35_000)
            }
        );
        assert_eq!(encoder.validate(), Ok(()));
    }

    #[test]
    fn square_pixels_without_stretch() {
        let encoder = encoder_config(&Config::default(), 2560, 1440, (1920, 1080));
        assert_eq!(encoder.stretch_to, None);
        assert_eq!((encoder.max_width, encoder.max_height), (2560, 1440));
        let encoder = encoder_config(&Config::default(), 1920, 1080, (2560, 1600));
        assert_eq!((encoder.width, encoder.height), (1920, 1080));
        assert_eq!((encoder.max_width, encoder.max_height), (2560, 1600));
        assert_eq!(encoder.validate(), Ok(()));
        assert_eq!(encoder.bitrate_kbps, Config::default().video_bitrate_kbps);
    }

    #[test]
    fn audio_config_maps_every_allowed_bitrate() {
        for kbps in AUDIO_BITRATES_KBPS {
            let config = Config {
                audio_bitrate_kbps: kbps,
                ..Config::default()
            };
            assert_eq!(audio_config(&config).unwrap().bitrate.kbps(), kbps);
        }
        let bad = Config {
            audio_bitrate_kbps: 100,
            ..Config::default()
        };
        assert_eq!(audio_config(&bad), Err(UnsupportedBitrate(100)));
    }

    #[test]
    fn config_and_encoder_agree_on_audio_bitrates() {
        assert_eq!(AUDIO_BITRATES_KBPS, AacBitrate::ALL.map(AacBitrate::kbps));
    }

    #[test]
    fn ring_size_covers_the_clip_at_both_bitrates() {
        let config = Config {
            clip_seconds: 60,
            video_bitrate_kbps: 20_000,
            audio_bitrate_kbps: 160,
            ..Config::default()
        };
        let size = RingSize::for_config(&config);
        assert_eq!(size.window, Timestamp::from_secs(63));
        assert_eq!(
            size.byte_capacity,
            ClipRing::recommended_byte_capacity(Timestamp::from_secs(63), 20_160_000)
        );
        // At least the raw clip: 60 s at 20.16 Mbit/s.
        assert!(size.byte_capacity >= 60 * 20_160_000 / 8);
        let ring = size.build();
        drop(ring);
    }

    #[test]
    fn ring_size_tracks_clip_length() {
        let short = RingSize::for_config(&Config {
            clip_seconds: 10,
            ..Config::default()
        });
        let long = RingSize::for_config(&Config {
            clip_seconds: 120,
            ..Config::default()
        });
        assert!(long.byte_capacity > short.byte_capacity);
    }
}
