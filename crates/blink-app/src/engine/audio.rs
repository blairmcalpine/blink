//! Supervises the blink-audio capture thread: starts it with a sink that
//! pushes into the ring, and restarts it with backoff after a fatal error.
//!
//! Every method runs on the tray thread, which never holds the ring lock, so
//! stopping the capture (which joins the audio thread while it flushes into
//! the sink) cannot deadlock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use blink_audio::{AudioCapture, AudioConfig};
use blink_ring::media::{AudioFormat, PacketMeta};
use blink_sys::{Backoff, ErrorChain};

use crate::shared::{Producer, SharedRing};

/// First and longest delay before restarting audio after it failed.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(60);

/// The audio capture, if enabled, and its restart schedule.
pub(super) struct AudioSupervisor {
    /// The settings to run with; `None` while audio is disabled.
    config: Option<AudioConfig>,
    capture: Option<AudioCapture>,
    /// When the running capture started.
    started_at: Option<Instant>,
    retry_at: Option<Instant>,
    backoff: Backoff,
}

impl Default for AudioSupervisor {
    fn default() -> Self {
        Self {
            config: None,
            capture: None,
            started_at: None,
            retry_at: None,
            backoff: Backoff::new(FIRST_RETRY, MAX_RETRY),
        }
    }
}

impl AudioSupervisor {
    /// Starts capturing with `config`, stopping any previous capture first.
    pub(super) fn start(&mut self, config: AudioConfig, ring: &Arc<SharedRing>) {
        self.stop();
        self.config = Some(config);
        self.backoff.reset();
        self.try_start(ring);
    }

    /// Stops capturing and forgets the config, so [`Self::poll`] does nothing.
    pub(super) fn stop(&mut self) {
        self.config = None;
        self.retry_at = None;
        if let Some(capture) = self.capture.take() {
            match capture.stop() {
                Ok(()) => tracing::info!("audio capture stopped"),
                Err(error) => {
                    tracing::warn!(error = %ErrorChain(&error), "audio capture had failed");
                }
            }
        }
    }

    /// Notices a capture thread that exited on its own and restarts it when
    /// its backoff delay has passed. Call it every few hundred milliseconds.
    pub(super) fn poll(&mut self, ring: &Arc<SharedRing>) {
        if self.config.is_none() {
            return;
        }
        if self.capture.as_ref().is_some_and(AudioCapture::is_finished)
            && let Some(capture) = self.capture.take()
        {
            match capture.stop() {
                Ok(()) => tracing::warn!("audio capture ended; restarting"),
                Err(error) => {
                    tracing::warn!(error = %ErrorChain(&error), "audio capture ended; restarting");
                }
            }
            // A capture that ran a good while (until a device change, say)
            // restarts promptly instead of continuing the old backoff.
            if self
                .started_at
                .take()
                .is_some_and(|started| started.elapsed() >= MAX_RETRY)
            {
                self.backoff.reset();
            }
            self.schedule_retry();
        }
        if self.capture.is_none() && self.retry_at.is_none_or(|at| Instant::now() >= at) {
            self.try_start(ring);
        }
    }

    fn try_start(&mut self, ring: &Arc<SharedRing>) {
        let Some(config) = self.config else {
            return;
        };
        let ring = Arc::clone(ring);
        match AudioCapture::start(config, move |format: &AudioFormat| sink(ring, format)) {
            Ok(capture) => {
                tracing::info!(
                    kbps = config.bitrate.kbps(),
                    format = ?capture.audio_format(),
                    "audio capture started"
                );
                self.capture = Some(capture);
                self.started_at = Some(Instant::now());
                self.retry_at = None;
            }
            Err(error) => {
                tracing::warn!(error = %ErrorChain(&error), "audio capture failed to start");
                self.schedule_retry();
            }
        }
    }

    fn schedule_retry(&mut self) {
        self.retry_at = Instant::now().checked_add(self.backoff.next_delay());
    }
}

/// The audio thread's sink: pushes every packet, setting the format first
/// on a new ring, which is the only time it allocates.
fn sink(
    ring: Arc<SharedRing>,
    format: &AudioFormat,
) -> impl FnMut(PacketMeta, &[u8]) + Send + use<> {
    let mut producer = Producer::new(format.clone());
    move |meta, payload| {
        if let Err(error) = producer.push(&ring, meta, payload) {
            tracing::warn!(error = %ErrorChain(&error), "audio packet rejected by the ring");
        }
    }
}
