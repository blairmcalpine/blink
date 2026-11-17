//! The clip ring shared by the capture, audio, and save threads, and the
//! producer side of it: pushes that re-set their stream's format after the
//! ring was replaced.

use blink_ring::media::{AudioFormat, PacketMeta, Timestamp, VideoFormat};
use blink_ring::{ClipRing, PushError, PushOutcome};
use parking_lot::{Mutex, MutexGuard};

use crate::mapping::RingSize;

/// The ring and a counter of how often it has been replaced.
pub(crate) struct RingSlot {
    /// The packets of the last clip length.
    pub(crate) ring: ClipRing,
    /// Bumped every time [`SharedRing::replace`] swaps in a new ring. A fresh
    /// ring has no formats, so each producer compares this with the epoch it
    /// last set its format on, and sets it again when they differ. A save in
    /// progress aborts when it changes under its cursor.
    pub(crate) epoch: u64,
}

/// The clip ring behind one mutex.
///
/// Every lock is short: one packet push, or one bounded chunk of a save. The
/// lock is never held across I/O, an encode, or a thread join. The save
/// thread locks it through [`SharedRing::read_fair`] between chunks, so a
/// producer waiting on it always gets it next.
pub(crate) struct SharedRing {
    slot: Mutex<RingSlot>,
}

impl SharedRing {
    pub(crate) fn new(size: RingSize) -> Self {
        Self {
            slot: Mutex::new(RingSlot {
                ring: size.build(),
                epoch: 0,
            }),
        }
    }

    /// Locks the ring.
    pub(crate) fn lock(&self) -> MutexGuard<'_, RingSlot> {
        self.slot.lock()
    }

    /// Runs `read` with the ring locked, then unlocks fairly: a thread
    /// waiting for the lock gets it before the caller can lock again. For
    /// the save thread's chunked copy, which locks back to back.
    pub(crate) fn read_fair<T>(&self, read: impl FnOnce(&RingSlot) -> T) -> T {
        let slot = self.lock();
        let value = read(&slot);
        MutexGuard::unlock_fair(slot);
        value
    }

    /// Swaps in an empty ring of `size`. The new arena is allocated and the
    /// old one freed outside the lock, so producers wait only for the swap.
    pub(crate) fn replace(&self, size: RingSize) {
        let mut ring = size.build();
        {
            let mut slot = self.lock();
            std::mem::swap(&mut slot.ring, &mut ring);
            slot.epoch = slot.epoch.wrapping_add(1);
        }
        drop(ring);
        tracing::info!(
            window_secs = size.window.ticks() / Timestamp::TICKS_PER_SECOND,
            bytes = size.byte_capacity,
            "clip ring replaced"
        );
    }
}

/// One producer's stream format, set on the ring again whenever the ring
/// epoch moves on. Cloning the format (the only allocation) happens just
/// then.
pub(crate) struct Producer<F> {
    format: F,
    epoch: Option<u64>,
}

impl<F: StreamFormat> Producer<F> {
    /// A producer whose format is set on its first push.
    pub(crate) const fn new(format: F) -> Self {
        Self {
            format,
            epoch: None,
        }
    }

    /// Sets the format on the ring now, as after a new encoder session.
    pub(crate) fn set_format(&mut self, ring: &SharedRing, format: F) {
        let mut slot = ring.lock();
        format.clone().set_on(&mut slot.ring);
        self.epoch = Some(slot.epoch);
        drop(slot);
        self.format = format;
    }

    /// Pushes one packet, first setting the format if the ring is new.
    pub(crate) fn push(
        &mut self,
        ring: &SharedRing,
        meta: PacketMeta,
        payload: &[u8],
    ) -> Result<PushOutcome, PushError> {
        let mut slot = ring.lock();
        if self.epoch != Some(slot.epoch) {
            self.format.clone().set_on(&mut slot.ring);
            self.epoch = Some(slot.epoch);
        }
        slot.ring.push(meta, payload)
    }
}

/// A stream format that can be set on the ring.
pub(crate) trait StreamFormat: Clone {
    /// Sets this format on `ring`.
    fn set_on(self, ring: &mut ClipRing);
}

impl StreamFormat for VideoFormat {
    fn set_on(self, ring: &mut ClipRing) {
        ring.set_video_format(self);
    }
}

impl StreamFormat for AudioFormat {
    fn set_on(self, ring: &mut ClipRing) {
        ring.set_audio_format(self);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use blink_ring::media::StreamKind;

    use super::*;

    pub(crate) const MS: i64 = 10_000;

    pub(crate) fn video_format(width: u32) -> VideoFormat {
        VideoFormat {
            width,
            height: 720,
            fps: 30,
            sps: vec![0x67, 0x42, 0xc0, 0x1f],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
            sample_aspect: None,
        }
    }

    pub(crate) fn audio_format() -> AudioFormat {
        AudioFormat {
            sample_rate: 48_000,
            channels: 2,
            audio_specific_config: vec![0x11, 0x90],
        }
    }

    pub(crate) fn video(pts_ms: i64, keyframe: bool) -> PacketMeta {
        PacketMeta {
            kind: StreamKind::Video,
            pts: Timestamp(pts_ms * MS),
            duration: Timestamp(33 * MS),
            keyframe,
        }
    }

    pub(crate) fn audio(pts_ms: i64) -> PacketMeta {
        PacketMeta {
            kind: StreamKind::Audio,
            pts: Timestamp(pts_ms * MS),
            duration: Timestamp(21 * MS),
            keyframe: false,
        }
    }

    fn small_ring() -> SharedRing {
        SharedRing::new(RingSize {
            window: Timestamp::from_secs(5),
            byte_capacity: 1 << 16,
        })
    }

    #[test]
    fn producers_set_their_format_again_after_a_replace() {
        let ring = Arc::new(small_ring());
        let mut video_out = Producer::new(video_format(1280));
        let mut audio_out = Producer::new(audio_format());
        assert_eq!(
            video_out.push(&ring, video(0, true), &[1; 10]),
            Ok(PushOutcome::Stored)
        );
        assert_eq!(
            audio_out.push(&ring, audio(0), &[2; 10]),
            Ok(PushOutcome::Stored)
        );

        ring.replace(RingSize {
            window: Timestamp::from_secs(6),
            byte_capacity: 1 << 16,
        });
        assert!(ring.lock().ring.video_format().is_none());

        assert_eq!(
            video_out.push(&ring, video(33, true), &[1; 10]),
            Ok(PushOutcome::Stored)
        );
        assert_eq!(
            audio_out.push(&ring, audio(42), &[2; 10]),
            Ok(PushOutcome::Stored)
        );
        let slot = ring.lock();
        assert_eq!(slot.epoch, 1);
        assert_eq!(slot.ring.len(), 2);
        assert_eq!(slot.ring.video_format(), Some(&video_format(1280)));
        assert_eq!(slot.ring.audio_format(), Some(&audio_format()));
    }

    #[test]
    fn set_format_switches_the_stream() {
        let ring = small_ring();
        let mut video_out = Producer::new(video_format(1280));
        video_out.set_format(&ring, video_format(1920));
        assert_eq!(ring.lock().ring.video_format(), Some(&video_format(1920)));
        assert_eq!(
            video_out.push(&ring, video(0, true), &[1; 10]),
            Ok(PushOutcome::Stored)
        );
        assert_eq!(ring.lock().ring.len(), 1);
    }
}
