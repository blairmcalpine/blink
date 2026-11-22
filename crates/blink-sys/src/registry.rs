//! Values under `HKEY_CURRENT_USER`, read, written, and deleted by name.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt as _;

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, ERROR_SUCCESS,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_ROUTINE_FLAGS, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::HSTRING;

use crate::SysError;

/// How many times a read retries when the value grows between the size query
/// and the read. Expanding a `REG_EXPAND_SZ` can also outgrow the size the
/// query reported.
const READ_ATTEMPTS: usize = 4;

/// Reads the string value `name` of `HKCU\subkey`. `RRF_RT_REG_SZ` without
/// `RRF_NOEXPAND` also accepts a `REG_EXPAND_SZ` and expands it; asking for
/// `RRF_RT_REG_EXPAND_SZ` too would fail. `None` if the key or the value
/// does not exist.
pub(crate) fn read_string(
    subkey: &str,
    name: &str,
    context: &'static str,
) -> Result<Option<String>, SysError> {
    let Some(bytes) = read(subkey, name, RRF_RT_REG_SZ, context)? else {
        return Ok(None);
    };
    let (pairs, _) = bytes.as_chunks::<2>();
    let wide: Vec<u16> = pairs
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    Ok(Some(String::from_utf16_lossy(&wide)))
}

/// Reads the binary value `name` of `HKCU\subkey`. `None` if the key or the
/// value does not exist.
pub(crate) fn read_binary(
    subkey: &str,
    name: &str,
    context: &'static str,
) -> Result<Option<Vec<u8>>, SysError> {
    read(subkey, name, RRF_RT_REG_BINARY, context)
}

/// Writes `value` as the `REG_SZ` value `name` of `HKCU\subkey`, creating
/// the key if needed.
pub(crate) fn write_string(
    subkey: &str,
    name: &str,
    value: &OsStr,
    context: &'static str,
) -> Result<(), SysError> {
    let subkey = HSTRING::from(subkey);
    let name = HSTRING::from(name);
    let data: Vec<u16> = value.encode_wide().chain([0]).collect();
    let size = std::mem::size_of_val(data.as_slice());
    let size =
        u32::try_from(size).map_err(|_| windows_error(ERROR_INVALID_PARAMETER.into(), context))?;
    // SAFETY: `subkey` and `name` are NUL-terminated wide strings, and `data`
    // is `size` bytes of a NUL-terminated wide string, all alive for the
    // call. HKEY_CURRENT_USER is a predefined key that needs no opening.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &subkey,
            &name,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            size,
        )
    };
    status.ok().map_err(|source| windows_error(source, context))
}

/// Deletes the value `name` of `HKCU\subkey`. A missing key or value is not
/// an error.
pub(crate) fn delete(subkey: &str, name: &str, context: &'static str) -> Result<(), SysError> {
    let subkey = HSTRING::from(subkey);
    let name = HSTRING::from(name);
    // SAFETY: `subkey` and `name` are NUL-terminated wide strings alive for
    // the call. HKEY_CURRENT_USER is a predefined key that needs no opening.
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &subkey, &name) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    status.ok().map_err(|source| windows_error(source, context))
}

/// Reads a value's bytes with `RegGetValueW`, restricted to the types in
/// `flags`.
fn read(
    subkey: &str,
    name: &str,
    flags: REG_ROUTINE_FLAGS,
    context: &'static str,
) -> Result<Option<Vec<u8>>, SysError> {
    let subkey = HSTRING::from(subkey);
    let name = HSTRING::from(name);
    let mut data: Vec<u8> = Vec::new();
    for _ in 0..READ_ATTEMPTS {
        let mut size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let buffer = (!data.is_empty()).then(|| data.as_mut_ptr().cast());
        // SAFETY: `subkey` and `name` are NUL-terminated wide strings alive
        // for the call. `buffer` is either absent (a size query) or points to
        // `data`, which is `size` bytes long, and `size` is a live u32 the
        // call writes the value's length to.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &subkey,
                &name,
                flags,
                None,
                buffer,
                Some(&raw mut size),
            )
        };
        let len = usize::try_from(size)
            .map_err(|_| windows_error(ERROR_INVALID_PARAMETER.into(), context))?;
        match status {
            ERROR_SUCCESS if buffer.is_some() || len == 0 => {
                data.truncate(len);
                return Ok(Some(data));
            }
            ERROR_SUCCESS | ERROR_MORE_DATA => data.resize(len, 0),
            ERROR_FILE_NOT_FOUND => return Ok(None),
            error => return Err(windows_error(error.into(), context)),
        }
    }
    Err(windows_error(ERROR_MORE_DATA.into(), context))
}

const fn windows_error(source: windows::core::Error, context: &'static str) -> SysError {
    SysError::Windows { context, source }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows::Win32::System::Registry::{REG_EXPAND_SZ, RegDeleteKeyW};

    use super::*;

    /// A per-run key under `HKCU\Software`, deleted on drop.
    struct ScratchKey(String);

    impl ScratchKey {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(format!(
                r"Software\blink-test-{}-{nanos}",
                std::process::id()
            ))
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            let key = HSTRING::from(self.0.as_str());
            // SAFETY: `key` is a NUL-terminated wide string alive for the
            // call; the scratch key has values but no subkeys.
            let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &key) };
            assert!(
                status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND,
                "{status:?}"
            );
        }
    }

    #[test]
    fn strings_round_trip_and_delete() {
        let key = ScratchKey::new();
        let value = r#""C:\Users\Ada Lovelace\blink\blink.exe""#;
        write_string(&key.0, "run", OsStr::new(value), "write").unwrap();
        assert_eq!(
            read_string(&key.0, "run", "read").unwrap().as_deref(),
            Some(value)
        );
        delete(&key.0, "run", "delete").unwrap();
        assert_eq!(read_string(&key.0, "run", "read").unwrap(), None);
        delete(&key.0, "run", "delete again").unwrap();
    }

    #[test]
    fn missing_keys_and_values_read_as_none() {
        let key = ScratchKey::new();
        assert_eq!(read_string(&key.0, "absent", "read").unwrap(), None);
        assert_eq!(read_binary(&key.0, "absent", "read").unwrap(), None);
        delete(&key.0, "absent", "delete").unwrap();
        write_string(&key.0, "present", OsStr::new("x"), "write").unwrap();
        assert_eq!(read_string(&key.0, "absent", "read").unwrap(), None);
    }

    #[test]
    fn expandable_strings_read_expanded() {
        let key = ScratchKey::new();
        let subkey = HSTRING::from(key.0.as_str());
        let name = HSTRING::from("expand");
        let data: Vec<u16> = OsStr::new(r"%SystemRoot%\blink.exe")
            .encode_wide()
            .chain([0])
            .collect();
        let size = u32::try_from(std::mem::size_of_val(data.as_slice())).unwrap();
        // SAFETY: as in `write_string`, with `data` a NUL-terminated wide
        // string of `size` bytes.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &subkey,
                &name,
                REG_EXPAND_SZ.0,
                Some(data.as_ptr().cast()),
                size,
            )
        };
        status.ok().unwrap();
        let read = read_string(&key.0, "expand", "read").unwrap().unwrap();
        assert!(!read.contains('%'), "{read}");
        assert!(read.to_ascii_lowercase().ends_with(r"\blink.exe"), "{read}");
    }

    #[test]
    fn a_string_is_not_read_as_binary() {
        let key = ScratchKey::new();
        write_string(&key.0, "text", OsStr::new("x"), "write").unwrap();
        read_binary(&key.0, "text", "read").unwrap_err();
    }
}
