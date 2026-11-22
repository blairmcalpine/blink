//! Non-Windows stand-in for the audio thread: capture is unavailable.

use blink_ring::media::AudioFormat;

use crate::{AudioConfig, AudioError, AudioSink};

/// A running audio capture thread. On this host it cannot be created:
/// [`AudioCapture::start`] always fails with [`AudioError::Unsupported`].
#[derive(Debug)]
pub struct AudioCapture {
    format: AudioFormat,
}

impl AudioCapture {
    /// Starts capture. Always fails on non-Windows hosts, without calling
    /// `make_sink`.
    ///
    /// # Errors
    ///
    /// [`AudioError::Unsupported`].
    pub fn start<S, F>(config: AudioConfig, make_sink: F) -> Result<Self, AudioError>
    where
        S: AudioSink,
        F: FnOnce(&AudioFormat) -> S + Send + 'static,
    {
        drop((config, make_sink));
        Err(AudioError::Unsupported)
    }

    /// Format of the AAC stream.
    #[must_use]
    pub const fn audio_format(&self) -> &AudioFormat {
        &self.format
    }

    /// Whether the audio thread has exited.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        true
    }

    /// Stops capture and reports how the audio thread ended.
    ///
    /// # Errors
    ///
    /// Never fails on this host.
    pub fn stop(self) -> Result<(), AudioError> {
        drop(self);
        Ok(())
    }
}
