//! `tracing` setup for both processes.
//!
//! On Windows, logs go to daily files under `%LOCALAPPDATA%\blink\logs`
//! through a non-blocking writer, so the capture and encode threads never wait
//! on disk; release builds have no console, so that file is the only output.
//! Debug builds and other hosts also log to stderr. `RUST_LOG` overrides the
//! default `info` filter.
//!
//! A panic is also appended, synchronously, to `crash.log` in the same folder:
//! the non-blocking writer may never flush once the process aborts.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Keeps the log writer flushing; drop it last, when the process exits.
pub(crate) struct LogGuard {
    #[cfg(windows)]
    _worker: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Installs the global subscriber. `prefix` names the log files
/// (`<prefix>.YYYY-MM-DD.log`), so the engine and the settings process write
/// separate files. Failing to create the log folder only loses the file
/// output; it is reported once logging works.
pub(crate) fn init(prefix: &str) -> LogGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = cfg!(any(not(windows), debug_assertions))
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));

    #[cfg(windows)]
    {
        let (file, worker, file_error) = match file::writer(prefix) {
            Ok((writer, worker)) => (Some(writer), Some(worker), None),
            Err(error) => (None, None, Some(error)),
        };
        let file = file.map(|writer| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
        });
        let installed = tracing_subscriber::registry()
            .with(filter)
            .with(stderr)
            .with(file)
            .try_init();
        if installed.is_ok()
            && let Some(error) = file_error
        {
            tracing::warn!(error = format!("{error:#}"), "logging to stderr only");
        }
        LogGuard { _worker: worker }
    }
    #[cfg(not(windows))]
    {
        let _ = prefix;
        // Fails only if a subscriber is already installed, which then keeps working.
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(stderr)
            .try_init();
        LogGuard {}
    }
}

/// The crash report file in the log folder.
const CRASH_FILE: &str = "crash.log";

/// `%LOCALAPPDATA%\blink\logs` on Windows; `None` elsewhere, where logs go
/// to stderr only.
pub(crate) fn log_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(|local| PathBuf::from(local).join("blink").join("logs"))
    } else {
        None
    }
}

/// Installs a panic hook that writes the panic to `crash.log`, logs it, runs
/// the default hook, and then calls `notify` with a message for the user
/// (a dialog on Windows, since release builds have no console).
pub(crate) fn install_panic_hook(notify: fn(&str)) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let entry = crash_entry(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            std::thread::current().name().unwrap_or("unnamed"),
            &info.to_string(),
            &std::backtrace::Backtrace::force_capture().to_string(),
        );
        let file = log_dir().map(|dir| dir.join(CRASH_FILE));
        let written = file.as_deref().is_some_and(|path| append(path, &entry));
        tracing::error!(panic = %info, "blink crashed");
        previous(info);
        let mut message = format!("blink crashed and has to close.\n\n{info}");
        if written && let Some(path) = &file {
            let _ = write!(message, "\n\nDetails are in {}.", path.display());
        }
        notify(&message);
    }));
}

/// One crash report: when, which thread, the panic message, and the
/// backtrace.
fn crash_entry(unix_secs: u64, thread: &str, panic: &str, backtrace: &str) -> String {
    format!("=== crash at unix time {unix_secs} on thread {thread}\n{panic}\n{backtrace}\n")
}

/// Appends `entry` to `path` and flushes it to disk. Returns whether it worked;
/// there is nothing better to do with an error while panicking.
fn append(path: &Path, entry: &str) -> bool {
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return false;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| {
            file.write_all(entry.as_bytes())?;
            file.sync_all()
        })
        .is_ok()
}

#[cfg(windows)]
mod file {
    use anyhow::Context as _;
    use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
    use tracing_appender::rolling::{RollingFileAppender, Rotation};

    /// Daily log files kept before the oldest is deleted.
    const KEPT_FILES: usize = 7;

    pub(super) fn writer(prefix: &str) -> anyhow::Result<(NonBlocking, WorkerGuard)> {
        let dir = super::log_dir().context("LOCALAPPDATA is not set")?;
        let appender = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(prefix)
            .filename_suffix("log")
            .max_log_files(KEPT_FILES)
            .build(&dir)
            .with_context(|| format!("failed to open a log file in {}", dir.display()))?;
        Ok(tracing_appender::non_blocking(appender))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_entry_has_time_thread_and_message() {
        let entry = crash_entry(
            1_700_000_000,
            "blink-save",
            "panicked at x.rs:1:2:\nboom",
            "bt",
        );
        assert_eq!(
            entry,
            "=== crash at unix time 1700000000 on thread blink-save\npanicked at x.rs:1:2:\nboom\nbt\n"
        );
    }

    #[test]
    fn crash_entries_append() {
        let dir = std::env::temp_dir().join(format!("blink-crash-test-{}", std::process::id()));
        let path = dir.join(CRASH_FILE);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(append(&path, "one\n"));
        assert!(append(&path, "two\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
