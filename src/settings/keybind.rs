//! Hand-mapped `KeyCode` <-> stable-string conversion (decision D003/MEM003).
//!
//! The settings TOML file stores keybinds as plain strings ("KeyW", "Digit0").
//! Mapping is decoupled from winit's serde feature so the on-disk format
//! survives future winit bumps: this crate owns the string table, and the
//! tests pin that every mapped string is exactly the `KeyCode` Debug spelling.
//!
//! Parsing is case-sensitive; any string not in the table maps to `None`.

use winit::keyboard::KeyCode;

/// Explicit hand-mapped subset of `KeyCode` variants supported in settings
/// files: letters `KeyA`-`KeyZ`, digits `Digit0`-`Digit9`, `Space`,
/// `ShiftLeft`/`ShiftRight`, `ControlLeft`/`ControlRight`,
/// `AltLeft`/`AltRight`, `Tab`, `Enter`, `Escape`, `Backquote`,
/// `ArrowUp`/`ArrowDown`/`ArrowLeft`/`ArrowRight`, `F1`-`F12`, and
/// `Numpad0`-`Numpad9`.
const KEY_TABLE: &[(KeyCode, &str)] = &[
    (KeyCode::KeyA, "KeyA"),
    (KeyCode::KeyB, "KeyB"),
    (KeyCode::KeyC, "KeyC"),
    (KeyCode::KeyD, "KeyD"),
    (KeyCode::KeyE, "KeyE"),
    (KeyCode::KeyF, "KeyF"),
    (KeyCode::KeyG, "KeyG"),
    (KeyCode::KeyH, "KeyH"),
    (KeyCode::KeyI, "KeyI"),
    (KeyCode::KeyJ, "KeyJ"),
    (KeyCode::KeyK, "KeyK"),
    (KeyCode::KeyL, "KeyL"),
    (KeyCode::KeyM, "KeyM"),
    (KeyCode::KeyN, "KeyN"),
    (KeyCode::KeyO, "KeyO"),
    (KeyCode::KeyP, "KeyP"),
    (KeyCode::KeyQ, "KeyQ"),
    (KeyCode::KeyR, "KeyR"),
    (KeyCode::KeyS, "KeyS"),
    (KeyCode::KeyT, "KeyT"),
    (KeyCode::KeyU, "KeyU"),
    (KeyCode::KeyV, "KeyV"),
    (KeyCode::KeyW, "KeyW"),
    (KeyCode::KeyX, "KeyX"),
    (KeyCode::KeyY, "KeyY"),
    (KeyCode::KeyZ, "KeyZ"),
    (KeyCode::Digit0, "Digit0"),
    (KeyCode::Digit1, "Digit1"),
    (KeyCode::Digit2, "Digit2"),
    (KeyCode::Digit3, "Digit3"),
    (KeyCode::Digit4, "Digit4"),
    (KeyCode::Digit5, "Digit5"),
    (KeyCode::Digit6, "Digit6"),
    (KeyCode::Digit7, "Digit7"),
    (KeyCode::Digit8, "Digit8"),
    (KeyCode::Digit9, "Digit9"),
    (KeyCode::Space, "Space"),
    (KeyCode::ShiftLeft, "ShiftLeft"),
    (KeyCode::ShiftRight, "ShiftRight"),
    (KeyCode::ControlLeft, "ControlLeft"),
    (KeyCode::ControlRight, "ControlRight"),
    (KeyCode::AltLeft, "AltLeft"),
    (KeyCode::AltRight, "AltRight"),
    (KeyCode::Tab, "Tab"),
    (KeyCode::Enter, "Enter"),
    (KeyCode::Escape, "Escape"),
    (KeyCode::Backquote, "Backquote"),
    (KeyCode::ArrowUp, "ArrowUp"),
    (KeyCode::ArrowDown, "ArrowDown"),
    (KeyCode::ArrowLeft, "ArrowLeft"),
    (KeyCode::ArrowRight, "ArrowRight"),
    (KeyCode::F1, "F1"),
    (KeyCode::F2, "F2"),
    (KeyCode::F3, "F3"),
    (KeyCode::F4, "F4"),
    (KeyCode::F5, "F5"),
    (KeyCode::F6, "F6"),
    (KeyCode::F7, "F7"),
    (KeyCode::F8, "F8"),
    (KeyCode::F9, "F9"),
    (KeyCode::F10, "F10"),
    (KeyCode::F11, "F11"),
    (KeyCode::F12, "F12"),
    (KeyCode::Numpad0, "Numpad0"),
    (KeyCode::Numpad1, "Numpad1"),
    (KeyCode::Numpad2, "Numpad2"),
    (KeyCode::Numpad3, "Numpad3"),
    (KeyCode::Numpad4, "Numpad4"),
    (KeyCode::Numpad5, "Numpad5"),
    (KeyCode::Numpad6, "Numpad6"),
    (KeyCode::Numpad7, "Numpad7"),
    (KeyCode::Numpad8, "Numpad8"),
    (KeyCode::Numpad9, "Numpad9"),
];

/// Convert a `KeyCode` to its stable settings-file string (the `KeyCode`
/// Debug spelling, e.g. "KeyW"). Returns `None` for keys outside the
/// supported subset.
pub fn key_to_string(code: KeyCode) -> Option<&'static str> {
    KEY_TABLE
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, s)| *s)
}

/// Parse a settings-file string back into a `KeyCode`. Case-sensitive;
/// unknown or unsupported strings return `None`.
pub fn parse_key(s: &str) -> Option<KeyCode> {
    KEY_TABLE
        .iter()
        .find(|(_, name)| *name == s)
        .map(|(c, _)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_letter_keys() {
        assert_eq!(parse_key("KeyA"), Some(KeyCode::KeyA));
        assert_eq!(parse_key("KeyM"), Some(KeyCode::KeyM));
        assert_eq!(parse_key("KeyW"), Some(KeyCode::KeyW));
        assert_eq!(parse_key("KeyZ"), Some(KeyCode::KeyZ));
    }

    #[test]
    fn parses_digit_keys() {
        assert_eq!(parse_key("Digit0"), Some(KeyCode::Digit0));
        assert_eq!(parse_key("Digit5"), Some(KeyCode::Digit5));
        assert_eq!(parse_key("Digit9"), Some(KeyCode::Digit9));
    }

    #[test]
    fn parses_whitespace_and_function_and_arrow_keys() {
        assert_eq!(parse_key("Space"), Some(KeyCode::Space));
        assert_eq!(parse_key("Tab"), Some(KeyCode::Tab));
        assert_eq!(parse_key("Enter"), Some(KeyCode::Enter));
        assert_eq!(parse_key("Escape"), Some(KeyCode::Escape));
        assert_eq!(parse_key("Backquote"), Some(KeyCode::Backquote));
        assert_eq!(parse_key("F1"), Some(KeyCode::F1));
        assert_eq!(parse_key("F12"), Some(KeyCode::F12));
        assert_eq!(parse_key("ArrowUp"), Some(KeyCode::ArrowUp));
        assert_eq!(parse_key("ArrowDown"), Some(KeyCode::ArrowDown));
        assert_eq!(parse_key("ArrowLeft"), Some(KeyCode::ArrowLeft));
        assert_eq!(parse_key("ArrowRight"), Some(KeyCode::ArrowRight));
        assert_eq!(parse_key("Numpad0"), Some(KeyCode::Numpad0));
        assert_eq!(parse_key("Numpad9"), Some(KeyCode::Numpad9));
    }

    #[test]
    fn parses_modifier_variants_independently() {
        assert_eq!(parse_key("ShiftLeft"), Some(KeyCode::ShiftLeft));
        assert_eq!(parse_key("ShiftRight"), Some(KeyCode::ShiftRight));
        assert_eq!(parse_key("ControlLeft"), Some(KeyCode::ControlLeft));
        assert_eq!(parse_key("ControlRight"), Some(KeyCode::ControlRight));
        assert_eq!(parse_key("AltLeft"), Some(KeyCode::AltLeft));
        assert_eq!(parse_key("AltRight"), Some(KeyCode::AltRight));
    }

    #[test]
    fn unsupported_strings_return_none() {
        assert_eq!(parse_key("NotAKey"), None);
        assert_eq!(parse_key("keyw"), None); // lowercase: case-sensitive
        assert_eq!(parse_key("KEYW"), None);
        assert_eq!(parse_key(""), None);
        assert_eq!(parse_key("Semicolon"), None); // real key, deliberately unmapped
        assert_eq!(parse_key("F13"), None); // beyond the supported F-range
    }

    #[test]
    fn keys_outside_the_supported_subset_return_none_from_key_to_string() {
        assert_eq!(key_to_string(KeyCode::Semicolon), None);
        assert_eq!(key_to_string(KeyCode::Minus), None);
        assert_eq!(key_to_string(KeyCode::Backslash), None);
    }

    #[test]
    fn every_supported_key_round_trips() {
        for (code, name) in KEY_TABLE {
            assert_eq!(key_to_string(*code), Some(*name), "missing key_to_string entry for {:?}", code);
            assert_eq!(parse_key(name), Some(*code), "missing parse_key entry for {}", name);
        }
    }

    #[test]
    fn every_mapped_string_is_the_keycode_debug_spelling() {
        for (code, name) in KEY_TABLE {
            assert_eq!(&format!("{:?}", code), name, "string for {:?} is not the Debug spelling", code);
        }
    }
}
