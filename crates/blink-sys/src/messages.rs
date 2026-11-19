//! The message pump for the tray and hotkey thread.

use std::time::Duration;

use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    TranslateMessage, WM_QUIT,
};

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
