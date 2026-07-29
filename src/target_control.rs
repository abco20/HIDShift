#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonIntent {
    NextConnectedTarget,
    EnterPairingMode,
    ClearActiveHostBond,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootRecoveryAction {
    Normal,
    Suppress,
    FactoryReset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootRecoveryControl {
    arm_until_ms: u64,
    pressed_since_ms: Option<u64>,
    hold_ms: u64,
}

impl BootRecoveryControl {
    pub const fn new(now_ms: u64, arm_window_ms: u64, hold_ms: u64) -> Self {
        Self {
            arm_until_ms: now_ms.saturating_add(arm_window_ms),
            pressed_since_ms: None,
            hold_ms,
        }
    }

    pub fn sample(&mut self, pressed: bool, now_ms: u64) -> BootRecoveryAction {
        if let Some(pressed_since_ms) = self.pressed_since_ms {
            if pressed {
                return BootRecoveryAction::Suppress;
            }
            self.pressed_since_ms = None;
            return if now_ms.saturating_sub(pressed_since_ms) >= self.hold_ms {
                BootRecoveryAction::FactoryReset
            } else {
                BootRecoveryAction::Suppress
            };
        }

        if pressed && now_ms <= self.arm_until_ms {
            self.pressed_since_ms = Some(now_ms);
            return BootRecoveryAction::Suppress;
        }

        BootRecoveryAction::Normal
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetSwitchControl {
    button: DebouncedButton,
    long_press_ms: u64,
    very_long_press_ms: u64,
    pressed_since_ms: Option<u64>,
}

impl TargetSwitchControl {
    pub const DEFAULT_DEBOUNCE_MS: u64 = 30;
    pub const DEFAULT_LONG_PRESS_MS: u64 = 3_000;
    pub const DEFAULT_VERY_LONG_PRESS_MS: u64 = 8_000;

    pub const fn new() -> Self {
        Self::with_timings(
            Self::DEFAULT_DEBOUNCE_MS,
            Self::DEFAULT_LONG_PRESS_MS,
            Self::DEFAULT_VERY_LONG_PRESS_MS,
        )
    }

    pub const fn with_debounce_ms(debounce_ms: u64) -> Self {
        Self::with_timings(
            debounce_ms,
            Self::DEFAULT_LONG_PRESS_MS,
            Self::DEFAULT_VERY_LONG_PRESS_MS,
        )
    }

    pub const fn with_timings(
        debounce_ms: u64,
        long_press_ms: u64,
        very_long_press_ms: u64,
    ) -> Self {
        Self {
            button: DebouncedButton::new(debounce_ms),
            long_press_ms,
            very_long_press_ms,
            pressed_since_ms: None,
        }
    }

    pub fn target_button_sample(&mut self, pressed: bool, now_ms: u64) -> Option<ButtonIntent> {
        match self.button.update(pressed, now_ms) {
            Some(DebouncedButtonEvent::Pressed) => {
                self.pressed_since_ms = Some(now_ms);
                None
            }
            Some(DebouncedButtonEvent::Released) => {
                let pressed_since_ms = self.pressed_since_ms.take()?;
                let held_ms = now_ms.saturating_sub(pressed_since_ms);
                if held_ms >= self.very_long_press_ms {
                    Some(ButtonIntent::ClearActiveHostBond)
                } else if held_ms >= self.long_press_ms {
                    Some(ButtonIntent::EnterPairingMode)
                } else {
                    Some(ButtonIntent::NextConnectedTarget)
                }
            }
            None => None,
        }
    }
}

impl Default for TargetSwitchControl {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DebouncedButton {
    debounce_ms: u64,
    stable_pressed: bool,
    candidate_pressed: bool,
    candidate_since_ms: u64,
    press_reported: bool,
}

impl DebouncedButton {
    pub const fn new(debounce_ms: u64) -> Self {
        Self {
            debounce_ms,
            stable_pressed: false,
            candidate_pressed: false,
            candidate_since_ms: 0,
            press_reported: false,
        }
    }

    pub fn update(&mut self, pressed: bool, now_ms: u64) -> Option<DebouncedButtonEvent> {
        if pressed != self.candidate_pressed {
            self.candidate_pressed = pressed;
            self.candidate_since_ms = now_ms;
            return None;
        }

        if pressed == self.stable_pressed {
            return None;
        }

        if now_ms.saturating_sub(self.candidate_since_ms) < self.debounce_ms {
            return None;
        }

        self.stable_pressed = pressed;
        if self.stable_pressed {
            if self.press_reported {
                None
            } else {
                self.press_reported = true;
                Some(DebouncedButtonEvent::Pressed)
            }
        } else {
            self.press_reported = false;
            Some(DebouncedButtonEvent::Released)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebouncedButtonEvent {
    Pressed,
    Released,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_press_requests_next_connected_target() {
        let mut control = TargetSwitchControl::with_debounce_ms(30);

        assert_eq!(control.target_button_sample(true, 10), None);
        assert_eq!(control.target_button_sample(true, 40), None);
        assert_eq!(control.target_button_sample(false, 50), None);
        assert_eq!(
            control.target_button_sample(false, 80),
            Some(ButtonIntent::NextConnectedTarget)
        );
    }

    #[test]
    fn long_press_requests_pairing_mode() {
        let mut control = TargetSwitchControl::with_timings(30, 300, 800);

        assert_eq!(control.target_button_sample(true, 0), None);
        assert_eq!(control.target_button_sample(true, 30), None);
        assert_eq!(control.target_button_sample(true, 330), None);
        assert_eq!(control.target_button_sample(false, 400), None);
        assert_eq!(
            control.target_button_sample(false, 430),
            Some(ButtonIntent::EnterPairingMode)
        );
    }

    #[test]
    fn very_long_press_requests_bond_clear() {
        let mut control = TargetSwitchControl::with_timings(30, 300, 800);

        assert_eq!(control.target_button_sample(true, 0), None);
        assert_eq!(control.target_button_sample(true, 30), None);
        assert_eq!(control.target_button_sample(true, 330), None);
        assert_eq!(control.target_button_sample(true, 830), None);
        assert_eq!(control.target_button_sample(false, 900), None);
        assert_eq!(
            control.target_button_sample(false, 930),
            Some(ButtonIntent::ClearActiveHostBond)
        );
    }

    #[test]
    fn repeated_samples_do_not_duplicate_press_events() {
        let mut control = TargetSwitchControl::with_debounce_ms(30);

        assert_eq!(control.target_button_sample(true, 10), None);
        assert_eq!(control.target_button_sample(true, 40), None);
        assert_eq!(control.target_button_sample(false, 50), None);
        assert_eq!(
            control.target_button_sample(false, 80),
            Some(ButtonIntent::NextConnectedTarget)
        );
        assert_eq!(control.target_button_sample(true, 90), None);
        assert_eq!(control.target_button_sample(true, 120), None);
        assert_eq!(control.target_button_sample(false, 130), None);
        assert_eq!(
            control.target_button_sample(false, 160),
            Some(ButtonIntent::NextConnectedTarget)
        );
    }

    #[test]
    fn boot_recovery_arms_when_pressed_during_the_startup_window() {
        let mut recovery = BootRecoveryControl::new(0, 3_000, 5_000);

        assert_eq!(recovery.sample(false, 2_000), BootRecoveryAction::Normal);
        assert_eq!(recovery.sample(true, 2_500), BootRecoveryAction::Suppress);
        assert_eq!(recovery.sample(true, 7_499), BootRecoveryAction::Suppress);
        assert_eq!(
            recovery.sample(false, 7_500),
            BootRecoveryAction::FactoryReset
        );
        assert_eq!(recovery.sample(false, 7_510), BootRecoveryAction::Normal);
    }

    #[test]
    fn short_startup_press_is_consumed_without_triggering_a_runtime_button_action() {
        let mut recovery = BootRecoveryControl::new(0, 3_000, 5_000);

        assert_eq!(recovery.sample(true, 1_000), BootRecoveryAction::Suppress);
        assert_eq!(recovery.sample(false, 2_000), BootRecoveryAction::Suppress);
        assert_eq!(recovery.sample(false, 2_010), BootRecoveryAction::Normal);
    }

    #[test]
    fn boot_recovery_is_inactive_after_the_startup_window() {
        let mut recovery = BootRecoveryControl::new(0, 3_000, 5_000);

        assert_eq!(recovery.sample(false, 3_001), BootRecoveryAction::Normal);
        assert_eq!(recovery.sample(true, 3_010), BootRecoveryAction::Normal);
    }
}
