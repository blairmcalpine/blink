use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A global hotkey, written as a string such as `"Ctrl+Shift+F9"`.
///
/// Parsing is case-insensitive; [`Display`](fmt::Display) produces the
/// canonical form, with modifiers in the order `Ctrl+Alt+Shift+Win`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Hotkey {
    /// Modifier keys held with [`Hotkey::key`].
    pub modifiers: Modifiers,
    /// The key that triggers the hotkey.
    pub key: Key,
}

/// Modifier keys of a [`Hotkey`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each modifier is an independent flag"
)]
pub struct Modifiers {
    /// Either Ctrl key.
    pub ctrl: bool,
    /// Either Alt key.
    pub alt: bool,
    /// Either Shift key.
    pub shift: bool,
    /// Either Windows key.
    pub win: bool,
}

macro_rules! keys {
    (
        bare: { $($bare:ident => $bare_name:literal),* $(,)? }
        typing: { $($typing:ident => $typing_name:literal),* $(,)? }
    ) => {
        /// A portable key identifier. The Windows virtual-key mapping lives
        /// in `blink-app`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Key {
            $(
                #[doc = concat!("The `", $bare_name, "` key.")]
                $bare,
            )*
            $(
                #[doc = concat!("The `", $typing_name, "` key.")]
                $typing,
            )*
        }

        impl Key {
            /// Every key, in display order.
            pub const ALL: &'static [Self] = &[$(Self::$bare,)* $(Self::$typing,)*];

            /// The canonical name used in hotkey strings.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$bare => $bare_name,)*
                    $(Self::$typing => $typing_name,)*
                }
            }

            /// Whether the key types text or moves the caret, so registering
            /// it without Ctrl, Alt, or Win would swallow it system-wide.
            const fn is_typing(self) -> bool {
                match self {
                    $(Self::$bare)|* => false,
                    $(Self::$typing)|* => true,
                }
            }
        }
    };
}

keys! {
    bare: {
        F1 => "F1", F2 => "F2", F3 => "F3", F4 => "F4", F5 => "F5", F6 => "F6",
        F7 => "F7", F8 => "F8", F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12",
        F13 => "F13", F14 => "F14", F15 => "F15", F16 => "F16", F17 => "F17", F18 => "F18",
        F19 => "F19", F20 => "F20", F21 => "F21", F22 => "F22", F23 => "F23", F24 => "F24",
        Insert => "Insert", Delete => "Delete", Home => "Home", End => "End",
        PageUp => "PageUp", PageDown => "PageDown",
        PrintScreen => "PrintScreen", ScrollLock => "ScrollLock", Pause => "Pause",
    }
    typing: {
        A => "A", B => "B", C => "C", D => "D", E => "E", F => "F", G => "G", H => "H",
        I => "I", J => "J", K => "K", L => "L", M => "M", N => "N", O => "O", P => "P",
        Q => "Q", R => "R", S => "S", T => "T", U => "U", V => "V", W => "W", X => "X",
        Y => "Y", Z => "Z",
        Digit0 => "0", Digit1 => "1", Digit2 => "2", Digit3 => "3", Digit4 => "4",
        Digit5 => "5", Digit6 => "6", Digit7 => "7", Digit8 => "8", Digit9 => "9",
        Space => "Space", Enter => "Enter", Tab => "Tab", Escape => "Escape",
        Backspace => "Backspace", Up => "Up", Down => "Down", Left => "Left", Right => "Right",
    }
}

/// Errors produced while parsing a [`Hotkey`].
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum HotkeyParseError {
    /// The text is empty or contains an empty `+`-separated part.
    #[error("hotkey {0:?} has an empty part")]
    EmptyPart(String),
    /// The hotkey has only modifiers and no key.
    #[error("hotkey {0:?} has no non-modifier key")]
    MissingKey(String),
    /// A modifier name is not one of `Ctrl`, `Alt`, `Shift`, or `Win`.
    #[error("unknown modifier {0:?}; expected Ctrl, Alt, Shift, or Win")]
    UnknownModifier(String),
    /// The same modifier appears twice.
    #[error("modifier {0:?} appears more than once")]
    DuplicateModifier(String),
    /// The key name is not a supported [`Key`].
    #[error("unknown key {0:?}")]
    UnknownKey(String),
}

impl Modifiers {
    /// Returns the flag for a modifier name, matched case-insensitively.
    fn flag_mut(&mut self, name: &str) -> Option<&mut bool> {
        match name.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(&mut self.ctrl),
            "alt" => Some(&mut self.alt),
            "shift" => Some(&mut self.shift),
            "win" => Some(&mut self.win),
            _ => None,
        }
    }
}

/// Alternate key names accepted by the parser. Display always uses
/// [`Key::name`].
const KEY_ALIASES: [(&str, Key); 8] = [
    ("Esc", Key::Escape),
    ("Return", Key::Enter),
    ("Ins", Key::Insert),
    ("Del", Key::Delete),
    ("PgUp", Key::PageUp),
    ("PgDn", Key::PageDown),
    ("PrtSc", Key::PrintScreen),
    ("ScrLk", Key::ScrollLock),
];

impl FromStr for Key {
    type Err = HotkeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let canonical = Self::ALL.iter().map(|&key| (key.name(), key));
        canonical
            .chain(KEY_ALIASES)
            .find(|(name, _)| name.eq_ignore_ascii_case(s))
            .map(|(_, key)| key)
            .ok_or_else(|| HotkeyParseError::UnknownKey(s.to_owned()))
    }
}

impl Hotkey {
    /// Whether the hotkey is safe to register globally: typing keys need
    /// Ctrl, Alt, or Win (Shift alone still swallows capitals), and bare F12
    /// is reserved for debuggers.
    pub(crate) const fn is_allowed(self) -> bool {
        let Modifiers {
            ctrl,
            alt,
            shift,
            win,
        } = self.modifiers;
        if self.key.is_typing() {
            return ctrl || alt || win;
        }
        !matches!(self.key, Key::F12) || ctrl || alt || shift || win
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Hotkey {
    type Err = HotkeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(HotkeyParseError::EmptyPart(s.to_owned()));
        }
        let key = parts.pop().unwrap_or_default();
        let mut modifiers = Modifiers::default();
        if modifiers.flag_mut(key).is_some() {
            return Err(HotkeyParseError::MissingKey(s.to_owned()));
        }
        for name in parts {
            let flag = modifiers
                .flag_mut(name)
                .ok_or_else(|| HotkeyParseError::UnknownModifier(name.to_owned()))?;
            if *flag {
                return Err(HotkeyParseError::DuplicateModifier(name.to_owned()));
            }
            *flag = true;
        }
        Ok(Self {
            modifiers,
            key: key.parse()?,
        })
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Modifiers {
            ctrl,
            alt,
            shift,
            win,
        } = self.modifiers;
        let names = [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (win, "Win")];
        for (_, name) in names.into_iter().filter(|(held, _)| *held) {
            write!(f, "{name}+")?;
        }
        write!(f, "{}", self.key)
    }
}

impl From<Hotkey> for String {
    fn from(hotkey: Hotkey) -> Self {
        hotkey.to_string()
    }
}

impl TryFrom<String> for Hotkey {
    type Error = HotkeyParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Hotkey, HotkeyParseError> {
        s.parse()
    }

    #[test]
    fn parses_modifiers_and_key() {
        let hotkey = parse("Ctrl+Shift+F9").unwrap();
        assert_eq!(
            hotkey,
            Hotkey {
                modifiers: Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Modifiers::default()
                },
                key: Key::F9,
            }
        );
    }

    #[test]
    fn parse_is_case_insensitive_and_display_is_canonical() {
        let cases = [
            ("ctrl+shift+f9", "Ctrl+Shift+F9"),
            ("SHIFT + control + f9", "Ctrl+Shift+F9"),
            ("win+alt+pageup", "Alt+Win+PageUp"),
            ("alt+f10", "Alt+F10"),
            ("f24", "F24"),
            ("Ctrl+7", "Ctrl+7"),
            ("shift+Win+Alt+Ctrl+z", "Ctrl+Alt+Shift+Win+Z"),
        ];
        for (input, canonical) in cases {
            assert_eq!(parse(input).unwrap().to_string(), canonical, "{input:?}");
        }
    }

    #[test]
    fn aliases_parse_to_canonical_names() {
        let cases = [
            ("ctrl+esc", "Ctrl+Escape"),
            ("Alt+Return", "Alt+Enter"),
            ("ins", "Insert"),
            ("Shift+DEL", "Shift+Delete"),
            ("pgup", "PageUp"),
            ("PgDn", "PageDown"),
            ("prtsc", "PrintScreen"),
            ("ScrLk", "ScrollLock"),
        ];
        for (input, canonical) in cases {
            assert_eq!(parse(input).unwrap().to_string(), canonical, "{input:?}");
        }
    }

    #[test]
    fn aliases_do_not_shadow_canonical_names() {
        for (alias, _) in KEY_ALIASES {
            assert!(
                Key::ALL
                    .iter()
                    .all(|key| !key.name().eq_ignore_ascii_case(alias)),
                "{alias}"
            );
        }
    }

    #[test]
    fn every_key_round_trips() {
        for &key in Key::ALL {
            let hotkey = Hotkey {
                modifiers: Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
                key,
            };
            assert_eq!(parse(&hotkey.to_string()).unwrap(), hotkey);
            assert_eq!(key.name().to_lowercase().parse::<Key>().unwrap(), key);
        }
    }

    #[test]
    fn key_names_are_unique() {
        for (i, a) in Key::ALL.iter().enumerate() {
            for b in &Key::ALL[i + 1..] {
                assert!(!a.name().eq_ignore_ascii_case(b.name()), "{a} and {b}");
            }
        }
    }

    #[test]
    fn rejects_invalid_hotkeys() {
        let cases = [
            ("", HotkeyParseError::EmptyPart(String::new())),
            ("Ctrl+", HotkeyParseError::EmptyPart("Ctrl+".into())),
            ("+F9", HotkeyParseError::EmptyPart("+F9".into())),
            ("Ctrl++F9", HotkeyParseError::EmptyPart("Ctrl++F9".into())),
            (
                "Ctrl+Shift",
                HotkeyParseError::MissingKey("Ctrl+Shift".into()),
            ),
            ("Alt", HotkeyParseError::MissingKey("Alt".into())),
            (
                "Hyper+F9",
                HotkeyParseError::UnknownModifier("Hyper".into()),
            ),
            ("F9+F10", HotkeyParseError::UnknownModifier("F9".into())),
            (
                "Ctrl+ctrl+F9",
                HotkeyParseError::DuplicateModifier("ctrl".into()),
            ),
            ("Ctrl+F25", HotkeyParseError::UnknownKey("F25".into())),
            (
                "Ctrl+Numpad1",
                HotkeyParseError::UnknownKey("Numpad1".into()),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input), Err(expected), "{input:?}");
        }
    }
}
