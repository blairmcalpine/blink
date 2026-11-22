//! Errors from the Win32 wrappers.

/// Errors from the clock, thread, and instance wrappers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SysError {
    /// A Windows API call failed.
    #[cfg(windows)]
    #[error("failed to {context}")]
    Windows {
        /// What was being attempted.
        context: &'static str,
        /// The underlying Windows error.
        #[source]
        source: windows::core::Error,
    },
    /// `QueryPerformanceFrequency` returned a non-positive frequency.
    #[error("invalid QPC frequency {0}")]
    InvalidQpcFrequency(i64),
    /// A sound to play is not a well-formed WAV file image.
    #[cfg(windows)]
    #[error("the sound is not a well-formed WAV image")]
    InvalidWav,
    /// `PlaySoundW` could not play a sound.
    #[cfg(windows)]
    #[error("Windows could not play the sound")]
    PlaySound,
}

/// Attaches context to a `windows` result.
#[cfg(windows)]
pub(crate) trait Context<T> {
    fn context(self, context: &'static str) -> Result<T, SysError>;
}

#[cfg(windows)]
impl<T> Context<T> for windows::core::Result<T> {
    fn context(self, context: &'static str) -> Result<T, SysError> {
        self.map_err(|source| SysError::Windows { context, source })
    }
}
