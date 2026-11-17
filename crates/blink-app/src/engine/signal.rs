//! A stop flag that a sleeping thread can wait on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

/// Tells a worker thread to stop, and wakes it from [`StopSignal::wait`].
#[derive(Default)]
pub(super) struct StopSignal {
    stopped: AtomicBool,
    lock: Mutex<()>,
    wake: Condvar,
}

impl StopSignal {
    pub(super) fn stop(&self) {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.stopped.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    /// Cheap enough to check once per captured frame.
    pub(super) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Sleeps for `timeout` or until stopped. Returns whether it was stopped.
    pub(super) fn wait(&self, timeout: Duration) -> bool {
        drop(
            self.wake
                .wait_timeout_while(
                    self.lock.lock().unwrap_or_else(PoisonError::into_inner),
                    timeout,
                    |()| !self.is_stopped(),
                )
                .unwrap_or_else(PoisonError::into_inner),
        );
        self.is_stopped()
    }
}
