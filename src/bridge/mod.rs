mod host_state;
mod pairing;

use crate::ids::{DeviceId, HostId, InterfaceId};
use crate::input::{
    ConsumerState, InputAggregator, InputFrame, KeyboardLedState, KeyboardSuppression,
    MouseButtons, MouseMovement, PhysicalInputState, StandardInputFrame,
};
use crate::reports::{
    BleConsumerReport, BleHidReport, BleKeyboard6KroReport, BleKeyboardLedOutputReport,
    BleKeyboardOutputError, BleMouseReport, KEYBOARD_6KRO_KEY_CAPACITY, ReportKind,
};
use crate::storage::{StorageError, StorageState, StoredBond};

pub use host_state::{BleHostStateMachine, HostRuntimeState, HostStateError, ReportReady};
pub use pairing::{PairingMode, PairingSession};

pub const BRIDGE_USB_HID_INTERFACES_MAX: usize = 8;
pub const BRIDGE_INPUT_INTERFACES_MAX: usize = BRIDGE_USB_HID_INTERFACES_MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbHidInterfaceRegistration {
    pub interface_id: InterfaceId,
    pub device_id: DeviceId,
    pub keyboard_led_sink: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bridge<const HOSTS: usize> {
    state: BridgeState<HOSTS>,
}

impl<const HOSTS: usize> Bridge<HOSTS> {
    pub const fn new() -> Self {
        Self {
            state: BridgeState::new(),
        }
    }

    pub const fn state(&self) -> &BridgeState<HOSTS> {
        &self.state
    }

    /// Quiesce is the one path that intentionally changes only BLE session
    /// state. Normal runtime inputs must continue through `handle_event` so
    /// their actions cannot be discarded.
    pub fn mark_host_disconnected_for_quiesce(&mut self, host_id: HostId) {
        self.state.hosts.on_disconnected(host_id);
    }

    pub fn prepare_for_quiesce(&mut self) -> Result<(), BridgeError> {
        self.state
            .suppression
            .keyboard
            .capture_from(&self.state.input.keyboard)?;
        self.state.suppression.mouse_buttons = self.state.input.mouse.buttons;
        self.state.suppression.consumer = self.state.input.consumer;
        Ok(())
    }

    pub fn set_discovered_host_name(
        &mut self,
        host_id: HostId,
        name: crate::storage::FixedName,
    ) -> Result<bool, HostStateError> {
        self.state.hosts.set_discovered_name(host_id, name)
    }

    pub fn handle_event<const ACTIONS: usize>(
        &mut self,
        event: BridgeEvent,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        let mut next = self.clone();
        let mut next_actions = heapless::Vec::new();
        next.handle_event_in_place(event, &mut next_actions)?;
        *self = next;
        *out = next_actions;
        Ok(())
    }

    pub(crate) fn handle_event_in_place<const ACTIONS: usize>(
        &mut self,
        event: BridgeEvent,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        match event {
            BridgeEvent::InputFrame(frame) => self.handle_input_frame(frame, out),
            BridgeEvent::HostConnected { host_id } => {
                self.state.hosts.on_connected(host_id)?;
                self.push_status(out)
            }
            BridgeEvent::HostDisconnected { host_id } => {
                self.state.hosts.on_disconnected(host_id);
                self.push_status(out)
            }
            BridgeEvent::HostSecurityChanged {
                host_id,
                encrypted,
                bonded,
                bond,
            } => {
                let persist = self
                    .state
                    .hosts
                    .on_security_changed(host_id, encrypted, bonded, bond)?;
                if persist {
                    push_action(out, BridgeAction::PersistProfiles)?;
                }
                if bonded && self.state.pairable_host == Some(host_id) {
                    self.state.pairable_host = None;
                    push_action(out, BridgeAction::RejectPairing { host_id })?;
                }
                self.push_status(out)
            }
            BridgeEvent::CccdChanged {
                host_id,
                report,
                enabled,
            } => {
                let persist = self.state.hosts.on_cccd_changed(host_id, report, enabled)?;
                if persist {
                    push_action(out, BridgeAction::PersistProfiles)?;
                }
                self.push_status(out)
            }
            BridgeEvent::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                keyboard_led_sink,
            } => {
                self.state
                    .upsert_usb_hid_interface(interface_id, device_id, keyboard_led_sink)?;
                self.apply_active_host_leds(out)
            }
            BridgeEvent::HostKeyboardLedChanged { host_id, leds } => {
                self.state.hosts.on_keyboard_led_changed(host_id, leds)?;
                if self.state.hosts.active_target() == Some(host_id) {
                    self.push_usb_keyboard_leds(leds, out)
                } else {
                    Ok(())
                }
            }
            BridgeEvent::SetHostName { host_id, name } => {
                if self.state.hosts.set_name(host_id, name)? {
                    push_action(out, BridgeAction::PersistProfiles)?;
                }
                self.push_status(out)
            }
            BridgeEvent::EnterPairingMode { host_id } => self.enter_pairing_mode(host_id, out),
            BridgeEvent::PairingModeExpired { host_id } => self.expire_pairing_mode(host_id, out),
            BridgeEvent::ClearHost { host_id } => self.clear_host(host_id, out),
            BridgeEvent::SwitchTarget { target } => self.switch_target(target, out),
            BridgeEvent::UsbDeviceRemoved { interface_id } => {
                self.usb_device_removed(interface_id, out)
            }
        }
    }

    fn handle_input_frame<const ACTIONS: usize>(
        &mut self,
        frame: InputFrame,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        let InputFrame::Standard(frame) = frame else {
            self.state.stats.vendor_frames_ignored =
                self.state.stats.vendor_frames_ignored.saturating_add(1);
            return Ok(());
        };

        self.handle_standard_input_frame(frame, out)
    }

    fn handle_standard_input_frame<const ACTIONS: usize>(
        &mut self,
        frame: StandardInputFrame,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        let prepared_input_devices = self.state.input_devices.prepare_frame(&frame)?;
        let aggregate = prepared_input_devices.aggregate(&self.state.input_devices);
        let mut next_keyboard = None;
        let mut next_keyboard_suppression = None;
        let mut next_mouse = None;
        let mut next_mouse_suppression = None;
        let mut next_consumer = None;
        let mut next_consumer_suppression = None;
        let mut next_stats = self.state.stats;
        let mut next_out = out.clone();

        if frame.keyboard.is_some() {
            let mut keyboard_state = self.state.input.keyboard.clone();
            let mut keyboard_suppression = self.state.suppression.keyboard.clone();
            keyboard_state.apply_state(&aggregate.keyboard, &mut keyboard_suppression)?;

            let suppressed = keyboard_suppression
                .suppress_visible_after(&keyboard_state, KEYBOARD_6KRO_KEY_CAPACITY)?;
            if suppressed != 0 {
                next_stats.keyboard_reports_truncated =
                    next_stats.keyboard_reports_truncated.saturating_add(1);
            }

            if let Some(host_id) = self.ready_active_host(ReportKind::Keyboard) {
                let build = BleKeyboard6KroReport::from_physical_state(
                    &keyboard_state,
                    &keyboard_suppression,
                );
                debug_assert!(!build.truncated);
                push_action(
                    &mut next_out,
                    BridgeAction::BleNotify {
                        host_id,
                        report: BleHidReport::Keyboard(build.report),
                        reason: if keyboard_contains_release(
                            &self.state.input.keyboard,
                            &keyboard_state,
                        ) {
                            NotifyReason::InputRelease
                        } else {
                            NotifyReason::Input
                        },
                    },
                )?;
            } else {
                next_stats.input_reports_dropped_no_ready_target = next_stats
                    .input_reports_dropped_no_ready_target
                    .saturating_add(1);
            }
            next_keyboard = Some(keyboard_state);
            next_keyboard_suppression = Some(keyboard_suppression);
        }

        if let Some(mouse) = frame.mouse {
            let mouse_state = aggregate.mouse;
            let mouse_suppression = self
                .state
                .suppression
                .mouse_buttons
                .intersection(mouse_state.buttons);
            let visible_buttons = mouse_state.buttons.without(mouse_suppression);
            if let Some(host_id) = self.ready_active_host(ReportKind::Mouse) {
                let released_buttons =
                    self.state.input.mouse.buttons.bits() & !mouse_state.buttons.bits() != 0;
                push_action(
                    &mut next_out,
                    BridgeAction::BleNotify {
                        host_id,
                        report: BleHidReport::Mouse(BleMouseReport::from_frame(
                            visible_buttons,
                            mouse.movement,
                        )),
                        reason: if released_buttons
                            || self.state.input.mouse.buttons != mouse_state.buttons
                        {
                            NotifyReason::InputEdge
                        } else {
                            NotifyReason::Input
                        },
                    },
                )?;
            } else if mouse.movement != MouseMovement::neutral() {
                next_stats.mouse_movements_dropped =
                    next_stats.mouse_movements_dropped.saturating_add(1);
            }
            next_mouse = Some(mouse_state);
            next_mouse_suppression = Some(mouse_suppression);
        }

        if frame.consumer.is_some() {
            let consumer_state = aggregate.consumer;
            let mut consumer_suppression = self.state.suppression.consumer;
            if consumer_state.active != consumer_suppression.active {
                consumer_suppression.active = None;
            }
            if let Some(host_id) = self.ready_active_host(ReportKind::Consumer) {
                let visible_consumer = if consumer_state.active == consumer_suppression.active {
                    None
                } else {
                    consumer_state.active
                };
                let report = match visible_consumer {
                    Some(usage) => BleConsumerReport::from_usage(usage),
                    None => BleConsumerReport::release(),
                };
                push_action(
                    &mut next_out,
                    BridgeAction::BleNotify {
                        host_id,
                        report: BleHidReport::Consumer(report),
                        reason: if self.state.input.consumer.active.is_some()
                            && consumer_state.active.is_none()
                        {
                            NotifyReason::InputRelease
                        } else {
                            NotifyReason::Input
                        },
                    },
                )?;
            }
            next_consumer = Some(consumer_state);
            next_consumer_suppression = Some(consumer_suppression);
        }

        self.state
            .input_devices
            .commit_frame(prepared_input_devices);
        if let Some(keyboard) = next_keyboard {
            self.state.input.keyboard = keyboard;
        }
        if let Some(suppression) = next_keyboard_suppression {
            self.state.suppression.keyboard = suppression;
        }
        if let Some(mouse) = next_mouse {
            self.state.input.mouse = mouse;
        }
        if let Some(suppression) = next_mouse_suppression {
            self.state.suppression.mouse_buttons = suppression;
        }
        if let Some(consumer) = next_consumer {
            self.state.input.consumer = consumer;
        }
        if let Some(suppression) = next_consumer_suppression {
            self.state.suppression.consumer = suppression;
        }
        self.state.stats = next_stats;
        *out = next_out;
        Ok(())
    }

    fn switch_target<const ACTIONS: usize>(
        &mut self,
        target: HostId,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        self.activate_target(target, NotifyReason::TargetSwitchRelease, out)?;
        self.push_status(out)
    }

    fn enter_pairing_mode<const ACTIONS: usize>(
        &mut self,
        host_id: HostId,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        self.activate_target(host_id, NotifyReason::TargetSwitchRelease, out)?;
        if let Some(previous) = self.state.pairable_host.replace(host_id)
            && previous != host_id
        {
            push_action(out, BridgeAction::RejectPairing { host_id: previous })?;
        }
        push_action(out, BridgeAction::AllowPairing { host_id })?;
        self.push_status(out)
    }

    fn activate_target<const ACTIONS: usize>(
        &mut self,
        target: HostId,
        release_reason: NotifyReason,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<bool, BridgeError> {
        if self.state.hosts.active_target() == Some(target) {
            return Ok(false);
        }
        if let Some(old_host) = self.state.hosts.active_target() {
            self.push_release_reports(old_host, release_reason, out)?;
        }
        self.state
            .suppression
            .keyboard
            .capture_from(&self.state.input.keyboard)?;
        self.state.suppression.mouse_buttons = self.state.input.mouse.buttons;
        self.state.suppression.consumer = self.state.input.consumer;
        self.state.hosts.set_active_target(target)?;
        push_action(out, BridgeAction::ActivateInput { host_id: target })?;
        self.apply_active_host_leds(out)?;
        push_action(out, BridgeAction::PersistProfiles)?;
        Ok(true)
    }

    fn expire_pairing_mode<const ACTIONS: usize>(
        &mut self,
        host_id: HostId,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        if self.state.pairable_host == Some(host_id) {
            self.state.pairable_host = None;
            push_action(out, BridgeAction::RejectPairing { host_id })?;
            self.push_status(out)?;
        }
        Ok(())
    }

    fn clear_host<const ACTIONS: usize>(
        &mut self,
        host_id: HostId,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        let was_active = self.state.hosts.active_target() == Some(host_id);
        let bond = self.state.hosts.host(host_id).and_then(|host| host.bond);
        if was_active {
            self.push_release_reports(host_id, NotifyReason::SafetyRelease, out)?;
        }
        if self.state.pairable_host == Some(host_id) {
            self.state.pairable_host = None;
            push_action(out, BridgeAction::RejectPairing { host_id })?;
        }
        let removed = self.state.hosts.clear_host(host_id);
        if removed {
            push_action(out, BridgeAction::ClearBond { host_id, bond })?;
            push_action(out, BridgeAction::PersistProfiles)?;
            if was_active {
                self.push_usb_keyboard_leds(KeyboardLedState::empty(), out)?;
            }
            self.push_status(out)?;
        }
        Ok(())
    }

    fn usb_device_removed<const ACTIONS: usize>(
        &mut self,
        interface_id: InterfaceId,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        let previous_input = self.state.input.clone();
        self.state.remove_usb_hid_interface(interface_id);
        self.state.input_devices.remove_interface(interface_id);
        self.state.input = self.state.input_devices.aggregate().clone();

        if let Some(host_id) = self.state.hosts.active_target() {
            self.emit_detach_updates(
                host_id,
                &previous_input,
                NotifyReason::UsbDeviceRemovedRelease,
                out,
            )?;
        }
        self.push_status(out)
    }

    fn apply_active_host_leds<const ACTIONS: usize>(
        &self,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        let Some(active_target) = self.state.hosts.active_target() else {
            return Ok(());
        };
        let leds = self
            .state
            .hosts
            .host(active_target)
            .and_then(|host| host.keyboard_leds)
            .unwrap_or_else(KeyboardLedState::empty);
        self.push_usb_keyboard_leds(leds, out)
    }

    fn push_usb_keyboard_leds<const ACTIONS: usize>(
        &self,
        leds: KeyboardLedState,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        for sink in self
            .state
            .usb_hid_interfaces
            .iter()
            .copied()
            .filter(|sink| sink.keyboard_led_sink)
        {
            push_action(
                out,
                BridgeAction::UsbSetKeyboardLeds {
                    interface_id: sink.interface_id,
                    device_id: sink.device_id,
                    leds,
                },
            )?;
        }
        Ok(())
    }

    fn emit_detach_updates<const ACTIONS: usize>(
        &mut self,
        host_id: HostId,
        previous_input: &PhysicalInputState,
        reason: NotifyReason,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        let keyboard = self.state.input.keyboard.to_frame()?;
        self.state
            .input
            .keyboard
            .apply_frame(&keyboard, &mut self.state.suppression.keyboard)?;

        if self.can_send(host_id, ReportKind::Keyboard)
            && previous_input.keyboard != self.state.input.keyboard
        {
            let visible = self
                .state
                .input
                .keyboard
                .visible_against(&self.state.suppression.keyboard);
            push_action(
                out,
                BridgeAction::BleNotify {
                    host_id,
                    report: BleHidReport::Keyboard(
                        BleKeyboard6KroReport::from_visible_state(&visible).report,
                    ),
                    reason,
                },
            )?;
        }

        self.state.suppression.mouse_buttons = self
            .state
            .suppression
            .mouse_buttons
            .intersection(self.state.input.mouse.buttons);
        if self.can_send(host_id, ReportKind::Mouse)
            && previous_input.mouse.buttons != self.state.input.mouse.buttons
        {
            let visible_buttons = self
                .state
                .input
                .mouse
                .buttons
                .without(self.state.suppression.mouse_buttons);
            push_action(
                out,
                BridgeAction::BleNotify {
                    host_id,
                    report: BleHidReport::Mouse(BleMouseReport::from_frame(
                        visible_buttons,
                        MouseMovement::neutral(),
                    )),
                    reason,
                },
            )?;
        }

        if self.state.input.consumer.active != self.state.suppression.consumer.active {
            self.state.suppression.consumer.active = None;
        }
        if self.can_send(host_id, ReportKind::Consumer)
            && previous_input.consumer.active != self.state.input.consumer.active
        {
            let report = match self.state.input.consumer.active {
                Some(usage) => BleConsumerReport::from_usage(usage),
                None => BleConsumerReport::release(),
            };
            push_action(
                out,
                BridgeAction::BleNotify {
                    host_id,
                    report: BleHidReport::Consumer(report),
                    reason,
                },
            )?;
        }

        Ok(())
    }

    fn push_release_reports<const ACTIONS: usize>(
        &self,
        host_id: HostId,
        reason: NotifyReason,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        if self.can_send(host_id, ReportKind::Keyboard) {
            push_action(
                out,
                BridgeAction::BleNotify {
                    host_id,
                    report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                    reason,
                },
            )?;
        }
        if self.can_send(host_id, ReportKind::Mouse) {
            push_action(
                out,
                BridgeAction::BleNotify {
                    host_id,
                    report: BleHidReport::Mouse(BleMouseReport::release_buttons()),
                    reason,
                },
            )?;
        }
        if self.can_send(host_id, ReportKind::Consumer) {
            push_action(
                out,
                BridgeAction::BleNotify {
                    host_id,
                    report: BleHidReport::Consumer(BleConsumerReport::release()),
                    reason,
                },
            )?;
        }
        Ok(())
    }

    fn push_status<const ACTIONS: usize>(
        &self,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        push_action(out, BridgeAction::StatusChanged(self.status()))
    }

    fn status(&self) -> BridgeStatus {
        BridgeStatus {
            active_target: self.state.hosts.active_target(),
            pairable_host: self.state.pairable_host,
        }
    }

    fn ready_active_host(&self, kind: ReportKind) -> Option<HostId> {
        let host_id = self.state.hosts.active_target()?;
        self.can_send(host_id, kind).then_some(host_id)
    }

    pub fn can_send(&self, host_id: HostId, kind: ReportKind) -> bool {
        self.state.hosts.can_send(host_id, kind)
    }

    pub fn storage_state(&self, generation: u32) -> Result<StorageState, StorageError> {
        self.state.hosts.storage_state(generation)
    }

    pub fn restore_storage_state<const ACTIONS: usize>(
        &mut self,
        storage: &StorageState,
        out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    ) -> Result<(), BridgeError> {
        out.clear();
        self.state.hosts.restore(storage)?;
        self.state.pairable_host = None;
        Ok(())
    }
}

impl<const HOSTS: usize> Default for Bridge<HOSTS> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeState<const HOSTS: usize> {
    pub input: PhysicalInputState,
    pub input_devices: InputAggregator<BRIDGE_INPUT_INTERFACES_MAX>,
    pub suppression: SuppressionState,
    pub usb_hid_interfaces:
        heapless::Vec<UsbHidInterfaceRegistration, BRIDGE_USB_HID_INTERFACES_MAX>,
    pub hosts: BleHostStateMachine<HOSTS>,
    pub pairable_host: Option<HostId>,
    pub stats: BridgeStats,
}

impl<const HOSTS: usize> BridgeState<HOSTS> {
    pub const fn new() -> Self {
        Self {
            input: PhysicalInputState::new(),
            input_devices: InputAggregator::new(),
            suppression: SuppressionState::new(),
            usb_hid_interfaces: heapless::Vec::new(),
            hosts: BleHostStateMachine::new(),
            pairable_host: None,
            stats: BridgeStats::new(),
        }
    }

    fn upsert_usb_hid_interface(
        &mut self,
        interface_id: InterfaceId,
        device_id: DeviceId,
        keyboard_led_sink: bool,
    ) -> Result<(), BridgeError> {
        if let Some(index) = self
            .usb_hid_interfaces
            .iter()
            .position(|sink| sink.interface_id == interface_id)
        {
            self.usb_hid_interfaces[index] = UsbHidInterfaceRegistration {
                interface_id,
                device_id,
                keyboard_led_sink,
            };
            return Ok(());
        }
        self.usb_hid_interfaces
            .push(UsbHidInterfaceRegistration {
                interface_id,
                device_id,
                keyboard_led_sink,
            })
            .map_err(|_| BridgeError::UsbHidInterfaceCapacity)
    }

    fn remove_usb_hid_interface(&mut self, interface_id: InterfaceId) {
        if let Some(index) = self
            .usb_hid_interfaces
            .iter()
            .position(|sink| sink.interface_id == interface_id)
        {
            self.usb_hid_interfaces.swap_remove(index);
        }
    }
}

impl<const HOSTS: usize> Default for BridgeState<HOSTS> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuppressionState {
    pub keyboard: KeyboardSuppression,
    pub mouse_buttons: MouseButtons,
    pub consumer: ConsumerState,
}

impl SuppressionState {
    pub const fn new() -> Self {
        Self {
            keyboard: KeyboardSuppression::new(),
            mouse_buttons: MouseButtons::empty(),
            consumer: ConsumerState::new(),
        }
    }
}

impl Default for SuppressionState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BridgeStats {
    pub input_reports_dropped_no_ready_target: u32,
    pub mouse_movements_dropped: u32,
    pub keyboard_reports_truncated: u32,
    pub vendor_frames_ignored: u32,
}

impl BridgeStats {
    pub const fn new() -> Self {
        Self {
            input_reports_dropped_no_ready_target: 0,
            mouse_movements_dropped: 0,
            keyboard_reports_truncated: 0,
            vendor_frames_ignored: 0,
        }
    }
}

impl Default for BridgeStats {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeEvent {
    InputFrame(InputFrame),
    UsbDeviceRemoved {
        interface_id: InterfaceId,
    },
    UsbHidInterfaceConnected {
        interface_id: InterfaceId,
        device_id: DeviceId,
        keyboard_led_sink: bool,
    },
    HostConnected {
        host_id: HostId,
    },
    HostDisconnected {
        host_id: HostId,
    },
    HostSecurityChanged {
        host_id: HostId,
        encrypted: bool,
        bonded: bool,
        bond: Option<StoredBond>,
    },
    CccdChanged {
        host_id: HostId,
        report: ReportKind,
        enabled: bool,
    },
    HostKeyboardLedChanged {
        host_id: HostId,
        leds: KeyboardLedState,
    },
    SetHostName {
        host_id: HostId,
        name: crate::storage::FixedName,
    },
    EnterPairingMode {
        host_id: HostId,
    },
    PairingModeExpired {
        host_id: HostId,
    },
    ClearHost {
        host_id: HostId,
    },
    SwitchTarget {
        target: HostId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeAction {
    BleNotify {
        host_id: HostId,
        report: BleHidReport,
        reason: NotifyReason,
    },
    UsbSetKeyboardLeds {
        interface_id: InterfaceId,
        device_id: DeviceId,
        leds: KeyboardLedState,
    },
    AllowPairing {
        host_id: HostId,
    },
    RejectPairing {
        host_id: HostId,
    },
    ClearBond {
        host_id: HostId,
        bond: Option<StoredBond>,
    },
    ActivateInput {
        host_id: HostId,
    },
    PersistProfiles,
    StatusChanged(BridgeStatus),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotifyReason {
    Input,
    /// An ordered mouse button transition. Both press and release edges use
    /// the critical lane so movement and click ordering cannot be lost.
    InputEdge,
    InputRelease,
    TargetSwitchRelease,
    UsbDeviceRemovedRelease,
    SafetyRelease,
}

fn keyboard_contains_release(
    previous: &crate::input::PhysicalKeyboardState,
    current: &crate::input::PhysicalKeyboardState,
) -> bool {
    previous
        .keys()
        .iter()
        .any(|key| !current.keys().contains(key))
        || !(previous.modifiers & !current.modifiers).is_empty()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BridgeStatus {
    pub active_target: Option<HostId>,
    pub pairable_host: Option<HostId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeError {
    ActionCapacity,
    UsbHidInterfaceCapacity,
    HostState(HostStateError),
    Input(crate::input::InputError),
    Storage(StorageError),
}

impl From<crate::input::InputError> for BridgeError {
    fn from(error: crate::input::InputError) -> Self {
        Self::Input(error)
    }
}

impl From<StorageError> for BridgeError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<HostStateError> for BridgeError {
    fn from(error: HostStateError) -> Self {
        Self::HostState(error)
    }
}

fn push_action<const ACTIONS: usize>(
    out: &mut heapless::Vec<BridgeAction, ACTIONS>,
    action: BridgeAction,
) -> Result<(), BridgeError> {
    out.push(action).map_err(|_| BridgeError::ActionCapacity)
}

pub fn keyboard_led_event_from_ble_output(
    host_id: HostId,
    bytes: &[u8],
) -> Result<BridgeEvent, BleKeyboardOutputError> {
    let report = BleKeyboardLedOutputReport::from_bytes(bytes)?;
    Ok(BridgeEvent::HostKeyboardLedChanged {
        host_id,
        leds: report.leds(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        ConsumerFrame, ConsumerUsage, KeyUsage, KeyboardFrame, ModifierState, MouseButton,
        MouseButtons, MouseFrame, MouseMovement,
    };
    use crate::storage::StoredHostProfile;

    const HOST_A: HostId = HostId(1);
    const HOST_B: HostId = HostId(2);
    const DEVICE: DeviceId = DeviceId(1);

    #[test]
    fn keyboard_input_is_not_sent_until_active_host_is_ready() {
        let mut bridge = Bridge::<2>::new();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();

        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_A }, &mut actions)
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04]))),
                &mut actions,
            )
            .unwrap();

        assert!(actions.is_empty());
        assert_eq!(
            bridge.state().stats.input_reports_dropped_no_ready_target,
            1
        );
    }

    #[test]
    fn host_capacity_errors_without_panicking() {
        let mut bridge = Bridge::<1>::new();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();

        bridge
            .handle_event(BridgeEvent::HostConnected { host_id: HOST_A }, &mut actions)
            .unwrap();

        assert_eq!(
            bridge.handle_event(BridgeEvent::HostConnected { host_id: HOST_B }, &mut actions),
            Err(BridgeError::HostState(HostStateError::HostCapacity))
        );
    }

    #[test]
    fn keyboard_input_is_sent_only_to_active_ready_host() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04, 0x05]))),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::BleNotify {
                host_id: HOST_A,
                report: BleHidReport::Keyboard(
                    BleKeyboard6KroReport::from_visible_state(
                        &bridge
                            .state()
                            .input
                            .keyboard
                            .visible_against(&bridge.state().suppression.keyboard)
                    )
                    .report
                ),
                reason: NotifyReason::Input,
            }]
        );
    }

    #[test]
    fn in_place_input_action_capacity_error_does_not_commit_state_or_output() {
        let mut bridge = ready_bridge();
        let before = bridge.clone();
        let mut actions = heapless::Vec::<BridgeAction, 0>::new();

        assert_eq!(
            bridge.handle_event_in_place(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04]))),
                &mut actions,
            ),
            Err(BridgeError::ActionCapacity)
        );
        assert_eq!(bridge, before);
        assert!(actions.is_empty());
    }

    #[test]
    fn partial_key_release_is_critical() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04, 0x05]))),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x05]))),
                &mut actions,
            )
            .unwrap();

        assert!(matches!(
            actions.as_slice(),
            [BridgeAction::BleNotify {
                reason: NotifyReason::InputRelease,
                ..
            }]
        ));
    }

    #[test]
    fn modifier_only_release_is_critical_but_press_is_realtime() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame_with_modifiers(
                    &[],
                    ModifierState::LEFT_SHIFT,
                ))),
                &mut actions,
            )
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [BridgeAction::BleNotify {
                reason: NotifyReason::Input,
                ..
            }]
        ));
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[]))),
                &mut actions,
            )
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [BridgeAction::BleNotify {
                reason: NotifyReason::InputRelease,
                ..
            }]
        ));
    }

    #[test]
    fn ctrl_release_while_a_is_held_is_critical() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame_with_modifiers(
                    &[0x04],
                    ModifierState::LEFT_CTRL,
                ))),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04]))),
                &mut actions,
            )
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [BridgeAction::BleNotify {
                reason: NotifyReason::InputRelease,
                ..
            }]
        ));
    }

    #[test]
    fn target_switch_releases_old_host_and_suppresses_held_keys_for_new_host() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04]))),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        make_ready(&mut bridge, HOST_B);
        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_B }, &mut actions)
            .unwrap();

        assert_eq!(
            actions.as_slice()[0],
            BridgeAction::BleNotify {
                host_id: HOST_A,
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::TargetSwitchRelease,
            }
        );

        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04]))),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::BleNotify {
                host_id: HOST_B,
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }]
        );

        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[]))),
                &mut actions,
            )
            .unwrap();
        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[0x04]))),
                &mut actions,
            )
            .unwrap();

        assert_eq!(actions.len(), 1);
        match actions[0] {
            BridgeAction::BleNotify {
                host_id,
                report: BleHidReport::Keyboard(report),
                reason,
            } => {
                assert_eq!(host_id, HOST_B);
                assert_eq!(reason, NotifyReason::Input);
                assert_eq!(report.as_bytes(), &[0, 0, 0x04, 0, 0, 0, 0, 0]);
            }
            _ => panic!("unexpected action"),
        }
    }

    #[test]
    fn target_switch_releases_mouse_and_suppresses_held_buttons_for_new_host() {
        let mut bridge = ready_bridge_with(&[ReportKind::Mouse]);
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(mouse_frame(
                    mouse_buttons(&[MouseButton::Left]),
                    MouseMovement::neutral(),
                ))),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        make_ready_with(&mut bridge, HOST_B, &[ReportKind::Mouse]);
        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_B }, &mut actions)
            .unwrap();

        assert_eq!(
            actions.as_slice()[0],
            BridgeAction::BleNotify {
                host_id: HOST_A,
                report: BleHidReport::Mouse(BleMouseReport::release_buttons()),
                reason: NotifyReason::TargetSwitchRelease,
            }
        );

        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(mouse_frame(
                    mouse_buttons(&[MouseButton::Left]),
                    MouseMovement {
                        x: 4,
                        y: -2,
                        wheel: 0,
                        pan: 0,
                    },
                ))),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::BleNotify {
                host_id: HOST_B,
                report: BleHidReport::Mouse(BleMouseReport::from_frame(
                    MouseButtons::empty(),
                    MouseMovement {
                        x: 4,
                        y: -2,
                        wheel: 0,
                        pan: 0,
                    }
                )),
                reason: NotifyReason::Input,
            }]
        );

        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(mouse_frame(
                    MouseButtons::empty(),
                    MouseMovement::neutral(),
                ))),
                &mut actions,
            )
            .unwrap();
        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(mouse_frame(
                    mouse_buttons(&[MouseButton::Left]),
                    MouseMovement::neutral(),
                ))),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::BleNotify {
                host_id: HOST_B,
                report: BleHidReport::Mouse(BleMouseReport::from_frame(
                    mouse_buttons(&[MouseButton::Left]),
                    MouseMovement::neutral()
                )),
                reason: NotifyReason::InputEdge,
            }]
        );
    }

    #[test]
    fn target_switch_releases_consumer_and_suppresses_held_usage_for_new_host() {
        let mut bridge = ready_bridge_with(&[ReportKind::Consumer]);
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(consumer_frame(Some(0x00e9)))),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        make_ready_with(&mut bridge, HOST_B, &[ReportKind::Consumer]);
        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_B }, &mut actions)
            .unwrap();

        assert_eq!(
            actions.as_slice()[0],
            BridgeAction::BleNotify {
                host_id: HOST_A,
                report: BleHidReport::Consumer(BleConsumerReport::release()),
                reason: NotifyReason::TargetSwitchRelease,
            }
        );

        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(consumer_frame(Some(0x00e9)))),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::BleNotify {
                host_id: HOST_B,
                report: BleHidReport::Consumer(BleConsumerReport::release()),
                reason: NotifyReason::Input,
            }]
        );

        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(consumer_frame(None))),
                &mut actions,
            )
            .unwrap();
        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(consumer_frame(Some(0x00e9)))),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::BleNotify {
                host_id: HOST_B,
                report: BleHidReport::Consumer(BleConsumerReport::from_usage(ConsumerUsage(
                    0x00e9
                ))),
                reason: NotifyReason::Input,
            }]
        );
    }

    #[test]
    fn usb_removal_emits_all_ready_release_reports_and_clears_state() {
        let mut bridge = ready_bridge_with(&[
            ReportKind::Keyboard,
            ReportKind::Mouse,
            ReportKind::Consumer,
        ]);
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(StandardInputFrame {
                    device_id: DEVICE,
                    interface_id: InterfaceId(1),
                    keyboard: Some({
                        let mut frame = KeyboardFrame::new(ModifierState::empty());
                        frame.push_key(KeyUsage(0x04)).unwrap();
                        frame
                    }),
                    mouse: Some(MouseFrame {
                        buttons: mouse_buttons(&[MouseButton::Left]),
                        movement: MouseMovement::neutral(),
                    }),
                    consumer: Some(ConsumerFrame {
                        active: Some(ConsumerUsage(0x00e9)),
                    }),
                })),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::UsbDeviceRemoved {
                    interface_id: InterfaceId(1),
                },
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            &actions.as_slice()[..3],
            &[
                BridgeAction::BleNotify {
                    host_id: HOST_A,
                    report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                    reason: NotifyReason::UsbDeviceRemovedRelease,
                },
                BridgeAction::BleNotify {
                    host_id: HOST_A,
                    report: BleHidReport::Mouse(BleMouseReport::release_buttons()),
                    reason: NotifyReason::UsbDeviceRemovedRelease,
                },
                BridgeAction::BleNotify {
                    host_id: HOST_A,
                    report: BleHidReport::Consumer(BleConsumerReport::release()),
                    reason: NotifyReason::UsbDeviceRemovedRelease,
                },
            ]
        );
        assert!(bridge.state().input.keyboard.keys().is_empty());
        assert_eq!(bridge.state().input.mouse.buttons, MouseButtons::empty());
        assert_eq!(bridge.state().input.consumer.active, None);
    }

    #[test]
    fn usb_removal_recomputes_remaining_keyboard_state_instead_of_releasing_everything() {
        let mut bridge = ready_bridge_with(&[ReportKind::Keyboard]);
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DeviceId(1),
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(2),
                    device_id: DeviceId(2),
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(StandardInputFrame {
                    device_id: DeviceId(1),
                    interface_id: InterfaceId(1),
                    keyboard: Some({
                        let mut frame = KeyboardFrame::new(ModifierState::empty());
                        frame.push_key(KeyUsage(0x04)).unwrap();
                        frame
                    }),
                    mouse: None,
                    consumer: None,
                })),
                &mut actions,
            )
            .unwrap();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(StandardInputFrame {
                    device_id: DeviceId(2),
                    interface_id: InterfaceId(2),
                    keyboard: Some({
                        let mut frame = KeyboardFrame::new(ModifierState::LEFT_SHIFT);
                        frame.push_key(KeyUsage(0x05)).unwrap();
                        frame
                    }),
                    mouse: None,
                    consumer: None,
                })),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::UsbDeviceRemoved {
                    interface_id: InterfaceId(1),
                },
                &mut actions,
            )
            .unwrap();

        assert!(actions.iter().any(|action| matches!(
            action,
            BridgeAction::BleNotify {
                host_id: HOST_A,
                report: BleHidReport::Keyboard(report),
                reason: NotifyReason::UsbDeviceRemovedRelease,
            } if report.as_bytes() == &[0x02, 0, 0x05, 0, 0, 0, 0, 0]
        )));
    }

    #[test]
    fn active_host_keyboard_led_write_is_applied_to_usb_hid_interface() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::HostKeyboardLedChanged {
                    host_id: HOST_A,
                    leds: KeyboardLedState::CAPS_LOCK,
                },
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::UsbSetKeyboardLeds {
                interface_id: InterfaceId(1),
                device_id: DEVICE,
                leds: KeyboardLedState::CAPS_LOCK,
            }]
        );
    }

    #[test]
    fn active_host_keyboard_led_write_is_fanned_out_to_all_connected_keyboards() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(2),
                    device_id: DeviceId(2),
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::HostKeyboardLedChanged {
                    host_id: HOST_A,
                    leds: KeyboardLedState::CAPS_LOCK,
                },
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[
                BridgeAction::UsbSetKeyboardLeds {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    leds: KeyboardLedState::CAPS_LOCK,
                },
                BridgeAction::UsbSetKeyboardLeds {
                    interface_id: InterfaceId(2),
                    device_id: DeviceId(2),
                    leds: KeyboardLedState::CAPS_LOCK,
                },
            ]
        );
    }

    #[test]
    fn inactive_host_keyboard_led_write_is_stored_but_not_applied_until_target_switch() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        make_ready(&mut bridge, HOST_B);
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::HostKeyboardLedChanged {
                    host_id: HOST_B,
                    leds: KeyboardLedState::NUM_LOCK | KeyboardLedState::CAPS_LOCK,
                },
                &mut actions,
            )
            .unwrap();

        assert!(actions.is_empty());

        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_B }, &mut actions)
            .unwrap();

        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::UsbSetKeyboardLeds {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    leds: KeyboardLedState::NUM_LOCK | KeyboardLedState::CAPS_LOCK,
                })
        );
    }

    #[test]
    fn target_switch_applies_all_off_when_host_has_no_stored_led_state() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        make_ready(&mut bridge, HOST_B);
        actions.clear();

        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_B }, &mut actions)
            .unwrap();

        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::UsbSetKeyboardLeds {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    leds: KeyboardLedState::empty(),
                })
        );
    }

    #[test]
    fn removing_one_keyboard_keeps_led_fanout_for_remaining_keyboards() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(2),
                    device_id: DeviceId(2),
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::UsbDeviceRemoved {
                    interface_id: InterfaceId(2),
                },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::HostKeyboardLedChanged {
                    host_id: HOST_A,
                    leds: KeyboardLedState::NUM_LOCK,
                },
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::UsbSetKeyboardLeds {
                interface_id: InterfaceId(1),
                device_id: DEVICE,
                leds: KeyboardLedState::NUM_LOCK,
            }]
        );
    }

    #[test]
    fn ble_keyboard_led_output_write_drives_usb_led_action() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DEVICE,
                    keyboard_led_sink: true,
                },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        let event = keyboard_led_event_from_ble_output(HOST_A, &[0b0000_0010]).unwrap();
        bridge.handle_event(event, &mut actions).unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::UsbSetKeyboardLeds {
                interface_id: InterfaceId(1),
                device_id: DEVICE,
                leds: KeyboardLedState::CAPS_LOCK,
            }]
        );
    }

    #[test]
    fn ble_keyboard_led_output_write_rejects_invalid_payload_length() {
        assert_eq!(
            keyboard_led_event_from_ble_output(HOST_A, &[0, 0]),
            Err(BleKeyboardOutputError::InvalidLength)
        );
    }

    #[test]
    fn bond_change_requests_profile_persistence() {
        let mut bridge = Bridge::<2>::new();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(
                BridgeEvent::HostSecurityChanged {
                    host_id: HOST_A,
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut actions,
            )
            .unwrap();

        assert!(actions.as_slice().contains(&BridgeAction::PersistProfiles));
    }

    #[test]
    fn successful_pairing_closes_pairing_mode() {
        let mut bridge = Bridge::<2>::new();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::EnterPairingMode { host_id: HOST_A },
                &mut actions,
            )
            .unwrap();

        bridge
            .handle_event(
                BridgeEvent::HostSecurityChanged {
                    host_id: HOST_A,
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut actions,
            )
            .unwrap();

        assert_eq!(bridge.state().pairable_host, None);
        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::RejectPairing { host_id: HOST_A })
        );
    }

    #[test]
    fn cccd_change_requests_profile_persistence() {
        let mut bridge = Bridge::<2>::new();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(
                BridgeEvent::CccdChanged {
                    host_id: HOST_A,
                    report: ReportKind::Mouse,
                    enabled: true,
                },
                &mut actions,
            )
            .unwrap();

        assert!(actions.as_slice().contains(&BridgeAction::PersistProfiles));
    }

    #[test]
    fn target_switch_requests_last_active_persistence() {
        let mut bridge = Bridge::<2>::new();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_A }, &mut actions)
            .unwrap();

        assert!(actions.as_slice().contains(&BridgeAction::PersistProfiles));
    }

    #[test]
    fn entering_pairing_mode_enables_pairing_for_active_host() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(
                BridgeEvent::EnterPairingMode { host_id: HOST_A },
                &mut actions,
            )
            .unwrap();

        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::AllowPairing { host_id: HOST_A })
        );
        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::StatusChanged(BridgeStatus {
                    active_target: Some(HOST_A),
                    pairable_host: Some(HOST_A),
                }))
        );
    }

    #[test]
    fn entering_new_pairing_mode_rejects_previous_host() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(
                BridgeEvent::EnterPairingMode { host_id: HOST_A },
                &mut actions,
            )
            .unwrap();
        actions.clear();
        bridge
            .handle_event(
                BridgeEvent::EnterPairingMode { host_id: HOST_B },
                &mut actions,
            )
            .unwrap();

        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::RejectPairing { host_id: HOST_A })
        );
        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::AllowPairing { host_id: HOST_B })
        );
    }

    #[test]
    fn clearing_host_requests_bond_clear_and_persistence() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(
                BridgeEvent::EnterPairingMode { host_id: HOST_A },
                &mut actions,
            )
            .unwrap();
        actions.clear();

        bridge
            .handle_event(BridgeEvent::ClearHost { host_id: HOST_A }, &mut actions)
            .unwrap();

        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::RejectPairing { host_id: HOST_A })
        );
        assert!(actions.as_slice().contains(&BridgeAction::ClearBond {
            host_id: HOST_A,
            bond: None,
        }));
        assert!(actions.as_slice().contains(&BridgeAction::PersistProfiles));
        assert!(
            actions
                .as_slice()
                .contains(&BridgeAction::StatusChanged(BridgeStatus {
                    active_target: None,
                    pairable_host: None,
                }))
        );
    }

    #[test]
    fn storage_state_snapshot_contains_bond_cccd_and_last_active_host() {
        let bridge = ready_bridge_with(&[
            ReportKind::Keyboard,
            ReportKind::Mouse,
            ReportKind::Consumer,
            ReportKind::KeyboardOutput,
        ]);

        let snapshot = bridge.storage_state(10).unwrap();

        assert_eq!(snapshot.generation, 10);
        assert_eq!(snapshot.last_active_host, Some(HOST_A));
        assert_eq!(
            snapshot.hosts(),
            &[StoredHostProfile {
                host_id: HOST_A,
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: true,
                consumer_cccd_enabled: true,
                keyboard_output_cccd_enabled: true,
                name: crate::storage::FixedName::empty(),
                bond: None,
            }]
        );
    }

    #[test]
    fn restore_storage_state_recovers_bond_cccd_and_last_active_without_connecting_hosts() {
        let mut stored = StorageState::new(11);
        stored.last_active_host = Some(HOST_B);
        stored
            .push_host(StoredHostProfile {
                host_id: HOST_B,
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: false,
                consumer_cccd_enabled: true,
                keyboard_output_cccd_enabled: true,
                name: crate::storage::FixedName::from_ascii("desktop").unwrap(),
                bond: None,
            })
            .unwrap();
        let mut bridge = Bridge::<2>::new();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge.restore_storage_state(&stored, &mut actions).unwrap();

        assert_eq!(bridge.state().hosts.active_target(), Some(HOST_B));
        assert!(!bridge.can_send(HOST_B, ReportKind::Keyboard));
        assert!(actions.is_empty());

        let snapshot = bridge.storage_state(12).unwrap();
        assert_eq!(snapshot.last_active_host, Some(HOST_B));
        assert_eq!(
            snapshot.hosts(),
            &[StoredHostProfile {
                host_id: HOST_B,
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: false,
                consumer_cccd_enabled: true,
                keyboard_output_cccd_enabled: true,
                name: crate::storage::FixedName::from_ascii("desktop").unwrap(),
                bond: None,
            }]
        );
    }

    #[test]
    fn restore_storage_state_errors_when_profile_count_exceeds_bridge_capacity() {
        let mut stored = StorageState::new(11);
        let mut first = StoredHostProfile::empty();
        first.host_id = HOST_A;
        stored.push_host(first).unwrap();
        let mut second = StoredHostProfile::empty();
        second.host_id = HOST_B;
        stored.push_host(second).unwrap();
        let mut bridge = Bridge::<1>::new();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        assert_eq!(
            bridge.restore_storage_state(&stored, &mut actions),
            Err(BridgeError::HostState(HostStateError::HostCapacity))
        );
    }

    #[test]
    fn no_ready_target_keeps_button_and_consumer_state_but_drops_mouse_movement() {
        let mut bridge = Bridge::<2>::new();
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();

        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_A }, &mut actions)
            .unwrap();
        actions.clear();

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(StandardInputFrame {
                    device_id: DEVICE,
                    interface_id: InterfaceId(1),
                    keyboard: None,
                    mouse: Some(MouseFrame {
                        buttons: mouse_buttons(&[MouseButton::Right]),
                        movement: MouseMovement {
                            x: 12,
                            y: 0,
                            wheel: 1,
                            pan: 0,
                        },
                    }),
                    consumer: Some(ConsumerFrame {
                        active: Some(ConsumerUsage(0x00ea)),
                    }),
                })),
                &mut actions,
            )
            .unwrap();

        assert!(actions.is_empty());
        assert_eq!(
            bridge.state().input.mouse.buttons,
            mouse_buttons(&[MouseButton::Right])
        );
        assert_eq!(
            bridge.state().input.consumer.active,
            Some(ConsumerUsage(0x00ea))
        );
        assert_eq!(bridge.state().stats.mouse_movements_dropped, 1);
    }

    #[test]
    fn seventh_key_stays_suppressed_until_its_physical_release() {
        let mut bridge = ready_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[
                    0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
                ]))),
                &mut actions,
            )
            .unwrap();
        let first = match actions[0] {
            BridgeAction::BleNotify {
                report: BleHidReport::Keyboard(report),
                ..
            } => report,
            _ => panic!("expected keyboard notification"),
        };
        assert_eq!(first.as_bytes(), &[0, 0, 4, 5, 6, 7, 8, 9]);

        bridge
            .handle_event(
                BridgeEvent::InputFrame(InputFrame::Standard(keyboard_frame(&[
                    0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
                ]))),
                &mut actions,
            )
            .unwrap();
        let after_release = match actions[0] {
            BridgeAction::BleNotify {
                report: BleHidReport::Keyboard(report),
                ..
            } => report,
            _ => panic!("expected keyboard notification"),
        };
        assert_eq!(after_release.as_bytes(), &[0, 0, 5, 6, 7, 8, 9, 0]);
        assert!(
            bridge
                .state()
                .suppression
                .keyboard
                .contains_key(KeyUsage(0x0a))
        );
    }

    fn ready_bridge() -> Bridge<2> {
        ready_bridge_with(&[ReportKind::Keyboard])
    }

    fn ready_bridge_with(kinds: &[ReportKind]) -> Bridge<2> {
        let mut bridge = Bridge::<2>::new();
        make_ready_with(&mut bridge, HOST_A, kinds);
        let mut actions = heapless::Vec::<BridgeAction, 8>::new();
        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST_A }, &mut actions)
            .unwrap();
        bridge
    }

    fn make_ready<const HOSTS: usize>(bridge: &mut Bridge<HOSTS>, host_id: HostId) {
        make_ready_with(bridge, host_id, &[ReportKind::Keyboard]);
    }

    fn make_ready_with<const HOSTS: usize>(
        bridge: &mut Bridge<HOSTS>,
        host_id: HostId,
        kinds: &[ReportKind],
    ) {
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();
        bridge
            .handle_event(BridgeEvent::HostConnected { host_id }, &mut actions)
            .unwrap();
        bridge
            .handle_event(
                BridgeEvent::HostSecurityChanged {
                    host_id,
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut actions,
            )
            .unwrap();
        for report in kinds {
            bridge
                .handle_event(
                    BridgeEvent::CccdChanged {
                        host_id,
                        report: *report,
                        enabled: true,
                    },
                    &mut actions,
                )
                .unwrap();
        }
    }

    fn keyboard_frame(keys: &[u8]) -> StandardInputFrame {
        keyboard_frame_with_modifiers(keys, ModifierState::empty())
    }

    fn keyboard_frame_with_modifiers(keys: &[u8], modifiers: ModifierState) -> StandardInputFrame {
        let mut keyboard = KeyboardFrame::new(modifiers);
        for key in keys {
            keyboard.push_key(KeyUsage(*key)).unwrap();
        }
        StandardInputFrame {
            device_id: DEVICE,
            interface_id: InterfaceId(1),
            keyboard: Some(keyboard),
            mouse: None,
            consumer: None,
        }
    }

    fn mouse_frame(buttons: MouseButtons, movement: MouseMovement) -> StandardInputFrame {
        StandardInputFrame {
            device_id: DEVICE,
            interface_id: InterfaceId(1),
            keyboard: None,
            mouse: Some(MouseFrame { buttons, movement }),
            consumer: None,
        }
    }

    fn consumer_frame(active: Option<u16>) -> StandardInputFrame {
        StandardInputFrame {
            device_id: DEVICE,
            interface_id: InterfaceId(1),
            keyboard: None,
            mouse: None,
            consumer: Some(ConsumerFrame {
                active: active.map(ConsumerUsage),
            }),
        }
    }

    fn mouse_buttons(buttons: &[MouseButton]) -> MouseButtons {
        let mut state = MouseButtons::empty();
        for button in buttons {
            state.set(*button, true);
        }
        state
    }
}
