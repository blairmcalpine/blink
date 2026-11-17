//! Small safe wrappers over Win32 calls that `blink-app`, which forbids
//! `unsafe`, needs on its own threads: the shared QPC clock, thread priority
//! and background mode, a single-instance mutex, and a message pump for the
//! tray and hotkey thread.

use std::marker::PhantomData;
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    THREAD_MODE_BACKGROUND_END, THREAD_PRIORITY, THREAD_PRIORITY_BELOW_NORMAL,
    THREAD_PRIORITY_LOWEST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    TranslateMessage, WM_QUIT,
};
use windows::core::HSTRING;

use crate::clock::QpcClock;
use crate::error::{CaptureError, Context};

/// The current time on the shared QPC timeline, in 100 ns ticks: the same
/// clock as [`CapturedFrame::timestamp`](crate::CapturedFrame::timestamp)
/// and blink-audio's packet timestamps.
///
/// # Errors
///
/// [`CaptureError::InvalidQpcFrequency`] or [`CaptureError::Windows`] if the
/// counter cannot be read, which Windows documents as impossible on XP and
/// later.
pub fn qpc_now() -> Result<i64, CaptureError> {
    static CLOCK: OnceLock<Result<QpcClock, i64>> = OnceLock::new();
    let clock = CLOCK.get_or_init(|| {
        let mut frequency = 0;
        // SAFETY: `frequency` is a valid, writable i64 for the call.
        let read = unsafe { QueryPerformanceFrequency(&raw mut frequency) };
        read.map_err(|_| frequency)
            .and_then(|()| QpcClock::new(frequency).ok_or(frequency))
    });
    let clock = (*clock).map_err(CaptureError::InvalidQpcFrequency)?;
    let mut counter = 0;
    // SAFETY: `counter` is a valid, writable i64 for the call.
    unsafe { QueryPerformanceCounter(&raw mut counter) }.context("read the QPC counter")?;
    Ok(clock.to_ticks(counter))
}

/// A scheduling priority for one of blink's threads. Both levels sit below
/// the game's threads, per the CPU rules in `docs/ARCHITECTURE.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPriority {
    /// `THREAD_PRIORITY_BELOW_NORMAL`: capture, encode, and the tray thread.
    BelowNormal,
    /// `THREAD_PRIORITY_LOWEST`: background work such as saving clips.
    Lowest,
}

/// Sets the calling thread's scheduling priority.
///
/// # Errors
///
/// [`CaptureError::Windows`] if `SetThreadPriority` fails.
pub fn set_current_thread_priority(priority: ThreadPriority) -> Result<(), CaptureError> {
    let level: THREAD_PRIORITY = match priority {
        ThreadPriority::BelowNormal => THREAD_PRIORITY_BELOW_NORMAL,
        ThreadPriority::Lowest => THREAD_PRIORITY_LOWEST,
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
    /// [`CaptureError::Windows`] if `SetThreadPriority` fails, for example
    /// because the thread is already in background mode.
    pub fn enter() -> Result<Self, CaptureError> {
        set_priority(THREAD_MODE_BACKGROUND_BEGIN).context("enter thread background mode")?;
        Ok(Self {
            _thread_bound: PhantomData,
        })
    }
}

impl Drop for BackgroundMode {
    fn drop(&mut self) {
        if let Err(error) = set_priority(THREAD_MODE_BACKGROUND_END) {
            tracing::warn!(%error, "failed to leave thread background mode");
        }
    }
}

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
    /// [`CaptureError::Windows`] if the mutex cannot be created or opened.
    pub fn acquire(name: &str) -> Result<Option<Self>, CaptureError> {
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

/// Dispatches the calling thread's queued window messages, waits up to
/// `timeout` for more, and dispatches those too.
///
/// Windows created on this thread (the tray icon's and the global hotkey's
/// hidden windows) only receive their messages while the thread pumps, so a
/// thread that owns them calls this in a loop. Returns `false` once `WM_QUIT`
/// has been received.
#[must_use]
pub fn pump_messages(timeout: Duration) -> bool {
    if !dispatch_queued() {
        return false;
    }
    // INFINITE is u32::MAX; anything that long is capped just below it.
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: no handles are passed, so the call only waits on this thread's
    // message queue for at most `millis`. The result (a message arrived, or
    // the wait timed out) is irrelevant: the queue is drained either way.
    let _wake = unsafe { MsgWaitForMultipleObjects(None, false, millis, QS_ALLINPUT) };
    dispatch_queued()
}

/// Dispatches every message already in the queue. `false` on `WM_QUIT`.
fn dispatch_queued() -> bool {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid, writable MSG; a `None` window retrieves
        // messages for any window of this thread plus thread messages.
        let got = unsafe { PeekMessageW(&raw mut msg, None, 0, 0, PM_REMOVE) };
        if !got.as_bool() {
            return true;
        }
        if msg.message == WM_QUIT {
            return false;
        }
        // SAFETY: `msg` was just filled in by PeekMessageW. The return value
        // only says whether a character message was posted.
        let _translated = unsafe { TranslateMessage(&raw const msg) };
        // SAFETY: as above; the window procedure's result is not needed.
        let _result = unsafe { DispatchMessageW(&raw const msg) };
    }
}
