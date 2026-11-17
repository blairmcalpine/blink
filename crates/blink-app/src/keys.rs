//! Mapping blink's portable hotkey types onto the `keyboard-types` codes and
//! modifiers that `global-hotkey` registers.

use blink_config::{Hotkey, Key, Modifiers};
use keyboard_types::{Code, Modifiers as KeyModifiers};

/// The physical key code for `key`. The match is exhaustive, so a new
/// [`Key`] does not compile until it is mapped here.
pub(crate) const fn code(key: Key) -> Code {
    match key {
        Key::F1 => Code::F1,
        Key::F2 => Code::F2,
        Key::F3 => Code::F3,
        Key::F4 => Code::F4,
        Key::F5 => Code::F5,
        Key::F6 => Code::F6,
        Key::F7 => Code::F7,
        Key::F8 => Code::F8,
        Key::F9 => Code::F9,
        Key::F10 => Code::F10,
        Key::F11 => Code::F11,
        Key::F12 => Code::F12,
        Key::F13 => Code::F13,
        Key::F14 => Code::F14,
        Key::F15 => Code::F15,
        Key::F16 => Code::F16,
        Key::F17 => Code::F17,
        Key::F18 => Code::F18,
        Key::F19 => Code::F19,
        Key::F20 => Code::F20,
        Key::F21 => Code::F21,
        Key::F22 => Code::F22,
        Key::F23 => Code::F23,
        Key::F24 => Code::F24,
        Key::Insert => Code::Insert,
        Key::Delete => Code::Delete,
        Key::Home => Code::Home,
        Key::End => Code::End,
        Key::PageUp => Code::PageUp,
        Key::PageDown => Code::PageDown,
        Key::PrintScreen => Code::PrintScreen,
        Key::ScrollLock => Code::ScrollLock,
        Key::Pause => Code::Pause,
        Key::A => Code::KeyA,
        Key::B => Code::KeyB,
        Key::C => Code::KeyC,
        Key::D => Code::KeyD,
        Key::E => Code::KeyE,
        Key::F => Code::KeyF,
        Key::G => Code::KeyG,
        Key::H => Code::KeyH,
        Key::I => Code::KeyI,
        Key::J => Code::KeyJ,
        Key::K => Code::KeyK,
        Key::L => Code::KeyL,
        Key::M => Code::KeyM,
        Key::N => Code::KeyN,
        Key::O => Code::KeyO,
        Key::P => Code::KeyP,
        Key::Q => Code::KeyQ,
        Key::R => Code::KeyR,
        Key::S => Code::KeyS,
        Key::T => Code::KeyT,
        Key::U => Code::KeyU,
        Key::V => Code::KeyV,
        Key::W => Code::KeyW,
        Key::X => Code::KeyX,
        Key::Y => Code::KeyY,
        Key::Z => Code::KeyZ,
        Key::Digit0 => Code::Digit0,
        Key::Digit1 => Code::Digit1,
        Key::Digit2 => Code::Digit2,
        Key::Digit3 => Code::Digit3,
        Key::Digit4 => Code::Digit4,
        Key::Digit5 => Code::Digit5,
        Key::Digit6 => Code::Digit6,
        Key::Digit7 => Code::Digit7,
        Key::Digit8 => Code::Digit8,
        Key::Digit9 => Code::Digit9,
        Key::Space => Code::Space,
        Key::Enter => Code::Enter,
        Key::Tab => Code::Tab,
        Key::Escape => Code::Escape,
        Key::Backspace => Code::Backspace,
        Key::Up => Code::ArrowUp,
        Key::Down => Code::ArrowDown,
        Key::Left => Code::ArrowLeft,
        Key::Right => Code::ArrowRight,
    }
}

/// The modifier flags for `modifiers`. Win is `META`, which `global-hotkey`
/// registers as `MOD_WIN`.
pub(crate) fn modifiers(modifiers: Modifiers) -> KeyModifiers {
    let Modifiers {
        ctrl,
        alt,
        shift,
        win,
    } = modifiers;
    [
        (ctrl, KeyModifiers::CONTROL),
        (alt, KeyModifiers::ALT),
        (shift, KeyModifiers::SHIFT),
        (win, KeyModifiers::META),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .fold(KeyModifiers::empty(), |all, (_, flag)| all | flag)
}

/// The modifiers and key code that register `hotkey`.
pub(crate) fn hotkey_parts(hotkey: Hotkey) -> (KeyModifiers, Code) {
    (modifiers(hotkey.modifiers), code(hotkey.key))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_key_maps_to_a_distinct_code() {
        let codes: HashSet<Code> = Key::ALL.iter().map(|&key| code(key)).collect();
        assert_eq!(codes.len(), Key::ALL.len());
    }

    #[test]
    fn key_names_match_their_codes() {
        for &key in Key::ALL {
            let code = code(key).to_string();
            let name = key.name();
            let expected = match name.len() {
                1 if name.starts_with(|c: char| c.is_ascii_digit()) => format!("Digit{name}"),
                1 => format!("Key{name}"),
                _ if matches!(key, Key::Up | Key::Down | Key::Left | Key::Right) => {
                    format!("Arrow{name}")
                }
                _ => name.to_owned(),
            };
            assert_eq!(code, expected, "{key}");
        }
    }

    #[test]
    fn modifiers_map_one_to_one() {
        let none = Modifiers::default();
        assert_eq!(modifiers(none), KeyModifiers::empty());
        let cases = [
            (Modifiers { ctrl: true, ..none }, KeyModifiers::CONTROL),
            (Modifiers { alt: true, ..none }, KeyModifiers::ALT),
            (
                Modifiers {
                    shift: true,
                    ..none
                },
                KeyModifiers::SHIFT,
            ),
            (Modifiers { win: true, ..none }, KeyModifiers::META),
        ];
        for (input, expected) in cases {
            assert_eq!(modifiers(input), expected);
        }
    }

    #[test]
    fn hotkey_parts_combine_both() {
        let hotkey: Hotkey = "Ctrl+Shift+F9".parse().unwrap();
        assert_eq!(
            hotkey_parts(hotkey),
            (KeyModifiers::CONTROL | KeyModifiers::SHIFT, Code::F9)
        );
    }
}
