//! The audio thread: WASAPI loopback and microphone capture, silence fill,
//! mixing, and AAC encoding.
//!
//! One dedicated thread does everything. It runs at below-normal priority, in
//! line with the CPU rules in `docs/ARCHITECTURE.md`, rather than registering
//! with MMCSS ("Audio" / "Pro Audio"): its work is tiny (one AAC frame per
//! 21 ms), the 500 ms WASAPI buffers ride out long starvation, and an overrun
//! costs a short silent gap that the timeline fills, never an A/V desync. An
//! MMCSS boost would let the thread preempt the game, which is the one thing
//! blink must not do.
//!
//! The loop waits for the desktop stream's event (or polls), reads the clock,
//! drains every desktop packet and then every microphone packet into the
//! [`Mix`], fills silence up to the clock reading taken before the drain (so a
//! packet that was in flight at the read is never overtaken by fill), sends
//! the chunks that are ready to the encoder, and checks once a second whether
//! either default endpoint changed. A lost or replaced endpoint is reopened
//! with backoff while the timeline keeps emitting silence, so the AAC stream
//! stays continuous through device switches, unplugs, and audio-service
//! restarts. A microphone that is missing, denied by the privacy settings, or
//! lost only drops out of the mix; the desktop audio carries on. A default
//! microphone that is a Bluetooth headset's hands-free endpoint is never
//! opened (see [`wasapi`]) and is rechecked once a second, so switching the
//! default to another microphone picks that one up.

mod com;
mod encoder;
mod wasapi;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use blink_ring::media::AudioFormat;
use blink_sys::ErrorChain;
use blink_sys::{Backoff, ThreadPriority};
use tracing::{debug, info, warn};
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

use self::com::MediaRuntime;
use self::encoder::AacEncoder;
use self::wasapi::{CaptureStream, PacketInfo, Source, is_access_denied, is_not_found};
use crate::mic::OpenFailure;
use crate::mix::{Mix, MixStats};
use crate::timeline::{Correction, Event, Packet};
use crate::{AudioConfig, AudioError, AudioSink, MicState};

/// How often the loop asks whether a default endpoint changed.
const DEVICE_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Sleep per iteration while no desktop endpoint is open (silence is still
/// emitted, and the microphone still drained).
const IDLE_INTERVAL: Duration = Duration::from_millis(20);

/// Delay before the second attempt to open an endpoint; the first reopen
/// after a loss is immediate because a default-device switch usually
/// succeeds at once.
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Longest delay between attempts on the desktop endpoint. Reopening never
/// gives up (a headset can be unplugged for hours), so the cap keeps polling
/// cheap rather than ending it.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Longest delay between attempts on the microphone. Longer than the
/// desktop's: a machine without a microphone, or with access denied, stays
/// that way for a long time, and each attempt is a device activation.
const MIC_MAX_RETRY_DELAY: Duration = Duration::from_secs(10);

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
    mic: Arc<AtomicU8>,
    thread: Option<JoinHandle<Result<(), AudioError>>>,
}

impl AudioCapture {
    /// Starts loopback capture of the default render endpoint, and of the
    /// default recording endpoint if `config.microphone` is set, on a new
    /// thread and delivers encoded AAC frames to the sink `make_sink`
    /// returns.
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
        let initial = if config.microphone {
            MicState::Starting
        } else {
            MicState::Off
        };
        let mic = Arc::new(AtomicU8::new(initial.to_u8()));
        let (ready, started) = mpsc::sync_channel(1);
        let thread_stop = Arc::clone(&stop);
        let thread_mic = Arc::clone(&mic);
        let thread = thread::Builder::new()
            .name("blink-audio".to_owned())
            .spawn(move || run(config, make_sink, &thread_stop, &thread_mic, ready))
            .map_err(AudioError::Spawn)?;
        match started.recv() {
            Ok(format) => Ok(Self {
                format,
                stop,
                mic,
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

    /// What the microphone capture is doing, as of the audio thread's last
    /// loop iteration. [`MicState::Off`] once the thread has exited.
    #[must_use]
    pub fn mic_state(&self) -> MicState {
        if self.is_finished() {
            return MicState::Off;
        }
        MicState::from_u8(self.mic.load(Ordering::Relaxed))
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
            Some(Ok(Err(error))) => warn!(error = %ErrorChain(&error), "audio thread failed"),
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
    shown_mic_state: &AtomicU8,
    ready: SyncSender<AudioFormat>,
) -> Result<(), AudioError>
where
    S: AudioSink,
    F: FnOnce(&AudioFormat) -> S,
{
    if let Err(error) = blink_sys::set_current_thread_priority(ThreadPriority::BelowNormal) {
        warn!(error = %ErrorChain(&error), "could not lower the audio thread priority");
    }
    let runtime = MediaRuntime::init()?;
    let result = capture(config, make_sink, stop, shown_mic_state, ready);
    drop(runtime);
    if let Err(error) = &result {
        warn!(error = %ErrorChain(error), "audio capture stopped");
    }
    result
}

fn capture<S, F>(
    config: AudioConfig,
    make_sink: F,
    stop: &AtomicBool,
    shown_mic_state: &AtomicU8,
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
    info!(
        bitrate_kbps = config.bitrate.kbps(),
        microphone = config.microphone,
        "audio encoder ready"
    );

    let mut mix = Mix::new(blink_sys::qpc_now()?.ticks());
    let mut encode = |event: Event<'_>| match event {
        Event::Chunk(chunk) => encoder.encode(chunk, &mut sink),
        Event::SegmentEnd => encoder.drain(&mut sink),
    };
    let mut desktop = Endpoint::new(enumerator.clone(), Source::Desktop);
    let mut mic = config
        .microphone
        .then(|| Endpoint::new(enumerator, Source::Microphone));
    let mut desktop_stats = Stats::new(Source::Desktop);
    let mut mic_stats = Stats::new(Source::Microphone);

    while !stop.load(Ordering::Acquire) {
        desktop.ensure_open();
        if let Some(mic) = &mut mic
            && mic.ensure_open()
        {
            mix.open_mic();
        }
        desktop.wait();
        let now = blink_sys::qpc_now()?.ticks();
        desktop.drain(&mut |packet, info: PacketInfo| {
            desktop_stats.discontinuity(info);
            let correction =
                mix.push_desktop(packet, info.timestamp, info.discontinuity, &mut encode)?;
            desktop_stats.record(correction);
            Ok(())
        })?;
        if let Some(mic) = &mut mic {
            let lost = mic.drain(&mut |packet, info: PacketInfo| {
                mic_stats.discontinuity(info);
                if let Some(correction) = mix.push_mic(packet, info.timestamp, info.discontinuity) {
                    mic_stats.record(correction);
                }
                Ok(())
            })?;
            if lost {
                mix.close_mic();
            }
            shown_mic_state.store(mic.mic_state().to_u8(), Ordering::Relaxed);
        }
        let before = mix.desktop_frames_written();
        let (desktop_fill, mic_fill) = mix.tick(now, &mut encode)?;
        match desktop_fill {
            Correction::None => {
                desktop_stats.idle_fill += mix.desktop_frames_written().saturating_sub(before);
            }
            correction => desktop_stats.record(correction),
        }
        if let Some(correction) = mic_fill {
            mic_stats.record(correction);
        }
        if desktop_stats.maybe_log() && mic.is_some() {
            mic_stats.log();
            log_mix(mix.take_stats());
        }
    }
    drop(desktop);
    drop(mic);
    mix.finish(&mut encode)
}

fn log_mix(stats: MixStats) {
    info!(
        mixed_chunks = stats.mixed,
        desktop_only_chunks = stats.desktop_only,
        dropped_mic_chunks = stats.mic_dropped,
        "microphone mix stats (last 60 s)"
    );
}

/// Timeline correction counters for one source, logged every
/// [`STATS_INTERVAL`] so drift and device trouble are visible without
/// per-packet logs.
#[derive(Debug)]
struct Stats {
    source: Source,
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
    fn new(source: Source) -> Self {
        Self {
            source,
            discontinuities: 0,
            silence: 0,
            skipped: 0,
            repeated: 0,
            dropped: 0,
            reanchors: 0,
            idle_fill: 0,
            next_log: Instant::now().checked_add(STATS_INTERVAL),
        }
    }

    fn discontinuity(&mut self, info: PacketInfo) {
        if info.discontinuity {
            debug!(source = ?self.source, "capture data discontinuity");
            self.discontinuities += 1;
        }
    }

    fn record(&mut self, correction: Correction) {
        let source = self.source;
        match correction {
            Correction::None => {}
            Correction::Silence(frames) => {
                debug!(?source, frames, "inserted silence to realign audio");
                self.silence += frames;
            }
            Correction::Skipped(frames) => {
                debug!(?source, frames, "dropped overlapping audio frames");
                self.skipped += frames;
            }
            Correction::Repeated(frames) => self.repeated += frames,
            Correction::Dropped(frames) => self.dropped += frames,
            Correction::Reanchored => {
                info!(?source, "audio clock gap too long to fill; new segment");
                self.reanchors += 1;
            }
        }
    }

    /// Logs and resets the counters once per interval. Returns whether it
    /// logged.
    fn maybe_log(&mut self) -> bool {
        if self.next_log.is_some_and(|next| Instant::now() < next) {
            return false;
        }
        self.log();
        true
    }

    /// Logs and resets the counters now.
    fn log(&mut self) {
        info!(
            source = ?self.source,
            discontinuities = self.discontinuities,
            silence_frames = self.silence,
            skipped_frames = self.skipped,
            slew_repeated_frames = self.repeated,
            slew_dropped_frames = self.dropped,
            reanchors = self.reanchors,
            idle_fill_frames = self.idle_fill,
            "audio timeline stats (last 60 s)"
        );
        *self = Self::new(self.source);
    }
}

/// Classifies why opening an endpoint failed.
fn open_failure(error: &AudioError) -> OpenFailure {
    if is_not_found(error) {
        OpenFailure::NotFound
    } else if is_access_denied(error) {
        OpenFailure::Denied
    } else {
        OpenFailure::Other
    }
}

/// The default endpoint's capture stream for one source, reopened with
/// backoff whenever it is lost or the default endpoint changes.
struct Endpoint {
    source: Source,
    enumerator: IMMDeviceEnumerator,
    stream: Option<CaptureStream>,
    /// When the open stream was opened.
    opened_at: Instant,
    backoff: Backoff,
    next_open: Instant,
    next_device_check: Instant,
    /// Why the last attempt failed, until one succeeds. A failure is warned
    /// about once, when it first happens or changes kind; repeats are debug.
    failure: Option<OpenFailure>,
    open_failures: u32,
}

impl Endpoint {
    fn new(enumerator: IMMDeviceEnumerator, source: Source) -> Self {
        let now = Instant::now();
        let max_delay = match source {
            Source::Desktop => MAX_RETRY_DELAY,
            Source::Microphone => MIC_MAX_RETRY_DELAY,
        };
        Self {
            source,
            enumerator,
            stream: None,
            opened_at: now,
            backoff: Backoff::new(INITIAL_RETRY_DELAY, max_delay),
            next_open: now,
            next_device_check: now,
            failure: None,
            open_failures: 0,
        }
    }

    /// The state to report for this endpoint as the microphone.
    const fn mic_state(&self) -> MicState {
        MicState::of_endpoint(self.stream.is_some(), self.failure)
    }

    /// Opens the stream if none is open and the backoff delay has passed.
    /// Returns whether a stream was opened now.
    fn ensure_open(&mut self) -> bool {
        if self.stream.is_some() || Instant::now() < self.next_open {
            return false;
        }
        match CaptureStream::open(&self.enumerator, self.source) {
            Ok(None) => {
                if self.failure != Some(OpenFailure::BluetoothHandsFree) {
                    warn!(
                        "the default microphone is a Bluetooth headset's hands-free microphone; not \
                         recording it, because opening it would switch the headset to low-quality \
                         mono audio. Recording desktop audio only"
                    );
                }
                self.failure = Some(OpenFailure::BluetoothHandsFree);
                // Not a failure to back off from: recheck as often as a
                // default change is checked for.
                self.backoff.reset();
                self.next_open = after(DEVICE_CHECK_INTERVAL);
                false
            }
            Ok(Some(stream)) => {
                info!(
                    source = ?self.source,
                    format = ?stream.format(),
                    event_driven = stream.is_event_driven(),
                    enumerator = stream.enumerator_name(),
                    "audio capture stream started"
                );
                self.failure = None;
                self.open_failures = 0;
                self.opened_at = Instant::now();
                self.next_device_check = after(DEVICE_CHECK_INTERVAL);
                self.stream = Some(stream);
                true
            }
            Err(error) => {
                let failure = open_failure(&error);
                if self.failure == Some(failure) {
                    debug!(
                        source = ?self.source,
                        error = %ErrorChain(&error),
                        attempt = self.open_failures + 1,
                        "endpoint still unavailable"
                    );
                } else {
                    self.warn_failure(failure, &error);
                }
                self.failure = Some(failure);
                self.open_failures = self.open_failures.saturating_add(1);
                self.next_open = after(self.backoff.next_delay());
                false
            }
        }
    }

    fn warn_failure(&self, failure: OpenFailure, error: &AudioError) {
        let error = ErrorChain(error);
        match (self.source, failure) {
            (Source::Desktop, _) => {
                warn!(%error, "cannot open the default render endpoint; retrying");
            }
            (Source::Microphone, OpenFailure::NotFound) => {
                warn!(%error, "no microphone; recording desktop audio only until one appears");
            }
            (Source::Microphone, OpenFailure::Denied) => warn!(
                %error,
                "microphone access is denied in the Windows privacy settings; recording desktop \
                 audio only"
            ),
            // A hands-free endpoint is not an open error; it has its own
            // message in `ensure_open`.
            (Source::Microphone, OpenFailure::Other | OpenFailure::BluetoothHandsFree) => {
                warn!(%error, "cannot open the microphone; recording desktop audio only, retrying");
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
    /// Endpoint failures close the stream and return `Ok(true)`; only errors
    /// from `on_packet` are returned. A stream that ran for a while is
    /// reopened at once and resets the backoff; one lost soon after opening
    /// waits out the next backoff delay, so a device that opens and then
    /// fails at once is not reopened every iteration.
    ///
    /// Losses log at info: the reopen's failure, if any, carries the warning.
    fn drain(
        &mut self,
        on_packet: &mut impl FnMut(Packet<'_>, PacketInfo) -> Result<(), AudioError>,
    ) -> Result<bool, AudioError> {
        let Some(stream) = &self.stream else {
            return Ok(false);
        };
        let source = self.source;
        let lost = match stream.drain(on_packet) {
            Err(Fault::Fatal(error)) => return Err(error),
            Err(Fault::Device(error)) => {
                info!(?source, error = %ErrorChain(&error), "capture stream lost; reopening");
                true
            }
            Ok(()) if Instant::now() >= self.next_device_check => {
                self.next_device_check = after(DEVICE_CHECK_INTERVAL);
                match stream.is_default(&self.enumerator) {
                    Ok(true) => false,
                    Ok(false) => {
                        info!(?source, "default endpoint changed; reopening");
                        true
                    }
                    Err(error) => {
                        info!(?source, error = %ErrorChain(&error), "default endpoint unavailable");
                        true
                    }
                }
            }
            Ok(()) => false,
        };
        if lost {
            self.stream = None;
            if self.opened_at.elapsed() >= DEVICE_CHECK_INTERVAL {
                self.backoff.reset();
                self.next_open = Instant::now();
            } else {
                self.next_open = after(self.backoff.next_delay());
            }
        }
        Ok(lost)
    }
}

/// `delay` from now, or now if that is past the end of `Instant`'s range.
fn after(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}
