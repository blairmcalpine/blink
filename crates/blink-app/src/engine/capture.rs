//! The capture-plus-encode thread: Desktop Duplication frames go straight to
//! NVENC on the same D3D11 device, and the packets into the ring.
//!
//! Steady state per frame: one `next_frame`, one `encode`, one short ring
//! lock for the push. Nothing allocates and nothing waits on I/O or on a save.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use blink_capture::{CaptureDevice, CaptureError, CaptureEvent, CapturedFrame, DesktopDuplicator};
use blink_config::Config;
use blink_encode::{EncodeError, Encoder};
use blink_ring::PushOutcome;
use blink_ring::media::{Timestamp, VideoFormat};
use blink_sys::{ErrorChain, SysError, ThreadPriority};

use super::EngineEvent;
use super::signal::StopSignal;
use crate::decide::{ModeAction, RetryPolicy, mode_change_action, next_pts, should_refresh};
use crate::mapping::encoder_config;
use crate::shared::{Producer, SharedRing};
use crate::status::CaptureState;

/// Longest wait in `next_frame`. Bounds how late a stop request or a
/// static-screen IDR refresh is noticed.
const POLL: Duration = Duration::from_millis(100);

/// How often the encode counters are logged.
const STATS_PERIOD: Duration = Duration::from_secs(60);

const _: () = assert!(
    blink_capture::TEXTURE_POOL_SIZE <= blink_encode::MAX_REGISTERED_TEXTURES,
    "every pooled capture texture must stay registered with NVENC"
);

/// First and longest delay before rebuilding the device after a failure.
const FIRST_RETRY: Duration = Duration::from_millis(250);
const MAX_RETRY: Duration = Duration::from_secs(10);

/// Starts the thread. It runs until `stop` is set, rebuilding the device,
/// duplicator, and encoder with backoff after failures. It reports its state
/// as [`EngineEvent::Capture`], and errors it cannot recover from (no NVIDIA
/// GPU or driver) as [`EngineEvent::CaptureFatal`].
pub(super) fn spawn(
    config: &Config,
    ring: Arc<SharedRing>,
    stop: Arc<StopSignal>,
    events: Sender<EngineEvent>,
) -> anyhow::Result<JoinHandle<()>> {
    let config = config.clone();
    thread::Builder::new()
        .name("blink-capture".to_owned())
        .spawn(move || run(&config, &ring, &stop, &events))
        .context("failed to spawn the capture thread")
}

/// Why a capture session ended.
enum Failure {
    /// Rebuild the device and try again after a delay.
    Retry(anyhow::Error),
    /// Capture can never work on this machine; tell the user and exit.
    Fatal(String),
}

impl From<CaptureError> for Failure {
    fn from(error: CaptureError) -> Self {
        Self::Retry(error.into())
    }
}

impl From<SysError> for Failure {
    fn from(error: SysError) -> Self {
        Self::Retry(error.into())
    }
}

impl From<EncodeError> for Failure {
    fn from(error: EncodeError) -> Self {
        match error {
            EncodeError::NoNvidiaDriver(_)
            | EncodeError::DriverTooOld { .. }
            | EncodeError::MissingExport(_)
            | EncodeError::MissingFunction(_) => {
                Self::Fatal(format!("NVENC is unavailable: {}", ErrorChain(&error)))
            }
            error => Self::Retry(error.into()),
        }
    }
}

/// What the capture thread remembers across sessions.
struct Run<'a> {
    events: &'a Sender<EngineEvent>,
    /// The largest supported mode seen, which sizes new encoder sessions.
    largest: (u32, u32),
    /// When this session encoded its first frame (`None` until it has, or
    /// while paused); a failure long enough after it resets the retry backoff.
    encoding_since: Option<Instant>,
    /// Frames this session encoded, for the failure log.
    frames: u64,
    /// When encoding paused for an unsupported mode; kept across sessions
    /// and repeated mode changes until a frame is encoded again.
    paused_since: Option<Timestamp>,
}

impl Run<'_> {
    fn report(&self, state: CaptureState) {
        let _ = self.events.send(EngineEvent::Capture(state));
    }
}

fn run(config: &Config, ring: &SharedRing, stop: &StopSignal, events: &Sender<EngineEvent>) {
    if let Err(error) = blink_sys::set_current_thread_priority(ThreadPriority::BelowNormal) {
        tracing::warn!(error = %ErrorChain(&error), "could not lower the capture thread priority");
    }
    let Some(fps) = NonZeroU32::new(config.fps) else {
        let _ = events.send(EngineEvent::CaptureFatal(format!(
            "invalid capture frame rate {}",
            config.fps
        )));
        return;
    };
    let mut run = Run {
        events,
        largest: (0, 0),
        encoding_since: None,
        frames: 0,
        paused_since: None,
    };
    let mut retry = RetryPolicy::new(FIRST_RETRY, MAX_RETRY);
    while !stop.is_stopped() {
        run.encoding_since = None;
        run.frames = 0;
        match session(config, fps, ring, stop, &mut run) {
            Ok(()) => return,
            Err(Failure::Fatal(message)) => {
                tracing::error!(message, "capture cannot run on this machine");
                let _ = events.send(EngineEvent::CaptureFatal(message));
                return;
            }
            Err(Failure::Retry(error)) => {
                let encoding_for = run.encoding_since.map(|since| since.elapsed());
                let (failures, delay) = retry.on_failure(encoding_for);
                let error = ErrorChain(error.as_ref()).to_string();
                tracing::warn!(
                    error,
                    failures,
                    frames = run.frames,
                    encoding_ms = encoding_for.map(|elapsed| elapsed.as_millis()),
                    retry_in_ms = delay.as_millis(),
                    "capture stopped; rebuilding the device"
                );
                run.report(CaptureState::Retrying { failures, error });
                if stop.wait(delay) {
                    return;
                }
            }
        }
    }
}

/// One device's worth of capture. Returns `Ok` only when stopped.
fn session(
    config: &Config,
    fps: NonZeroU32,
    ring: &SharedRing,
    stop: &StopSignal,
    run: &mut Run<'_>,
) -> Result<(), Failure> {
    let device: CaptureDevice = blink_capture::create_device()?;
    if !device.adapter.is_nvidia() {
        return Err(Failure::Fatal(format!(
            "The primary display is driven by {}; NVENC unavailable. blink needs the primary \
             display to be connected to an NVIDIA GPU.",
            device.adapter.description
        )));
    }
    let mut duplicator = DesktopDuplicator::new(&device.device, device.primary_output, fps)?;
    let mut state = State {
        config,
        ring,
        encoder: None,
        video: None,
        paused: false,
        last_frame: None,
        last_pts: None,
        stats: EncodeStats::default(),
        stats_since: Instant::now(),
    };
    while !stop.is_stopped() {
        state.log_stats();
        match duplicator.next_frame(POLL)? {
            CaptureEvent::ModeChanged { width, height } => {
                state.mode_changed(&device, run, width, height)?;
            }
            CaptureEvent::Frame(frame) => {
                if state.frame(frame)? {
                    run.frames = run.frames.saturating_add(1);
                    if run.encoding_since.is_none() {
                        run.encoding_since = Some(Instant::now());
                        run.paused_since = None;
                        run.report(CaptureState::Recording);
                    }
                }
            }
            CaptureEvent::Unchanged { timestamp } => state.refresh_idr(timestamp)?,
            CaptureEvent::Timeout => state.refresh_idr(blink_sys::qpc_now()?)?,
        }
    }
    Ok(())
}

/// The encoder and what the loop remembers between events.
struct State<'a> {
    config: &'a Config,
    ring: &'a SharedRing,
    /// Created on the first supported mode change.
    encoder: Option<Encoder>,
    /// Pushes video with the encoder's current format, re-setting it on a
    /// replaced ring without borrowing the encoder.
    video: Option<Producer<VideoFormat>>,
    /// The display mode is too large to encode; frames are skipped.
    paused: bool,
    /// The newest captured frame, re-encoded as an IDR on a static screen.
    /// The duplicator overwrites its texture only after three newer frames,
    /// and this is replaced by each new frame, so it is always intact.
    last_frame: Option<CapturedFrame>,
    /// Timestamp of the last encoded frame; encode timestamps must increase.
    last_pts: Option<Timestamp>,
    stats: EncodeStats,
    stats_since: Instant,
}

/// Counters for the periodic encode summary.
#[derive(Default)]
struct EncodeStats {
    /// Captured frames encoded.
    encoded: u64,
    /// Static-screen IDR refreshes that re-encoded the last frame.
    refreshed: u64,
}

impl State<'_> {
    fn mode_changed(
        &mut self,
        device: &CaptureDevice,
        run: &mut Run<'_>,
        width: u32,
        height: u32,
    ) -> Result<(), Failure> {
        // The duplicator recreated its texture pool.
        self.last_frame = None;
        tracing::info!(width, height, "capture mode changed");
        let current_max = self.encoder.as_ref().map(|encoder| {
            let config = encoder.config();
            (config.max_width, config.max_height)
        });
        let changed = match mode_change_action(current_max, run.largest, width, height) {
            ModeAction::Unsupported => {
                tracing::error!(width, height, "display mode too large for H.264; paused");
                // The duplicator's old texture pool is gone; drop NVENC's
                // registrations so it is freed while paused.
                if let Some(encoder) = &mut self.encoder {
                    encoder.release_textures();
                }
                self.paused = true;
                run.encoding_since = None;
                let since = match run.paused_since {
                    Some(since) => since,
                    None => blink_sys::qpc_now()?,
                };
                run.paused_since = Some(since);
                run.report(CaptureState::Unsupported {
                    width,
                    height,
                    since,
                });
                return Ok(());
            }
            ModeAction::Reconfigure => match &mut self.encoder {
                Some(encoder) => encoder.reconfigure(width, height)?,
                None => false,
            },
            ModeAction::Recreate { max } => {
                // Free the old NVENC session first: GeForce cards cap the
                // number of concurrent sessions.
                self.encoder = None;
                let encoder = Encoder::new(
                    &device.device,
                    encoder_config(self.config, width, height, max),
                )?;
                self.encoder = Some(encoder);
                true
            }
        };
        self.paused = false;
        run.largest = (run.largest.0.max(width), run.largest.1.max(height));
        if changed && let Some(encoder) = &self.encoder {
            let format = encoder.video_format().clone();
            self.video
                .get_or_insert_with(|| Producer::new(format.clone()))
                .set_format(self.ring, format);
        }
        Ok(())
    }

    /// Encodes a new frame. Returns whether it was encoded (not paused).
    fn frame(&mut self, frame: CapturedFrame) -> Result<bool, Failure> {
        if self.paused {
            return Ok(false);
        }
        let pts = next_pts(self.last_pts, frame.timestamp());
        let encoded = self.encode(&frame, pts)?;
        if encoded {
            self.stats.encoded = self.stats.encoded.saturating_add(1);
        }
        self.last_frame = Some(frame);
        Ok(encoded)
    }

    /// On a static screen nothing is encoded, so re-encode the last frame
    /// when the encoder wants an IDR: once a second, or right after the ring
    /// dropped packets waiting for a keyframe.
    fn refresh_idr(&mut self, now: Timestamp) -> Result<(), Failure> {
        let due = self
            .encoder
            .as_ref()
            .is_some_and(|encoder| encoder.idr_due(now));
        if !should_refresh(due, self.last_pts, now, self.last_frame.is_some()) {
            return Ok(());
        }
        let Some(frame) = self.last_frame.take() else {
            return Ok(());
        };
        let result = self.encode(&frame, now);
        self.last_frame = Some(frame);
        if matches!(result, Ok(true)) {
            self.stats.refreshed = self.stats.refreshed.saturating_add(1);
        }
        result.map(drop)
    }

    fn log_stats(&mut self) {
        if self.stats_since.elapsed() < STATS_PERIOD {
            return;
        }
        let stats = std::mem::take(&mut self.stats);
        tracing::info!(
            encoded = stats.encoded,
            refreshed = stats.refreshed,
            "encode stats (last 60 s)"
        );
        self.stats_since = Instant::now();
    }

    fn encode(&mut self, frame: &CapturedFrame, pts: Timestamp) -> Result<bool, Failure> {
        let (Some(encoder), Some(video)) = (&mut self.encoder, &mut self.video) else {
            return Ok(false);
        };
        let packet = encoder.encode(frame.texture(), pts)?;
        self.last_pts = Some(pts);
        match video.push(self.ring, packet.meta, packet.data) {
            Ok(PushOutcome::Stored) => {}
            Ok(PushOutcome::DroppedAwaitingKeyframe) => encoder.request_idr(),
            Err(error) => {
                tracing::warn!(error = %ErrorChain(&error), "video packet rejected by the ring");
            }
        }
        Ok(true)
    }
}
