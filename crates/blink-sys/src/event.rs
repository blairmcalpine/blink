//! A named auto-reset event, which another process can set to wake this one.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::core::HSTRING;

use crate::SysError;
use crate::error::Context as _;

/// A handle to a named per-session auto-reset event. Closed on drop.
///
/// Every process that creates the same name shares one event, so one can
/// [`set`](Self::set) it and another [`take`](Self::take) it. The event lives
/// while any handle to it is open.
#[derive(Debug)]
pub struct NamedEvent {
    handle: HANDLE,
}

impl NamedEvent {
    /// Creates (or opens) the named event `name`, such as
    /// `Local\blink-open-settings`, initially not set.
    ///
    /// # Errors
    ///
    /// [`SysError::Windows`] if the event cannot be created or opened.
    pub fn create(name: &str) -> Result<Self, SysError> {
        let name = HSTRING::from(name);
        // SAFETY: default security attributes (null), auto-reset, initially
        // not set, and `name` is a NUL-terminated wide string that outlives
        // the call.
        let handle =
            unsafe { CreateEventW(None, false, false, &name) }.context("create a named event")?;
        Ok(Self { handle })
    }

    /// Sets the event, waking whichever process takes it next.
    ///
    /// # Errors
    ///
    /// [`SysError::Windows`] if the event cannot be set.
    pub fn set(&self) -> Result<(), SysError> {
        // SAFETY: `handle` is an open event handle owned by `self`.
        unsafe { SetEvent(self.handle) }.context("set a named event")
    }

    /// Whether the event was set since the last call, without waiting. Taking
    /// it resets it.
    #[must_use]
    pub fn take(&self) -> bool {
        // SAFETY: `handle` is an open event handle owned by `self`; a zero
        // timeout only polls it, and the wait resets an auto-reset event.
        let wait = unsafe { WaitForSingleObject(self.handle, 0) };
        wait == WAIT_OBJECT_0
    }
}

impl Drop for NamedEvent {
    fn drop(&mut self) {
        // SAFETY: `handle` came from CreateEventW and is closed exactly once.
        if let Err(error) = unsafe { CloseHandle(self.handle) } {
            tracing::warn!(
                error = %crate::ErrorChain(&error),
                "failed to close a named event"
            );
        }
    }
}
