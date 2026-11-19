//! Cutting a clip out of the shared ring while the producers keep pushing.
//!
//! The buffer is sized and its pages touched before the first lock, so the
//! locked phases only copy bytes. The lock is held for one chunk at a time
//! and handed back with a fair unlock, so a capture or audio push waits at
//! most one chunk (about 100 µs per MB) behind a save.

use blink_ring::media::Timestamp;
use blink_ring::{ClipBuf, ClipError, CopyProgress};

use crate::shared::SharedRing;

/// Bytes copied out of the ring per lock.
pub(crate) const CHUNK_BYTES: usize = 1 << 20;

/// Copies restarted after the ring evicted packets the copy still needed.
const ATTEMPTS: u32 = 3;

/// Headroom over the ring's contents when sizing the clip buffer, for packets
/// with a PTS before the save time that arrive during the copy.
const EXTRA_BYTES: usize = 1 << 20;
const EXTRA_PACKETS: usize = 256;

/// Why no clip could be cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CutError {
    /// No keyframe is stored before the save time.
    #[error("nothing to save yet: no keyframe has been captured before the save time")]
    NothingYet,
    /// The ring kept evicting packets the copy still needed.
    #[error("the clip buffer overwrote the clip faster than it could be copied")]
    Evicted,
    /// A stream's format changed mid-copy: the display mode, an encoder
    /// restart, or the audio device's format.
    #[error("the video or audio format changed while saving, which starts a new recording")]
    FormatChanged,
    /// A settings change replaced the ring mid-copy; its packets are gone.
    #[error("a settings change replaced the clip buffer while saving")]
    RingReplaced,
    /// Any other ring error.
    #[error("failed to copy the clip")]
    Ring(#[source] ClipError),
}

/// Copies the clip that ends at `now` and covers about `duration` into
/// `clip`, retrying when eviction overtakes the copy.
pub(crate) fn cut(
    ring: &SharedRing,
    clip: &mut ClipBuf,
    now: Timestamp,
    duration: Timestamp,
) -> Result<(), CutError> {
    cut_with(ring, clip, now, duration, CHUNK_BYTES, &mut || {})
}

/// [`cut`] with a chunk size, calling `between` each time the lock is
/// released so tests can push while a copy is in flight.
fn cut_with(
    ring: &SharedRing,
    clip: &mut ClipBuf,
    now: Timestamp,
    duration: Timestamp,
    chunk: usize,
    between: &mut dyn FnMut(),
) -> Result<(), CutError> {
    let mut attempt = 1;
    loop {
        match copy(ring, clip, now, duration, chunk, between) {
            Err(CutError::Evicted) if attempt < ATTEMPTS => {
                tracing::debug!(attempt, "ring evicted packets mid-copy; starting over");
                attempt += 1;
            }
            result => return result,
        }
    }
}

fn copy(
    ring: &SharedRing,
    clip: &mut ClipBuf,
    now: Timestamp,
    duration: Timestamp,
    chunk: usize,
    between: &mut dyn FnMut(),
) -> Result<(), CutError> {
    let (bytes, packets, epoch) = {
        let slot = ring.lock();
        (slot.ring.bytes_used(), slot.ring.len(), slot.epoch)
    };
    clip.prepare(
        bytes.saturating_add(EXTRA_BYTES),
        packets.saturating_add(EXTRA_PACKETS),
    );
    let mut cursor = ring.read_fair(|slot| {
        if slot.epoch != epoch {
            return Err(CutError::RingReplaced);
        }
        slot.ring
            .begin_clip(now, duration, clip)
            .map_err(clip_error)
    })?;
    loop {
        between();
        let progress = ring.read_fair(|slot| {
            if slot.epoch != epoch {
                return Err(CutError::RingReplaced);
            }
            slot.ring
                .copy_clip(&mut cursor, clip, chunk)
                .map_err(clip_error)
        })?;
        if progress == CopyProgress::Done {
            return Ok(());
        }
    }
}

const fn clip_error(error: ClipError) -> CutError {
    match error {
        ClipError::NoKeyframe => CutError::NothingYet,
        ClipError::Evicted => CutError::Evicted,
        ClipError::FormatChanged => CutError::FormatChanged,
        error => CutError::Ring(error),
    }
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
    /// that arrive while the save waits for the encoder to drain, then cuts
    /// the clip while the source pushes another 50 ms every time the copy
    /// releases the lock.
    fn save_during_pushes(ring: &SharedRing) -> (Result<(), CutError>, ClipBuf) {
        let mut source = Source::new();
        let mut pushed_until = NOW_MS + 300;
        source.run_until(ring, pushed_until);
        let mut clip = ClipBuf::new();
        let result = cut_with(
            ring,
            &mut clip,
            Timestamp::from_ticks(NOW_MS * MS),
            Timestamp::from_secs(CLIP_SECS.into()),
            8 << 10,
            &mut || {
                pushed_until += 50;
                source.run_until(ring, pushed_until);
            },
        );
        (result, clip)
    }

    #[test]
    fn saving_while_pushes_arrive_gets_the_full_clip() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        let (result, clip) = save_during_pushes(&ring);
        assert_eq!(result, Ok(()));
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
        let (result, clip) = save_during_pushes(&ring);
        assert_eq!(result, Ok(()));
        assert!(
            clip.duration() < Timestamp::from_secs(CLIP_SECS.into()),
            "without slack the keyframe before the clip start is already evicted: {:?}",
            clip.duration()
        );
    }

    #[test]
    fn a_replaced_ring_aborts_the_save() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        let mut source = Source::new();
        source.run_until(&ring, 10_000);
        let mut clip = ClipBuf::new();
        let replacement = RingSize::for_config(&Config {
            clip_seconds: 30,
            ..Config::default()
        });
        let result = cut_with(
            &ring,
            &mut clip,
            Timestamp::from_ticks(10_000 * MS),
            Timestamp::from_secs(CLIP_SECS.into()),
            8 << 10,
            &mut || ring.replace(replacement),
        );
        assert_eq!(result, Err(CutError::RingReplaced));
    }

    #[test]
    fn a_format_change_aborts_the_save() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        let mut source = Source::new();
        source.run_until(&ring, 10_000);
        let mut clip = ClipBuf::new();
        let result = cut_with(
            &ring,
            &mut clip,
            Timestamp::from_ticks(10_000 * MS),
            Timestamp::from_secs(CLIP_SECS.into()),
            8 << 10,
            &mut || source.video.set_format(&ring, video_format(1920)),
        );
        assert_eq!(result, Err(CutError::FormatChanged));
    }

    #[test]
    fn eviction_mid_copy_is_retried() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        let mut source = Source::new();
        source.run_until(&ring, 20_000);
        let mut clip = ClipBuf::new();
        let mut calls = 0;
        let result = cut_with(
            &ring,
            &mut clip,
            Timestamp::from_ticks(20_000 * MS),
            Timestamp::from_secs(CLIP_SECS.into()),
            8 << 10,
            &mut || {
                calls += 1;
                if calls == 1 {
                    // Far enough ahead to evict the start of the clip.
                    source.run_until(&ring, 25_000);
                }
            },
        );
        assert_eq!(result, Ok(()));
        assert!(calls > 2, "the copy started over");
        assert!(clip.duration() > Timestamp::ZERO);
        assert_eq!(clip.end(), Timestamp::from_ticks(20_000 * MS));
    }

    #[test]
    fn an_empty_ring_has_nothing_to_save() {
        let ring = SharedRing::new(RingSize::for_config(&config()));
        let mut clip = ClipBuf::new();
        assert_eq!(
            cut(
                &ring,
                &mut clip,
                Timestamp::from_ticks(MS),
                Timestamp::from_secs(5)
            ),
            Err(CutError::NothingYet)
        );
    }

    #[test]
    fn errors_explain_themselves() {
        for error in [
            CutError::NothingYet,
            CutError::Evicted,
            CutError::FormatChanged,
            CutError::RingReplaced,
        ] {
            assert!(error.to_string().len() > 20);
        }
    }
}
