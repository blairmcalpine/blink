//! A named mutex that keeps a second copy of the engine from starting.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::HSTRING;

use crate::SysError;
use crate::error::Context as _;

/// Ownership of a named per-session mutex, held while the process runs so a
/// second copy can tell that one is already running. Closed on drop.
#[derive(Debug)]
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// Creates (or opens) the named mutex `name`, such as `Local\blink-engine`.
    /// Returns `None` when another process already holds it.
    ///
    /// # Errors
    ///
    /// [`SysError::Windows`] if the mutex cannot be created or opened.
    pub fn acquire(name: &str) -> Result<Option<Self>, SysError> {
        let name = HSTRING::from(name);
        // SAFETY: default security attributes (null), no initial ownership
        // (the mutex is only used for its name), and `name` is a
        // NUL-terminated wide string that outlives the call.
        let handle = unsafe { CreateMutexW(None, false, &name) }
            .context("create the single-instance mutex")?;
        // SAFETY: no preconditions; it reads this thread's last error, which
        // CreateMutexW sets to ERROR_ALREADY_EXISTS when the name exists.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let instance = Self { handle };
        Ok((!existed).then_some(instance))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: `handle` came from CreateMutexW and is closed exactly once.
        if let Err(error) = unsafe { CloseHandle(self.handle) } {
            tracing::warn!(%error, "failed to close the single-instance mutex");
        }
    }
}
