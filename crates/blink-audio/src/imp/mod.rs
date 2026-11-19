//! The audio thread: WASAPI loopback, silence fill, and AAC encoding.
//!
//! One dedicated thread does everything. It runs at below-normal priority, in
//! line with the CPU rules in `docs/ARCHITECTURE.md`, rather than registering
//! with MMCSS ("Audio" / "Pro Audio"): its work is tiny (one AAC frame per
//! 21 ms), the 500 ms WASAPI buffer rides out long starvation, and an overrun
//! costs a short silent gap that the timeline fills, never an A/V desync. An
//! MMCSS boost would let the thread preempt the game, which is the one thing
//! blink must not do.
//!
//! The loop waits for the stream's event (or polls), reads the clock, drains
//! every packet into the [`Timeline`], fills silence up to the clock reading
//! taken before the drain (so a packet that was in flight at the read is never
//! overtaken by fill), and checks once a second whether the default render endpoint changed. A lost or replaced
//! endpoint is reopened with backoff while the timeline keeps emitting
//! silence, so the AAC stream stays continuous through device switches,
//! unplugs, and audio-service restarts.

mod com;
mod encoder;
mod wasapi;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use blink_ring::media::AudioFormat;
use blink_sys::ThreadPriority;
use tracing::{debug, info, warn};
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

use self::com::MediaRuntime;
use self::encoder::AacEncoder;
use self::wasapi::{LoopbackStream, PacketInfo};
use crate::backoff::Backoff;
use crate::timeline::{Correction, Event, Packet, Timeline};
use crate::{AudioConfig, AudioError, AudioSink};

/// How often the loop asks whether the default render endpoint changed.
const DEVICE_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Sleep per iteration while no endpoint is open (silence is still emitted).
const IDLE_INTERVAL: Duration = Duration::from_millis(20);

/// How often the timeline correction counters are logged.
const STATS_INTERVAL: Duration = Duration::from_secs(60);

/// Attaches context to a `windows` result.
trait Context<T> {
    fn context(self, context: &'static str) -> Result<T, AudioError>;
}

impl<T> Context<T> for windows::core::Result<T> {
    fn context(self, context: &'static str) -> Result<T, AudioError> {
        self.map_err(|source| AudioError::Windows { context, source })
    }
}

/// Whether the `i32` flag constant `flag` is set in `flags`.
fn has_flag(flags: u32, flag: i32) -> bool {
    u32::try_from(flag).is_ok_and(|flag| flags & flag != 0)
}

/// Why draining a stream stopped.
enum Fault {
    /// The endpoint failed; reopen it.
    Device(AudioError),
    /// The encoder or sink failed; stop the thread.
    Fatal(AudioError),
}

/// A running audio capture thread. Dropping it stops and joins the thread.
///
/// Stopping (through [`Self::stop`] or `Drop`) drains the encoder on the audio
/// thread, which delivers the last frames to the sink while the caller blocks
/// in the join. Never stop or drop this while holding a lock the sink takes
/// (such as the clip ring's mutex): the two threads would deadlock.
///
/// The thread only exits on its own after a fatal encoder or COM error, and
/// then nothing is captured until a restart. Poll [`Self::is_finished`] (for
/// example from the hotkey loop) and call [`Self::stop`] to get the error.
#[derive(Debug)]
pub struct AudioCapture {
    format: AudioFormat,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), AudioError>>>,
}

impl AudioCapture {
    /// Starts loopback capture of the default render endpoint on a new thread
    /// and delivers encoded AAC frames to the sink `make_sink` returns.
    ///
    /// `make_sink` runs once on the audio thread, after the encoder is
    /// configured and before the first frame, with the stream's final format
    /// (for example to register the audio track in the ring). The format never
    /// changes afterwards, not even across device switches, because the
    /// encoder is fixed at 48 kHz stereo. Returns once `make_sink` has run, so
    /// [`Self::audio_format`] is final. A missing or failing endpoint does not
    /// fail startup: the thread emits silence and keeps retrying with backoff
    /// until one opens.
    ///
    /// # Errors
    ///
    /// Any error from initializing COM, Media Foundation, or the AAC encoder.
    pub fn start<S, F>(config: AudioConfig, make_sink: F) -> Result<Self, AudioError>
    where
        S: AudioSink,
        F: FnOnce(&AudioFormat) -> S + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, started) = mpsc::sync_channel(1);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("blink-audio".to_owned())
            .spawn(move || run(config, make_sink, &thread_stop, ready))
            .map_err(AudioError::Spawn)?;
        match started.recv() {
            Ok(format) => Ok(Self {
                format,
                stop,
                thread: Some(thread),
            }),
            // The sender is dropped without sending only when the thread
            // exits during startup; its result says why.
            Err(_) => match thread.join() {
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(AudioError::StartupLost),
                Err(_) => Err(AudioError::ThreadPanicked),
            },
        }
    }

    /// Format of the AAC stream: 48 kHz, stereo, and the encoder's
    /// `AudioSpecificConfig`. Constant for the life of the capture.
    #[must_use]
    pub const fn audio_format(&self) -> &AudioFormat {
        &self.format
    }

    /// Whether the audio thread has exited (after a fatal encoder error).
    /// [`Self::stop`] returns the error.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Stops capture, flushing the encoder's last frames to the sink from the
    /// audio thread, and reports how the audio thread ended. Blocks until the
    /// thread exits; see the deadlock note on [`AudioCapture`].
    ///
    /// # Errors
    ///
    /// The error that stopped the thread early, or
    /// [`AudioError::ThreadPanicked`].
    pub fn stop(mut self) -> Result<(), AudioError> {
        self.stop.store(true, Ordering::Release);
        match self.thread.take().map(JoinHandle::join) {
            None | Some(Ok(Ok(()))) => Ok(()),
            Some(Ok(Err(error))) => Err(error),
            Some(Err(_)) => Err(AudioError::ThreadPanicked),
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        match self.thread.take().map(JoinHandle::join) {
            None | Some(Ok(Ok(()))) => {}
            Some(Ok(Err(error))) => warn!(?error, "audio thread failed"),
            Some(Err(_)) => warn!("audio thread panicked"),
        }
    }
}

/// Audio thread body. COM and Media Foundation outlive every COM object,
/// which all live inside `capture`.
fn run<S, F>(
    config: AudioConfig,
    make_sink: F,
    stop: &AtomicBool,
    ready: SyncSender<AudioFormat>,
) -> Result<(), AudioError>
where
    S: AudioSink,
    F: FnOnce(&AudioFormat) -> S,
{
    if let Err(error) = blink_sys::set_current_thread_priority(ThreadPriority::BelowNormal) {
        warn!(?error, "could not lower the audio thread priority");
    }
    let runtime = MediaRuntime::init()?;
    let result = capture(config, make_sink, stop, ready);
    drop(runtime);
    if let Err(error) = &result {
        warn!(?error, "audio capture stopped");
    }
    result
}

fn capture<S, F>(
    config: AudioConfig,
    make_sink: F,
    stop: &AtomicBool,
    ready: SyncSender<AudioFormat>,
) -> Result<(), AudioError>
where
    S: AudioSink,
    F: FnOnce(&AudioFormat) -> S,
{
    let mut encoder = AacEncoder::new(config)?;
    // SAFETY: COM is initialized on this thread; creates an in-process object
    // with no aggregation.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .context("create the audio device enumerator")?;
    let format = encoder.format().clone();
    let mut sink = make_sink(&format);
    if ready.send(format).is_err() {
        return Ok(());
    }
    drop(ready);
    info!(bitrate_kbps = config.bitrate.kbps(), "audio encoder ready");

    let mut timeline = Timeline::new(blink_sys::qpc_now()?.ticks());
    let mut on_event = |event: Event<'_>| match event {
        Event::Chunk(chunk) => encoder.encode(chunk, &mut sink),
        Event::SegmentEnd => encoder.drain(&mut sink),
    };
    let mut endpoint = Endpoint::new(enumerator);
    let mut stats = Stats::new();

    while !stop.load(Ordering::Acquire) {
        endpoint.ensure_open();
        endpoint.wait();
        let now = blink_sys::qpc_now()?.ticks();
        endpoint.drain(&mut |packet, info: PacketInfo| {
            if info.discontinuity {
                debug!("loopback data discontinuity");
                stats.discontinuities += 1;
            }
            let correction =
                timeline.push_packet(packet, info.timestamp, info.discontinuity, &mut on_event)?;
            stats.record(correction);
            Ok(())
        })?;
        let before = timeline.frames_written();
        match timeline.fill_silence(now, &mut on_event)? {
            Correction::None => {
                stats.idle_fill += timeline.frames_written().saturating_sub(before);
            }
            correction => stats.record(correction),
        }
        stats.maybe_log();
    }
    drop(endpoint);
    timeline.finish(&mut on_event)
}

/// Timeline correction counters, logged every [`STATS_INTERVAL`] so drift and
/// device trouble are visible without per-packet logs.
#[derive(Debug, Default)]
struct Stats {
    discontinuities: u64,
    silence: u64,
    skipped: u64,
    repeated: u64,
    dropped: u64,
    reanchors: u64,
    idle_fill: u64,
    next_log: Option<Instant>,
}

impl Stats {
    fn new() -> Self {
        Self {
            next_log: Some(Instant::now() + STATS_INTERVAL),
            ..Self::default()
        }
    }

    fn record(&mut self, correction: Correction) {
        match correction {
            Correction::None => {}
            Correction::Silence(frames) => {
                debug!(frames, "inserted silence to realign audio");
                self.silence += frames;
            }
            Correction::Skipped(frames) => {
                debug!(frames, "dropped overlapping audio frames");
                self.skipped += frames;
            }
            Correction::Repeated(frames) => self.repeated += frames,
            Correction::Dropped(frames) => self.dropped += frames,
            Correction::Reanchored => {
                info!("audio clock gap too long to fill; new segment");
                self.reanchors += 1;
            }
        }
    }

    /// Logs and resets the counters once per interval.
    fn maybe_log(&mut self) {
        let now = Instant::now();
        if self.next_log.is_some_and(|next| now < next) {
            return;
        }
        info!(
            discontinuities = self.discontinuities,
            silence_frames = self.silence,
            skipped_frames = self.skipped,
            slew_repeated_frames = self.repeated,
            slew_dropped_frames = self.dropped,
            reanchors = self.reanchors,
            idle_fill_frames = self.idle_fill,
            "audio timeline stats (last 60 s)"
        );
        *self = Self {
            next_log: Some(now + STATS_INTERVAL),
            ..Self::default()
        };
    }
}

/// The default render endpoint's loopback stream, reopened with backoff
/// whenever it is lost or the default endpoint changes.
struct Endpoint {
    enumerator: IMMDeviceEnumerator,
    stream: Option<LoopbackStream>,
    backoff: Backoff,
    next_open: Instant,
    next_device_check: Instant,
    open_failures: u32,
}

impl Endpoint {
    fn new(enumerator: IMMDeviceEnumerator) -> Self {
        let now = Instant::now();
        Self {
            enumerator,
            stream: None,
            backoff: Backoff::default(),
            next_open: now,
            next_device_check: now,
            open_failures: 0,
        }
    }

    /// Opens the stream if none is open and the backoff delay has passed.
    fn ensure_open(&mut self) {
        if self.stream.is_some() || Instant::now() < self.next_open {
            return;
        }
        match LoopbackStream::open(&self.enumerator) {
            Ok(stream) => {
                info!(
                    format = ?stream.format(),
                    event_driven = stream.is_event_driven(),
                    "loopback capture started"
                );
                self.backoff.reset();
                self.open_failures = 0;
                self.next_device_check = Instant::now() + DEVICE_CHECK_INTERVAL;
                self.stream = Some(stream);
            }
            Err(error) => {
                if self.open_failures == 0 {
                    warn!(?error, "cannot open the default render endpoint; retrying");
                } else {
                    debug!(
                        ?error,
                        attempt = self.open_failures + 1,
                        "endpoint still unavailable"
                    );
                }
                self.open_failures = self.open_failures.saturating_add(1);
                self.next_open = Instant::now() + self.backoff.next_delay();
            }
        }
    }

    /// Waits for the open stream's next packet, or idles when none is open.
    fn wait(&self) {
        match &self.stream {
            Some(stream) => stream.wait(),
            None => thread::sleep(IDLE_INTERVAL),
        }
    }

    /// Drains the open stream, then checks for a default-endpoint change.
    /// Endpoint failures close the stream for an immediate reopen; only
    /// errors from `on_packet` are returned.
    fn drain(
        &mut self,
        on_packet: &mut impl FnMut(Packet<'_>, PacketInfo) -> Result<(), AudioError>,
    ) -> Result<(), AudioError> {
        let Some(stream) = &self.stream else {
            return Ok(());
        };
        let lost = match stream.drain(on_packet) {
            Err(Fault::Fatal(error)) => return Err(error),
            Err(Fault::Device(error)) => {
                warn!(?error, "loopback stream lost; reopening");
                true
            }
            Ok(()) if Instant::now() >= self.next_device_check => {
                self.next_device_check = Instant::now() + DEVICE_CHECK_INTERVAL;
                match stream.is_default(&self.enumerator) {
                    Ok(true) => false,
                    Ok(false) => {
                        info!("default render endpoint changed; reopening");
                        true
                    }
                    Err(error) => {
                        info!(?error, "default render endpoint unavailable");
                        true
                    }
                }
            }
            Ok(()) => false,
        };
        if lost {
            self.stream = None;
            self.next_open = Instant::now();
        }
        Ok(())
    }
}
