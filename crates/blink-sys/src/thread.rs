//! Thread priority and background mode for blink's own threads.

use std::marker::PhantomData;

use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END,
    THREAD_PRIORITY, THREAD_PRIORITY_BELOW_NORMAL,
};

use crate::SysError;
use crate::error::Context as _;

/// A scheduling priority for one of blink's threads. Every level sits below
/// the game's threads, per the CPU rules in `docs/ARCHITECTURE.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ThreadPriority {
    /// `THREAD_PRIORITY_BELOW_NORMAL`: capture, encode, audio, the tray
    /// thread, and the clip cutter.
    BelowNormal,
}

/// Sets the calling thread's scheduling priority.
///
/// # Errors
///
/// [`SysError::Windows`] if `SetThreadPriority` fails.
pub fn set_current_thread_priority(priority: ThreadPriority) -> Result<(), SysError> {
    let level = match priority {
        ThreadPriority::BelowNormal => THREAD_PRIORITY_BELOW_NORMAL,
    };
    set_priority(level).context("set the thread priority")
}

fn set_priority(level: THREAD_PRIORITY) -> windows::core::Result<()> {
    // SAFETY: returns a pseudo-handle for the calling thread; no arguments.
    let thread = unsafe { GetCurrentThread() };
    // SAFETY: the pseudo-handle is always valid for the calling thread, and
    // `level` is one of the documented priority or mode constants.
    unsafe { SetThreadPriority(thread, level) }
}

/// The calling thread in background processing mode.
///
/// Background mode gives the thread very low CPU, I/O, and memory priority,
/// for disk-heavy work such as writing a clip. Leaving the mode (on drop)
/// restores the thread's previous priority.
///
/// Not `Send`: the mode belongs to the thread that entered it.
#[derive(Debug)]
pub struct BackgroundMode {
    _thread_bound: PhantomData<*const ()>,
}

impl BackgroundMode {
    /// Enters background mode on the calling thread.
    ///
    /// # Errors
    ///
    /// [`SysError::Windows`] if `SetThreadPriority` fails, for example
    /// because the thread is already in background mode.
    pub fn enter() -> Result<Self, SysError> {
        set_priority(THREAD_MODE_BACKGROUND_BEGIN).context("enter thread background mode")?;
        Ok(Self {
            _thread_bound: PhantomData,
        })
    }
}

impl Drop for BackgroundMode {
    fn drop(&mut self) {
        if let Err(error) = set_priority(THREAD_MODE_BACKGROUND_END) {
            tracing::warn!(error = %crate::ErrorChain(&error), "failed to leave thread background mode");
        }
    }
}
