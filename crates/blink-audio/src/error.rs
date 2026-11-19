//! Audio capture errors.

/// Errors from starting or running audio capture.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AudioError {
    /// Audio capture only exists on Windows; this is a stub build.
    #[error("audio capture is only available on Windows")]
    Unsupported,
    /// A Windows API call failed.
    #[cfg(windows)]
    #[error("{context}")]
    Windows {
        /// What was being attempted.
        context: &'static str,
        /// The underlying COM error.
        #[source]
        source: windows::core::Error,
    },
    /// The QPC clock could not be read.
    #[cfg(windows)]
    #[error(transparent)]
    Clock(#[from] blink_sys::SysError),
    /// The endpoint accepted none of the capture formats blink can consume
    /// (48 kHz stereo float or 16-bit PCM, converted by the audio engine or
    /// native), and resampling is out of scope.
    #[error(
        "unsupported endpoint mix format: {sample_rate} Hz, {channels} channels, {bits} bits (tag {tag:#06x})"
    )]
    UnsupportedMixFormat {
        /// `WAVEFORMATEX::wFormatTag` of the mix format.
        tag: u16,
        /// Mix format sample rate.
        sample_rate: u32,
        /// Mix format channel count.
        channels: u16,
        /// Mix format container bits per sample.
        bits: u16,
    },
    /// The AAC encoder's output type does not describe 48 kHz stereo AAC-LC.
    #[error("AAC encoder reported an unexpected AudioSpecificConfig {0:02x?}")]
    InvalidAudioSpecificConfig(Vec<u8>),
    /// The encoder produced a frame larger than its output buffer, or a PCM
    /// chunk did not fit an input buffer.
    #[error("AAC encoder buffer of {0} bytes does not fit the expected layout")]
    InvalidEncoderBuffer(usize),
    /// The audio thread could not be spawned.
    #[error("failed to spawn the audio thread")]
    Spawn(#[source] std::io::Error),
    /// The audio thread exited before reporting whether startup succeeded.
    #[error("audio thread exited during startup")]
    StartupLost,
    /// The audio thread panicked.
    #[error("audio thread panicked")]
    ThreadPanicked,
}
