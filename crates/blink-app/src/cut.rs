//! Cutting a clip out of the shared ring while the producers keep pushing.
//!
//! The cut copies no payload: the clip shares the ring's blocks, so the lock
//! is held only while the clip's packet list is built (tens of microseconds
//! for a 30 s clip at 60 fps), and handed back with a fair unlock so a
//! waiting capture or audio push goes next. The blocks stay alive while the
//! clip is written even if the ring evicts them meanwhile.

use blink_ring::media::Timestamp;
use blink_ring::{ClipBuf, ClipError};

use crate::shared::SharedRing;

/// Why no clip could be cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CutError {
    /// No keyframe is stored before the save time: no video has been
    /// captured yet, for example on a screen that has not changed since
    /// recording started.
    #[error("nothing to save yet: no video has been captured yet")]
    NothingYet,
    /// Any other ring error.
    #[error("failed to cut the clip")]
    Ring(#[source] ClipError),
}

/// Cuts the clip that ends at `now` and covers about `duration`.
pub(crate) fn cut(
    ring: &SharedRing,
    now: Timestamp,
    duration: Timestamp,
) -> Result<ClipBuf, CutError> {
    let mut clip = ClipBuf::new();
    ring.read_fair(|slot| slot.ring.snapshot(now, duration, &mut clip))
        .map_err(|error| match error {
            ClipError::NoKeyframe => CutError::NothingYet,
            error => CutError::Ring(error),
        })?;
    Ok(clip)
}

#[cfg(test)]
pub(crate) mod tests {
    use blink_config::Config;
    use blink_ring::media::PacketMeta;

    use super::*;
    use crate::mapping::RingSize;
    use crate::shared::Producer;
    use crate::shared::tests::{MS, audio, audio_format, video, video_format};

    pub(crate) const CLIP_SECS: u32 = 5;

    /// 30 fps video with a keyframe every second, plus 48 kHz AAC audio,
    /// pushed in PTS order like the capture and audio threads do.
    pub(crate) struct Source {
        video: Producer<blink_ring::media::VideoFormat>,
        audio: Producer<blink_ring::media::AudioFormat>,
        next_frame: i64,
        next_audio_ms: i64,
    }

    impl Source {
        pub(crate) fn new() -> Self {
            Self {
                video: Producer::new(video_format(1280)),
                audio: Producer::new(audio_format()),
                next_frame: 0,
                next_audio_ms: 0,
            }
        }

        /// Pushes everything with a PTS before `until_ms`.
        pub(crate) fn run_until(&mut self, ring: &SharedRing, until_ms: i64) {
            loop {
                let video_ms = self.next_frame * 1000 / 30;
                if video_ms.min(self.next_audio_ms) >= until_ms {
                    return;
                }
                if video_ms <= self.next_audio_ms {
                    let keyframe = self.next_frame % 30 == 0;
                    let size = if keyframe { 4000 } else { 1000 };
                    self.push_video(ring, video(video_ms, keyframe), size);
                    self.next_frame += 1;
                } else {
                    let pts = self.next_audio_ms;
                    self.audio
                        .push(ring, audio(pts), &[2; 300])
                        .expect("audio accepted");
                    self.next_audio_ms += 21;
                }
            }
        }

        fn push_video(&mut self, ring: &SharedRing, meta: PacketMeta, size: usize) {
            self.video
                .push(ring, meta, &vec![1; size])
                .expect("video accepted");
        }
    }

    pub(crate) fn config() -> Config {
        Config {
            clip_seconds: CLIP_SECS,
            ..Config::default()
        }
    }

    /// The save time: just before a keyframe, the worst case, since the
    /// clip has to start at the keyframe almost a whole GOP before
    /// `now - CLIP_SECS`.
    pub(crate) const NOW_MS: i64 = 20_900;

    /// Fills `ring` up to a save at [`NOW_MS`], plus the 300 ms of packets
    /// that arrive while the save waits for the encoder to drain, then cuts.
    fn save_after_settle(ring: &SharedRing) -> Result<ClipBuf, CutError> {
        let mut source = Source::new();
        source.run_until(ring, NOW_MS + 300);
        cut(
            ring,
            Timestamp::from_ticks(NOW_MS * MS),
            Timestamp::from_secs(CLIP_SECS.into()),
        )
    }

    #[test]
    fn saving_after_the_settle_gets_the_full_clip() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        let clip = save_after_settle(&ring).unwrap();
        assert!(
            clip.duration() >= Timestamp::from_secs(CLIP_SECS.into()),
            "clip is {:?} long",
            clip.duration()
        );
        assert_eq!(clip.end(), Timestamp::from_ticks(NOW_MS * MS));
        assert!(clip.video().all(|(meta, _)| meta.pts < clip.end()));
        assert!(clip.audio_len() > 0);
    }

    #[test]
    fn a_window_of_exactly_the_clip_length_cuts_the_clip_short() {
        let size = RingSize::for_config(&config());
        let ring = SharedRing::new(RingSize {
            window: Timestamp::from_secs(CLIP_SECS.into()),
            ..size
        });
        let clip = save_after_settle(&ring).unwrap();
        assert!(
            clip.duration() < Timestamp::from_secs(CLIP_SECS.into()),
            "without slack the keyframe before the clip start is already evicted: {:?}",
            clip.duration()
        );
    }

    #[test]
    fn a_clip_survives_the_ring_moving_on_and_replacement() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        let mut source = Source::new();
        let clip = save_after_settle(&ring).unwrap();
        let before: Vec<Vec<u8>> = clip.video().map(|(_, data)| data.to_vec()).collect();
        // Long enough to evict the whole clip, then a settings change.
        source.run_until(&ring, NOW_MS + 30_000);
        ring.replace(RingSize::for_config(&config()));
        let after: Vec<Vec<u8>> = clip.video().map(|(_, data)| data.to_vec()).collect();
        assert_eq!(before, after);
        assert_ne!(before.len(), 0);
    }

    #[test]
    fn an_empty_ring_has_nothing_to_save() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        assert_eq!(
            cut(&ring, Timestamp::from_ticks(MS), Timestamp::from_secs(5)).err(),
            Some(CutError::NothingYet)
        );
    }

    #[test]
    fn errors_explain_themselves() {
        assert!(CutError::NothingYet.to_string().len() > 20);
    }
}
