//! Platform-neutral view-model helpers shared by Web and desktop frontends.

use hidshift::{KeyUsage, KeyboardShortcut, ModifierState};

pub fn shortcut_from_code(
    code: &str,
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> Option<KeyboardShortcut> {
    let key = code_to_usage(code)?;
    let mut modifiers = ModifierState::empty();
    modifiers.set(ModifierState::LEFT_CTRL, ctrl);
    modifiers.set(ModifierState::LEFT_SHIFT, shift);
    modifiers.set(ModifierState::LEFT_ALT, alt);
    modifiers.set(ModifierState::LEFT_GUI, meta);
    KeyboardShortcut::new(modifiers, KeyUsage(key))
}

pub fn code_to_usage(code: &str) -> Option<u8> {
    if let [b'K', b'e', b'y', letter @ b'A'..=b'Z'] = code.as_bytes() {
        return Some(0x04 + letter - b'A');
    }
    if let [b'D', b'i', b'g', b'i', b't', digit @ b'1'..=b'9'] = code.as_bytes() {
        return Some(0x1e + digit - b'1');
    }
    if let Some(number) = code
        .strip_prefix('F')
        .and_then(|value| value.parse::<u8>().ok())
        && (1..=12).contains(&number)
    {
        return Some(0x3a + number - 1);
    }
    Some(match code {
        "Digit0" => 0x27,
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2a,
        "Tab" => 0x2b,
        "Space" => 0x2c,
        "Minus" => 0x2d,
        "Equal" => 0x2e,
        "BracketLeft" => 0x2f,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "CapsLock" => 0x39,
        "PrintScreen" => 0x46,
        "ScrollLock" => 0x47,
        "Pause" => 0x48,
        "Insert" => 0x49,
        "Home" => 0x4a,
        "PageUp" => 0x4b,
        "Delete" => 0x4c,
        "End" => 0x4d,
        "PageDown" => 0x4e,
        "ArrowRight" => 0x4f,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_codes_map_to_layout_independent_hid_usages() {
        assert_eq!(code_to_usage("KeyA"), Some(0x04));
        assert_eq!(code_to_usage("KeyZ"), Some(0x1d));
        assert_eq!(code_to_usage("Digit1"), Some(0x1e));
        assert_eq!(code_to_usage("ArrowUp"), Some(0x52));
        assert_eq!(code_to_usage("ControlLeft"), None);
        assert_eq!(code_to_usage("AudioVolumeUp"), None);
    }

    #[test]
    fn captured_shortcut_packs_current_modifier_state() {
        let shortcut = shortcut_from_code("KeyK", true, true, false, false).unwrap();
        assert_eq!(shortcut.key, KeyUsage(0x0e));
        assert_eq!(
            shortcut.modifiers,
            ModifierState::LEFT_CTRL | ModifierState::LEFT_SHIFT
        );
    }
}
