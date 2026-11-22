//! The "Start blink when I sign in" row. Its state lives in the Windows
//! registry, not the config file, so the window reads it once when it opens
//! and writes it on Save only if the checkbox changed.

use std::error::Error;

use blink_sys::{Autostart, ErrorChain};

/// Reads and writes whether blink starts when the user signs in.
///
/// That state lives outside the config file (on Windows, in the per-user
/// Run key), so the caller of [`run`](crate::run) supplies it; without one
/// the window has no row for it.
pub trait AutostartSetting {
    /// Reads the current state.
    ///
    /// # Errors
    ///
    /// Any error reading it. The row then shows the error and is disabled.
    fn read(&self) -> Result<Autostart, Box<dyn Error + Send + Sync>>;

    /// Turns starting at sign-in on or off.
    ///
    /// # Errors
    ///
    /// Any error writing it. Save then keeps the window open and shows it.
    fn write(&self, enabled: bool) -> Result<(), Box<dyn Error + Send + Sync>>;
}

/// The checkbox and the state it was opened with.
pub(crate) struct Toggle<'a> {
    setting: &'a dyn AutostartSetting,
    /// The state as last read or written, or why it could not be read.
    state: Result<Autostart, String>,
    pub(crate) checked: bool,
}

impl<'a> Toggle<'a> {
    pub(crate) fn new(setting: &'a dyn AutostartSetting) -> Self {
        let state = setting.read().map_err(|error| {
            let message = ErrorChain(&*error).to_string();
            tracing::warn!(error = %message, "failed to read whether blink starts at sign-in");
            message
        });
        // Only a Run value Windows will act on counts as on; anything else
        // opens unchecked rather than claiming blink will start.
        let checked = state == Ok(Autostart::On);
        Self {
            setting,
            state,
            checked,
        }
    }

    /// Whether the checkbox can be changed.
    pub(crate) const fn is_readable(&self) -> bool {
        self.state.is_ok()
    }

    /// Why the state could not be read, for the row's error line.
    pub(crate) fn error(&self) -> Option<String> {
        self.state
            .as_ref()
            .err()
            .map(|message| format!("Could not read: {message}"))
    }

    /// A note for when Windows' startup apps has blink turned off.
    pub(crate) fn note(&self) -> Option<&'static str> {
        (self.state == Ok(Autostart::DisabledByWindows)).then_some(if self.checked {
            "Saving also turns blink on in Windows startup apps."
        } else {
            "Turned off in Windows startup apps."
        })
    }

    /// Writes the checkbox if it differs from the state as read. Returns
    /// whether it wrote.
    pub(crate) fn apply(&mut self) -> Result<bool, String> {
        let Ok(state) = self.state else {
            return Ok(false);
        };
        if self.checked == (state == Autostart::On) {
            return Ok(false);
        }
        self.setting
            .write(self.checked)
            .map_err(|error| ErrorChain(&*error).to_string())?;
        tracing::info!(
            enabled = self.checked,
            "changed whether blink starts at sign-in"
        );
        self.state = Ok(if self.checked {
            Autostart::On
        } else {
            Autostart::Off
        });
        Ok(true)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use super::*;

    /// An in-memory setting that records every write.
    pub(crate) struct Fake {
        pub(crate) state: Result<Autostart, &'static str>,
        pub(crate) fail_writes: bool,
        pub(crate) writes: RefCell<Vec<bool>>,
    }

    impl Fake {
        pub(crate) fn new(state: Autostart) -> Self {
            Self {
                state: Ok(state),
                fail_writes: false,
                writes: RefCell::default(),
            }
        }
    }

    impl AutostartSetting for Fake {
        fn read(&self) -> Result<Autostart, Box<dyn Error + Send + Sync>> {
            self.state.map_err(Into::into)
        }

        fn write(&self, enabled: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
            if self.fail_writes {
                return Err("access denied".into());
            }
            self.writes.borrow_mut().push(enabled);
            Ok(())
        }
    }

    #[test]
    fn only_a_working_run_value_opens_checked() {
        for (state, checked) in [
            (Autostart::On, true),
            (Autostart::Off, false),
            (Autostart::DisabledByWindows, false),
        ] {
            assert_eq!(Toggle::new(&Fake::new(state)).checked, checked, "{state:?}");
        }
    }

    #[test]
    fn unchanged_checkbox_writes_nothing() {
        for state in [Autostart::On, Autostart::Off, Autostart::DisabledByWindows] {
            let fake = Fake::new(state);
            Toggle::new(&fake).apply().unwrap();
            assert!(fake.writes.borrow().is_empty(), "{state:?}");
        }
    }

    #[test]
    fn changed_checkbox_writes_once() {
        let fake = Fake::new(Autostart::On);
        let mut toggle = Toggle::new(&fake);
        toggle.checked = false;
        assert_eq!(toggle.apply(), Ok(true));
        assert_eq!(toggle.apply(), Ok(false));
        assert_eq!(*fake.writes.borrow(), [false]);
    }

    #[test]
    fn checking_it_while_windows_has_it_off_turns_it_on() {
        let fake = Fake::new(Autostart::DisabledByWindows);
        let mut toggle = Toggle::new(&fake);
        assert_eq!(toggle.note(), Some("Turned off in Windows startup apps."));
        toggle.checked = true;
        assert_eq!(
            toggle.note(),
            Some("Saving also turns blink on in Windows startup apps.")
        );
        toggle.apply().unwrap();
        assert_eq!(*fake.writes.borrow(), [true]);
        assert_eq!(toggle.note(), None);
    }

    #[test]
    fn unreadable_state_is_shown_and_never_written() {
        let fake = Fake {
            state: Err("registry unavailable"),
            ..Fake::new(Autostart::Off)
        };
        let mut toggle = Toggle::new(&fake);
        assert!(!toggle.is_readable());
        assert_eq!(
            toggle.error().as_deref(),
            Some("Could not read: registry unavailable")
        );
        toggle.checked = true;
        toggle.apply().unwrap();
        assert!(fake.writes.borrow().is_empty());
    }

    #[test]
    fn failed_write_reports_and_retries() {
        let fake = Fake {
            fail_writes: true,
            ..Fake::new(Autostart::Off)
        };
        let mut toggle = Toggle::new(&fake);
        toggle.checked = true;
        assert_eq!(toggle.apply(), Err("access denied".to_owned()));
        assert_eq!(toggle.apply(), Err("access denied".to_owned()));
    }
}
