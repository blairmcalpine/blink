//! The hotkey recorder: maps egui key events onto [`Hotkey`]s.
//!
//! The key that records must be the key the engine's hotkey fires on, and
//! `global-hotkey` resolves a key code differently per platform:
//!
//! - Windows maps a letter code to its virtual key (`KeyQ` to `VK_Q`), and
//!   letter virtual keys follow the active layout; X11 maps it to a keysym.
//!   Both are logical, so the recorder prefers egui's logical key there: on
//!   AZERTY the key labelled A records as `A` and fires as `A`. It falls back
//!   to the physical key when the logical one is not a [`Key`], which keeps
//!   the AZERTY digit row, Shift+digit, and non-Latin layouts recordable;
//!   their virtual keys are still the digits and Latin letters.
//! - macOS registers by key position, so the recorder prefers egui's
//!   `physical_key` there, falling back to the logical key.
//!
//! Numpad digits arrive as the same egui keys as the main-row digits, so they
//! record as `0` to `9`, which the engine registers on the main row only.
//!
//! eframe turns Ctrl+C, Ctrl+X, and Ctrl+V (Cmd on macOS) into copy and paste
//! events before the recorder sees them, so the text fields keep a working
//! clipboard and those combinations cannot be recorded.
//!
//! egui's [`Modifiers`](egui::Modifiers) has no Windows-key flag on Windows or
//! Linux (`mac_cmd` is only set on macOS, and `command` mirrors Ctrl there),
//! but egui does report the Super keys themselves as
//! [`egui::Key::SuperLeft`] and [`egui::Key::SuperRight`] presses and
//! releases. The recorder tracks those to set [`Modifiers::win`]; on macOS
//! Cmd is the Super key and also sets `win`. Combinations the OS shell
//! reserves (Win+L, Win+D, and so on) never reach the window.

use blink_config::{Hotkey, Key, Modifiers};
use eframe::egui;

/// The egui key for each [`Key`]. The match is exhaustive, so a new [`Key`]
/// does not compile until it is mapped here.
pub(crate) const fn egui_key(key: Key) -> egui::Key {
    use egui::Key as E;
    match key {
        Key::F1 => E::F1,
        Key::F2 => E::F2,
        Key::F3 => E::F3,
        Key::F4 => E::F4,
        Key::F5 => E::F5,
        Key::F6 => E::F6,
        Key::F7 => E::F7,
        Key::F8 => E::F8,
        Key::F9 => E::F9,
        Key::F10 => E::F10,
        Key::F11 => E::F11,
        Key::F12 => E::F12,
        Key::F13 => E::F13,
        Key::F14 => E::F14,
        Key::F15 => E::F15,
        Key::F16 => E::F16,
        Key::F17 => E::F17,
        Key::F18 => E::F18,
        Key::F19 => E::F19,
        Key::F20 => E::F20,
        Key::F21 => E::F21,
        Key::F22 => E::F22,
        Key::F23 => E::F23,
        Key::F24 => E::F24,
        Key::Insert => E::Insert,
        Key::Delete => E::Delete,
        Key::Home => E::Home,
        Key::End => E::End,
        Key::PageUp => E::PageUp,
        Key::PageDown => E::PageDown,
        Key::PrintScreen => E::PrintScreen,
        Key::ScrollLock => E::ScrollLock,
        Key::Pause => E::Pause,
        Key::A => E::A,
        Key::B => E::B,
        Key::C => E::C,
        Key::D => E::D,
        Key::E => E::E,
        Key::F => E::F,
        Key::G => E::G,
        Key::H => E::H,
        Key::I => E::I,
        Key::J => E::J,
        Key::K => E::K,
        Key::L => E::L,
        Key::M => E::M,
        Key::N => E::N,
        Key::O => E::O,
        Key::P => E::P,
        Key::Q => E::Q,
        Key::R => E::R,
        Key::S => E::S,
        Key::T => E::T,
        Key::U => E::U,
        Key::V => E::V,
        Key::W => E::W,
        Key::X => E::X,
        Key::Y => E::Y,
        Key::Z => E::Z,
        Key::Digit0 => E::Num0,
        Key::Digit1 => E::Num1,
        Key::Digit2 => E::Num2,
        Key::Digit3 => E::Num3,
        Key::Digit4 => E::Num4,
        Key::Digit5 => E::Num5,
        Key::Digit6 => E::Num6,
        Key::Digit7 => E::Num7,
        Key::Digit8 => E::Num8,
        Key::Digit9 => E::Num9,
        Key::Space => E::Space,
        Key::Enter => E::Enter,
        Key::Tab => E::Tab,
        Key::Escape => E::Escape,
        Key::Backspace => E::Backspace,
        Key::Up => E::ArrowUp,
        Key::Down => E::ArrowDown,
        Key::Left => E::ArrowLeft,
        Key::Right => E::ArrowRight,
    }
}

/// The [`Key`] for an egui key, if blink can register it.
pub(crate) fn from_egui(key: egui::Key) -> Option<Key> {
    Key::ALL.iter().copied().find(|&k| egui_key(k) == key)
}

const fn is_super(key: egui::Key) -> bool {
    matches!(key, egui::Key::SuperLeft | egui::Key::SuperRight)
}

/// Modifier and lock keys, which never complete a hotkey on their own.
const fn is_modifier(key: egui::Key) -> bool {
    use egui::Key as E;
    matches!(
        key,
        E::ShiftLeft
            | E::ShiftRight
            | E::ControlLeft
            | E::ControlRight
            | E::AltLeft
            | E::AltRight
            | E::SuperLeft
            | E::SuperRight
            | E::CapsLock
            | E::NumLock
    )
}

/// The hotkey modifiers for egui's modifier state plus whether a Super key is
/// held. egui's `command` is ignored: it is Ctrl on Windows and Linux and Cmd
/// on macOS, both already covered.
pub(crate) const fn modifiers(held: egui::Modifiers, super_held: bool) -> Modifiers {
    Modifiers {
        ctrl: held.ctrl,
        alt: held.alt,
        shift: held.shift,
        win: held.mac_cmd || super_held,
    }
}

/// What a key press does while recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capture {
    /// A modifier on its own: keep recording.
    Ignore,
    /// Bare Escape: stop recording and keep the old hotkey.
    Cancel,
    /// A key that no [`Key`] represents.
    Unsupported,
    /// A complete combination.
    Hotkey(Hotkey),
}

/// Whether `global-hotkey` registers keys by position on this platform; see
/// the module docs.
const PHYSICAL_FIRST: bool = cfg!(target_os = "macos");

/// Maps one key press, given egui's logical and physical keys, to what the
/// recorder should do.
pub(crate) fn capture(
    logical: egui::Key,
    physical: Option<egui::Key>,
    modifiers: Modifiers,
) -> Capture {
    if is_modifier(physical.unwrap_or(logical)) || is_modifier(logical) {
        return Capture::Ignore;
    }
    let (first, second) = if PHYSICAL_FIRST {
        (physical, Some(logical))
    } else {
        (Some(logical), physical)
    };
    let Some(key) = first
        .and_then(from_egui)
        .or_else(|| second.and_then(from_egui))
    else {
        return Capture::Unsupported;
    };
    if key == Key::Escape && modifiers == Modifiers::default() {
        return Capture::Cancel;
    }
    Capture::Hotkey(Hotkey { modifiers, key })
}

/// Records the next key combination from egui input events.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    recording: Option<Recording>,
}

#[derive(Debug, Default)]
struct Recording {
    super_held: bool,
    /// Why the last key pressed during this recording was rejected.
    problem: Option<Problem>,
}

/// Why the recorder rejected a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Problem {
    /// No [`Key`] represents the key.
    Unsupported,
    /// egui turned the press into copy, cut, or paste.
    Clipboard,
}

impl Problem {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Unsupported => "That key cannot be used in a hotkey.",
            Self::Clipboard if cfg!(target_os = "macos") => {
                "Cmd+C/X/V are reserved for copy and paste."
            }
            Self::Clipboard => "Ctrl+C/X/V are reserved for copy and paste.",
        }
    }
}

/// What the recorder did with one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Not a recording event; widgets should still see it.
    Pass,
    /// Swallowed by the recorder.
    Consumed,
    /// Recording finished with this hotkey.
    Recorded(Hotkey),
}

impl Recorder {
    pub(crate) fn start(&mut self) {
        self.recording = Some(Recording::default());
    }

    pub(crate) const fn stop(&mut self) {
        self.recording = None;
    }

    pub(crate) const fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Why the last key pressed while recording was rejected, if it was.
    pub(crate) fn problem(&self) -> Option<Problem> {
        self.recording.as_ref().and_then(|r| r.problem)
    }

    /// Feeds one input event.
    pub(crate) fn on_event(&mut self, event: &egui::Event) -> Step {
        let Some(recording) = &mut self.recording else {
            return Step::Pass;
        };
        match *event {
            egui::Event::WindowFocused(false) => {
                self.recording = None;
                Step::Pass
            }
            egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers: held,
            } => {
                if is_super(physical_key.unwrap_or(key)) || is_super(key) {
                    recording.super_held = pressed;
                }
                if !pressed || repeat {
                    return Step::Consumed;
                }
                match capture(key, physical_key, modifiers(held, recording.super_held)) {
                    Capture::Ignore => {}
                    Capture::Cancel => self.recording = None,
                    Capture::Unsupported => recording.problem = Some(Problem::Unsupported),
                    Capture::Hotkey(hotkey) => {
                        self.recording = None;
                        return Step::Recorded(hotkey);
                    }
                }
                Step::Consumed
            }
            // egui-winit turns Ctrl+C/X/V (Cmd on macOS), Ctrl+Insert,
            // Shift+Insert, and Shift+Delete into these instead of key
            // presses. Paste only arrives when the clipboard has content.
            egui::Event::Copy
            | egui::Event::Cut
            | egui::Event::Paste(_)
            | egui::Event::PasteImage(_) => {
                recording.problem = Some(Problem::Clipboard);
                Step::Consumed
            }
            egui::Event::Text(_) => Step::Consumed,
            _ => Step::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
    };

    /// Parses modifiers written like a hotkey prefix, such as `"Ctrl+Alt"`.
    fn mods(names: &str) -> Modifiers {
        format!("{names}+F1").parse::<Hotkey>().unwrap().modifiers
    }

    fn hotkey(text: &str) -> Capture {
        Capture::Hotkey(text.parse().unwrap())
    }

    #[test]
    fn every_key_maps_to_a_distinct_egui_key_and_back() {
        for &key in Key::ALL {
            assert_eq!(from_egui(egui_key(key)), Some(key), "{key}");
        }
        let mut egui_keys: Vec<_> = Key::ALL.iter().map(|&k| egui_key(k).name()).collect();
        egui_keys.sort_unstable();
        egui_keys.dedup();
        assert_eq!(egui_keys.len(), Key::ALL.len());
    }

    #[test]
    fn mapping_table() {
        use egui::Key as E;
        let cases = [
            (E::A, "A"),
            (E::Z, "Z"),
            (E::Num0, "0"),
            (E::Num9, "9"),
            (E::F1, "F1"),
            (E::F24, "F24"),
            (E::ArrowUp, "Up"),
            (E::ArrowDown, "Down"),
            (E::ArrowLeft, "Left"),
            (E::ArrowRight, "Right"),
            (E::Space, "Space"),
            (E::Enter, "Enter"),
            (E::Tab, "Tab"),
            (E::Escape, "Escape"),
            (E::Backspace, "Backspace"),
            (E::Insert, "Insert"),
            (E::Delete, "Delete"),
            (E::Home, "Home"),
            (E::End, "End"),
            (E::PageUp, "PageUp"),
            (E::PageDown, "PageDown"),
            (E::PrintScreen, "PrintScreen"),
            (E::ScrollLock, "ScrollLock"),
            (E::Pause, "Pause"),
        ];
        for (egui_key, name) in cases {
            assert_eq!(
                from_egui(egui_key).map(Key::name),
                Some(name),
                "{egui_key:?}"
            );
        }
        for egui_key in [E::F25, E::Minus, E::Comma, E::Exclamationmark, E::Menu] {
            assert_eq!(from_egui(egui_key), None, "{egui_key:?}");
        }
    }

    #[test]
    fn egui_modifiers_map_to_hotkey_modifiers() {
        let windows_ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..egui::Modifiers::NONE
        };
        assert_eq!(modifiers(windows_ctrl, false), mods("Ctrl"));
        assert_eq!(modifiers(egui::Modifiers::ALT, true), mods("Alt+Win"));
        assert_eq!(modifiers(egui::Modifiers::SHIFT, false), mods("Shift"));
        let mac_cmd = egui::Modifiers {
            mac_cmd: true,
            command: true,
            ..egui::Modifiers::NONE
        };
        assert_eq!(modifiers(mac_cmd, false), mods("Win"));
        assert_eq!(modifiers(egui::Modifiers::NONE, false), NONE);
    }

    #[test]
    fn letters_follow_the_platform_hotkey_registration() {
        // AZERTY: the key labelled A sits where US has Q. Windows and X11
        // register `A` on the key labelled A; macOS registers by position.
        let expected = if cfg!(target_os = "macos") {
            "Ctrl+Q"
        } else {
            "Ctrl+A"
        };
        assert_eq!(
            capture(egui::Key::A, Some(egui::Key::Q), mods("Ctrl")),
            hotkey(expected)
        );
    }

    #[test]
    fn digit_row_records_digits_on_every_layout() {
        // US Shift+1 types `!`, and the AZERTY digit row types symbols. None
        // is a `Key`, so the physical digit is used; its virtual key is the
        // digit on every layout.
        for logical in [
            egui::Key::Exclamationmark,
            egui::Key::Minus,
            egui::Key::Quote,
        ] {
            assert_eq!(
                capture(logical, Some(egui::Key::Num1), mods("Ctrl+Shift")),
                hotkey("Ctrl+Shift+1"),
                "{logical:?}"
            );
        }
    }

    #[test]
    fn non_latin_layouts_fall_back_to_the_position() {
        // A Cyrillic letter has no egui key of its own, so egui reports the
        // physical key as the logical one too.
        assert_eq!(
            capture(egui::Key::Q, Some(egui::Key::Q), mods("Alt")),
            hotkey("Alt+Q")
        );
        assert_eq!(capture(egui::Key::F9, None, mods("Alt")), hotkey("Alt+F9"));
    }

    #[test]
    fn the_other_key_is_the_fallback() {
        assert_eq!(
            capture(egui::Key::G, Some(egui::Key::IntlBackslash), mods("Alt")),
            hotkey("Alt+G")
        );
        assert_eq!(
            capture(egui::Key::Minus, Some(egui::Key::Num6), mods("Alt")),
            hotkey("Alt+6")
        );
    }

    #[test]
    fn unmappable_keys_are_unsupported() {
        for (logical, physical) in [
            (egui::Key::Minus, Some(egui::Key::Minus)),
            (egui::Key::F30, None),
            (egui::Key::Exclamationmark, None),
        ] {
            assert_eq!(
                capture(logical, physical, mods("Ctrl")),
                Capture::Unsupported
            );
        }
    }

    #[test]
    fn modifiers_alone_keep_recording() {
        use egui::Key as E;
        for key in [
            E::ShiftLeft,
            E::ShiftRight,
            E::ControlLeft,
            E::ControlRight,
            E::AltLeft,
            E::AltRight,
            E::SuperLeft,
            E::SuperRight,
            E::CapsLock,
        ] {
            assert_eq!(
                capture(key, Some(key), mods("Ctrl")),
                Capture::Ignore,
                "{key:?}"
            );
        }
    }

    #[test]
    fn bare_escape_cancels_but_modified_escape_records() {
        let esc = egui::Key::Escape;
        assert_eq!(capture(esc, Some(esc), NONE), Capture::Cancel);
        assert_eq!(capture(esc, Some(esc), mods("Ctrl")), hotkey("Ctrl+Escape"));
    }

    fn key(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn recorder_ignores_events_until_started() {
        let mut recorder = Recorder::default();
        let event = key(egui::Key::F9, true, egui::Modifiers::CTRL);
        assert_eq!(recorder.on_event(&event), Step::Pass);
        assert!(!recorder.is_recording());
    }

    #[test]
    fn recorder_records_a_combination_and_stops() {
        let mut recorder = Recorder::default();
        recorder.start();
        let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        let steps: Vec<_> = [
            key(egui::Key::ControlLeft, true, egui::Modifiers::CTRL),
            key(egui::Key::ShiftLeft, true, ctrl_shift),
            key(egui::Key::F9, true, ctrl_shift),
        ]
        .iter()
        .map(|event| recorder.on_event(event))
        .collect();
        assert_eq!(
            steps,
            [
                Step::Consumed,
                Step::Consumed,
                Step::Recorded("Ctrl+Shift+F9".parse().unwrap())
            ]
        );
        assert!(!recorder.is_recording());
    }

    #[test]
    fn recorder_tracks_the_windows_key() {
        let mut recorder = Recorder::default();
        recorder.start();
        let none = egui::Modifiers::NONE;
        recorder.on_event(&key(egui::Key::SuperLeft, true, none));
        assert_eq!(
            recorder.on_event(&key(egui::Key::F9, true, egui::Modifiers::ALT)),
            Step::Recorded("Alt+Win+F9".parse().unwrap())
        );

        recorder.start();
        recorder.on_event(&key(egui::Key::SuperRight, true, none));
        recorder.on_event(&key(egui::Key::SuperRight, false, none));
        assert_eq!(
            recorder.on_event(&key(egui::Key::F9, true, egui::Modifiers::ALT)),
            Step::Recorded("Alt+F9".parse().unwrap())
        );
    }

    #[test]
    fn unsupported_key_error_clears_on_escape_or_rerecord() {
        let mut recorder = Recorder::default();
        let ctrl = egui::Modifiers::CTRL;
        recorder.start();
        recorder.on_event(&key(egui::Key::Minus, true, ctrl));
        assert!(recorder.is_recording());
        assert_eq!(recorder.problem(), Some(Problem::Unsupported));
        recorder.on_event(&key(egui::Key::Escape, true, egui::Modifiers::NONE));
        assert!(!recorder.is_recording());
        assert_eq!(recorder.problem(), None);

        recorder.start();
        assert_eq!(recorder.problem(), None);
        recorder.on_event(&key(egui::Key::Minus, true, ctrl));
        assert_eq!(recorder.problem(), Some(Problem::Unsupported));
        assert_eq!(
            recorder.on_event(&key(egui::Key::F7, true, ctrl)),
            Step::Recorded("Ctrl+F7".parse().unwrap())
        );
        assert_eq!(recorder.problem(), None);
    }

    #[test]
    fn clipboard_shortcuts_are_reserved() {
        let mut recorder = Recorder::default();
        for event in [
            egui::Event::Copy,
            egui::Event::Cut,
            egui::Event::Paste("x".to_owned()),
        ] {
            recorder.start();
            assert_eq!(recorder.on_event(&event), Step::Consumed, "{event:?}");
            assert!(recorder.is_recording());
            assert_eq!(recorder.problem(), Some(Problem::Clipboard));
        }
        assert_eq!(
            recorder.on_event(&key(egui::Key::F7, true, egui::Modifiers::CTRL)),
            Step::Recorded("Ctrl+F7".parse().unwrap())
        );
        assert!(Problem::Clipboard.message().contains("C/X/V"));
    }

    #[test]
    fn recorder_swallows_releases_repeats_and_text() {
        let mut recorder = Recorder::default();
        recorder.start();
        let repeat = egui::Event::Key {
            key: egui::Key::A,
            physical_key: Some(egui::Key::A),
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::CTRL,
        };
        for event in [
            key(egui::Key::A, false, egui::Modifiers::CTRL),
            repeat,
            egui::Event::Text("a".to_owned()),
        ] {
            assert_eq!(recorder.on_event(&event), Step::Consumed, "{event:?}");
        }
        assert!(recorder.is_recording());
        assert_eq!(recorder.on_event(&egui::Event::PointerGone), Step::Pass);
    }

    #[test]
    fn losing_focus_stops_recording() {
        let mut recorder = Recorder::default();
        recorder.start();
        assert_eq!(
            recorder.on_event(&egui::Event::WindowFocused(false)),
            Step::Pass
        );
        assert!(!recorder.is_recording());
    }
}
