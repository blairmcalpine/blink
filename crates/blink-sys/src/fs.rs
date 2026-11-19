//! File renames that survive antivirus and the search indexer.
//!
//! Both briefly open freshly written files without `FILE_SHARE_DELETE`, so a
//! rename right after a write can fail with a sharing violation. The renames
//! here retry that for about 300 ms on Windows.

use std::io;
use std::path::Path;

/// Renames `from` to `to`, replacing `to` if it exists.
///
/// # Errors
///
/// The rename's error, once a sharing violation outlasts the retries.
pub fn rename_replacing(from: &Path, to: &Path) -> io::Result<()> {
    retry_sharing_violations(|| std::fs::rename(from, to))
}

/// Renames `from` to `to`, failing with [`io::ErrorKind::AlreadyExists`]
/// instead of replacing `to` if it exists. Both must be on the same volume.
///
/// # Errors
///
/// [`io::ErrorKind::AlreadyExists`] if `to` exists, or the rename's error
/// once a sharing violation outlasts the retries.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    retry_sharing_violations(|| imp::rename_no_replace(from, to))
}

#[cfg(not(windows))]
fn retry_sharing_violations(mut rename: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    rename()
}

#[cfg(windows)]
fn retry_sharing_violations(mut rename: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    use std::time::Duration;

    const RETRIES: u32 = 5;
    const BACKOFF: Duration = Duration::from_millis(20);
    const ERROR_SHARING_VIOLATION: i32 = 32;

    let mut attempt = 0;
    loop {
        match rename() {
            Err(err)
                if attempt < RETRIES
                    && (err.kind() == io::ErrorKind::PermissionDenied
                        || err.raw_os_error() == Some(ERROR_SHARING_VIOLATION)) =>
            {
                attempt += 1;
                std::thread::sleep(BACKOFF.saturating_mul(attempt));
            }
            result => return result,
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::path::Path;

    use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW};
    use windows::core::{HRESULT, HSTRING};

    pub(super) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let from = HSTRING::from(from);
        let to = HSTRING::from(to);
        // SAFETY: both arguments are NUL-terminated wide strings that live
        // until the call returns. Without `MOVEFILE_REPLACE_EXISTING` the
        // move fails if `to` exists.
        unsafe { MoveFileExW(&from, &to, MOVE_FILE_FLAGS(0)) }.map_err(io_error)
    }

    /// Unwraps a Win32 error code from its `HRESULT` so [`io::Error::kind`]
    /// recognizes it.
    fn io_error(error: windows::core::Error) -> io::Error {
        let code = error.code();
        let win32 = code.0.cast_unsigned() & 0xFFFF;
        match i32::try_from(win32) {
            Ok(raw) if HRESULT::from_win32(win32) == code => io::Error::from_raw_os_error(raw),
            _ => io::Error::other(error),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::path::Path;

    /// A hard link fails if `to` exists, which makes the check and the
    /// rename one atomic step.
    pub(super) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::hard_link(from, to)?;
        std::fs::remove_file(from)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn no_replace_refuses_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"old").unwrap();
        let error = rename_no_replace(&from, &to).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&to).unwrap(), b"old");
        assert_eq!(fs::read(&from).unwrap(), b"new");
    }

    #[test]
    fn no_replace_moves_to_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::write(&from, b"new").unwrap();
        rename_no_replace(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
    }

    #[test]
    fn replacing_overwrites_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"old").unwrap();
        rename_replacing(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
    }
}
