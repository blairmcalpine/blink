//! Starting blink when the user signs in to Windows.
//!
//! The per-user Run key is the only record of the setting. The installer
//! writes blink's value there when the user picks the option, the settings
//! window writes or deletes the same value, and uninstalling deletes it.
//! Nothing in the config file mirrors it, so the two cannot disagree.
//!
//! Windows' own switch for startup apps (Settings > Apps > Startup, or Task
//! Manager's Startup tab) leaves the Run value alone and records a disabled
//! entry under `Explorer\StartupApproved\Run` instead, which makes Windows
//! skip the Run value. `read_autostart` reports that as
//! [`Autostart::DisabledByWindows`], and `write_autostart` deletes the entry
//! either way, so after a change the Run value alone decides again.

#[cfg(any(windows, test))]
use std::ffi::{OsStr, OsString};
#[cfg(any(windows, test))]
use std::path::Path;

#[cfg(windows)]
use crate::{SysError, registry};

/// The per-user key whose values Windows starts at sign-in.
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Where Windows' startup-apps switch records that a Run value is disabled.
#[cfg(windows)]
const STARTUP_APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// blink's value name under both keys. The installer uses the same name.
#[cfg(windows)]
const VALUE_NAME: &str = "blink";

/// Whether Windows starts this copy of blink when the user signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autostart {
    /// Nothing starts this copy at sign-in: there is no Run value, or it
    /// starts a different file, such as a copy of blink that has moved.
    Off,
    /// The Run value starts this copy.
    On,
    /// The Run value starts this copy, but the user turned blink off in
    /// Windows' startup apps, so Windows skips it.
    DisabledByWindows,
}

/// Reads whether Windows starts `exe`, normally
/// [`std::env::current_exe`], at sign-in.
///
/// # Errors
///
/// [`SysError::Windows`] if the Run value exists but cannot be read.
#[cfg(windows)]
pub fn read_autostart(exe: &Path) -> Result<Autostart, SysError> {
    let run = registry::read_string(RUN_KEY, VALUE_NAME, "read blink's Run value")?;
    // Windows writes this entry, not blink. One it cannot read (such as a
    // value of another type) is treated as approved rather than hiding the
    // setting behind an error.
    let approval = registry::read_binary(
        STARTUP_APPROVED_KEY,
        VALUE_NAME,
        "read blink's startup-apps entry",
    )
    .unwrap_or_else(|error| {
        tracing::warn!(error = %crate::ErrorChain(&error), "ignoring blink's startup-apps entry");
        None
    });
    Ok(classify(run.as_deref(), approval.as_deref(), exe))
}

/// Makes Windows start `exe` at sign-in, or stop starting blink. Either way
/// it also clears blink's entry in Windows' startup apps, so turning the
/// setting on here overrides an earlier "off" there.
///
/// # Errors
///
/// [`SysError::Windows`] if a registry value cannot be written or deleted.
#[cfg(windows)]
pub fn write_autostart(exe: &Path, enabled: bool) -> Result<(), SysError> {
    if enabled {
        let command = run_command(exe.as_os_str());
        registry::write_string(RUN_KEY, VALUE_NAME, &command, "write blink's Run value")?;
    } else {
        registry::delete(RUN_KEY, VALUE_NAME, "delete blink's Run value")?;
    }
    registry::delete(
        STARTUP_APPROVED_KEY,
        VALUE_NAME,
        "delete blink's startup-apps entry",
    )
}

/// The Run value that starts `exe`: its path in quotes, as the installer
/// writes it, so a path with spaces is not split.
#[cfg(any(windows, test))]
fn run_command(exe: &OsStr) -> OsString {
    let mut command = OsString::from("\"");
    command.push(exe);
    command.push("\"");
    command
}

/// The program a Run value starts: the quoted path, or without quotes,
/// everything before the first space, as `CreateProcess` reads it.
#[cfg(any(windows, test))]
fn program(command: &str) -> &str {
    let command = command.trim_start();
    command.strip_prefix('"').map_or_else(
        || {
            command
                .split(char::is_whitespace)
                .next()
                .unwrap_or_default()
        },
        |quoted| quoted.split('"').next().unwrap_or_default(),
    )
}

/// Whether `program` names the file `exe`. Windows paths ignore case and
/// accept either slash; failing that, both must resolve to the same file,
/// which covers short names and `..`.
#[cfg(any(windows, test))]
fn same_program(program: &str, exe: &Path) -> bool {
    let fold = |text: &str| -> Vec<char> {
        text.chars()
            .map(|c| if c == '/' { '\\' } else { c })
            .flat_map(char::to_uppercase)
            .collect()
    };
    if !program.is_empty() && fold(program) == fold(&exe.to_string_lossy()) {
        return true;
    }
    match (std::fs::canonicalize(program), std::fs::canonicalize(exe)) {
        (Ok(program), Ok(exe)) => program == exe,
        _ => false,
    }
}

/// Whether a `StartupApproved\Run` entry lets the Run value start. The first
/// byte is a flag word whose low bit marks the entry disabled (`03` or `07`
/// off, `02` or `06` on); the rest is when it was last turned off.
#[cfg(any(windows, test))]
fn approved(entry: &[u8]) -> bool {
    entry.first().is_none_or(|flags| flags & 1 == 0)
}

/// The state for a Run value and a startup-apps entry, as read, for `exe`.
#[cfg(any(windows, test))]
fn classify(run: Option<&str>, approval: Option<&[u8]>, exe: &Path) -> Autostart {
    match run {
        Some(command) if same_program(program(command), exe) => {
            if approval.is_none_or(approved) {
                Autostart::On
            } else {
                Autostart::DisabledByWindows
            }
        }
        _ => Autostart::Off,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const INSTALLED: &str = r"C:\Users\Ada Lovelace\AppData\Local\Programs\blink\blink.exe";

    #[test]
    fn run_command_quotes_the_path() {
        assert_eq!(
            run_command(OsStr::new(INSTALLED)),
            OsString::from(format!("\"{INSTALLED}\""))
        );
    }

    #[test]
    fn program_reads_quoted_and_bare_commands() {
        let quoted = format!("\"{INSTALLED}\"");
        assert_eq!(program(&quoted), INSTALLED);
        assert_eq!(program(&format!("  {quoted} --flag")), INSTALLED);
        assert_eq!(program(r"C:\blink\blink.exe --flag"), r"C:\blink\blink.exe");
        assert_eq!(program(r"C:\blink\blink.exe"), r"C:\blink\blink.exe");
        assert_eq!(program("\"unterminated"), "unterminated");
        assert_eq!(program(""), "");
        assert_eq!(program("   "), "");
    }

    #[test]
    fn program_round_trips_the_run_command() {
        let command = run_command(OsStr::new(INSTALLED));
        assert_eq!(program(command.to_str().unwrap()), INSTALLED);
    }

    #[test]
    fn paths_compare_without_case_or_slash_differences() {
        let exe = Path::new(INSTALLED);
        assert!(same_program(INSTALLED, exe));
        assert!(same_program(
            "c:/users/ada lovelace/appdata/local/programs/BLINK/Blink.EXE",
            exe
        ));
        assert!(!same_program(r"C:\Program Files\blink\blink.exe", exe));
        assert!(!same_program("", exe));
    }

    #[test]
    fn paths_to_the_same_file_compare_equal() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("blink.exe");
        std::fs::write(&exe, b"").unwrap();
        let roundabout: PathBuf = dir.path().join(".").join("blink.exe");
        assert!(same_program(roundabout.to_str().unwrap(), &exe));
        let other = dir.path().join("other.exe");
        std::fs::write(&other, b"").unwrap();
        assert!(!same_program(other.to_str().unwrap(), &exe));
    }

    #[test]
    fn startup_apps_flags() {
        assert!(approved(&[]));
        assert!(approved(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(approved(&[0x06, 0, 0, 0]));
        assert!(!approved(&[0x03, 0, 0, 0, 0x10, 0x20, 0, 0, 0, 0, 0, 0]));
        assert!(!approved(&[0x07]));
    }

    #[test]
    fn classify_states() {
        let exe = Path::new(INSTALLED);
        let ours = format!("\"{INSTALLED}\"");
        let elsewhere = r#""D:\old\blink.exe""#;
        let disabled: &[u8] = &[0x03, 0, 0, 0];
        let enabled: &[u8] = &[0x02, 0, 0, 0];
        assert_eq!(classify(None, None, exe), Autostart::Off);
        assert_eq!(classify(None, Some(disabled), exe), Autostart::Off);
        assert_eq!(classify(Some(&ours), None, exe), Autostart::On);
        assert_eq!(classify(Some(&ours), Some(enabled), exe), Autostart::On);
        assert_eq!(
            classify(Some(&ours), Some(disabled), exe),
            Autostart::DisabledByWindows
        );
        assert_eq!(classify(Some(elsewhere), None, exe), Autostart::Off);
        assert_eq!(
            classify(Some(elsewhere), Some(disabled), exe),
            Autostart::Off
        );
    }
}
