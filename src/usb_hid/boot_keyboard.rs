use crate::input::{InputEvent, KeyCode, KeyboardEvent, Modifier};

pub const BOOT_KEYBOARD_REPORT_LEN: usize = 8;
pub const BOOT_KEYBOARD_KEY_SLOTS: usize = 6;
pub const MAX_BOOT_KEYBOARD_EVENTS: usize = 14;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootKeyboardError {
    InvalidKeyArray,
    TooManyEvents,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootKeyboardReport {
    pub modifiers: u8,
    pub keys: [u8; BOOT_KEYBOARD_KEY_SLOTS],
}

impl BootKeyboardReport {
    pub const fn empty() -> Self {
        Self {
            modifiers: 0,
            keys: [0; BOOT_KEYBOARD_KEY_SLOTS],
        }
    }

    pub const fn from_bytes(bytes: [u8; BOOT_KEYBOARD_REPORT_LEN]) -> Self {
        Self {
            modifiers: bytes[0],
            keys: [bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]],
        }
    }

    pub fn from_key_array(
        modifiers: u8,
        keys: &[Option<core::num::NonZeroU8>; BOOT_KEYBOARD_KEY_SLOTS],
    ) -> Self {
        let mut report = Self {
            modifiers,
            keys: [0; BOOT_KEYBOARD_KEY_SLOTS],
        };

        let mut index = 0;
        while index < BOOT_KEYBOARD_KEY_SLOTS {
            report.keys[index] = keys[index].map_or(0, core::num::NonZeroU8::get);
            index += 1;
        }

        report
    }

    pub fn diff_into(
        &self,
        previous: &Self,
        events: &mut BootKeyboardEvents,
    ) -> Result<(), BootKeyboardError> {
        events.clear();

        let mut bit = 0;
        while bit < 8 {
            let mask = 1 << bit;
            let was_pressed = previous.modifiers & mask != 0;
            let is_pressed = self.modifiers & mask != 0;

            if was_pressed != is_pressed {
                let modifier = modifier_from_bit(bit);
                let event = if is_pressed {
                    KeyboardEvent::ModifierPress(modifier)
                } else {
                    KeyboardEvent::ModifierRelease(modifier)
                };
                events.push(InputEvent::Keyboard(event))?;
            }

            bit += 1;
        }

        for key in previous.keys {
            if is_real_key(key) && !contains_key(&self.keys, key) {
                events.push(InputEvent::Keyboard(KeyboardEvent::Release(
                    KeyCode::HidUsage(key),
                )))?;
            }
        }

        for key in self.keys {
            if is_real_key(key) && !contains_key(&previous.keys, key) {
                events.push(InputEvent::Keyboard(KeyboardEvent::Press(
                    KeyCode::HidUsage(key),
                )))?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootKeyboardEvents {
    len: usize,
    events: [Option<InputEvent>; MAX_BOOT_KEYBOARD_EVENTS],
}

impl BootKeyboardEvents {
    pub const fn new() -> Self {
        Self {
            len: 0,
            events: [None; MAX_BOOT_KEYBOARD_EVENTS],
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[Option<InputEvent>] {
        &self.events[..self.len]
    }

    pub fn iter(&self) -> impl Iterator<Item = InputEvent> + '_ {
        self.as_slice().iter().filter_map(|event| *event)
    }

    pub fn clear(&mut self) {
        let mut index = 0;
        while index < self.len {
            self.events[index] = None;
            index += 1;
        }
        self.len = 0;
    }

    fn push(&mut self, event: InputEvent) -> Result<(), BootKeyboardError> {
        if self.len == MAX_BOOT_KEYBOARD_EVENTS {
            return Err(BootKeyboardError::TooManyEvents);
        }

        self.events[self.len] = Some(event);
        self.len += 1;
        Ok(())
    }
}

impl Default for BootKeyboardEvents {
    fn default() -> Self {
        Self::new()
    }
}

const fn modifier_from_bit(bit: u8) -> Modifier {
    match bit {
        0 => Modifier::LeftCtrl,
        1 => Modifier::LeftShift,
        2 => Modifier::LeftAlt,
        3 => Modifier::LeftGui,
        4 => Modifier::RightCtrl,
        5 => Modifier::RightShift,
        6 => Modifier::RightAlt,
        _ => Modifier::RightGui,
    }
}

const fn is_real_key(key: u8) -> bool {
    key >= 4
}

fn contains_key(keys: &[u8; BOOT_KEYBOARD_KEY_SLOTS], needle: u8) -> bool {
    keys.contains(&needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_boot_report_bytes_to_internal_press_event() {
        let previous = BootKeyboardReport::empty();
        let current = BootKeyboardReport::from_bytes([0, 0, 0x04, 0, 0, 0, 0, 0]);
        let mut events = BootKeyboardEvents::new();

        current.diff_into(&previous, &mut events).unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![InputEvent::Keyboard(KeyboardEvent::Press(
                KeyCode::HidUsage(0x04)
            ))]
        );
    }

    #[test]
    fn emits_release_when_key_disappears_from_boot_report() {
        let previous = BootKeyboardReport::from_bytes([0, 0, 0x04, 0, 0, 0, 0, 0]);
        let current = BootKeyboardReport::empty();
        let mut events = BootKeyboardEvents::new();

        current.diff_into(&previous, &mut events).unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![InputEvent::Keyboard(KeyboardEvent::Release(
                KeyCode::HidUsage(0x04)
            ))]
        );
    }

    #[test]
    fn emits_modifier_changes_before_key_changes() {
        let previous = BootKeyboardReport::empty();
        let current = BootKeyboardReport::from_bytes([0b0000_0010, 0, 0x04, 0, 0, 0, 0, 0]);
        let mut events = BootKeyboardEvents::new();

        current.diff_into(&previous, &mut events).unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![
                InputEvent::Keyboard(KeyboardEvent::ModifierPress(Modifier::LeftShift)),
                InputEvent::Keyboard(KeyboardEvent::Press(KeyCode::HidUsage(0x04))),
            ]
        );
    }

    #[test]
    fn ignores_empty_and_rollover_key_slots() {
        let previous = BootKeyboardReport::empty();
        let current = BootKeyboardReport::from_bytes([0, 0, 0x00, 0x01, 0x02, 0x03, 0x04, 0]);
        let mut events = BootKeyboardEvents::new();

        current.diff_into(&previous, &mut events).unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![InputEvent::Keyboard(KeyboardEvent::Press(
                KeyCode::HidUsage(0x04)
            ))]
        );
    }
}
