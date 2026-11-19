//! DXGI Desktop Duplication capture for blink.
//!
//! `create_device` makes the hardware D3D11 device that capture and the
//! encoder share, and `DesktopDuplicator` turns one output into a stream of
//! `CaptureEvent`s: BGRA textures on that device, decimated to the target
//! frame rate and stamped with a `blink_ring` `Timestamp` on the shared QPC
//! timeline. Frames never leave
//! the GPU. The duplicator recovers from `DXGI_ERROR_ACCESS_LOST` on display
//! mode changes and reports size changes.
//!
//! Windows only. On other hosts the crate is a stub: [`create_device`] exists
//! but always returns `CaptureError::Unsupported`, so callers report that
//! capture is unavailable instead of silently doing nothing. Every `unsafe`
//! block needs a `// SAFETY:` comment and may contain only one unsafe
//! operation.

// The internal helper modules below are direct children of the crate root, so
// any restricted visibility on their items (`pub(crate)` or `pub(super)`, which
// are the same here) trips `clippy::redundant_pub_crate`, while plain `pub`
// trips the denied `unreachable_pub`. `expect` documents that conflict.
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod decision;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod rotation;
#[cfg(any(windows, test))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
)]
mod schedule;

#[cfg(windows)]
mod device;
#[cfg(windows)]
mod duplicator;
#[cfg_attr(
    windows,
    expect(
        clippy::redundant_pub_crate,
        reason = "restricted visibility in a root-level module conflicts with the denied unreachable_pub"
    )
)]
mod error;
#[cfg(not(windows))]
mod unsupported;

#[cfg(windows)]
pub use device::{AdapterInfo, CaptureDevice, create_device};
#[cfg(windows)]
pub use duplicator::{CaptureEvent, CapturedFrame, DesktopDuplicator, TEXTURE_POOL_SIZE};
pub use error::CaptureError;
#[cfg(not(windows))]
pub use unsupported::{CaptureDevice, create_device};
