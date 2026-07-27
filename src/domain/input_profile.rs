use heapless::Vec;

use crate::input::{ConsumerUsage, KeyUsage, KeyboardFrame, StandardInputFrame};

pub const INPUT_PROFILE_CAPACITY: usize = 8;
pub const REMAP_RULE_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InputProfileId(u8);

impl InputProfileId {
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct InputIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    /// Stable serial hash when present, otherwise a stable physical-path hash.
    pub instance_hash: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardLayout {
    Unchanged,
    Us,
    Jis,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Usage {
    pub page: u16,
    pub id: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemapRule {
    pub from: Usage,
    pub to: Usage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputProfile {
    pub id: InputProfileId,
    pub identity: InputIdentity,
    pub connected: bool,
    pub last_seen: u64,
    pub keyboard_layout: KeyboardLayout,
    pub mouse_sensitivity_percent: u16,
    pub scroll_multiplier_percent: u16,
    pub remaps: Vec<RemapRule, REMAP_RULE_CAPACITY>,
}

impl InputProfile {
    pub fn is_default(&self) -> bool {
        self.keyboard_layout == KeyboardLayout::Unchanged
            && self.mouse_sensitivity_percent == 100
            && self.scroll_multiplier_percent == 100
            && self.remaps.is_empty()
    }

    /// Applies device-owned transforms while the physical device and
    /// interface identity are still present. Callers run this before input
    /// aggregation so settings never become destination-owned.
    pub fn transform(&self, frame: &mut StandardInputFrame) {
        if let Some(keyboard) = frame.keyboard.take() {
            let mut transformed = KeyboardFrame::new(keyboard.modifiers);
            for key in keyboard.keys_down() {
                let layout_key = match (self.keyboard_layout, key.0) {
                    (KeyboardLayout::Us, 0x89) => 0x35,
                    (KeyboardLayout::Jis, 0x35) => 0x89,
                    _ => key.0,
                };
                let mapped = self
                    .remaps
                    .iter()
                    .find(|rule| rule.from.page == 0x07 && rule.from.id == u16::from(layout_key))
                    .filter(|rule| rule.to.page == 0x07 && rule.to.id <= u16::from(u8::MAX))
                    .map_or(layout_key, |rule| rule.to.id as u8);
                let _ = transformed.push_key(KeyUsage(mapped));
            }
            frame.keyboard = Some(transformed);
        }
        if let Some(mouse) = frame.mouse.as_mut() {
            mouse.movement.x = scale_i16(mouse.movement.x, self.mouse_sensitivity_percent);
            mouse.movement.y = scale_i16(mouse.movement.y, self.mouse_sensitivity_percent);
            mouse.movement.wheel = scale_i8(mouse.movement.wheel, self.scroll_multiplier_percent);
            mouse.movement.pan = scale_i8(mouse.movement.pan, self.scroll_multiplier_percent);
        }
        if let Some(consumer) = frame.consumer.as_mut()
            && let Some(active) = consumer.active
            && let Some(rule) = self
                .remaps
                .iter()
                .find(|rule| rule.from.page == 0x0c && rule.from.id == active.0)
                .filter(|rule| rule.to.page == 0x0c)
        {
            consumer.active = Some(ConsumerUsage(rule.to.id));
        }
    }
}

fn scale_i16(value: i16, percent: u16) -> i16 {
    (i32::from(value) * i32::from(percent) / 100).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
        as i16
}

fn scale_i8(value: i8, percent: u16) -> i8 {
    (i32::from(value) * i32::from(percent) / 100).clamp(i32::from(i8::MIN), i32::from(i8::MAX))
        as i8
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputProfileError {
    RegistryFullWithCustomizedProfiles,
    UnknownProfile,
    RemapCapacity,
    InvalidScale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputProfileRegistry {
    profiles: Vec<InputProfile, INPUT_PROFILE_CAPACITY>,
}

impl Default for InputProfileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InputProfileRegistry {
    pub const fn new() -> Self {
        Self {
            profiles: Vec::new(),
        }
    }

    pub fn profiles(&self) -> &[InputProfile] {
        &self.profiles
    }

    pub fn observe(
        &mut self,
        identity: InputIdentity,
        now: u64,
    ) -> Result<InputProfileId, InputProfileError> {
        if let Some(profile) = self
            .profiles
            .iter_mut()
            .find(|profile| profile.identity == identity)
        {
            profile.connected = true;
            profile.last_seen = now;
            return Ok(profile.id);
        }
        if self.profiles.is_full() {
            let removable = self
                .profiles
                .iter()
                .enumerate()
                .filter(|(_, profile)| !profile.connected && profile.is_default())
                .min_by_key(|(_, profile)| profile.last_seen)
                .map(|(index, _)| index)
                .ok_or(InputProfileError::RegistryFullWithCustomizedProfiles)?;
            self.profiles.swap_remove(removable);
        }
        let id = (1..=INPUT_PROFILE_CAPACITY as u8)
            .find(|candidate| {
                !self
                    .profiles
                    .iter()
                    .any(|profile| profile.id.get() == *candidate)
            })
            .map(InputProfileId)
            .ok_or(InputProfileError::RegistryFullWithCustomizedProfiles)?;
        self.profiles
            .push(InputProfile {
                id,
                identity,
                connected: true,
                last_seen: now,
                keyboard_layout: KeyboardLayout::Unchanged,
                mouse_sensitivity_percent: 100,
                scroll_multiplier_percent: 100,
                remaps: Vec::new(),
            })
            .map_err(|_| InputProfileError::RegistryFullWithCustomizedProfiles)?;
        Ok(id)
    }

    pub fn disconnect(&mut self, id: InputProfileId, now: u64) -> Result<(), InputProfileError> {
        let profile = self.profile_mut(id)?;
        profile.connected = false;
        profile.last_seen = now;
        Ok(())
    }

    pub fn set_layout(
        &mut self,
        id: InputProfileId,
        layout: KeyboardLayout,
    ) -> Result<(), InputProfileError> {
        self.profile_mut(id)?.keyboard_layout = layout;
        Ok(())
    }

    pub fn set_scales(
        &mut self,
        id: InputProfileId,
        mouse_percent: u16,
        scroll_percent: u16,
    ) -> Result<(), InputProfileError> {
        if !(10..=400).contains(&mouse_percent) || !(10..=400).contains(&scroll_percent) {
            return Err(InputProfileError::InvalidScale);
        }
        let profile = self.profile_mut(id)?;
        profile.mouse_sensitivity_percent = mouse_percent;
        profile.scroll_multiplier_percent = scroll_percent;
        Ok(())
    }

    pub fn set_remaps(
        &mut self,
        id: InputProfileId,
        rules: &[RemapRule],
    ) -> Result<(), InputProfileError> {
        let mut replacement = Vec::new();
        replacement
            .extend_from_slice(rules)
            .map_err(|_| InputProfileError::RemapCapacity)?;
        self.profile_mut(id)?.remaps = replacement;
        Ok(())
    }

    fn profile_mut(&mut self, id: InputProfileId) -> Result<&mut InputProfile, InputProfileError> {
        self.profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or(InputProfileError::UnknownProfile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DeviceId, InterfaceId};
    use crate::input::{
        ConsumerFrame, ModifierState, MouseButtons, MouseFrame, MouseMovement, StandardInputFrame,
    };

    fn identity(value: u16) -> InputIdentity {
        InputIdentity {
            vendor_id: value,
            product_id: value,
            instance_hash: value as u64,
        }
    }

    #[test]
    fn oldest_disconnected_default_profile_is_pruned_at_capacity() {
        let mut registry = InputProfileRegistry::new();
        let mut ids = Vec::<InputProfileId, INPUT_PROFILE_CAPACITY>::new();
        for value in 1..=8 {
            let id = registry.observe(identity(value), value as u64).unwrap();
            registry.disconnect(id, value as u64).unwrap();
            ids.push(id).unwrap();
        }
        registry.set_layout(ids[0], KeyboardLayout::Jis).unwrap();
        let replacement = registry.observe(identity(9), 9).unwrap();
        assert_eq!(replacement, ids[1]);
        assert!(
            registry
                .profiles()
                .iter()
                .any(|profile| profile.id == ids[0])
        );
    }

    #[test]
    fn connected_or_customized_profiles_are_never_automatically_deleted() {
        let mut registry = InputProfileRegistry::new();
        for value in 1..=8 {
            let id = registry.observe(identity(value), value as u64).unwrap();
            if value != 1 {
                registry.disconnect(id, value as u64).unwrap();
                registry.set_layout(id, KeyboardLayout::Us).unwrap();
            }
        }
        assert_eq!(
            registry.observe(identity(9), 9),
            Err(InputProfileError::RegistryFullWithCustomizedProfiles)
        );
    }

    #[test]
    fn remap_capacity_is_shared_across_usage_pages() {
        let mut registry = InputProfileRegistry::new();
        let id = registry.observe(identity(1), 1).unwrap();
        let rules = [RemapRule {
            from: Usage { page: 7, id: 4 },
            to: Usage { page: 12, id: 233 },
        }; REMAP_RULE_CAPACITY];
        registry.set_remaps(id, &rules).unwrap();
        let mut too_many = [rules[0]; REMAP_RULE_CAPACITY + 1];
        too_many[REMAP_RULE_CAPACITY].from.id = 5;
        assert_eq!(
            registry.set_remaps(id, &too_many),
            Err(InputProfileError::RemapCapacity)
        );
    }

    #[test]
    fn transforms_are_owned_by_input_and_run_before_aggregation() {
        let mut registry = InputProfileRegistry::new();
        let id = registry.observe(identity(1), 1).unwrap();
        registry.set_layout(id, KeyboardLayout::Us).unwrap();
        registry.set_scales(id, 200, 50).unwrap();
        registry
            .set_remaps(
                id,
                &[
                    RemapRule {
                        from: Usage {
                            page: 0x07,
                            id: 0x35,
                        },
                        to: Usage {
                            page: 0x07,
                            id: 0x04,
                        },
                    },
                    RemapRule {
                        from: Usage {
                            page: 0x0c,
                            id: 0xe9,
                        },
                        to: Usage {
                            page: 0x0c,
                            id: 0xea,
                        },
                    },
                ],
            )
            .unwrap();
        let profile = registry.profiles().first().unwrap();
        let mut keyboard = KeyboardFrame::new(ModifierState::empty());
        keyboard.push_key(KeyUsage(0x89)).unwrap();
        let mut frame = StandardInputFrame {
            device_id: DeviceId(1),
            interface_id: InterfaceId(1),
            keyboard: Some(keyboard),
            mouse: Some(MouseFrame {
                buttons: MouseButtons::empty(),
                movement: MouseMovement {
                    x: 10,
                    y: -10,
                    wheel: 4,
                    pan: -4,
                },
            }),
            consumer: Some(ConsumerFrame {
                active: Some(ConsumerUsage(0xe9)),
            }),
        };
        profile.transform(&mut frame);
        assert_eq!(frame.keyboard.unwrap().keys_down(), &[KeyUsage(0x04)]);
        assert_eq!(frame.mouse.unwrap().movement.x, 20);
        assert_eq!(frame.mouse.unwrap().movement.wheel, 2);
        assert_eq!(frame.consumer.unwrap().active, Some(ConsumerUsage(0xea)));
    }
}
