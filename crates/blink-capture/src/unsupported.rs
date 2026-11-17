//! The non-Windows stub: same entry point, always reports that capture is
//! unavailable.

use std::convert::Infallible;

use crate::CaptureError;

/// A capture device. Uninhabited off Windows: [`create_device`] never returns
/// one.
#[derive(Debug)]
pub struct CaptureDevice(Infallible);

/// Always fails with [`CaptureError::Unsupported`]: desktop capture is only
/// implemented on Windows.
///
/// # Errors
///
/// Always [`CaptureError::Unsupported`].
pub const fn create_device() -> Result<CaptureDevice, CaptureError> {
    Err(CaptureError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_capture_unavailable() {
        assert!(matches!(create_device(), Err(CaptureError::Unsupported)));
    }
}
