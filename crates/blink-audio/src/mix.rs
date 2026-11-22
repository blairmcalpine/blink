//! Mixing the microphone into the desktop audio.
//!
//! The desktop (loopback) and microphone endpoints run on their own device
//! clocks and deliver packets at their own pace. Each source gets a
//! [`Timeline`], so the same gap fill and drift slewing keep both on the
//! shared QPC timeline. The microphone's is built with [`Timeline::aligned`]
//! on the desktop's chunk grid, so chunk `n` of each carries the same
//! timestamp, and [`Mix`] pairs them by timestamp.
//!
//! Pairing by timestamp is not sample-exact sync. Each timeline lets its
//! source drift up to [`DRIFT_TOLERANCE`] from QPC before slewing it back
//! (and up to [`HARD_LIMIT`], briefly, before a lost-data correction), so the
//! two sources can sit up to about twice that apart.
//!
//! - Desktop chunks wait in a fixed queue of [`QUEUE_CHUNKS`] for the
//!   microphone chunk with their timestamp, are mixed with it, and go to the
//!   encoder. Nothing blocks: a waiting chunk stays queued until a later
//!   iteration of the audio loop.
//! - A desktop chunk goes to the encoder unmixed, without waiting, while no
//!   microphone is open or once the microphone timeline has moved past it
//!   (its chunk was lost). For a late microphone it waits until [`MIC_WAIT`]
//!   past its end, checked once per loop iteration (at most 20 ms apart).
//! - Microphone chunks older than the oldest waiting desktop chunk are
//!   dropped.
//! - Whenever the two timelines stop sharing an anchor (the desktop started
//!   a new segment after a long gap), the microphone's is rebuilt on the
//!   desktop's grid, from the oldest desktop chunk still waiting.
//! - A microphone timestamp the desktop timeline could not have reached yet,
//!   or one so far back that the microphone timeline would start a segment
//!   of its own, is bogus; the packet is placed as if contiguous instead.
//!
//! Both sources are summed at unity gain and the sum passes through a soft
//! clipper with its knee at -1 dBFS, so a loud voice over loud game audio
//! saturates smoothly instead of hard clipping. A desktop chunk without a
//! microphone chunk, or paired with a silent one, is encoded exactly as
//! captured. Nothing here allocates after [`Mix::new`].

use std::convert::Infallible;

use blink_ring::media::Timestamp;

use crate::PACKET_DURATION;
use crate::pcm;
use crate::timeline::{CHUNK_SAMPLES, Chunk, Correction, Event, MAX_GAP, Packet, Timeline};
#[cfg(doc)]
use crate::timeline::{DRIFT_TOLERANCE, HARD_LIMIT};

/// Chunks each queue holds: 40, about 853 ms, allocated once. After the audio
/// thread stalls, one drain can push a whole 500 ms WASAPI buffer of desktop
/// chunks before the microphone's are pushed; the queue holds those plus
/// [`MIC_WAIT`] and a loop iteration, so they still pair. A stall longer than
/// the WASAPI buffer loses data in WASAPI anyway.
pub(crate) const QUEUE_CHUNKS: usize = 40;

/// Longest a desktop chunk waits past its end for its microphone chunk:
/// 200 ms. Ordinary microphones deliver within a period or two (10 to
/// 20 ms), so the wait only runs out for a stalled or very slow device.
pub(crate) const MIC_WAIT: i64 = Timestamp::from_millis(200).ticks();

/// How far behind the wall clock the microphone timeline fills silence:
/// 150 ms. A capture endpoint delivers continuously, so fill only covers a
/// stall; the longer lag than loopback's keeps fill from overtaking (and so
/// trimming) the packets of a high-latency device such as a Bluetooth
/// headset. Shorter than [`MIC_WAIT`], so a stalled microphone's silence
/// still pairs with the desktop chunks before they stop waiting.
pub(crate) const MIC_FILL_LAG: i64 = Timestamp::from_millis(150).ticks();

/// Farthest a microphone timestamp may lie ahead of the desktop timeline's
/// next frame: 200 ms. The desktop is drained first, and its fill trails the
/// clock by its fill lag plus a loop iteration, so a real microphone packet
/// is never this far ahead; a timestamp that is would otherwise insert
/// silence up to it and then trim every real packet until the clock caught
/// up.
pub(crate) const MIC_MAX_LEAD: i64 = Timestamp::from_millis(200).ticks();

/// Linear gain of the desktop audio in the mix.
const DESKTOP_GAIN: f32 = 1.0;

/// Linear gain of the microphone in the mix.
const MIC_GAIN: f32 = 1.0;

/// Magnitude above which the soft clipper bends the signal: -1 dBFS.
const KNEE: f32 = 0.891;

/// Maps a mixed sample (nominal range `[-1.0, 1.0]`, but a sum can exceed
/// it) into `(-1.0, 1.0)`. The identity up to [`KNEE`]; above it a `tanh`
/// curve whose slope starts at 1, so the transition has no corner.
pub(crate) fn soft_clip(sample: f32) -> f32 {
    let magnitude = sample.abs();
    if magnitude <= KNEE {
        return sample;
    }
    let headroom = 1.0 - KNEE;
    let over = (magnitude - KNEE) / headroom;
    headroom.mul_add(over.tanh(), KNEE).copysign(sample)
}

/// Mixes one desktop and one microphone sample.
pub(crate) fn mix_sample(desktop: i16, mic: i16) -> i16 {
    let sum = f32::from(mic).mul_add(MIC_GAIN, f32::from(desktop) * DESKTOP_GAIN);
    pcm::f32_to_s16(soft_clip(sum / 32_768.0))
}

/// Chunk counters since the last [`Mix::take_stats`], for logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MixStats {
    /// Desktop chunks mixed with a microphone chunk.
    pub(crate) mixed: u64,
    /// Desktop chunks encoded without one.
    pub(crate) desktop_only: u64,
    /// Microphone chunks dropped: stale, or the queue was full.
    pub(crate) mic_dropped: u64,
}

/// The desktop and microphone timelines and the mixer that pairs their
/// chunks. See the module documentation.
pub(crate) struct Mix {
    desktop: Timeline,
    /// `None` while no microphone stream is open.
    mic: Option<Timeline>,
    mixer: Mixer,
}

impl Mix {
    /// Starts with the desktop timeline anchored at `anchor` and no
    /// microphone. Allocates the queues; nothing allocates afterwards.
    pub(crate) fn new(anchor: i64) -> Self {
        Self {
            desktop: Timeline::new(anchor),
            mic: None,
            mixer: Mixer::new(),
        }
    }

    /// Frames written to the desktop timeline's current segment.
    pub(crate) fn desktop_frames_written(&self) -> u64 {
        self.desktop.frames_written()
    }

    /// Returns and resets the chunk counters.
    pub(crate) fn take_stats(&mut self) -> MixStats {
        std::mem::take(&mut self.mixer.stats)
    }

    /// Starts mixing a newly opened microphone stream, on the desktop grid
    /// from the oldest desktop chunk still waiting (or the one the desktop is
    /// filling).
    pub(crate) fn open_mic(&mut self) {
        let behind = u64::try_from(self.mixer.held.len()).unwrap_or(0);
        self.mic = Some(self.desktop.aligned(MIC_FILL_LAG, behind));
        self.mixer.mic.clear();
    }

    /// Stops mixing: the microphone stream closed. Waiting desktop chunks go
    /// out unmixed on the next [`Self::tick`].
    pub(crate) const fn close_mic(&mut self) {
        self.mic = None;
        self.mixer.mic.clear();
    }

    /// Appends a desktop packet; see [`Timeline::push_packet`]. Chunks it
    /// completes reach `out` once paired, or at once if the queue is full.
    pub(crate) fn push_desktop<E>(
        &mut self,
        packet: Packet<'_>,
        timestamp: Option<i64>,
        discontinuity: bool,
        out: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<Correction, E> {
        let Self { desktop, mixer, .. } = self;
        let correction = desktop.push_packet(packet, timestamp, discontinuity, &mut |event| {
            mixer.desktop(event, out)
        })?;
        self.realign();
        Ok(correction)
    }

    /// Appends a microphone packet; see [`Timeline::push_packet`]. A bogus
    /// timestamp (see the module documentation) is treated as invalid.
    /// Returns `None` (and drops the packet) while no microphone is open.
    pub(crate) fn push_mic(
        &mut self,
        packet: Packet<'_>,
        timestamp: Option<i64>,
        discontinuity: bool,
    ) -> Option<Correction> {
        self.realign();
        let Self {
            desktop,
            mic,
            mixer,
        } = self;
        let mic = mic.as_mut()?;
        let timestamp = timestamp.filter(|&timestamp| {
            timestamp.saturating_sub(desktop.expected()) <= MIC_MAX_LEAD
                && mic.expected().saturating_sub(timestamp) <= MAX_GAP
        });
        let Ok(correction) = mic.push_packet(packet, timestamp, discontinuity, &mut |event| {
            mixer.mic(event);
            Ok::<(), Infallible>(())
        });
        Some(correction)
    }

    /// Fills silence on both timelines up to `now` (see
    /// [`Timeline::fill_silence`]), then sends every desktop chunk that is
    /// done waiting to `out`. Call once per loop iteration, after draining.
    /// Returns the desktop's and, if open, the microphone's fill.
    pub(crate) fn tick<E>(
        &mut self,
        now: i64,
        out: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(Correction, Option<Correction>), E> {
        let Self { desktop, mixer, .. } = self;
        let desktop_fill = desktop.fill_silence(now, &mut |event| mixer.desktop(event, out))?;
        // A desktop reanchor in that fill moves the microphone to the new
        // grid before it fills, not after a fill on the old one.
        self.realign();
        let Self { mic, mixer, .. } = self;
        let mic_fill = mic.as_mut().map(|mic| {
            let Ok(fill) = mic.fill_silence(now, &mut |event| {
                mixer.mic(event);
                Ok::<(), Infallible>(())
            });
            fill
        });
        self.realign();
        let mic_next = self.mic.as_ref().map(Timeline::next_chunk_pts);
        self.mixer.pump(now, mic_next, out)?;
        Ok((desktop_fill, mic_fill))
    }

    /// Pads and emits both partial chunks, flushes every waiting chunk, and
    /// ends the segment. Used on shutdown.
    pub(crate) fn finish<E>(
        &mut self,
        out: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        let Self {
            desktop,
            mic,
            mixer,
        } = self;
        if let Some(mic) = mic {
            let Ok(()) = mic.finish(&mut |event| {
                mixer.mic(event);
                Ok::<(), Infallible>(())
            });
        }
        desktop.finish(&mut |event| mixer.desktop(event, out))
    }

    /// Rebuilds the microphone timeline on the desktop grid if their anchors
    /// differ.
    fn realign(&mut self) {
        let anchor = self.desktop.anchor();
        if self.mic.as_ref().is_some_and(|mic| mic.anchor() != anchor) {
            self.open_mic();
        }
    }
}

/// One queued chunk.
#[derive(Clone, Copy)]
struct Slot {
    pts: Timestamp,
    pcm: [i16; CHUNK_SAMPLES],
}

/// A fixed ring of chunks, oldest first.
struct ChunkQueue {
    /// `QUEUE_CHUNKS` slots, on the heap: a 64 KiB array built on the stack
    /// first would be a large stack copy.
    slots: Box<[Slot]>,
    head: usize,
    len: usize,
}

impl ChunkQueue {
    fn new() -> Self {
        let empty = Slot {
            pts: Timestamp::ZERO,
            pcm: [0; CHUNK_SAMPLES],
        };
        Self {
            slots: vec![empty; QUEUE_CHUNKS].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    fn front(&self) -> Option<&Slot> {
        if self.len == 0 {
            return None;
        }
        self.slots.get(self.head)
    }

    const fn len(&self) -> usize {
        self.len
    }

    const fn is_full(&self) -> bool {
        self.len >= QUEUE_CHUNKS
    }

    const fn pop(&mut self) {
        if self.len > 0 {
            self.head = (self.head + 1) % QUEUE_CHUNKS;
            self.len -= 1;
        }
    }

    /// Appends a copy of `chunk`. The caller makes room first; a push to a
    /// full queue is ignored.
    fn push(&mut self, chunk: Chunk<'_>) {
        if self.is_full() {
            return;
        }
        let index = (self.head + self.len) % QUEUE_CHUNKS;
        if let Some(slot) = self.slots.get_mut(index) {
            slot.pts = chunk.pts;
            slot.pcm = *chunk.pcm;
            self.len += 1;
        }
    }

    const fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// Pairs desktop chunks with microphone chunks.
struct Mixer {
    /// Desktop chunks waiting for their microphone chunk.
    held: ChunkQueue,
    mic: ChunkQueue,
    /// The mixed chunk being emitted.
    mixed: Box<[i16; CHUNK_SAMPLES]>,
    stats: MixStats,
}

impl Mixer {
    fn new() -> Self {
        Self {
            held: ChunkQueue::new(),
            mic: ChunkQueue::new(),
            mixed: Box::new([0; CHUNK_SAMPLES]),
            stats: MixStats::default(),
        }
    }

    /// Queues a desktop chunk, emitting the oldest first if the queue is
    /// full. A segment end flushes the queue, then is passed on.
    fn desktop<E>(
        &mut self,
        event: Event<'_>,
        out: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        match event {
            Event::Chunk(chunk) => {
                if self.held.is_full() {
                    self.emit_front(out)?;
                }
                self.held.push(chunk);
                Ok(())
            }
            Event::SegmentEnd => {
                while self.held.front().is_some() {
                    self.emit_front(out)?;
                }
                self.mic.clear();
                out(Event::SegmentEnd)
            }
        }
    }

    /// Queues a microphone chunk, dropping the oldest if the queue is full.
    /// The microphone's own segment ends need no action: the next
    /// [`Mix::realign`] rebuilds its timeline.
    fn mic(&mut self, event: Event<'_>) {
        if let Event::Chunk(chunk) = event {
            if self.mic.is_full() {
                self.mic.pop();
                self.stats.mic_dropped += 1;
            }
            self.mic.push(chunk);
        }
    }

    /// Emits every waiting desktop chunk that is done waiting: there is no
    /// microphone (`mic_next` is `None`), its microphone chunk is queued, the
    /// microphone timeline is past it, or it is [`MIC_WAIT`] past its end at
    /// `now`. `mic_next` is the timestamp of the chunk the microphone
    /// timeline is filling.
    fn pump<E>(
        &mut self,
        now: i64,
        mic_next: Option<Timestamp>,
        out: &mut impl FnMut(Event<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        while let Some(pts) = self.held.front().map(|slot| slot.pts) {
            self.drop_stale_mic(pts);
            let ready = mic_next.is_none_or(|next| {
                let deadline = pts
                    .ticks()
                    .saturating_add(PACKET_DURATION.ticks())
                    .saturating_add(MIC_WAIT);
                self.mic.front().is_some() || next > pts || now >= deadline
            });
            if !ready {
                break;
            }
            self.emit_front(out)?;
        }
        Ok(())
    }

    /// Emits the oldest desktop chunk, mixed if its microphone chunk is
    /// queued and not silent.
    fn emit_front<E>(&mut self, out: &mut impl FnMut(Event<'_>) -> Result<(), E>) -> Result<(), E> {
        let Some(pts) = self.held.front().map(|slot| slot.pts) else {
            return Ok(());
        };
        self.drop_stale_mic(pts);
        let paired = self.mic.front().is_some_and(|mic| mic.pts == pts);
        let result = match (self.held.front(), self.mic.front()) {
            (Some(desktop), Some(mic)) if paired => {
                self.stats.mixed += 1;
                // Silence adds nothing, and skipping the clipper keeps the
                // desktop audio bit-exact while nobody talks.
                if mic.pcm.iter().all(|&sample| sample == 0) {
                    out(Event::Chunk(Chunk {
                        pcm: &desktop.pcm,
                        pts,
                    }))
                } else {
                    let pairs = desktop.pcm.iter().zip(&mic.pcm);
                    for (sample, (&desktop, &mic)) in self.mixed.iter_mut().zip(pairs) {
                        *sample = mix_sample(desktop, mic);
                    }
                    out(Event::Chunk(Chunk {
                        pcm: &self.mixed,
                        pts,
                    }))
                }
            }
            (Some(desktop), _) => {
                self.stats.desktop_only += 1;
                out(Event::Chunk(Chunk {
                    pcm: &desktop.pcm,
                    pts,
                }))
            }
            (None, _) => Ok(()),
        };
        if paired {
            self.mic.pop();
        }
        self.held.pop();
        result
    }

    /// Drops microphone chunks older than `pts`: their desktop chunk is gone.
    fn drop_stale_mic(&mut self, pts: Timestamp) {
        while self.mic.front().is_some_and(|mic| mic.pts < pts) {
            self.mic.pop();
            self.stats.mic_dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm::SampleFormat;
    use crate::timeline::{FILL_LAG, frames_to_ticks};

    const ANCHOR: i64 = 1_000_000_000;
    const CHUNK: i64 = 213_333;

    /// What the encoder saw.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Seen {
        Chunk { pts: i64, pcm: Vec<i16> },
        End,
    }

    #[derive(Default)]
    struct Encoder(Vec<Seen>);

    impl Encoder {
        fn sink(&mut self) -> impl FnMut(Event<'_>) -> Result<(), ()> + '_ {
            |event: Event<'_>| {
                self.0.push(match event {
                    Event::Chunk(chunk) => Seen::Chunk {
                        pts: chunk.pts.ticks(),
                        pcm: chunk.pcm.to_vec(),
                    },
                    Event::SegmentEnd => Seen::End,
                });
                Ok(())
            }
        }

        fn chunks(&self) -> Vec<(i64, i16)> {
            self.0
                .iter()
                .filter_map(|seen| match seen {
                    Seen::Chunk { pts, pcm } => Some((*pts, pcm[0])),
                    Seen::End => None,
                })
                .collect()
        }
    }

    fn s16(frames: usize, value: i16) -> Vec<u8> {
        std::iter::repeat_n(value.to_le_bytes(), frames * 2)
            .flatten()
            .collect()
    }

    fn pcm(data: &[u8]) -> Packet<'_> {
        Packet::Pcm {
            format: SampleFormat::S16,
            data,
        }
    }

    fn chunk_pts(k: u64) -> i64 {
        ANCHOR + frames_to_ticks(k * 1024)
    }

    #[test]
    fn soft_clip_is_transparent_below_the_knee_and_bounded_above() {
        for sample in [0.0, 0.25, -0.5, KNEE, -KNEE] {
            assert!((soft_clip(sample) - sample).abs() < f32::EPSILON);
        }
        let mut previous = 0.0;
        for step in 1_u16..=400 {
            let sample = f32::from(step) / 100.0;
            let clipped = soft_clip(sample);
            assert!(clipped >= previous, "monotonic at {sample}");
            assert!(clipped <= 1.0, "{sample} -> {clipped}");
            assert!((soft_clip(-sample) + clipped).abs() < f32::EPSILON, "odd");
            previous = clipped;
        }
        assert!(soft_clip(1.0) < 1.0);
        assert!(soft_clip(1.2) > soft_clip(1.0));
        // No corner at the knee: just above it the slope is still about 1.
        let delta = 1e-3;
        let slope = (soft_clip(KNEE + delta) - soft_clip(KNEE)) / delta;
        assert!((slope - 1.0).abs() < 0.02, "slope {slope}");
        assert!(soft_clip(f32::NAN).is_nan());
    }

    #[test]
    fn mixing_sums_quiet_signals_and_saturates_loud_ones_smoothly() {
        assert_eq!(mix_sample(1_000, 2_000), 3_000);
        assert_eq!(mix_sample(-1_000, 500), -500);
        assert_eq!(mix_sample(0, 0), 0);
        let loud = mix_sample(i16::MAX, i16::MAX);
        assert!(loud > 32_000, "{loud}");
        assert!(mix_sample(i16::MIN, i16::MIN) < -32_000);
        assert!(mix_sample(30_000, 10_000) < loud);
        assert!(mix_sample(30_000, 10_000) > mix_sample(30_000, 0));
    }

    #[test]
    fn without_a_microphone_desktop_chunks_pass_unchanged_at_once() {
        let mut mix = Mix::new(ANCHOR);
        let mut enc = Encoder::default();
        let data = s16(1024, i16::MAX);
        mix.push_desktop(pcm(&data), Some(ANCHOR), false, &mut enc.sink())
            .unwrap();
        mix.tick(ANCHOR, &mut enc.sink()).unwrap();
        assert_eq!(
            enc.0,
            [Seen::Chunk {
                pts: ANCHOR,
                pcm: vec![i16::MAX; CHUNK_SAMPLES]
            }]
        );
        assert_eq!(mix.take_stats().desktop_only, 1);
    }

    #[test]
    fn aligned_chunks_are_mixed() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let desktop = s16(2048, 1_000);
        let mic = s16(2048, 300);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        // Desktop chunks wait for the microphone.
        mix.tick(ANCHOR, &mut enc.sink()).unwrap();
        assert_eq!(enc.0, Vec::<Seen>::new());
        mix.push_mic(pcm(&mic), Some(ANCHOR), true);
        mix.tick(ANCHOR, &mut enc.sink()).unwrap();
        assert_eq!(enc.chunks(), [(chunk_pts(0), 1_300), (chunk_pts(1), 1_300)]);
        assert_eq!(
            mix.take_stats(),
            MixStats {
                mixed: 2,
                desktop_only: 0,
                mic_dropped: 0
            }
        );
    }

    #[test]
    fn a_microphone_that_never_delivers_releases_desktop_at_the_deadline() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let desktop = s16(1024, 7);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        // The microphone timeline is still filling chunk 0 and nothing is
        // queued: wait until the deadline, not past it.
        let deadline = ANCHOR + CHUNK + MIC_WAIT;
        let Mix { mixer, .. } = &mut mix;
        mixer
            .pump(
                deadline - 1,
                Some(Timestamp::from_ticks(ANCHOR)),
                &mut enc.sink(),
            )
            .unwrap();
        assert_eq!(enc.0, Vec::<Seen>::new());
        mixer
            .pump(
                deadline,
                Some(Timestamp::from_ticks(ANCHOR)),
                &mut enc.sink(),
            )
            .unwrap();
        assert_eq!(enc.chunks(), [(ANCHOR, 7)]);
        assert_eq!(mix.take_stats().desktop_only, 1);
    }

    #[test]
    fn a_stalled_microphone_pairs_its_fill_before_the_wait_runs_out() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let desktop = s16(1024, 7);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        let mut now = ANCHOR;
        while enc.0.is_empty() {
            now += 100_000;
            mix.tick(now, &mut enc.sink()).unwrap();
        }
        assert!(now < ANCHOR + CHUNK + MIC_WAIT, "released at {now}");
        assert!(now >= ANCHOR + CHUNK + MIC_FILL_LAG);
        assert_eq!(enc.chunks(), [(ANCHOR, 7)]);
        assert_eq!(mix.take_stats().mixed, 1);
    }

    #[test]
    fn a_chunk_the_microphone_timeline_is_past_does_not_wait() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let desktop = s16(1024, 5);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        // The microphone is filling chunk 1, so chunk 0 never gets a partner.
        let Mix { mixer, .. } = &mut mix;
        mixer
            .pump(
                ANCHOR,
                Some(Timestamp::from_ticks(chunk_pts(1))),
                &mut enc.sink(),
            )
            .unwrap();
        assert_eq!(enc.chunks(), [(chunk_pts(0), 5)]);
    }

    #[test]
    fn a_microphone_opened_with_desktop_chunks_waiting_mixes_them() {
        let mut mix = Mix::new(ANCHOR);
        let mut enc = Encoder::default();
        let desktop = s16(1024, 1_000);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        mix.tick(ANCHOR, &mut enc.sink()).unwrap();
        let two = s16(2048, 1_000);
        mix.push_desktop(pcm(&two), Some(chunk_pts(1)), false, &mut enc.sink())
            .unwrap();
        // Chunks 1 and 2 wait for the next tick; the microphone starts there.
        mix.open_mic();
        assert_eq!(
            mix.mic.as_ref().map(|mic| mic.next_chunk_pts().ticks()),
            Some(chunk_pts(1))
        );
        let mic = s16(2048, 300);
        mix.push_mic(pcm(&mic), Some(chunk_pts(1)), true);
        mix.tick(chunk_pts(3), &mut enc.sink()).unwrap();
        assert_eq!(
            enc.chunks(),
            [
                (chunk_pts(0), 1_000),
                (chunk_pts(1), 1_300),
                (chunk_pts(2), 1_300)
            ]
        );
    }

    #[test]
    fn a_late_microphone_is_waited_for_and_lines_up_by_timestamp() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        // A click at frame 1500 on both sources, captured at the same QPC
        // time. Desktop packets are 480 frames; the microphone delivers 441
        // frame packets 80 ms late.
        let click_frame = 1_500_u64;
        let mut desktop_frame = 0_u64;
        let mut mic_frame = 0_u64;
        let mut now = ANCHOR;
        let end = 4_096_u64;
        while desktop_frame < end || mic_frame < end {
            now += 100_000;
            while desktop_frame < end && ANCHOR + frames_to_ticks(desktop_frame + 480) <= now {
                let data = packet_with_click(480, desktop_frame, click_frame, 8_000);
                let ts = ANCHOR + frames_to_ticks(desktop_frame);
                mix.push_desktop(pcm(&data), Some(ts), desktop_frame == 0, &mut enc.sink())
                    .unwrap();
                desktop_frame += 480;
            }
            while mic_frame < end && ANCHOR + frames_to_ticks(mic_frame + 441) + 800_000 <= now {
                let data = packet_with_click(441, mic_frame, click_frame, 4_000);
                let ts = ANCHOR + frames_to_ticks(mic_frame);
                mix.push_mic(pcm(&data), Some(ts), mic_frame == 0);
                mic_frame += 441;
            }
            mix.tick(now, &mut enc.sink()).unwrap();
        }
        let stats = mix.take_stats();
        assert_eq!(stats.desktop_only, 0, "{stats:?}");
        let samples: Vec<i16> = enc
            .0
            .iter()
            .flat_map(|seen| match seen {
                Seen::Chunk { pcm, .. } => pcm.clone(),
                Seen::End => Vec::new(),
            })
            .collect();
        let click = usize::try_from(click_frame).unwrap() * 2;
        assert_eq!(samples[click], 12_000);
        assert_eq!(samples[click + 1], 12_000);
        assert_eq!(samples[click - 2], 0);
        assert_eq!(samples[click + 2], 0);
    }

    /// `frames` stereo frames starting at `first`, silent except `value` at
    /// frame `click`.
    fn packet_with_click(frames: u64, first: u64, click: u64, value: i16) -> Vec<u8> {
        (first..first + frames)
            .flat_map(|frame| {
                let sample = if frame == click { value } else { 0 };
                [sample.to_le_bytes(), sample.to_le_bytes()]
            })
            .flatten()
            .collect()
    }

    /// Drives both sources for `seconds` with the microphone's device clock
    /// `ppm` off QPC and returns the stats.
    fn run_with_mic_drift(ppm: i64, seconds: u64) -> MixStats {
        let mut mix = Mix::new(0);
        mix.open_mic();
        let mut sink = |_: Event<'_>| Ok::<(), ()>(());
        let desktop = s16(480, 1);
        let mic = s16(480, 1);
        let mut stats = MixStats::default();
        for i in 0..seconds * 100 {
            let nominal = frames_to_ticks(i * 480);
            mix.push_desktop(pcm(&desktop), Some(nominal), false, &mut sink)
                .unwrap();
            let drifted = nominal + nominal * ppm / 1_000_000;
            mix.push_mic(pcm(&mic), Some(drifted), false);
            mix.tick(nominal + frames_to_ticks(480), &mut sink).unwrap();
            let round = mix.take_stats();
            stats.mixed += round.mixed;
            stats.desktop_only += round.desktop_only;
            stats.mic_dropped += round.mic_dropped;
        }
        stats
    }

    #[test]
    fn microphone_clock_drift_is_slewed_and_every_chunk_stays_mixed() {
        for ppm in [-300, 300] {
            let stats = run_with_mic_drift(ppm, 300);
            assert_eq!(stats.desktop_only, 0, "{ppm} ppm: {stats:?}");
            assert!(stats.mixed > 14_000, "{ppm} ppm: {stats:?}");
            assert_eq!(stats.mic_dropped, 0, "{ppm} ppm: {stats:?}");
        }
    }

    #[test]
    fn closing_the_microphone_releases_waiting_chunks() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let desktop = s16(2048, 4);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        mix.tick(ANCHOR, &mut enc.sink()).unwrap();
        assert_eq!(enc.0, Vec::<Seen>::new());
        mix.close_mic();
        assert!(mix.mic.is_none());
        assert_eq!(mix.push_mic(pcm(&desktop), Some(ANCHOR), true), None);
        mix.tick(ANCHOR, &mut enc.sink()).unwrap();
        assert_eq!(enc.chunks(), [(chunk_pts(0), 4), (chunk_pts(1), 4)]);
    }

    #[test]
    fn a_full_queue_emits_its_oldest_chunk_in_order() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let frames = 1024 * (QUEUE_CHUNKS + 3);
        let desktop = s16(frames, 9);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        let pts: Vec<i64> = enc.chunks().iter().map(|(pts, _)| *pts).collect();
        assert_eq!(pts, [0, 1, 2].map(chunk_pts));
    }

    #[test]
    fn a_desktop_segment_end_flushes_and_realigns_the_microphone() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let data = s16(1024, 3);
        mix.push_desktop(pcm(&data), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        // A system sleep: the desktop resumes more than MAX_GAP later.
        let later = ANCHOR + crate::timeline::MAX_GAP + 10_000_000;
        mix.push_desktop(pcm(&data), Some(later), true, &mut enc.sink())
            .unwrap();
        assert_eq!(enc.chunks(), [(ANCHOR, 3)]);
        assert_eq!(enc.0.get(1), Some(&Seen::End));
        // The microphone now follows the new segment from its first chunk,
        // which is still waiting.
        let next = later + frames_to_ticks(1024);
        assert_eq!(
            mix.mic.as_ref().map(|mic| mic.next_chunk_pts().ticks()),
            Some(later)
        );
        mix.push_desktop(pcm(&data), Some(next), false, &mut enc.sink())
            .unwrap();
        mix.push_mic(pcm(&data), Some(next), true);
        mix.tick(later, &mut enc.sink()).unwrap();
        assert_eq!(enc.chunks(), [(ANCHOR, 3), (later, 3), (next, 6)]);
        let stats = mix.take_stats();
        assert_eq!((stats.desktop_only, stats.mixed), (1, 2), "{stats:?}");
    }

    #[test]
    fn finish_flushes_waiting_chunks_before_the_segment_end() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let data = s16(1500, 2);
        mix.push_desktop(pcm(&data), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        assert_eq!(mix.desktop_frames_written(), 1500);
        mix.finish(&mut enc.sink()).unwrap();
        assert_eq!(enc.chunks(), [(chunk_pts(0), 2), (chunk_pts(1), 2)]);
        assert_eq!(enc.0.last(), Some(&Seen::End));
    }

    #[test]
    fn finish_mixes_the_microphone_partial_chunk() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let desktop = s16(1500, 2);
        let mic = s16(1500, 1);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        mix.push_mic(pcm(&mic), Some(ANCHOR), true);
        mix.finish(&mut enc.sink()).unwrap();
        assert_eq!(enc.chunks(), [(chunk_pts(0), 3), (chunk_pts(1), 3)]);
        assert_eq!(mix.take_stats().mixed, 2);
    }

    #[test]
    fn a_silent_microphone_leaves_desktop_audio_bit_exact() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        // Loud enough that the clipper would bend it.
        let desktop = s16(1024, 32_000);
        let mic = s16(1024, 0);
        mix.push_desktop(pcm(&desktop), Some(ANCHOR), true, &mut enc.sink())
            .unwrap();
        mix.push_mic(pcm(&mic), Some(ANCHOR), true);
        mix.tick(ANCHOR, &mut enc.sink()).unwrap();
        assert_eq!(enc.chunks(), [(ANCHOR, 32_000)]);
        assert_eq!(mix.take_stats().mixed, 1);
        assert_ne!(mix_sample(32_000, 0), 32_000, "the clipper would bend it");
    }

    /// Ticks of `frames` frames after [`ANCHOR`].
    fn at(frames: u64) -> i64 {
        ANCHOR + frames_to_ticks(frames)
    }

    /// One 10 ms audio-loop iteration at desktop frame `frame`: a 480-frame
    /// desktop packet, a 480-frame microphone packet with timestamp
    /// `mic_ts` (value 1 each), and a tick at the packet's end.
    fn step(mix: &mut Mix, enc: &mut Encoder, frame: u64, mic_ts: Option<i64>) {
        let data = s16(480, 1);
        mix.push_desktop(pcm(&data), Some(at(frame)), frame == 0, &mut enc.sink())
            .unwrap();
        if let Some(ts) = mic_ts {
            mix.push_mic(pcm(&data), Some(ts), false);
        }
        mix.tick(at(frame + 480), &mut enc.sink()).unwrap();
    }

    #[test]
    fn a_bulk_drain_after_a_stall_still_pairs_every_chunk() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        // 25 chunks (533 ms) of each source arrive in one iteration, the
        // desktop's all drained before the microphone's.
        let desktop = s16(1024, 1_000);
        let mic = s16(1024, 300);
        for k in 0..25 {
            mix.push_desktop(pcm(&desktop), Some(chunk_pts(k)), k == 0, &mut enc.sink())
                .unwrap();
        }
        for k in 0..25 {
            mix.push_mic(pcm(&mic), Some(chunk_pts(k)), k == 0);
        }
        mix.tick(chunk_pts(25), &mut enc.sink()).unwrap();
        let stats = mix.take_stats();
        assert_eq!((stats.mixed, stats.mic_dropped), (25, 0), "{stats:?}");
        assert!(enc.chunks().iter().all(|&(_, value)| value == 1_300));
    }

    /// Whether every chunk was mixed with a delivering microphone, on the
    /// grid and in order from chunk 0.
    fn all_mixed_on_grid(enc: &Encoder) -> bool {
        enc.chunks()
            .iter()
            .zip(0..)
            .all(|(&(pts, value), k)| pts == chunk_pts(k) && value == 2)
    }

    #[test]
    fn a_bogus_microphone_timestamp_is_placed_contiguously() {
        for bogus in [
            Timestamp::from_secs(30).ticks(),
            MAX_GAP + Timestamp::from_secs(1).ticks(),
            -(MAX_GAP + Timestamp::from_secs(1).ticks()),
        ] {
            let mut mix = Mix::new(ANCHOR);
            mix.open_mic();
            let mut enc = Encoder::default();
            for i in 0..200 {
                let frame = i * 480;
                let ts = if i == 50 {
                    at(frame) + bogus
                } else {
                    at(frame)
                };
                step(&mut mix, &mut enc, frame, Some(ts));
            }
            let stats = mix.take_stats();
            assert_eq!(stats.desktop_only, 0, "{bogus}: {stats:?}");
            assert_eq!(stats.mic_dropped, 0, "{bogus}: {stats:?}");
            assert!(stats.mixed >= 90, "{bogus}: {stats:?}");
            assert!(all_mixed_on_grid(&enc), "{bogus}");
        }
    }

    #[test]
    fn a_microphone_ahead_of_the_desktop_pairs_every_chunk() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let data = s16(480, 1);
        // The microphone's packets land 50 ms before the desktop's.
        for i in 0..5 {
            mix.push_mic(pcm(&data), Some(at(i * 480)), i == 0);
        }
        for i in 0..200 {
            step(&mut mix, &mut enc, i * 480, Some(at((i + 5) * 480)));
        }
        let stats = mix.take_stats();
        assert_eq!((stats.desktop_only, stats.mic_dropped), (0, 0), "{stats:?}");
        assert_eq!(stats.mixed, 93, "{stats:?}");
        assert!(all_mixed_on_grid(&enc));
    }

    #[test]
    fn a_desktop_reanchor_in_the_fill_moves_the_microphone_before_its_fill() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        for i in 0..20 {
            step(&mut mix, &mut enc, i * 480, Some(at(i * 480)));
        }
        // A system sleep: nothing arrives for longer than MAX_GAP.
        let wake = at(20 * 480) + MAX_GAP + Timestamp::from_secs(5).ticks();
        mix.tick(wake, &mut enc.sink()).unwrap();
        assert_eq!(mix.take_stats().mic_dropped, 0);
        let base = wake - FILL_LAG;
        assert_eq!(mix.mic.as_ref().map(Timeline::anchor), Some(base));
        let data = s16(480, 1);
        for i in 0..100 {
            let ts = base + frames_to_ticks(i * 480);
            mix.push_desktop(pcm(&data), Some(ts), i == 0, &mut enc.sink())
                .unwrap();
            mix.push_mic(pcm(&data), Some(ts), i == 0);
            mix.tick(ts + frames_to_ticks(480), &mut enc.sink())
                .unwrap();
        }
        let stats = mix.take_stats();
        assert_eq!((stats.desktop_only, stats.mic_dropped), (0, 0), "{stats:?}");
        assert_eq!(stats.mixed, 46, "{stats:?}");
    }

    #[test]
    fn a_never_delivering_microphone_releases_desktop_in_time_under_tick() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let mut enc = Encoder::default();
        let data = s16(480, 1);
        let iteration = Timestamp::from_millis(20).ticks();
        let mut frame = 0;
        let mut released = 0;
        for i in 1..=100 {
            let now = ANCHOR + i * iteration;
            // Two 10 ms desktop packets per 20 ms iteration.
            while at(frame + 480) <= now {
                mix.push_desktop(pcm(&data), Some(at(frame)), frame == 0, &mut enc.sink())
                    .unwrap();
                frame += 480;
            }
            mix.tick(now, &mut enc.sink()).unwrap();
            for &(pts, _) in &enc.chunks()[released..] {
                assert!(
                    now <= pts + CHUNK + MIC_WAIT + iteration,
                    "chunk {pts} released at {now}"
                );
            }
            released = enc.chunks().len();
        }
        assert!(released >= 80, "{released}");
        assert!(enc.chunks().iter().all(|&(_, value)| value == 1));
    }

    #[test]
    fn microphone_overflow_drops_its_oldest_chunks() {
        let mut mix = Mix::new(ANCHOR);
        mix.open_mic();
        let data = s16(1024 * (QUEUE_CHUNKS + 2), 1);
        mix.push_mic(pcm(&data), Some(ANCHOR), true);
        assert_eq!(mix.take_stats().mic_dropped, 2);
    }

    #[test]
    fn mic_fill_lag_is_longer_than_loopback_and_shorter_than_the_wait() {
        const { assert!(FILL_LAG < MIC_FILL_LAG && MIC_FILL_LAG < MIC_WAIT) };
        assert_eq!(PACKET_DURATION.ticks(), CHUNK);
        let iteration = Timestamp::from_millis(20).ticks();
        let wasapi_buffer = Timestamp::from_millis(500).ticks();
        let queued = i64::try_from(QUEUE_CHUNKS).unwrap() * CHUNK;
        assert!(queued > wasapi_buffer + MIC_WAIT + iteration);
        // A real microphone packet is at most a packet ahead of the clock
        // reading the desktop fill used, which trails by its lag.
        assert!(FILL_LAG + iteration + CHUNK < MIC_MAX_LEAD);
    }
}
