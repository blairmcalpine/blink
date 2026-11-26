//! Capture errors.

#[cfg(windows)]
use windows::Win32::Foundation::E_ACCESSDENIED;
#[cfg(windows)]
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
#[cfg(windows)]
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_HUNG,
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_INVALID_CALL,
    DXGI_ERROR_MODE_CHANGE_IN_PROGRESS, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
    DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED,
};
#[cfg(windows)]
use windows::core::HRESULT;

/// Errors from device creation and desktop duplication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CaptureError {
    /// Desktop capture is only implemented on Windows; this build has no
    /// capture backend.
    #[cfg(not(windows))]
    #[error("desktop capture is only supported on Windows")]
    Unsupported,
    /// A Windows API call failed.
    #[cfg(windows)]
    #[error("failed to {context}")]
    Windows {
        /// What was being attempted.
        context: &'static str,
        /// The underlying COM error.
        #[source]
        source: windows::core::Error,
    },
    /// The D3D11 device was removed, reset, or hung. Capture cannot continue on
    /// it; the caller must recreate the device, the encoder, and the
    /// duplicator.
    #[cfg(windows)]
    #[error("D3D11 device lost while trying to {context}")]
    DeviceLost {
        /// What was being attempted.
        context: &'static str,
        /// The underlying COM error.
        #[source]
        source: windows::core::Error,
    },
    /// No output with this index is attached to the device's adapter.
    #[cfg(windows)]
    #[error("output {index} not found on the capture device's adapter")]
    OutputNotFound {
        /// The requested output index.
        index: u32,
    },
    /// No adapter has an output at desktop origin (0, 0).
    #[cfg(windows)]
    #[error("no adapter owns the primary desktop output")]
    NoPrimaryOutput,
    /// The QPC clock could not be read.
    #[cfg(windows)]
    #[error(transparent)]
    Clock(#[from] blink_sys::SysError),
    /// A Windows call reported success but returned no object.
    #[cfg(windows)]
    #[error("{call} succeeded but returned no {what}")]
    NullOutput {
        /// The Windows function.
        call: &'static str,
        /// The output that was missing.
        what: &'static str,
    },
    /// The duplicator's own state broke an invariant it relies on. A bug in
    /// blink, not a Windows failure.
    #[cfg(windows)]
    #[error("internal capture error: {0}")]
    Invariant(&'static str),
    /// The duplicated desktop image is not `DXGI_FORMAT_B8G8R8A8_UNORM`.
    #[cfg(windows)]
    #[error("unsupported desktop image format {0:?}")]
    UnsupportedFormat(DXGI_FORMAT),
    /// The D3D11 video processor cannot read (or write) a format the NV12
    /// conversion needs.
    #[cfg(windows)]
    #[error(
        "the video processor does not support {format:?} as {}",
        if *input { "input" } else { "output" }
    )]
    VideoProcessorFormat {
        /// The format.
        format: DXGI_FORMAT,
        /// Whether it was needed as input rather than output.
        input: bool,
    },
}

#[cfg(windows)]
impl CaptureError {
    /// Whether recreating the duplication session can clear this error:
    /// mode changes, desktop switches (UAC, lock screen), and session
    /// reconnects.
    pub(crate) fn is_session_lost(&self) -> bool {
        match self {
            Self::Windows { source, .. } => is_session_lost(source.code()),
            Self::OutputNotFound { .. } => true,
            _ => false,
        }
    }

    /// `DXGI_ERROR_UNSUPPORTED`: fatal on first duplication (wrong adapter,
    /// basic display driver), but `DuplicateOutput` can also return it during
    /// a mode transition.
    pub(crate) fn is_unsupported(&self) -> bool {
        matches!(self, Self::Windows { source, .. } if source.code() == DXGI_ERROR_UNSUPPORTED)
    }
}

/// HRESULTs that mean the duplication session ended and should be recreated.
///
/// `DXGI_ERROR_INVALID_CALL` is included because `AcquireNextFrame` returns it
/// after a desktop switch even though the frame was released. Our release
/// guard rules out the documented cause (acquiring while holding a frame).
#[cfg(windows)]
pub(crate) fn is_session_lost(code: HRESULT) -> bool {
    [
        DXGI_ERROR_ACCESS_LOST,
        DXGI_ERROR_INVALID_CALL,
        DXGI_ERROR_MODE_CHANGE_IN_PROGRESS,
        DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
        DXGI_ERROR_SESSION_DISCONNECTED,
        DXGI_ERROR_ACCESS_DENIED,
        E_ACCESSDENIED,
    ]
    .contains(&code)
}

#[cfg(windows)]
fn is_device_lost(code: HRESULT) -> bool {
    [
        DXGI_ERROR_DEVICE_REMOVED,
        DXGI_ERROR_DEVICE_RESET,
        DXGI_ERROR_DEVICE_HUNG,
    ]
    .contains(&code)
}

/// Attaches context to a `windows` result, routing device loss to
/// [`CaptureError::DeviceLost`].
#[cfg(windows)]
pub(crate) trait Context<T> {
    fn context(self, context: &'static str) -> Result<T, CaptureError>;
}

#[cfg(windows)]
impl<T> Context<T> for windows::core::Result<T> {
    fn context(self, context: &'static str) -> Result<T, CaptureError> {
        self.map_err(|source| {
            if is_device_lost(source.code()) {
                CaptureError::DeviceLost { context, source }
            } else {
                CaptureError::Windows { context, source }
            }
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use windows::Win32::Foundation::E_OUTOFMEMORY;
    use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAIT_TIMEOUT;

    use super::*;

    fn failed(code: HRESULT) -> windows::core::Result<()> {
        Err(windows::core::Error::from(code))
    }

    #[test]
    fn session_loss_codes_are_recoverable() {
        for code in [
            DXGI_ERROR_ACCESS_LOST,
            DXGI_ERROR_INVALID_CALL,
            E_ACCESSDENIED,
        ] {
            assert!(is_session_lost(code));
            assert!(failed(code).context("test").unwrap_err().is_session_lost());
        }
        assert!(!is_session_lost(DXGI_ERROR_WAIT_TIMEOUT));
        assert!(!is_session_lost(E_OUTOFMEMORY));
        assert!(!is_session_lost(DXGI_ERROR_UNSUPPORTED));
    }

    #[test]
    fn device_loss_is_routed_to_its_own_variant() {
        for code in [
            DXGI_ERROR_DEVICE_REMOVED,
            DXGI_ERROR_DEVICE_RESET,
            DXGI_ERROR_DEVICE_HUNG,
        ] {
            let error = failed(code).context("test").unwrap_err();
            assert!(matches!(error, CaptureError::DeviceLost { .. }));
            assert!(!error.is_session_lost());
        }
    }

    #[test]
    fn unsupported_is_distinguished() {
        let error = failed(DXGI_ERROR_UNSUPPORTED).context("test").unwrap_err();
        assert!(error.is_unsupported());
        assert!(!error.is_session_lost());
        assert!(CaptureError::OutputNotFound { index: 1 }.is_session_lost());
    }
}
