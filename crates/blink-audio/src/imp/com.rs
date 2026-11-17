//! COM and Media Foundation lifetimes, and `CoTaskMemAlloc`-owned pointers.

use std::marker::PhantomData;

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_LITE, MFShutdown, MFStartup};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::core::PWSTR;

use super::Context;
use crate::AudioError;

/// COM (multithreaded apartment) and Media Foundation, initialized on the
/// current thread and shut down in reverse order on drop.
///
/// Create it first on the audio thread and drop it last: every COM object the
/// thread creates must be released before `MFShutdown` and `CoUninitialize`.
/// It is `!Send` because both calls are per-thread.
pub(super) struct MediaRuntime {
    media_foundation: bool,
    _thread_bound: PhantomData<*const ()>,
}

impl MediaRuntime {
    /// Joins the MTA and starts Media Foundation without its socket library
    /// (`MFSTARTUP_LITE`; the in-process encoder needs no network sources).
    pub(super) fn init() -> Result<Self, AudioError> {
        // SAFETY: `CoInitializeEx` has no pointer arguments (the reserved one is
        // `None`). Success, including `S_FALSE` when this thread already joined
        // the MTA, must be balanced by `CoUninitialize`, which `Drop` does;
        // failure (for example `RPC_E_CHANGED_MODE`) returns before `Self`
        // exists, so nothing is uninitialized that was not initialized.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .context("join the COM multithreaded apartment")?;
        let mut runtime = Self {
            media_foundation: false,
            _thread_bound: PhantomData,
        };
        // SAFETY: COM is initialized on this thread (above). `MFStartup` takes
        // only integer arguments, and a success is balanced by `MFShutdown` in
        // `Drop` because `media_foundation` is set only on success.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.context("start Media Foundation")?;
        runtime.media_foundation = true;
        Ok(runtime)
    }
}

impl Drop for MediaRuntime {
    fn drop(&mut self) {
        if self.media_foundation {
            // SAFETY: balances the successful `MFStartup` in `init` on the same
            // thread (`Self` is `!Send`), after the caller released its Media
            // Foundation objects.
            if let Err(error) = unsafe { MFShutdown() } {
                tracing::debug!(?error, "MFShutdown failed");
            }
        }
        // SAFETY: balances the successful `CoInitializeEx` in `init` on the
        // same thread.
        unsafe { CoUninitialize() };
    }
}

/// A pointer allocated with `CoTaskMemAlloc` that this value owns and frees.
pub(super) struct CoTaskMem<T>(*mut T);

impl<T> CoTaskMem<T> {
    /// Takes ownership of `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr` must be null or allocated with `CoTaskMemAlloc`, and nothing else
    /// may free it.
    pub(super) const unsafe fn new(ptr: *mut T) -> Self {
        Self(ptr)
    }

    /// The owned pointer, valid until `self` is dropped.
    pub(super) const fn as_ptr(&self) -> *mut T {
        self.0
    }
}

impl CoTaskMem<u16> {
    /// Takes ownership of a string returned by a COM method such as
    /// `IMMDevice::GetId`.
    ///
    /// # Safety
    ///
    /// As for [`CoTaskMem::new`]; additionally the string must be null or
    /// NUL-terminated.
    pub(super) const unsafe fn from_pwstr(string: PWSTR) -> Self {
        Self(string.0)
    }

    /// The string's UTF-16 code units without the terminator. Empty if null.
    pub(super) fn as_wide(&self) -> &[u16] {
        if self.0.is_null() {
            return &[];
        }
        // SAFETY: the pointer is non-null and, per `from_pwstr`'s contract,
        // points to a NUL-terminated string, which `len` scans.
        let len = unsafe { PWSTR(self.0).len() };
        // SAFETY: the `len` code units before the terminator are initialized
        // and stay allocated until `self` is dropped, which outlives the
        // returned borrow; nothing mutates them.
        unsafe { std::slice::from_raw_parts(self.0, len) }
    }
}

impl<T> Drop for CoTaskMem<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from `CoTaskMemAlloc` and this value is
            // its only owner (`new`'s contract), so it is freed exactly once.
            unsafe { CoTaskMemFree(Some(self.0.cast_const().cast())) };
        }
    }
}
