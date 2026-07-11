use crate::ble::{BleHostAdapterError, BleHostAdapterEvent, bridge_events_from_ble_host_event};
use crate::bridge::{Bridge, BridgeAction, BridgeError, BridgeEvent, BridgeStatus, NotifyReason};
use crate::ids::{DeviceId, HostId, InterfaceId};
use crate::management::{
    ManagementCommand, ManagementDestination, ManagementDiagnostics, ManagementHistoryEvent,
    ManagementHostInfo, ManagementHostName, ManagementHostStatus, ManagementHostTiming,
    ManagementRequest, ManagementResponse, ManagementResponsePayload, ManagementResult,
    ManagementSchema, ManagementSetting, ManagementStatus, ManagementUsbDevice,
    ManagementUsbStatus,
};
use crate::reports::{BleConsumerReport, BleHidReport, BleKeyboard6KroReport, BleMouseReport};
use crate::settings::{
    GlobalSettings, HostSettings, SETTING_COUNT, SETTINGS_SCHEMA_HASH, SETTINGS_SCHEMA_VERSION,
    SettingId, SettingScope, SettingTarget, setting_descriptor, validate_setting_value,
};
use crate::storage::{
    FixedName, STORED_HOSTS_MAX, StorageError, StoragePersistPriority, StorageState, StoredBond,
};
use crate::target_control::ButtonIntent;
use crate::usb_hid::output::{
    KeyboardLedOutputBytes, KeyboardLedOutputError, KeyboardLedOutputReport,
};
use core::sync::atomic::{AtomicBool, Ordering};

#[path = "runtime/bootstrap.rs"]
pub mod bootstrap;
#[path = "runtime/driver.rs"]
pub mod driver;
#[path = "runtime/message.rs"]
pub mod message;
#[path = "runtime/owner.rs"]
pub mod owner;

pub const RUNTIME_HOSTS_MAX: usize = STORED_HOSTS_MAX;
pub const RUNTIME_USB_INTERFACES_MAX: usize = 8;
pub const RUNTIME_HISTORY_CAPACITY: usize = 16;
pub const RELEASE_REPORTS_MAX: usize = 3;
pub const USB_LED_ACTIONS_MAX: usize = RUNTIME_USB_INTERFACES_MAX;
pub const TARGET_CONTROL_ACTIONS_MAX: usize = 4;
pub const RUNTIME_BRIDGE_ACTION_CAPACITY: usize =
    RELEASE_REPORTS_MAX + USB_LED_ACTIONS_MAX + TARGET_CONTROL_ACTIONS_MAX + 1;
pub const RUNTIME_COMMAND_CAPACITY: usize = RUNTIME_BRIDGE_ACTION_CAPACITY;
pub const RUNTIME_BLE_EVENT_CAPACITY: usize = 2;
pub const RUNTIME_INPUT_QUEUE_CAPACITY: usize = 16;
pub const RUNTIME_BLE_GATT_WRITE_MAX_LEN: usize = 2;
pub const RUNTIME_BLE_COMMAND_QUEUE_CAPACITY: usize = RUNTIME_COMMAND_CAPACITY;
pub const RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY: usize = RUNTIME_COMMAND_CAPACITY;
pub const RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY: usize = RUNTIME_COMMAND_CAPACITY;
pub const RUNTIME_USB_COMMAND_QUEUE_CAPACITY: usize = RUNTIME_COMMAND_CAPACITY;
pub const RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY: usize = RUNTIME_COMMAND_CAPACITY;
pub const RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY: usize = RUNTIME_COMMAND_CAPACITY;

pub type DefaultBridgeRuntime = BridgeRuntime<RUNTIME_HOSTS_MAX, RUNTIME_USB_INTERFACES_MAX>;
pub type RuntimeCommandVec = heapless::Vec<RuntimeCommand, RUNTIME_COMMAND_CAPACITY>;
pub type BleTaskCommandVec = heapless::Vec<BleTaskCommand, RUNTIME_BLE_COMMAND_QUEUE_CAPACITY>;
pub type UsbTaskCommandVec = heapless::Vec<UsbTaskCommand, RUNTIME_USB_COMMAND_QUEUE_CAPACITY>;
pub type StorageTaskCommandVec =
    heapless::Vec<StorageTaskCommand, RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY>;
pub type StatusTaskCommandVec =
    heapless::Vec<StatusTaskCommand, RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCapacities {
    pub hosts: usize,
    pub usb_interfaces: usize,
    pub bridge_actions: usize,
    pub commands: usize,
    pub ble_events: usize,
    pub input_queue: usize,
    pub ble_command_queue: usize,
    pub usb_command_queue: usize,
    pub storage_command_queue: usize,
    pub status_command_queue: usize,
}

pub const DEFAULT_RUNTIME_CAPACITIES: RuntimeCapacities = RuntimeCapacities {
    hosts: RUNTIME_HOSTS_MAX,
    usb_interfaces: RUNTIME_USB_INTERFACES_MAX,
    bridge_actions: RUNTIME_BRIDGE_ACTION_CAPACITY,
    commands: RUNTIME_COMMAND_CAPACITY,
    ble_events: RUNTIME_BLE_EVENT_CAPACITY,
    input_queue: RUNTIME_INPUT_QUEUE_CAPACITY,
    ble_command_queue: RUNTIME_BLE_COMMAND_QUEUE_CAPACITY,
    usb_command_queue: RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    storage_command_queue: RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
    status_command_queue: RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
};

/// Coalesces periodic ticks without making their delivery depend on the input
/// queue becoming completely empty.
#[derive(Debug)]
pub struct RuntimeTickPending {
    pending: AtomicBool,
}

impl RuntimeTickPending {
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    pub fn try_mark_pending(&self) -> bool {
        self.pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn mark_processed(&self) {
        self.pending.store(false, Ordering::Release);
    }

    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

impl Default for RuntimeTickPending {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeInput<'a> {
    BridgeEvent(BridgeEvent),
    ButtonIntent {
        intent: ButtonIntent,
        now_ms: u64,
    },
    ManagementRequest {
        destination: ManagementDestination,
        request: ManagementRequest,
        now_ms: u64,
    },
    Tick {
        now_ms: u64,
    },
    BleHostEvent {
        host_id: HostId,
        event: BleHostAdapterEvent<'a>,
    },
    UsbHidInterfaceConnected {
        interface_id: InterfaceId,
        device_id: DeviceId,
        led_output: Option<KeyboardLedOutputReport>,
    },
    UsbHidInterfaceDisconnected {
        interface_id: InterfaceId,
    },
    UsbDeviceMetadataUpdated {
        device_id: DeviceId,
        vendor_id: u16,
        product_id: u16,
        name: FixedName,
        flags: u8,
    },
    HostNameDiscovered {
        host_id: HostId,
        name: FixedName,
    },
    DiagnosticsEvent(RuntimeDiagnosticsEvent),
    RestoreStorage(&'a StorageState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeRuntime<const HOSTS: usize, const USB_INTERFACES: usize> {
    bridge: Bridge<HOSTS>,
    usb_interfaces: [Option<UsbHidInterfaceRuntimeState>; USB_INTERFACES],
    storage_generation: u32,
    pairing_mode: Option<PairingModeState>,
    global_settings: GlobalSettings,
    host_settings: [HostSettings; HOSTS],
    now_ms: u64,
    diagnostics: ManagementDiagnostics,
    history: heapless::Deque<ManagementHistoryEvent, RUNTIME_HISTORY_CAPACITY>,
    next_history_sequence: u16,
    host_last_connected_seconds: [u32; HOSTS],
    host_last_disconnected_seconds: [u32; HOSTS],
    host_last_disconnect_reason: [u8; HOSTS],
    pending_target_switch: Option<PendingTargetSwitch>,
    status_sequence: u64,
    counters: RuntimeCounters,
    mouse_scale_remainders: [MouseScaleRemainders; HOSTS],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseScaleRemainders {
    x: i32,
    y: i32,
    wheel: i32,
    pan: i32,
}

impl MouseScaleRemainders {
    const ZERO: Self = Self {
        x: 0,
        y: 0,
        wheel: 0,
        pan: 0,
    };
}

impl<const HOSTS: usize, const USB_INTERFACES: usize> BridgeRuntime<HOSTS, USB_INTERFACES> {
    pub const fn new(storage_generation: u32) -> Self {
        Self {
            bridge: Bridge::new(),
            usb_interfaces: [None; USB_INTERFACES],
            storage_generation,
            pairing_mode: None,
            global_settings: GlobalSettings::DEFAULT,
            host_settings: [HostSettings::DEFAULT; HOSTS],
            now_ms: 0,
            diagnostics: ManagementDiagnostics {
                uptime_seconds: 0,
                reset_reason: 0,
                brownout_count: 0,
                ble_disconnect_count: 0,
                ble_notify_failure_count: 0,
                usb_error_count: 0,
                flash_write_count: 0,
                flash_failure_count: 0,
            },
            history: heapless::Deque::new(),
            next_history_sequence: 0,
            host_last_connected_seconds: [0; HOSTS],
            host_last_disconnected_seconds: [0; HOSTS],
            host_last_disconnect_reason: [0; HOSTS],
            pending_target_switch: None,
            status_sequence: 0,
            counters: RuntimeCounters::new(),
            mouse_scale_remainders: [MouseScaleRemainders::ZERO; HOSTS],
        }
    }

    pub const fn bridge(&self) -> &Bridge<HOSTS> {
        &self.bridge
    }

    pub const fn storage_generation(&self) -> u32 {
        self.storage_generation
    }

    pub const fn pairing_mode(&self) -> Option<PairingModeState> {
        self.pairing_mode
    }

    pub fn mark_host_disconnected_for_quiesce(&mut self, host_id: HostId) {
        self.bridge.mark_host_disconnected_for_quiesce(host_id);
    }

    pub fn prepare_for_quiesce(&mut self) -> Result<(), RuntimeError> {
        self.bridge.prepare_for_quiesce()?;
        Ok(())
    }

    pub const fn counters(&self) -> RuntimeCounters {
        self.counters
    }

    pub fn observe_outbox_usage(&mut self, ble: usize, usb: usize, storage: usize, status: usize) {
        self.counters.ble_control_queue_high_watermark = self
            .counters
            .ble_control_queue_high_watermark
            .max(ble.min(u16::MAX as usize) as u16);
        self.counters.ble_notify_queue_high_watermark = self
            .counters
            .ble_notify_queue_high_watermark
            .max(ble.min(u16::MAX as usize) as u16);
        self.counters.usb_command_queue_high_watermark = self
            .counters
            .usb_command_queue_high_watermark
            .max(usb.min(u16::MAX as usize) as u16);
        self.counters.storage_queue_high_watermark = self
            .counters
            .storage_queue_high_watermark
            .max(storage.min(u16::MAX as usize) as u16);
        self.counters.status_queue_high_watermark = self
            .counters
            .status_queue_high_watermark
            .max(status.min(u16::MAX as usize) as u16);
    }

    pub fn observe_transport_metrics(
        &mut self,
        runtime_input_depth: usize,
        mouse: crate::mouse_accumulator::MouseAccumulatorStats,
        status_updates_dropped: u32,
    ) {
        self.counters.runtime_input_queue_high_watermark = self
            .counters
            .runtime_input_queue_high_watermark
            .max(runtime_input_depth.min(u16::MAX as usize) as u16);
        self.counters.mouse_reports_coalesced = mouse.reports_coalesced;
        self.counters.mouse_movement_saturated = mouse.movement_saturated;
        self.counters.status_updates_dropped = status_updates_dropped;
    }

    pub fn storage_state(&self) -> Result<StorageState, StorageError> {
        let mut state = self.bridge.storage_state(self.storage_generation)?;
        state.global_settings = self.global_settings;
        for (destination, source) in state.host_settings.iter_mut().zip(self.host_settings) {
            *destination = source;
        }
        Ok(state)
    }

    pub fn restore_storage_state<const COMMANDS: usize>(
        &mut self,
        storage: &StorageState,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        let mut actions = heapless::Vec::<BridgeAction, 1>::new();
        let mut restored = storage.clone();
        restored.last_active_host = if restored.global_settings.boot_target != 0 {
            let requested = HostId(restored.global_settings.boot_target);
            restored
                .hosts()
                .iter()
                .any(|host| host.host_id == requested)
                .then_some(requested)
        } else if restored.global_settings.restore_last_target {
            restored.last_active_host
        } else {
            None
        };
        self.bridge.restore_storage_state(&restored, &mut actions)?;
        self.storage_generation = storage.generation;
        self.diagnostics.flash_write_count = storage.generation.min(u8::MAX as u32) as u8;
        self.global_settings = storage.global_settings;
        for (destination, source) in self.host_settings.iter_mut().zip(storage.host_settings) {
            *destination = source;
        }
        self.pairing_mode = None;
        push_command(
            commands,
            RuntimeCommand::ApplyEffect(RuntimeEffect::SetLogLevel(self.global_settings.log_level)),
        )?;
        Ok(())
    }

    pub fn handle_input<const COMMANDS: usize, const ACTIONS: usize, const EVENTS: usize>(
        &mut self,
        input: RuntimeInput<'_>,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        let mut next = self.clone();
        let mut next_commands = heapless::Vec::new();
        next.handle_input_in_place::<COMMANDS, ACTIONS, EVENTS>(input, &mut next_commands)?;
        *self = next;
        *commands = next_commands;
        Ok(())
    }

    pub(crate) fn handle_input_in_place<
        const COMMANDS: usize,
        const ACTIONS: usize,
        const EVENTS: usize,
    >(
        &mut self,
        input: RuntimeInput<'_>,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        match input {
            RuntimeInput::BridgeEvent(event) => {
                self.handle_bridge_event::<COMMANDS, ACTIONS>(event, commands)
            }
            RuntimeInput::ButtonIntent { intent, now_ms } => {
                self.handle_button_intent::<COMMANDS, ACTIONS>(intent, now_ms, commands)
            }
            RuntimeInput::ManagementRequest {
                destination,
                request,
                now_ms,
            } => self.handle_management_request::<COMMANDS, ACTIONS>(
                destination,
                request,
                now_ms,
                commands,
            ),
            RuntimeInput::Tick { now_ms } => {
                self.now_ms = now_ms;
                self.diagnostics.uptime_seconds =
                    now_ms.saturating_div(1000).min(u32::MAX as u64) as u32;
                self.handle_tick::<COMMANDS, ACTIONS>(now_ms, commands)
            }
            RuntimeInput::BleHostEvent { host_id, event } => {
                self.handle_ble_host_event::<COMMANDS, ACTIONS, EVENTS>(host_id, event, commands)
            }
            RuntimeInput::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                led_output,
            } => self.register_usb_hid_interface::<COMMANDS, ACTIONS>(
                interface_id,
                device_id,
                led_output,
                commands,
            ),
            RuntimeInput::UsbHidInterfaceDisconnected { interface_id } => {
                self.unregister_usb_device::<COMMANDS, ACTIONS>(interface_id, commands)
            }
            RuntimeInput::UsbDeviceMetadataUpdated {
                device_id,
                vendor_id,
                product_id,
                name,
                flags,
            } => {
                commands.clear();
                for interface in self.usb_interfaces.iter_mut().flatten() {
                    if interface.device_id == device_id {
                        interface.vendor_id = vendor_id;
                        interface.product_id = product_id;
                        interface.name = name;
                        interface.flags |= flags;
                    }
                }
                if let Some(last) = self.history.back_mut()
                    && last.kind == 3
                    && last.subject == device_id.0
                {
                    last.vendor_id = vendor_id;
                    last.product_id = product_id;
                }
                Ok(())
            }
            RuntimeInput::HostNameDiscovered { host_id, name } => {
                commands.clear();
                self.bridge
                    .set_discovered_host_name(host_id, name)
                    .map_err(BridgeError::HostState)?;
                Ok(())
            }
            RuntimeInput::DiagnosticsEvent(event) => {
                commands.clear();
                self.apply_diagnostics_event(event);
                Ok(())
            }
            RuntimeInput::RestoreStorage(storage) => {
                self.restore_storage_state::<COMMANDS>(storage, commands)
            }
        }
    }

    pub fn register_usb_hid_interface<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        interface_id: InterfaceId,
        device_id: DeviceId,
        led_output: Option<KeyboardLedOutputReport>,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        let first_interface_for_device = !self
            .usb_interfaces
            .iter()
            .flatten()
            .any(|interface| interface.device_id == device_id);
        self.upsert_usb_hid_interface(interface_id, device_id, led_output)?;
        let result = self.handle_event::<COMMANDS, ACTIONS>(
            BridgeEvent::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                keyboard_led_sink: led_output.is_some(),
            },
            commands,
        );
        if first_interface_for_device {
            self.record_history(3, device_id.0, 0, 0, 0);
        }
        result
    }

    pub fn unregister_usb_device<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        interface_id: InterfaceId,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        let removed_device = self
            .usb_hid_interface_index(interface_id)
            .and_then(|index| self.usb_interfaces[index].map(|state| state.device_id));
        if let Some(index) = self.usb_hid_interface_index(interface_id) {
            self.usb_interfaces[index] = None;
        }
        let result = self.handle_event::<COMMANDS, ACTIONS>(
            BridgeEvent::UsbDeviceRemoved { interface_id },
            commands,
        );
        if let Some(device_id) = removed_device
            && !self
                .usb_interfaces
                .iter()
                .flatten()
                .any(|interface| interface.device_id == device_id)
        {
            self.record_history(4, device_id.0, 0, 0, 0);
        }
        result
    }

    pub fn handle_event<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        event: BridgeEvent,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        self.handle_bridge_event_append::<COMMANDS, ACTIONS>(event, commands)
    }

    pub fn handle_ble_host_event<
        const COMMANDS: usize,
        const ACTIONS: usize,
        const EVENTS: usize,
    >(
        &mut self,
        host_id: HostId,
        event: BleHostAdapterEvent<'_>,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        let mut events = heapless::Vec::<BridgeEvent, EVENTS>::new();
        bridge_events_from_ble_host_event(host_id, event, &mut events)?;

        for event in events {
            self.handle_bridge_event_append::<COMMANDS, ACTIONS>(event, commands)?;
        }

        Ok(())
    }

    fn handle_button_intent<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        intent: ButtonIntent,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        let configured_action = match intent {
            ButtonIntent::NextConnectedTarget => self.global_settings.button_short_action,
            ButtonIntent::EnterPairingMode => self.global_settings.button_long_action,
            ButtonIntent::ClearActiveHostBond => self.global_settings.button_very_long_action,
        };
        let intent = match configured_action {
            0 => {
                commands.clear();
                return Ok(());
            }
            1 => ButtonIntent::NextConnectedTarget,
            2 => ButtonIntent::EnterPairingMode,
            3 => ButtonIntent::ClearActiveHostBond,
            _ => {
                commands.clear();
                return Ok(());
            }
        };
        match intent {
            ButtonIntent::NextConnectedTarget => {
                let Some(target) = self.bridge.state().hosts.next_connected_target() else {
                    commands.clear();
                    return Ok(());
                };
                self.request_target_switch::<COMMANDS, ACTIONS>(target, now_ms, commands)
            }
            ButtonIntent::EnterPairingMode => {
                let Some(host_id) = self.bridge.state().hosts.pairing_candidate() else {
                    commands.clear();
                    return Ok(());
                };
                self.pairing_mode = Some(PairingModeState {
                    host_id,
                    deadline_ms: now_ms.saturating_add(PAIRING_MODE_TIMEOUT_MS),
                });
                self.handle_bridge_event::<COMMANDS, ACTIONS>(
                    BridgeEvent::EnterPairingMode { host_id },
                    commands,
                )
            }
            ButtonIntent::ClearActiveHostBond => {
                let Some(host_id) = self.bridge.state().hosts.active_target() else {
                    commands.clear();
                    return Ok(());
                };
                if self.pairing_mode.map(|state| state.host_id) == Some(host_id) {
                    self.pairing_mode = None;
                }
                self.handle_bridge_event::<COMMANDS, ACTIONS>(
                    BridgeEvent::ClearHost { host_id },
                    commands,
                )
            }
        }
    }

    fn handle_management_request<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        destination: ManagementDestination,
        request: ManagementRequest,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        let result = match request.command {
            ManagementCommand::GetStatus => ManagementResult::Ok,
            ManagementCommand::SelectHost(host_id) => {
                if !valid_management_host::<HOSTS>(host_id) {
                    ManagementResult::InvalidHost
                } else if self.bridge.state().hosts.host(host_id).is_none() {
                    ManagementResult::HostNotFound
                } else {
                    self.request_target_switch_append::<COMMANDS, ACTIONS>(
                        host_id, now_ms, commands,
                    )?;
                    ManagementResult::Ok
                }
            }
            ManagementCommand::StartPairing(host_id) => {
                if !valid_management_host::<HOSTS>(host_id) {
                    ManagementResult::InvalidHost
                } else if self
                    .bridge
                    .state()
                    .hosts
                    .host(host_id)
                    .is_some_and(|host| host.bonded || host.bond.is_some())
                {
                    ManagementResult::HostAlreadyBonded
                } else {
                    self.pairing_mode = Some(PairingModeState {
                        host_id,
                        deadline_ms: now_ms.saturating_add(PAIRING_MODE_TIMEOUT_MS),
                    });
                    self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                        BridgeEvent::EnterPairingMode { host_id },
                        commands,
                    )?;
                    ManagementResult::Ok
                }
            }
            ManagementCommand::ForgetHost(host_id) => {
                if !valid_management_host::<HOSTS>(host_id) {
                    ManagementResult::InvalidHost
                } else if self.bridge.state().hosts.host(host_id).is_none() {
                    ManagementResult::HostNotFound
                } else {
                    self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                        BridgeEvent::ClearHost { host_id },
                        commands,
                    )?;
                    ManagementResult::Ok
                }
            }
            ManagementCommand::GetHostInfo(host_id) => {
                if !valid_management_host::<HOSTS>(host_id) {
                    ManagementResult::InvalidHost
                } else if self.bridge.state().hosts.host(host_id).is_none() {
                    ManagementResult::HostNotFound
                } else {
                    ManagementResult::Ok
                }
            }
            ManagementCommand::SetHostName { host_id, name } => {
                if !valid_management_host::<HOSTS>(host_id) {
                    ManagementResult::InvalidHost
                } else if self.bridge.state().hosts.host(host_id).is_none() {
                    ManagementResult::HostNotFound
                } else {
                    let name = core::str::from_utf8(name.as_bytes())
                        .ok()
                        .and_then(FixedName::from_ascii);
                    if let Some(name) = name {
                        self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                            BridgeEvent::SetHostName { host_id, name },
                            commands,
                        )?;
                        ManagementResult::Ok
                    } else {
                        ManagementResult::InvalidName
                    }
                }
            }
            ManagementCommand::CancelPairing => {
                if let Some(pairing) = self.pairing_mode.take() {
                    self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                        BridgeEvent::PairingModeExpired {
                            host_id: pairing.host_id,
                        },
                        commands,
                    )?;
                    ManagementResult::Ok
                } else {
                    ManagementResult::NotFound
                }
            }
            ManagementCommand::GetUsbDevice { index, .. } => {
                if self.management_usb_device(index, 0).is_some() {
                    ManagementResult::Ok
                } else {
                    ManagementResult::NotFound
                }
            }
            ManagementCommand::GetDiagnostics
            | ManagementCommand::GetHistory { .. }
            | ManagementCommand::GetSchema => ManagementResult::Ok,
            ManagementCommand::GetHostTiming(host_id) => {
                if valid_management_host::<HOSTS>(host_id) {
                    ManagementResult::Ok
                } else {
                    ManagementResult::InvalidHost
                }
            }
            ManagementCommand::GetSetting { id, target } => {
                if self.setting_value(id, target).is_some() {
                    ManagementResult::Ok
                } else {
                    ManagementResult::InvalidSetting
                }
            }
            ManagementCommand::SetSetting { id, target, value } => {
                let changed = self.setting_value(id, target) != Some(value);
                if self.set_setting(id, target, value) {
                    self.push_storage_snapshot(commands, StoragePersistPriority::Critical)?;
                    if changed && id == SettingId::LogLevel {
                        push_command(
                            commands,
                            RuntimeCommand::ApplyEffect(RuntimeEffect::SetLogLevel(value as u8)),
                        )?;
                    }
                    ManagementResult::Ok
                } else {
                    ManagementResult::InvalidSetting
                }
            }
        };

        let payload = match request.command {
            ManagementCommand::GetHostInfo(host_id)
            | ManagementCommand::SetHostName { host_id, .. }
                if result == ManagementResult::Ok =>
            {
                self.management_host_info(host_id)
                    .map(ManagementResponsePayload::HostInfo)
                    .unwrap_or(ManagementResponsePayload::None)
            }
            ManagementCommand::GetUsbDevice { index, name_offset }
                if result == ManagementResult::Ok =>
            {
                self.management_usb_device(index, name_offset)
                    .map(ManagementResponsePayload::UsbDevice)
                    .unwrap_or(ManagementResponsePayload::None)
            }
            ManagementCommand::GetDiagnostics => {
                ManagementResponsePayload::Diagnostics(self.diagnostics)
            }
            ManagementCommand::GetHostTiming(host_id) if result == ManagementResult::Ok => {
                let index = host_id.0.saturating_sub(1) as usize;
                ManagementResponsePayload::HostTiming(ManagementHostTiming {
                    host_id,
                    last_connected_seconds: self.host_last_connected_seconds[index],
                    last_disconnected_seconds: self.host_last_disconnected_seconds[index],
                    last_disconnect_reason: self.host_last_disconnect_reason[index],
                })
            }
            ManagementCommand::GetHistory { index } => self
                .history
                .iter()
                .rev()
                .nth(index as usize)
                .copied()
                .map(ManagementResponsePayload::History)
                .unwrap_or(ManagementResponsePayload::None),
            ManagementCommand::GetSchema => ManagementResponsePayload::Schema(ManagementSchema {
                version: SETTINGS_SCHEMA_VERSION,
                setting_count: SETTING_COUNT as u8,
                history_capacity: RUNTIME_HISTORY_CAPACITY as u8,
                usb_capacity: USB_INTERFACES.min(u8::MAX as usize) as u8,
                hash: SETTINGS_SCHEMA_HASH,
                firmware_major: 0,
                firmware_minor: 1,
                firmware_patch: 0,
            }),
            ManagementCommand::GetSetting { id, target }
            | ManagementCommand::SetSetting { id, target, .. }
                if result == ManagementResult::Ok =>
            {
                ManagementResponsePayload::Setting(ManagementSetting {
                    id,
                    target,
                    value: self.setting_value(id, target).unwrap_or_default(),
                })
            }
            _ => ManagementResponsePayload::Status(self.management_status()),
        };

        push_command(
            commands,
            RuntimeCommand::ManagementResponse {
                destination,
                response: ManagementResponse {
                    request_id: request.request_id,
                    result,
                    payload,
                },
            },
        )
    }

    pub fn management_status(&self) -> ManagementStatus {
        let mut status = ManagementStatus::empty(u8::try_from(HOSTS.min(4)).unwrap_or(4));
        status.active_host = self.bridge.state().hosts.active_target();
        status.pairing_host = self.bridge.state().pairable_host;
        status.usb = self.management_usb_status();
        for index in 0..HOSTS.min(4) {
            let host_id = HostId((index + 1) as u8);
            if let Some(host) = self.bridge.state().hosts.host(host_id) {
                status.hosts[index] = ManagementHostStatus {
                    known: true,
                    connected: host.connected,
                    encrypted: host.encrypted,
                    bonded: host.bonded || host.bond.is_some(),
                };
            }
        }
        status
    }

    fn management_host_info(&self, host_id: HostId) -> Option<ManagementHostInfo> {
        let host = self.bridge.state().hosts.host(host_id)?;
        let (selected_name, name_source) = if host.name.as_bytes().is_empty() {
            (host.discovered_name, 1)
        } else {
            (host.name, 2)
        };
        let name = core::str::from_utf8(selected_name.as_bytes())
            .ok()
            .and_then(|name| ManagementHostName::from_ascii(name).ok())
            .unwrap_or_else(ManagementHostName::empty);
        Some(ManagementHostInfo {
            host_id,
            status: ManagementHostStatus {
                known: true,
                connected: host.connected,
                encrypted: host.encrypted,
                bonded: host.bonded || host.bond.is_some(),
            },
            name,
            name_source,
        })
    }

    fn management_usb_status(&self) -> ManagementUsbStatus {
        let mut devices = [None; USB_INTERFACES];
        let mut device_count = 0usize;
        let mut interface_count = 0usize;
        let mut keyboard_devices = [None; USB_INTERFACES];
        let mut keyboard_count = 0usize;
        for interface in self.usb_interfaces.iter().flatten() {
            interface_count += 1;
            if (interface.flags & 0x02 != 0 || interface.led_output.is_some())
                && !keyboard_devices[..keyboard_count].contains(&Some(interface.device_id))
            {
                keyboard_devices[keyboard_count] = Some(interface.device_id);
                keyboard_count += 1;
            }
            if !devices[..device_count].contains(&Some(interface.device_id)) {
                devices[device_count] = Some(interface.device_id);
                device_count += 1;
            }
        }
        ManagementUsbStatus {
            device_count: device_count.min(u8::MAX as usize) as u8,
            interface_count: interface_count.min(u8::MAX as usize) as u8,
            keyboard_count: keyboard_count.min(u8::MAX as usize) as u8,
        }
    }

    fn management_usb_device(
        &self,
        requested_index: u8,
        name_offset: u8,
    ) -> Option<ManagementUsbDevice> {
        let mut seen = [None; USB_INTERFACES];
        let mut count = 0usize;
        let mut selected = None;
        for interface in self.usb_interfaces.iter().flatten() {
            if seen[..count].contains(&Some(interface.device_id)) {
                continue;
            }
            seen[count] = Some(interface.device_id);
            if count == requested_index as usize {
                selected = Some(*interface);
                break;
            }
            count += 1;
        }
        let selected = selected?;
        let name = selected.name.as_bytes();
        let offset = (name_offset as usize).min(name.len());
        let chunk_len = (name.len() - offset).min(5);
        let mut name_chunk = [0; 5];
        name_chunk[..chunk_len].copy_from_slice(&name[offset..offset + chunk_len]);
        let mut flags = selected.flags | 0x01;
        for interface in self.usb_interfaces.iter().flatten() {
            if interface.device_id == selected.device_id {
                flags |= interface.flags;
            }
        }
        Some(ManagementUsbDevice {
            index: requested_index,
            device_id: selected.device_id.0,
            flags,
            vendor_id: selected.vendor_id,
            product_id: selected.product_id,
            name_len: name.len().min(u8::MAX as usize) as u8,
            name_offset,
            name_chunk_len: chunk_len as u8,
            name_chunk,
        })
    }

    fn setting_value(&self, id: SettingId, target: SettingTarget) -> Option<i32> {
        let descriptor = setting_descriptor(id);
        if descriptor.scope
            != match target {
                SettingTarget::Global => SettingScope::Global,
                SettingTarget::Host(_) => SettingScope::Host,
            }
        {
            return None;
        }
        Some(match (id, target) {
            (SettingId::BootTarget, SettingTarget::Global) => {
                self.global_settings.boot_target as i32
            }
            (SettingId::RestoreLastTarget, SettingTarget::Global) => {
                self.global_settings.restore_last_target as i32
            }
            (SettingId::AutoReconnect, SettingTarget::Global) => {
                self.global_settings.auto_reconnect as i32
            }
            (SettingId::SwitchReleaseDelayMs, SettingTarget::Global) => {
                self.global_settings.switch_release_delay_ms as i32
            }
            (SettingId::ButtonShortAction, SettingTarget::Global) => {
                self.global_settings.button_short_action as i32
            }
            (SettingId::ButtonLongAction, SettingTarget::Global) => {
                self.global_settings.button_long_action as i32
            }
            (SettingId::ButtonVeryLongAction, SettingTarget::Global) => {
                self.global_settings.button_very_long_action as i32
            }
            (SettingId::LogLevel, SettingTarget::Global) => self.global_settings.log_level as i32,
            (id, SettingTarget::Host(host)) => {
                let settings = self.host_settings.get(host.0.checked_sub(1)? as usize)?;
                match id {
                    SettingId::KeyboardLayout => settings.keyboard_layout as i32,
                    SettingId::RemapFromUsage => settings.remap_from_usage as i32,
                    SettingId::RemapToUsage => settings.remap_to_usage as i32,
                    SettingId::MouseSensitivityPercent => settings.mouse_sensitivity_percent as i32,
                    SettingId::ScrollMultiplierPercent => settings.scroll_multiplier_percent as i32,
                    SettingId::ConsumerFromUsage => settings.consumer_from_usage as i32,
                    SettingId::ConsumerToUsage => settings.consumer_to_usage as i32,
                    _ => return None,
                }
            }
            _ => return None,
        })
    }

    fn set_setting(&mut self, id: SettingId, target: SettingTarget, value: i32) -> bool {
        if !validate_setting_value(id, value) || self.setting_value(id, target).is_none() {
            return false;
        }
        match (id, target) {
            (SettingId::BootTarget, SettingTarget::Global) => {
                self.global_settings.boot_target = value as u8
            }
            (SettingId::RestoreLastTarget, SettingTarget::Global) => {
                self.global_settings.restore_last_target = value != 0
            }
            (SettingId::AutoReconnect, SettingTarget::Global) => {
                self.global_settings.auto_reconnect = value != 0
            }
            (SettingId::SwitchReleaseDelayMs, SettingTarget::Global) => {
                self.global_settings.switch_release_delay_ms = value as u16
            }
            (SettingId::ButtonShortAction, SettingTarget::Global) => {
                self.global_settings.button_short_action = value as u8
            }
            (SettingId::ButtonLongAction, SettingTarget::Global) => {
                self.global_settings.button_long_action = value as u8
            }
            (SettingId::ButtonVeryLongAction, SettingTarget::Global) => {
                self.global_settings.button_very_long_action = value as u8
            }
            (SettingId::LogLevel, SettingTarget::Global) => {
                self.global_settings.log_level = value as u8;
            }
            (id, SettingTarget::Host(host)) => {
                let Some(settings) = host
                    .0
                    .checked_sub(1)
                    .and_then(|index| self.host_settings.get_mut(index as usize))
                else {
                    return false;
                };
                match id {
                    SettingId::KeyboardLayout => settings.keyboard_layout = value as u8,
                    SettingId::RemapFromUsage => settings.remap_from_usage = value as u8,
                    SettingId::RemapToUsage => settings.remap_to_usage = value as u8,
                    SettingId::MouseSensitivityPercent => {
                        settings.mouse_sensitivity_percent = value as u16
                    }
                    SettingId::ScrollMultiplierPercent => {
                        settings.scroll_multiplier_percent = value as u16
                    }
                    SettingId::ConsumerFromUsage => settings.consumer_from_usage = value as u16,
                    SettingId::ConsumerToUsage => settings.consumer_to_usage = value as u16,
                    _ => return false,
                }
            }
            _ => return false,
        }
        true
    }

    fn push_storage_snapshot<const COMMANDS: usize>(
        &mut self,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
        priority: StoragePersistPriority,
    ) -> Result<(), RuntimeError> {
        self.storage_generation = self.storage_generation.wrapping_add(1);
        let mut state = self.bridge.storage_state(self.storage_generation)?;
        state.global_settings = self.global_settings;
        for (destination, source) in state.host_settings.iter_mut().zip(self.host_settings) {
            *destination = source;
        }
        push_command(commands, RuntimeCommand::PersistStorage { state, priority })
    }

    fn handle_tick<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        if let Some(pending) = self.pending_target_switch
            && now_ms >= pending.deadline_ms
        {
            self.pending_target_switch = None;
            self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                BridgeEvent::SwitchTarget {
                    target: pending.target,
                },
                commands,
            )?;
        }
        if let Some(pairing_mode) = self.pairing_mode
            && now_ms >= pairing_mode.deadline_ms
        {
            self.pairing_mode = None;
            self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                BridgeEvent::PairingModeExpired {
                    host_id: pairing_mode.host_id,
                },
                commands,
            )?;
        }
        Ok(())
    }

    fn request_target_switch<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        target: HostId,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        self.request_target_switch_append::<COMMANDS, ACTIONS>(target, now_ms, commands)
    }

    fn request_target_switch_append<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        target: HostId,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        let delay_ms = self.global_settings.switch_release_delay_ms as u64;
        if delay_ms == 0 {
            self.pending_target_switch = None;
            return self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                BridgeEvent::SwitchTarget { target },
                commands,
            );
        }
        self.pending_target_switch = Some(PendingTargetSwitch {
            target,
            deadline_ms: now_ms.saturating_add(delay_ms),
        });
        Ok(())
    }

    fn handle_bridge_event<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        event: BridgeEvent,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        self.handle_bridge_event_append::<COMMANDS, ACTIONS>(event, commands)
    }

    fn handle_bridge_event_append<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        event: BridgeEvent,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        self.observe_bridge_event(&event);
        let persist_priority = storage_persist_priority_for_event(&event);
        let mut actions = heapless::Vec::<BridgeAction, ACTIONS>::new();
        self.bridge.handle_event_in_place(event, &mut actions)?;

        for action in actions {
            self.dispatch_bridge_action(action, persist_priority, commands)?;
        }

        Ok(())
    }

    fn observe_bridge_event(&mut self, event: &BridgeEvent) {
        match event {
            BridgeEvent::HostConnected { host_id } => {
                if let Some(value) = host_id
                    .0
                    .checked_sub(1)
                    .and_then(|index| self.host_last_connected_seconds.get_mut(index as usize))
                {
                    *value = self.now_ms.saturating_div(1000).min(u32::MAX as u64) as u32;
                }
                self.record_history(1, host_id.0, 0, 0, 0)
            }
            BridgeEvent::HostDisconnected { host_id } => {
                if let Some(value) = host_id
                    .0
                    .checked_sub(1)
                    .and_then(|index| self.host_last_disconnected_seconds.get_mut(index as usize))
                {
                    *value = self.now_ms.saturating_div(1000).min(u32::MAX as u64) as u32;
                }
                self.record_history(2, host_id.0, 0, 0, 0);
            }
            BridgeEvent::SwitchTarget { target } => self.record_history(5, target.0, 0, 0, 0),
            BridgeEvent::EnterPairingMode { host_id } => self.record_history(6, host_id.0, 0, 0, 0),
            _ => {}
        }
        match event {
            BridgeEvent::EnterPairingMode { host_id }
                if self.pairing_mode.map(|state| state.host_id) != Some(*host_id) =>
            {
                self.pairing_mode = Some(PairingModeState {
                    host_id: *host_id,
                    deadline_ms: self
                        .pairing_mode
                        .map(|state| state.deadline_ms)
                        .unwrap_or(PAIRING_MODE_TIMEOUT_MS),
                });
            }
            BridgeEvent::EnterPairingMode { .. } => {}
            BridgeEvent::PairingModeExpired { host_id } | BridgeEvent::ClearHost { host_id }
                if self.pairing_mode.map(|state| state.host_id) == Some(*host_id) =>
            {
                self.pairing_mode = None;
            }
            BridgeEvent::HostSecurityChanged {
                host_id,
                bonded: true,
                ..
            } if self.pairing_mode.map(|state| state.host_id) == Some(*host_id) => {
                self.pairing_mode = None;
            }
            _ => {}
        }
    }

    fn record_history(
        &mut self,
        kind: u8,
        subject: u8,
        detail: u8,
        vendor_id: u16,
        product_id: u16,
    ) {
        if self.history.is_full() {
            let _ = self.history.pop_front();
        }
        let event = ManagementHistoryEvent {
            kind,
            sequence: self.next_history_sequence,
            timestamp_seconds: self.now_ms.saturating_div(1000).min(u32::MAX as u64) as u32,
            subject,
            detail,
            vendor_id,
            product_id,
        };
        self.next_history_sequence = self.next_history_sequence.wrapping_add(1);
        let _ = self.history.push_back(event);
    }

    fn apply_diagnostics_event(&mut self, event: RuntimeDiagnosticsEvent) {
        match event {
            RuntimeDiagnosticsEvent::ResetReason(reason) => self.diagnostics.reset_reason = reason,
            RuntimeDiagnosticsEvent::Brownout => {
                self.diagnostics.brownout_count = self.diagnostics.brownout_count.saturating_add(1)
            }
            RuntimeDiagnosticsEvent::BleDisconnected { host_id, reason } => {
                self.diagnostics.ble_disconnect_count =
                    self.diagnostics.ble_disconnect_count.saturating_add(1);
                if let Some(last) = self.history.back_mut()
                    && last.kind == 2
                    && last.subject == host_id.0
                {
                    last.detail = reason;
                } else {
                    self.record_history(2, host_id.0, reason, 0, 0);
                }
                if let Some(value) = host_id
                    .0
                    .checked_sub(1)
                    .and_then(|index| self.host_last_disconnect_reason.get_mut(index as usize))
                {
                    *value = reason;
                }
            }
            RuntimeDiagnosticsEvent::BleNotifyFailed => {
                self.diagnostics.ble_notify_failure_count =
                    self.diagnostics.ble_notify_failure_count.saturating_add(1);
                self.counters.ble_notify_dropped =
                    self.counters.ble_notify_dropped.saturating_add(1);
            }
            RuntimeDiagnosticsEvent::BleNotifyTimedOut { critical_release } => {
                self.diagnostics.ble_notify_failure_count =
                    self.diagnostics.ble_notify_failure_count.saturating_add(1);
                self.counters.ble_notify_timeouts =
                    self.counters.ble_notify_timeouts.saturating_add(1);
                if critical_release {
                    self.counters.critical_release_failures =
                        self.counters.critical_release_failures.saturating_add(1);
                }
            }
            RuntimeDiagnosticsEvent::BleManagementNotifyTimedOut => {
                self.diagnostics.ble_notify_failure_count =
                    self.diagnostics.ble_notify_failure_count.saturating_add(1);
                self.counters.ble_notify_timeouts =
                    self.counters.ble_notify_timeouts.saturating_add(1);
            }
            RuntimeDiagnosticsEvent::UsbLedWriteTimedOut => {
                self.counters.usb_led_write_timeouts =
                    self.counters.usb_led_write_timeouts.saturating_add(1);
            }
            RuntimeDiagnosticsEvent::UsbError => {
                self.diagnostics.usb_error_count =
                    self.diagnostics.usb_error_count.saturating_add(1)
            }
            RuntimeDiagnosticsEvent::FlashWrite { success: true } => {
                self.diagnostics.flash_write_count =
                    self.diagnostics.flash_write_count.saturating_add(1)
            }
            RuntimeDiagnosticsEvent::FlashWrite { success: false } => {
                self.diagnostics.flash_failure_count =
                    self.diagnostics.flash_failure_count.saturating_add(1)
            }
        }
    }

    fn dispatch_bridge_action<const COMMANDS: usize>(
        &mut self,
        action: BridgeAction,
        persist_priority: Option<StoragePersistPriority>,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        match action {
            BridgeAction::BleNotify {
                host_id,
                report,
                reason,
            } => {
                let report = self.apply_host_report_settings(host_id, report);
                push_command(
                    commands,
                    RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                        host_id,
                        report,
                        reason,
                    }),
                )
            }
            BridgeAction::AllowPairing { host_id } => push_command(
                commands,
                RuntimeCommand::BleCommand(BleTaskCommand::AllowPairing { host_id }),
            ),
            BridgeAction::RejectPairing { host_id } => push_command(
                commands,
                RuntimeCommand::BleCommand(BleTaskCommand::RejectPairing { host_id }),
            ),
            BridgeAction::ClearBond { host_id, bond } => push_command(
                commands,
                RuntimeCommand::BleCommand(BleTaskCommand::ClearBond { host_id, bond }),
            ),
            BridgeAction::ActivateInput { host_id } => push_command(
                commands,
                RuntimeCommand::BleCommand(BleTaskCommand::ActivateInput { host_id }),
            ),
            BridgeAction::UsbSetKeyboardLeds {
                interface_id,
                device_id,
                leds,
            } => {
                let keyboard = self
                    .usb_hid_interface(interface_id)
                    .ok_or(RuntimeError::UsbHidInterfaceNotRegistered { interface_id })?;
                let led_output = keyboard
                    .led_output
                    .ok_or(RuntimeError::UsbHidInterfaceNotRegistered { interface_id })?;
                let bytes = led_output.build(leds)?;
                push_command(
                    commands,
                    RuntimeCommand::UsbKeyboardLedWrite {
                        interface_id,
                        device_id,
                        bytes,
                    },
                )
            }
            BridgeAction::PersistProfiles => self.push_storage_snapshot(
                commands,
                persist_priority.unwrap_or(StoragePersistPriority::Normal),
            ),
            BridgeAction::StatusChanged(status) => {
                if status.pairable_host.is_none() {
                    self.pairing_mode = None;
                }
                let snapshot = self.next_status_snapshot(status);
                push_command(commands, RuntimeCommand::StatusChanged(snapshot))
            }
        }
    }

    /// Raw USB usages remain owned by `Bridge`; host-specific conversion is
    /// deliberately delayed until the BLE report leaves the runtime.
    fn apply_host_report_settings(
        &mut self,
        host_id: HostId,
        report: BleHidReport,
    ) -> BleHidReport {
        let Some(index) = host_id.0.checked_sub(1).map(usize::from) else {
            return report;
        };
        let Some(settings) = self.host_settings.get(index).copied() else {
            return report;
        };
        match report {
            BleHidReport::Keyboard(report) => {
                let mut bytes = *report.as_bytes();
                for usage in &mut bytes[2..] {
                    let layout_usage = match (settings.keyboard_layout, *usage) {
                        (1, 0x89) => 0x35,
                        (2, 0x35) => 0x89,
                        _ => *usage,
                    };
                    *usage = if layout_usage == settings.remap_from_usage
                        && settings.remap_from_usage != 0
                    {
                        settings.remap_to_usage
                    } else {
                        layout_usage
                    };
                }
                BleHidReport::Keyboard(BleKeyboard6KroReport::from_bytes(bytes))
            }
            BleHidReport::Consumer(report) => {
                let usage = u16::from_le_bytes(*report.as_bytes());
                let usage =
                    if usage == settings.consumer_from_usage && settings.consumer_from_usage != 0 {
                        settings.consumer_to_usage
                    } else {
                        usage
                    };
                BleHidReport::Consumer(BleConsumerReport::from_usage_id(usage))
            }
            BleHidReport::Mouse(report) => {
                let mut bytes = *report.as_bytes();
                if let Some(remainders) = self.mouse_scale_remainders.get_mut(index) {
                    bytes[1] = scale_axis_with_remainder(
                        bytes[1] as i8,
                        settings.mouse_sensitivity_percent,
                        &mut remainders.x,
                    ) as u8;
                    bytes[2] = scale_axis_with_remainder(
                        bytes[2] as i8,
                        settings.mouse_sensitivity_percent,
                        &mut remainders.y,
                    ) as u8;
                    bytes[3] = scale_axis_with_remainder(
                        bytes[3] as i8,
                        settings.scroll_multiplier_percent,
                        &mut remainders.wheel,
                    ) as u8;
                    bytes[4] = scale_axis_with_remainder(
                        bytes[4] as i8,
                        settings.scroll_multiplier_percent,
                        &mut remainders.pan,
                    ) as u8;
                }
                BleHidReport::Mouse(BleMouseReport::from_bytes(bytes))
            }
        }
    }

    fn next_status_snapshot(&mut self, status: BridgeStatus) -> StatusSnapshot {
        self.status_sequence = self.status_sequence.wrapping_add(1);
        let mut connected_hosts = 0u8;
        for host in self.bridge.state().hosts.hosts().iter().flatten() {
            if host.connected && (1..=8).contains(&host.host_id.0) {
                connected_hosts |= 1 << (host.host_id.0 - 1);
            }
        }
        StatusSnapshot {
            sequence: self.status_sequence,
            active_host: status.active_target,
            connected_hosts,
            pairing_host: status.pairable_host,
            usb_interface_count: self.usb_interfaces.iter().flatten().count() as u8,
            quiescing: false,
            bridge_stats: self.bridge.state().stats,
            runtime_counters: self.counters,
        }
    }

    fn upsert_usb_hid_interface(
        &mut self,
        interface_id: InterfaceId,
        device_id: DeviceId,
        led_output: Option<KeyboardLedOutputReport>,
    ) -> Result<(), RuntimeError> {
        if let Some(index) = self.usb_hid_interface_index(interface_id) {
            self.usb_interfaces[index] = Some(UsbHidInterfaceRuntimeState {
                interface_id,
                device_id,
                led_output,
                vendor_id: 0,
                product_id: 0,
                name: FixedName::empty(),
                flags: if led_output.is_some() { 0x02 } else { 0 },
            });
            return Ok(());
        }

        let Some(index) = self.usb_interfaces.iter().position(Option::is_none) else {
            return Err(RuntimeError::UsbHidInterfaceCapacity);
        };
        self.usb_interfaces[index] = Some(UsbHidInterfaceRuntimeState {
            interface_id,
            device_id,
            led_output,
            vendor_id: 0,
            product_id: 0,
            name: FixedName::empty(),
            flags: if led_output.is_some() { 0x02 } else { 0 },
        });
        Ok(())
    }

    fn usb_hid_interface(&self, interface_id: InterfaceId) -> Option<UsbHidInterfaceRuntimeState> {
        self.usb_hid_interface_index(interface_id)
            .and_then(|index| self.usb_interfaces[index])
    }

    fn usb_hid_interface_index(&self, interface_id: InterfaceId) -> Option<usize> {
        self.usb_interfaces
            .iter()
            .position(|state| matches!(state, Some(state) if state.interface_id == interface_id))
    }
}

impl DefaultBridgeRuntime {
    pub fn handle_default_input(
        &mut self,
        input: RuntimeInput<'_>,
        commands: &mut RuntimeCommandVec,
    ) -> Result<(), RuntimeError> {
        self.handle_input::<
            RUNTIME_COMMAND_CAPACITY,
            RUNTIME_BRIDGE_ACTION_CAPACITY,
            RUNTIME_BLE_EVENT_CAPACITY,
        >(input, commands)
    }
}

impl<const HOSTS: usize, const USB_INTERFACES: usize> Default
    for BridgeRuntime<HOSTS, USB_INTERFACES>
{
    fn default() -> Self {
        Self::new(0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbHidInterfaceRuntimeState {
    pub interface_id: InterfaceId,
    pub device_id: DeviceId,
    pub led_output: Option<KeyboardLedOutputReport>,
    pub vendor_id: u16,
    pub product_id: u16,
    pub name: FixedName,
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDiagnosticsEvent {
    ResetReason(u8),
    Brownout,
    BleDisconnected { host_id: HostId, reason: u8 },
    BleNotifyFailed,
    BleNotifyTimedOut { critical_release: bool },
    BleManagementNotifyTimedOut,
    UsbLedWriteTimedOut,
    UsbError,
    FlashWrite { success: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCounters {
    pub runtime_input_queue_high_watermark: u16,
    pub ble_control_queue_high_watermark: u16,
    pub ble_notify_queue_high_watermark: u16,
    pub usb_command_queue_high_watermark: u16,
    pub storage_queue_high_watermark: u16,
    pub status_queue_high_watermark: u16,
    pub ble_notify_dropped: u32,
    pub ble_notify_timeouts: u32,
    pub critical_release_failures: u32,
    pub mouse_reports_coalesced: u32,
    pub mouse_movement_saturated: u32,
    pub runtime_input_dropped: u32,
    pub status_updates_dropped: u32,
    pub usb_led_write_timeouts: u32,
    pub runtime_processing_max_us: u32,
    pub usb_to_ble_latency_max_us: u32,
    pub ble_notify_max_us: u32,
}

impl RuntimeCounters {
    pub const fn new() -> Self {
        Self {
            runtime_input_queue_high_watermark: 0,
            ble_control_queue_high_watermark: 0,
            ble_notify_queue_high_watermark: 0,
            usb_command_queue_high_watermark: 0,
            storage_queue_high_watermark: 0,
            status_queue_high_watermark: 0,
            ble_notify_dropped: 0,
            ble_notify_timeouts: 0,
            critical_release_failures: 0,
            mouse_reports_coalesced: 0,
            mouse_movement_saturated: 0,
            runtime_input_dropped: 0,
            status_updates_dropped: 0,
            usb_led_write_timeouts: 0,
            runtime_processing_max_us: 0,
            usb_to_ble_latency_max_us: 0,
            ble_notify_max_us: 0,
        }
    }
}

impl Default for RuntimeCounters {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusSnapshot {
    pub sequence: u64,
    pub active_host: Option<HostId>,
    pub connected_hosts: u8,
    pub pairing_host: Option<HostId>,
    pub usb_interface_count: u8,
    pub quiescing: bool,
    pub bridge_stats: crate::bridge::BridgeStats,
    pub runtime_counters: RuntimeCounters,
}

impl StatusSnapshot {
    pub const fn empty() -> Self {
        Self {
            sequence: 0,
            active_host: None,
            connected_hosts: 0,
            pairing_host: None,
            usb_interface_count: 0,
            quiescing: false,
            bridge_stats: crate::bridge::BridgeStats::new(),
            runtime_counters: RuntimeCounters::new(),
        }
    }

    pub const fn bridge_status(self) -> BridgeStatus {
        BridgeStatus {
            active_target: self.active_host,
            pairable_host: self.pairing_host,
        }
    }
}

pub const PAIRING_MODE_TIMEOUT_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingModeState {
    pub host_id: HostId,
    pub deadline_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingTargetSwitch {
    target: HostId,
    deadline_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
// Storage snapshots remain inline so the core does not require alloc.
#[allow(clippy::large_enum_variant)]
pub enum RuntimeCommand {
    BleCommand(BleTaskCommand),
    UsbKeyboardLedWrite {
        interface_id: InterfaceId,
        device_id: DeviceId,
        bytes: KeyboardLedOutputBytes,
    },
    PersistStorage {
        state: StorageState,
        priority: StoragePersistPriority,
    },
    StatusChanged(StatusSnapshot),
    ManagementResponse {
        destination: ManagementDestination,
        response: ManagementResponse,
    },
    ApplyEffect(RuntimeEffect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEffect {
    SetLogLevel(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandClass {
    Critical,
    Realtime,
    BestEffort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleCommandLane {
    Control,
    Notify,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleTaskCommand {
    Notify {
        host_id: HostId,
        report: BleHidReport,
        reason: NotifyReason,
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
    ManagementResponse {
        host_id: HostId,
        response: ManagementResponse,
    },
}

impl BleTaskCommand {
    pub const fn lane(self) -> BleCommandLane {
        match self {
            Self::Notify {
                reason: NotifyReason::Input,
                ..
            } => BleCommandLane::Notify,
            Self::Notify { .. } => BleCommandLane::Control,
            Self::AllowPairing { .. }
            | Self::RejectPairing { .. }
            | Self::ClearBond { .. }
            | Self::ActivateInput { .. }
            | Self::ManagementResponse { .. } => BleCommandLane::Control,
        }
    }

    pub const fn class(self) -> CommandClass {
        match self {
            Self::Notify { reason, .. } => match reason {
                NotifyReason::Input => CommandClass::Realtime,
                NotifyReason::InputEdge
                | NotifyReason::InputRelease
                | NotifyReason::TargetSwitchRelease
                | NotifyReason::UsbDeviceRemovedRelease
                | NotifyReason::SafetyRelease => CommandClass::Critical,
            },
            Self::AllowPairing { .. }
            | Self::RejectPairing { .. }
            | Self::ClearBond { .. }
            | Self::ActivateInput { .. }
            | Self::ManagementResponse { .. } => CommandClass::Critical,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbTaskCommand {
    pub interface_id: InterfaceId,
    pub device_id: DeviceId,
    pub bytes: KeyboardLedOutputBytes,
}

impl UsbTaskCommand {
    pub const fn class(self) -> CommandClass {
        CommandClass::Realtime
    }

    pub fn matches_target(self, interface_id: InterfaceId, device_id: DeviceId) -> bool {
        self.interface_id == interface_id && self.device_id == device_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageTaskCommand {
    pub state: StorageState,
    pub priority: StoragePersistPriority,
}

impl StorageTaskCommand {
    pub const fn class(&self) -> CommandClass {
        CommandClass::Critical
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusTaskCommand {
    pub status: BridgeStatus,
    pub snapshot: StatusSnapshot,
    pub management: Option<ManagementTaskResponse>,
}

impl StatusTaskCommand {
    pub const fn class(self) -> CommandClass {
        if self.management.is_some() {
            CommandClass::Critical
        } else {
            CommandClass::BestEffort
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementTaskResponse {
    pub destination: ManagementDestination,
    pub response: ManagementResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDispatchError {
    BleQueueCapacity,
    UsbQueueCapacity,
    StorageQueueCapacity,
    StatusQueueCapacity,
    EffectQueueCapacity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCommandQueues<
    const BLE: usize,
    const USB: usize,
    const STORAGE: usize,
    const STATUS: usize,
> {
    pub ble: heapless::Vec<BleTaskCommand, BLE>,
    pub usb: heapless::Vec<UsbTaskCommand, USB>,
    pub storage: heapless::Vec<StorageTaskCommand, STORAGE>,
    pub status: heapless::Vec<StatusTaskCommand, STATUS>,
    pub effects: heapless::Vec<RuntimeEffect, STATUS>,
}

impl<const BLE: usize, const USB: usize, const STORAGE: usize, const STATUS: usize>
    RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>
{
    pub const fn new() -> Self {
        Self {
            ble: heapless::Vec::new(),
            usb: heapless::Vec::new(),
            storage: heapless::Vec::new(),
            status: heapless::Vec::new(),
            effects: heapless::Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.ble.clear();
        self.usb.clear();
        self.storage.clear();
        self.status.clear();
        self.effects.clear();
    }

    pub fn dispatch_from(
        &mut self,
        commands: &[RuntimeCommand],
    ) -> Result<(), RuntimeDispatchError> {
        let mut next = Self::new();
        for command in commands {
            next.dispatch_one(command)?;
        }
        *self = next;
        Ok(())
    }

    fn dispatch_one(&mut self, command: &RuntimeCommand) -> Result<(), RuntimeDispatchError> {
        match command {
            RuntimeCommand::BleCommand(command) => self
                .ble
                .push(*command)
                .map_err(|_| RuntimeDispatchError::BleQueueCapacity),
            RuntimeCommand::UsbKeyboardLedWrite {
                interface_id,
                device_id,
                bytes,
            } => self
                .usb
                .push(UsbTaskCommand {
                    interface_id: *interface_id,
                    device_id: *device_id,
                    bytes: *bytes,
                })
                .map_err(|_| RuntimeDispatchError::UsbQueueCapacity),
            RuntimeCommand::PersistStorage { state, priority } => self
                .storage
                .push(StorageTaskCommand {
                    state: state.clone(),
                    priority: *priority,
                })
                .map_err(|_| RuntimeDispatchError::StorageQueueCapacity),
            RuntimeCommand::StatusChanged(status) => self
                .status
                .push(StatusTaskCommand {
                    status: status.bridge_status(),
                    snapshot: *status,
                    management: None,
                })
                .map_err(|_| RuntimeDispatchError::StatusQueueCapacity),
            RuntimeCommand::ManagementResponse {
                destination,
                response,
            } => self
                .status
                .push(StatusTaskCommand {
                    status: match response.payload {
                        ManagementResponsePayload::Status(status) => BridgeStatus {
                            active_target: status.active_host,
                            pairable_host: status.pairing_host,
                        },
                        _ => BridgeStatus {
                            active_target: None,
                            pairable_host: None,
                        },
                    },
                    snapshot: StatusSnapshot::empty(),
                    management: Some(ManagementTaskResponse {
                        destination: *destination,
                        response: *response,
                    }),
                })
                .map_err(|_| RuntimeDispatchError::StatusQueueCapacity),
            RuntimeCommand::ApplyEffect(effect) => self
                .effects
                .push(*effect)
                .map_err(|_| RuntimeDispatchError::EffectQueueCapacity),
        }
    }
}

const fn valid_management_host<const HOSTS: usize>(host_id: HostId) -> bool {
    host_id.0 != 0 && (host_id.0 as usize) <= HOSTS && (host_id.0 as usize) <= 4
}

impl<const BLE: usize, const USB: usize, const STORAGE: usize, const STATUS: usize> Default
    for RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>
{
    fn default() -> Self {
        Self::new()
    }
}

pub type DefaultRuntimeCommandQueues = RuntimeCommandQueues<
    RUNTIME_BLE_COMMAND_QUEUE_CAPACITY,
    RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
    RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    Bridge(BridgeError),
    Storage(StorageError),
    UsbLed(KeyboardLedOutputError),
    BleHostAdapter(BleHostAdapterError),
    UsbHidInterfaceCapacity,
    UsbHidInterfaceNotRegistered { interface_id: InterfaceId },
    CommandCapacity,
}

impl From<BridgeError> for RuntimeError {
    fn from(error: BridgeError) -> Self {
        Self::Bridge(error)
    }
}

impl From<StorageError> for RuntimeError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<KeyboardLedOutputError> for RuntimeError {
    fn from(error: KeyboardLedOutputError) -> Self {
        Self::UsbLed(error)
    }
}

impl From<BleHostAdapterError> for RuntimeError {
    fn from(error: BleHostAdapterError) -> Self {
        Self::BleHostAdapter(error)
    }
}

fn push_command<const COMMANDS: usize>(
    commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    command: RuntimeCommand,
) -> Result<(), RuntimeError> {
    commands
        .push(command)
        .map_err(|_| RuntimeError::CommandCapacity)
}

const fn storage_persist_priority_for_event(event: &BridgeEvent) -> Option<StoragePersistPriority> {
    match event {
        BridgeEvent::HostSecurityChanged { .. }
        | BridgeEvent::ClearHost { .. }
        | BridgeEvent::SetHostName { .. } => Some(StoragePersistPriority::Critical),
        BridgeEvent::CccdChanged { .. } => Some(StoragePersistPriority::Normal),
        BridgeEvent::SwitchTarget { .. } => Some(StoragePersistPriority::Lazy),
        _ => None,
    }
}

fn scale_axis_with_remainder(value: i8, percent: u16, remainder: &mut i32) -> i8 {
    if value == 0 {
        return 0;
    }
    let scaled = remainder.saturating_add(i32::from(value) * i32::from(percent));
    let available = scaled / 100;
    let output = available.clamp(i8::MIN as i32, i8::MAX as i32) as i8;
    *remainder = scaled - i32::from(output) * 100;
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble::{BleHidAttribute, BleHostAdapterEvent};
    use crate::input::{KeyUsage, KeyboardFrame, KeyboardLedState, ModifierState};
    use crate::reports::{BleKeyboard6KroReport, ReportKind};
    use crate::storage::{FixedName, StoredHostProfile};

    fn management_request(command: ManagementCommand, request_id: u8) -> RuntimeInput<'static> {
        RuntimeInput::ManagementRequest {
            destination: ManagementDestination::Wired,
            request: ManagementRequest {
                request_id,
                command,
            },
            now_ms: 1_000,
        }
    }

    fn management_response(commands: &[RuntimeCommand]) -> ManagementResponse {
        commands
            .iter()
            .find_map(|command| match command {
                RuntimeCommand::ManagementResponse { response, .. } => Some(*response),
                _ => None,
            })
            .expect("management response")
    }

    fn management_status(commands: &[RuntimeCommand]) -> ManagementStatus {
        match management_response(commands).payload {
            ManagementResponsePayload::Status(status) => status,
            payload => panic!("expected management status, got {payload:?}"),
        }
    }

    #[test]
    fn management_status_reports_all_slots_and_live_state() {
        let mut runtime = BridgeRuntime::<4, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();
        runtime
            .handle_event::<12, 12>(
                BridgeEvent::HostConnected { host_id: HostId(2) },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<12, 12>(
                BridgeEvent::HostSecurityChanged {
                    host_id: HostId(2),
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::GetStatus, 7),
                &mut commands,
            )
            .unwrap();

        let response = management_response(&commands);
        assert_eq!(response.request_id, 7);
        assert_eq!(response.result, ManagementResult::Ok);
        let status = match response.payload {
            ManagementResponsePayload::Status(status) => status,
            _ => panic!("expected status"),
        };
        assert_eq!(status.host_count, 4);
        assert_eq!(
            status.hosts[1],
            ManagementHostStatus {
                known: true,
                connected: true,
                encrypted: true,
                bonded: true,
            }
        );
    }

    #[test]
    fn management_generated_settings_validate_persist_and_restore() {
        let mut runtime = BridgeRuntime::<4, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();
        let target = SettingTarget::Host(HostId(2));
        runtime
            .handle_input::<12, 12, 2>(
                management_request(
                    ManagementCommand::SetSetting {
                        id: SettingId::MouseSensitivityPercent,
                        target,
                        value: 175,
                    },
                    1,
                ),
                &mut commands,
            )
            .unwrap();
        assert!(matches!(
            management_response(&commands).payload,
            ManagementResponsePayload::Setting(ManagementSetting { value: 175, .. })
        ));
        let snapshot = commands
            .iter()
            .find_map(|command| match command {
                RuntimeCommand::PersistStorage { state, .. } => Some(state.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(snapshot.host_settings[1].mouse_sensitivity_percent, 175);

        runtime
            .handle_input::<12, 12, 2>(
                management_request(
                    ManagementCommand::SetSetting {
                        id: SettingId::MouseSensitivityPercent,
                        target,
                        value: 401,
                    },
                    2,
                ),
                &mut commands,
            )
            .unwrap();
        assert_eq!(
            management_response(&commands).result,
            ManagementResult::InvalidSetting
        );

        let mut restored = BridgeRuntime::<4, 1>::new(0);
        restored
            .restore_storage_state(&snapshot, &mut commands)
            .unwrap();
        restored
            .handle_input::<12, 12, 2>(
                management_request(
                    ManagementCommand::GetSetting {
                        id: SettingId::MouseSensitivityPercent,
                        target,
                    },
                    3,
                ),
                &mut commands,
            )
            .unwrap();
        assert!(matches!(
            management_response(&commands).payload,
            ManagementResponsePayload::Setting(ManagementSetting { value: 175, .. })
        ));
    }

    #[test]
    fn management_reports_usb_product_name_diagnostics_and_history() {
        let mut runtime = BridgeRuntime::<4, 2>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();
        runtime
            .handle_input::<12, 12, 2>(
                RuntimeInput::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DeviceId(9),
                    led_output: None,
                },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                RuntimeInput::UsbDeviceMetadataUpdated {
                    device_id: DeviceId(9),
                    vendor_id: 0x1234,
                    product_id: 0xabcd,
                    name: FixedName::from_ascii("Mechanical Keyboard").unwrap(),
                    flags: 0x02,
                },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                RuntimeInput::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(2),
                    device_id: DeviceId(9),
                    led_output: None,
                },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(RuntimeInput::Tick { now_ms: 12_345 }, &mut commands)
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                RuntimeInput::DiagnosticsEvent(RuntimeDiagnosticsEvent::UsbError),
                &mut commands,
            )
            .unwrap();

        runtime
            .handle_input::<12, 12, 2>(
                management_request(
                    ManagementCommand::GetUsbDevice {
                        index: 0,
                        name_offset: 5,
                    },
                    4,
                ),
                &mut commands,
            )
            .unwrap();
        let ManagementResponsePayload::UsbDevice(device) = management_response(&commands).payload
        else {
            panic!()
        };
        assert_eq!(device.vendor_id, 0x1234);
        assert_eq!(device.product_id, 0xabcd);
        assert_eq!(device.name_chunk(), b"nical");

        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::GetDiagnostics, 5),
                &mut commands,
            )
            .unwrap();
        let ManagementResponsePayload::Diagnostics(diagnostics) =
            management_response(&commands).payload
        else {
            panic!()
        };
        assert_eq!(diagnostics.uptime_seconds, 12);
        assert_eq!(diagnostics.usb_error_count, 1);

        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::GetHistory { index: 0 }, 6),
                &mut commands,
            )
            .unwrap();
        let ManagementResponsePayload::History(event) = management_response(&commands).payload
        else {
            panic!()
        };
        assert_eq!(
            (event.kind, event.vendor_id, event.product_id),
            (3, 0x1234, 0xabcd)
        );
        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::GetHistory { index: 1 }, 7),
                &mut commands,
            )
            .unwrap();
        assert_eq!(
            management_response(&commands).payload,
            ManagementResponsePayload::None
        );
    }

    #[test]
    fn automatically_discovered_ble_name_is_used_until_manually_overridden() {
        let mut runtime = BridgeRuntime::<4, 0>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();
        runtime
            .handle_input::<12, 12, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::HostConnected { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                RuntimeInput::HostNameDiscovered {
                    host_id: HostId(1),
                    name: FixedName::from_ascii("BLE-AABBCC").unwrap(),
                },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::GetHostInfo(HostId(1)), 1),
                &mut commands,
            )
            .unwrap();
        let ManagementResponsePayload::HostInfo(info) = management_response(&commands).payload
        else {
            panic!()
        };
        assert_eq!(info.name.as_bytes(), b"BLE-AABBCC");
        assert_eq!(info.name_source, 1);

        runtime
            .handle_input::<12, 12, 2>(
                management_request(
                    ManagementCommand::SetHostName {
                        host_id: HostId(1),
                        name: ManagementHostName::from_ascii("Work PC").unwrap(),
                    },
                    2,
                ),
                &mut commands,
            )
            .unwrap();
        let ManagementResponsePayload::HostInfo(info) = management_response(&commands).payload
        else {
            panic!()
        };
        assert_eq!(info.name.as_bytes(), b"Work PC");
        assert_eq!(info.name_source, 2);
    }

    #[test]
    fn management_can_pair_select_and_forget_a_host() {
        let mut runtime = BridgeRuntime::<4, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();

        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::StartPairing(HostId(3)), 1),
                &mut commands,
            )
            .unwrap();
        assert!(
            commands.contains(&RuntimeCommand::BleCommand(BleTaskCommand::AllowPairing {
                host_id: HostId(3)
            }))
        );
        assert_eq!(management_status(&commands).pairing_host, Some(HostId(3)));

        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::SelectHost(HostId(3)), 2),
                &mut commands,
            )
            .unwrap();
        assert_eq!(management_status(&commands).active_host, Some(HostId(3)));

        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::ForgetHost(HostId(3)), 3),
                &mut commands,
            )
            .unwrap();
        let response = management_response(&commands);
        assert_eq!(response.result, ManagementResult::Ok);
        let status = management_status(&commands);
        assert_eq!(status.active_host, None);
        assert_eq!(status.pairing_host, None);
        assert!(!status.hosts[2].known);
    }

    #[test]
    fn management_rejects_invalid_or_destructive_slot_requests() {
        let mut runtime = BridgeRuntime::<4, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();

        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::SelectHost(HostId(0)), 1),
                &mut commands,
            )
            .unwrap();
        assert_eq!(
            management_response(&commands).result,
            ManagementResult::InvalidHost
        );

        runtime
            .handle_event::<12, 12>(
                BridgeEvent::HostSecurityChanged {
                    host_id: HostId(1),
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::StartPairing(HostId(1)), 2),
                &mut commands,
            )
            .unwrap();
        assert_eq!(
            management_response(&commands).result,
            ManagementResult::HostAlreadyBonded
        );
    }

    #[test]
    fn management_reports_usb_devices_interfaces_and_keyboards() {
        let mut runtime = BridgeRuntime::<4, 4>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();
        runtime
            .register_usb_hid_interface::<12, 12>(
                InterfaceId(1),
                DeviceId(1),
                Some(KeyboardLedOutputReport::boot_keyboard()),
                &mut commands,
            )
            .unwrap();
        runtime
            .register_usb_hid_interface::<12, 12>(InterfaceId(2), DeviceId(1), None, &mut commands)
            .unwrap();
        runtime
            .register_usb_hid_interface::<12, 12>(InterfaceId(3), DeviceId(2), None, &mut commands)
            .unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::GetStatus, 8),
                &mut commands,
            )
            .unwrap();

        assert_eq!(
            management_status(&commands).usb,
            ManagementUsbStatus {
                device_count: 2,
                interface_count: 3,
                keyboard_count: 1,
            }
        );
    }

    #[test]
    fn management_host_name_is_returned_and_persisted() {
        let mut runtime = BridgeRuntime::<4, 1>::new(4);
        let mut commands = heapless::Vec::<RuntimeCommand, 12>::new();
        runtime
            .handle_event::<12, 12>(
                BridgeEvent::HostConnected { host_id: HostId(2) },
                &mut commands,
            )
            .unwrap();
        let name = ManagementHostName::from_ascii("Work laptop").unwrap();
        runtime
            .handle_input::<12, 12, 2>(
                management_request(
                    ManagementCommand::SetHostName {
                        host_id: HostId(2),
                        name,
                    },
                    9,
                ),
                &mut commands,
            )
            .unwrap();
        let persisted = commands.iter().find_map(|command| match command {
            RuntimeCommand::PersistStorage { state, priority } => Some((state, priority)),
            _ => None,
        });
        assert_eq!(
            persisted.map(|(_, priority)| *priority),
            Some(StoragePersistPriority::Critical)
        );
        assert_eq!(
            persisted
                .and_then(|(state, _)| state.hosts().iter().find(|host| host.host_id == HostId(2)))
                .map(|host| host.name.as_bytes()),
            Some(b"Work laptop".as_slice())
        );

        runtime
            .handle_input::<12, 12, 2>(
                management_request(ManagementCommand::GetHostInfo(HostId(2)), 10),
                &mut commands,
            )
            .unwrap();
        assert_eq!(
            management_response(&commands).payload,
            ManagementResponsePayload::HostInfo(ManagementHostInfo {
                host_id: HostId(2),
                status: ManagementHostStatus {
                    known: true,
                    connected: true,
                    encrypted: false,
                    bonded: false,
                },
                name,
                name_source: 2,
            })
        );
    }

    #[test]
    fn input_event_becomes_ble_notify_command() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_event::<8, 8>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(keyboard_input(
                    KeyUsage(0x04),
                ))),
                &mut commands,
            )
            .unwrap();

        assert_eq!(
            commands.as_slice(),
            &[RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(
                    BleKeyboard6KroReport::from_visible_state(
                        &runtime
                            .bridge()
                            .state()
                            .input
                            .keyboard
                            .visible_against(&runtime.bridge().state().suppression.keyboard)
                    )
                    .report
                ),
                reason: NotifyReason::Input,
            })]
        );
    }

    #[test]
    fn host_led_change_becomes_usb_keyboard_led_write_command() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        runtime
            .register_usb_hid_interface::<8, 8>(
                InterfaceId(1),
                DeviceId(7),
                Some(KeyboardLedOutputReport::boot_keyboard()),
                &mut commands,
            )
            .unwrap();
        commands.clear();

        runtime
            .handle_event::<8, 8>(
                BridgeEvent::HostKeyboardLedChanged {
                    host_id: HostId(1),
                    leds: KeyboardLedState::CAPS_LOCK | KeyboardLedState::SCROLL_LOCK,
                },
                &mut commands,
            )
            .unwrap();

        let RuntimeCommand::UsbKeyboardLedWrite {
            interface_id,
            device_id,
            bytes,
        } = &commands[0]
        else {
            panic!("expected USB LED write command");
        };
        assert_eq!(*interface_id, InterfaceId(1));
        assert_eq!(*device_id, DeviceId(7));
        assert_eq!(bytes.as_slice(), &[0b0000_0110]);
    }

    #[test]
    fn host_led_change_fans_out_to_all_registered_usb_interfaces() {
        let mut runtime = BridgeRuntime::<2, 4>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 16>::new();

        runtime
            .handle_event::<16, 16>(
                BridgeEvent::SwitchTarget { target: HostId(1) },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<16, 16>(
                BridgeEvent::HostConnected { host_id: HostId(1) },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<16, 16>(
                BridgeEvent::HostSecurityChanged {
                    host_id: HostId(1),
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut commands,
            )
            .unwrap();

        for device_id in 1..=4 {
            runtime
                .register_usb_hid_interface::<16, 16>(
                    InterfaceId(device_id),
                    DeviceId(device_id),
                    Some(KeyboardLedOutputReport::boot_keyboard()),
                    &mut commands,
                )
                .unwrap();
        }
        commands.clear();

        runtime
            .handle_event::<16, 16>(
                BridgeEvent::HostKeyboardLedChanged {
                    host_id: HostId(1),
                    leds: KeyboardLedState::CAPS_LOCK,
                },
                &mut commands,
            )
            .unwrap();

        let usb_commands = commands
            .iter()
            .filter(|command| matches!(command, RuntimeCommand::UsbKeyboardLedWrite { .. }))
            .count();
        assert_eq!(usb_commands, 4);
    }

    #[test]
    fn target_switch_persists_snapshot_with_incremented_generation() {
        let mut runtime = BridgeRuntime::<2, 1>::new(41);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_event::<8, 8>(
                BridgeEvent::SwitchTarget { target: HostId(2) },
                &mut commands,
            )
            .unwrap();

        assert_eq!(runtime.storage_generation(), 42);
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                RuntimeCommand::PersistStorage { state, priority }
                    if state.generation == 42
                        && state.last_active_host == Some(HostId(2))
                        && *priority == StoragePersistPriority::Lazy
            )
        }));
    }

    #[test]
    fn missing_usb_keyboard_registration_is_an_explicit_error() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        let err = runtime
            .handle_event::<8, 8>(
                BridgeEvent::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(9),
                    device_id: DeviceId(99),
                    keyboard_led_sink: true,
                },
                &mut commands,
            )
            .unwrap_err();

        assert_eq!(
            err,
            RuntimeError::UsbHidInterfaceNotRegistered {
                interface_id: InterfaceId(9)
            }
        );
    }

    #[test]
    fn ble_cccd_write_event_is_applied_through_runtime_and_persisted() {
        let mut runtime = BridgeRuntime::<2, 1>::new(10);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_ble_host_event::<8, 8, 2>(
                HostId(1),
                BleHostAdapterEvent::GattWrite {
                    attribute: BleHidAttribute::KeyboardInputCccd,
                    data: &[0x01, 0x00],
                },
                &mut commands,
            )
            .unwrap();

        assert!(!runtime.bridge().can_send(HostId(1), ReportKind::Keyboard));
        assert_eq!(runtime.storage_generation(), 11);
        assert!(commands.iter().any(|command| {
            matches!(
                command,
                RuntimeCommand::PersistStorage { state, priority }
                    if state.generation == 11
                        && state.hosts()[0].keyboard_cccd_enabled
                        && *priority == StoragePersistPriority::Normal
            )
        }));
    }

    #[test]
    fn clearing_host_persists_snapshot_as_critical() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        let next_generation = runtime.storage_generation().wrapping_add(1);

        runtime
            .handle_event::<8, 8>(BridgeEvent::ClearHost { host_id: HostId(1) }, &mut commands)
            .unwrap();

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                RuntimeCommand::PersistStorage { state, priority }
                    if state.generation == next_generation
                        && *priority == StoragePersistPriority::Critical
            )
        }));
    }

    #[test]
    fn ble_keyboard_output_write_event_drives_usb_led_command_through_runtime() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        runtime
            .register_usb_hid_interface::<8, 8>(
                InterfaceId(1),
                DeviceId(7),
                Some(KeyboardLedOutputReport::boot_keyboard()),
                &mut commands,
            )
            .unwrap();
        commands.clear();

        runtime
            .handle_ble_host_event::<8, 8, 2>(
                HostId(1),
                BleHostAdapterEvent::GattWrite {
                    attribute: BleHidAttribute::BootKeyboardOutputReport,
                    data: &[0b0000_0011],
                },
                &mut commands,
            )
            .unwrap();

        assert_eq!(
            commands.as_slice(),
            &[RuntimeCommand::UsbKeyboardLedWrite {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                bytes: KeyboardLedOutputReport::boot_keyboard()
                    .build(KeyboardLedState::NUM_LOCK | KeyboardLedState::CAPS_LOCK)
                    .unwrap(),
            }]
        );
    }

    #[test]
    fn runtime_input_handles_usb_registration_ble_write_and_storage_restore() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DeviceId(7),
                    led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
                },
                &mut commands,
            )
            .unwrap();
        assert_eq!(
            commands.as_slice(),
            &[RuntimeCommand::UsbKeyboardLedWrite {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                bytes: KeyboardLedOutputReport::boot_keyboard()
                    .build(KeyboardLedState::empty())
                    .unwrap(),
            }]
        );

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BleHostEvent {
                    host_id: HostId(1),
                    event: BleHostAdapterEvent::GattWrite {
                        attribute: BleHidAttribute::BootKeyboardOutputReport,
                        data: &[0b0000_0010],
                    },
                },
                &mut commands,
            )
            .unwrap();
        assert!(matches!(
            commands.as_slice(),
            [RuntimeCommand::UsbKeyboardLedWrite {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                ..
            }]
        ));

        let mut storage = StorageState::new(77);
        storage.last_active_host = Some(HostId(1));
        runtime
            .handle_input::<8, 8, 2>(RuntimeInput::RestoreStorage(&storage), &mut commands)
            .unwrap();
        assert_eq!(runtime.storage_generation(), 77);
        assert_eq!(
            commands.as_slice(),
            &[RuntimeCommand::ApplyEffect(RuntimeEffect::SetLogLevel(2))]
        );
    }

    #[test]
    fn runtime_input_handles_plain_bridge_control_event() {
        let mut runtime = BridgeRuntime::<2, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(2) }),
                &mut commands,
            )
            .unwrap();

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                RuntimeCommand::StatusChanged(StatusSnapshot {
                    active_host: Some(HostId(2)),
                    pairing_host: None,
                    ..
                })
            )
        }));
    }

    #[test]
    fn pairing_mode_event_becomes_ble_control_command() {
        let mut runtime = BridgeRuntime::<2, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::EnterPairingMode { host_id: HostId(2) }),
                &mut commands,
            )
            .unwrap();

        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::AllowPairing { host_id: HostId(2) })
        )));
    }

    #[test]
    fn button_intent_cycles_to_next_connected_target_in_runtime() {
        let mut runtime = BridgeRuntime::<3, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::HostConnected { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::HostConnected { host_id: HostId(3) }),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(1) }),
                &mut commands,
            )
            .unwrap();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::ButtonIntent {
                    intent: ButtonIntent::NextConnectedTarget,
                    now_ms: 100,
                },
                &mut commands,
            )
            .unwrap();

        assert!(commands.is_empty());
        runtime
            .handle_input::<8, 8, 2>(RuntimeInput::Tick { now_ms: 119 }, &mut commands)
            .unwrap();
        assert!(commands.is_empty());
        runtime
            .handle_input::<8, 8, 2>(RuntimeInput::Tick { now_ms: 120 }, &mut commands)
            .unwrap();

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                RuntimeCommand::StatusChanged(StatusSnapshot {
                    active_host: Some(HostId(3)),
                    ..
                })
            )
        }));
    }

    #[test]
    fn keyboard_layout_normalizes_jis_yen_and_us_grave_usages() {
        let mut runtime = ready_runtime();
        runtime.host_settings[0].keyboard_layout = 1;
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::InputFrame(
                    crate::input::InputFrame::Standard(keyboard_input(KeyUsage(0x89))),
                )),
                &mut commands,
            )
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                report: BleHidReport::Keyboard(report),
                ..
            }) if report.as_bytes()[2] == 0x35
        )));
        assert_eq!(
            runtime.bridge.state().input.keyboard.keys(),
            &[KeyUsage(0x89)]
        );

        runtime.host_settings[0].keyboard_layout = 2;
        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::InputFrame(
                    crate::input::InputFrame::Standard(keyboard_input(KeyUsage(0x35))),
                )),
                &mut commands,
            )
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                report: BleHidReport::Keyboard(report),
                ..
            }) if report.as_bytes()[2] == 0x89
        )));
        assert_eq!(
            runtime.bridge.state().input.keyboard.keys(),
            &[KeyUsage(0x35)]
        );
    }

    #[test]
    fn mouse_scaling_preserves_signed_fractional_movement_per_host_and_axis() {
        let mut runtime = BridgeRuntime::<2, 0>::new(0);
        runtime.host_settings[0].mouse_sensitivity_percent = 50;
        runtime.host_settings[0].scroll_multiplier_percent = 50;
        runtime.host_settings[1].mouse_sensitivity_percent = 50;
        runtime.host_settings[1].scroll_multiplier_percent = 50;

        let input = BleHidReport::Mouse(BleMouseReport::from_bytes([0, 1, 255, 1, 255]));
        let first = runtime.apply_host_report_settings(HostId(1), input);
        let second = runtime.apply_host_report_settings(HostId(1), input);
        let other_host_first = runtime.apply_host_report_settings(HostId(2), input);

        let BleHidReport::Mouse(first) = first else {
            panic!("mouse report");
        };
        let BleHidReport::Mouse(second) = second else {
            panic!("mouse report");
        };
        let BleHidReport::Mouse(other_host_first) = other_host_first else {
            panic!("mouse report");
        };
        assert_eq!(first.as_bytes(), &[0, 0, 0, 0, 0]);
        assert_eq!(second.as_bytes(), &[0, 1, 255, 1, 255]);
        assert_eq!(other_host_first.as_bytes(), &[0, 0, 0, 0, 0]);
    }

    #[test]
    fn held_raw_usage_is_suppressed_across_hosts_with_different_maps() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 16>::new();
        runtime.host_settings[0].keyboard_layout = 1;
        runtime.host_settings[0].consumer_from_usage = 0x00e9;
        runtime.host_settings[0].consumer_to_usage = 0x00cd;
        for event in [
            BridgeEvent::HostConnected { host_id: HostId(2) },
            BridgeEvent::HostSecurityChanged {
                host_id: HostId(2),
                encrypted: true,
                bonded: true,
                bond: None,
            },
            BridgeEvent::CccdChanged {
                host_id: HostId(2),
                report: ReportKind::Keyboard,
                enabled: true,
            },
            BridgeEvent::CccdChanged {
                host_id: HostId(2),
                report: ReportKind::Consumer,
                enabled: true,
            },
        ] {
            runtime
                .handle_event::<16, 16>(event, &mut commands)
                .unwrap();
        }

        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(keyboard_input(
                    KeyUsage(0x89),
                ))),
                &mut commands,
            )
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                report: BleHidReport::Keyboard(report),
                ..
            }) if report.as_bytes()[2] == 0x35
        )));
        assert_eq!(
            runtime.bridge.state().input.keyboard.keys(),
            &[KeyUsage(0x89)]
        );

        runtime
            .handle_event::<16, 16>(
                BridgeEvent::SwitchTarget { target: HostId(2) },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(keyboard_input(
                    KeyUsage(0x89),
                ))),
                &mut commands,
            )
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(2),
                report: BleHidReport::Keyboard(report),
                ..
            }) if report == &BleKeyboard6KroReport::release()
        )));

        let mut released = keyboard_input(KeyUsage(0x89));
        released.keyboard = Some(KeyboardFrame::new(ModifierState::empty()));
        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(released)),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(keyboard_input(
                    KeyUsage(0x89),
                ))),
                &mut commands,
            )
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(2),
                report: BleHidReport::Keyboard(report),
                ..
            }) if report.as_bytes()[2] == 0x89
        )));
    }

    #[test]
    fn held_consumer_usage_is_suppressed_across_hosts_with_different_maps() {
        let mut runtime = ready_runtime();
        let mut commands = heapless::Vec::<RuntimeCommand, 16>::new();
        runtime.host_settings[0].consumer_from_usage = 0x00e9;
        runtime.host_settings[0].consumer_to_usage = 0x00cd;
        for event in [
            BridgeEvent::HostConnected { host_id: HostId(2) },
            BridgeEvent::HostSecurityChanged {
                host_id: HostId(2),
                encrypted: true,
                bonded: true,
                bond: None,
            },
            BridgeEvent::CccdChanged {
                host_id: HostId(2),
                report: ReportKind::Consumer,
                enabled: true,
            },
        ] {
            runtime
                .handle_event::<16, 16>(event, &mut commands)
                .unwrap();
        }

        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(consumer_input(Some(
                    0x00e9,
                )))),
                &mut commands,
            )
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Consumer(report),
                ..
            }) if report.as_bytes() == &0x00cdu16.to_le_bytes()
        )));

        runtime
            .handle_event::<16, 16>(
                BridgeEvent::SwitchTarget { target: HostId(2) },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(consumer_input(Some(
                    0x00e9,
                )))),
                &mut commands,
            )
            .unwrap();
        assert!(!commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(2),
                report: BleHidReport::Consumer(report),
                ..
            }) if report.as_bytes() != &[0, 0]
        )));

        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(consumer_input(None))),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<16, 16>(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(consumer_input(Some(
                    0x00e9,
                )))),
                &mut commands,
            )
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(2),
                report: BleHidReport::Consumer(report),
                ..
            }) if report.as_bytes() == &0x00e9u16.to_le_bytes()
        )));
    }

    #[test]
    fn one_hundred_fifty_percent_scaling_preserves_small_report_sum() {
        let mut remainder = 0;
        let output: i32 = (0..4)
            .map(|_| i32::from(scale_axis_with_remainder(1, 150, &mut remainder)))
            .sum();
        assert_eq!(output, 6);
        assert_eq!(remainder, 0);
    }

    #[test]
    fn notify_timeout_is_counted_once_in_each_applicable_counter() {
        let mut runtime = BridgeRuntime::<1, 0>::new(0);
        runtime.apply_diagnostics_event(RuntimeDiagnosticsEvent::BleNotifyTimedOut {
            critical_release: true,
        });

        assert_eq!(runtime.diagnostics.ble_notify_failure_count, 1);
        assert_eq!(runtime.counters.ble_notify_timeouts, 1);
        assert_eq!(runtime.counters.critical_release_failures, 1);
        assert_eq!(runtime.counters.ble_notify_dropped, 0);

        runtime.apply_diagnostics_event(RuntimeDiagnosticsEvent::BleNotifyFailed);
        assert_eq!(runtime.diagnostics.ble_notify_failure_count, 2);
        assert_eq!(runtime.counters.ble_notify_dropped, 1);
    }

    #[test]
    fn tick_pending_coalesces_duplicates_and_rearms_after_processing() {
        let pending = RuntimeTickPending::new();
        assert!(pending.try_mark_pending());
        assert!(pending.is_pending());
        for _ in 0..100 {
            assert!(!pending.try_mark_pending());
        }
        pending.mark_processed();
        assert!(pending.try_mark_pending());
    }

    #[test]
    fn runtime_owns_pairing_timeout_from_button_intent() {
        let mut runtime = BridgeRuntime::<2, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(2) }),
                &mut commands,
            )
            .unwrap();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::ButtonIntent {
                    intent: ButtonIntent::EnterPairingMode,
                    now_ms: 1_000,
                },
                &mut commands,
            )
            .unwrap();
        assert_eq!(
            runtime.pairing_mode(),
            Some(PairingModeState {
                host_id: HostId(2),
                deadline_ms: 61_000,
            })
        );
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::AllowPairing { host_id: HostId(2) })
        )));

        runtime
            .handle_input::<8, 8, 2>(RuntimeInput::Tick { now_ms: 60_999 }, &mut commands)
            .unwrap();
        assert!(commands.is_empty());
        assert!(runtime.pairing_mode().is_some());

        runtime
            .handle_input::<8, 8, 2>(RuntimeInput::Tick { now_ms: 61_000 }, &mut commands)
            .unwrap();
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::RejectPairing { host_id: HostId(2) })
        )));
        assert_eq!(runtime.pairing_mode(), None);
    }

    #[test]
    fn successful_bond_closes_runtime_pairing_window() {
        let mut runtime = BridgeRuntime::<2, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::EnterPairingMode { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::HostSecurityChanged {
                    host_id: HostId(1),
                    encrypted: true,
                    bonded: true,
                    bond: None,
                }),
                &mut commands,
            )
            .unwrap();

        assert_eq!(runtime.pairing_mode(), None);
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::RejectPairing { host_id: HostId(1) })
        )));
    }

    #[test]
    fn long_press_pairs_first_unregistered_host_without_replacing_active_bond() {
        let mut runtime = BridgeRuntime::<4, 1>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        let mut storage = StorageState::new(2);
        storage.last_active_host = Some(HostId(1));
        storage
            .push_host(StoredHostProfile {
                host_id: HostId(1),
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: false,
                consumer_cccd_enabled: false,
                keyboard_output_cccd_enabled: false,
                name: FixedName::empty(),
                bond: None,
            })
            .unwrap();
        runtime
            .restore_storage_state::<8>(&storage, &mut commands)
            .unwrap();

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::ButtonIntent {
                    intent: ButtonIntent::EnterPairingMode,
                    now_ms: 1_000,
                },
                &mut commands,
            )
            .unwrap();

        assert_eq!(
            runtime.pairing_mode(),
            Some(PairingModeState {
                host_id: HostId(2),
                deadline_ms: 61_000,
            })
        );
        assert!(commands.iter().any(|command| matches!(
            command,
            RuntimeCommand::BleCommand(BleTaskCommand::AllowPairing { host_id: HostId(2) })
        )));
    }

    #[test]
    fn command_classes_match_runtime_delivery_policy() {
        assert_eq!(
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(crate::reports::BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }
            .lane(),
            BleCommandLane::Notify
        );
        assert_eq!(
            BleTaskCommand::AllowPairing { host_id: HostId(1) }.lane(),
            BleCommandLane::Control
        );
        assert_eq!(
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(crate::reports::BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }
            .class(),
            CommandClass::Realtime
        );
        assert_eq!(
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(crate::reports::BleKeyboard6KroReport::release()),
                reason: NotifyReason::TargetSwitchRelease,
            }
            .class(),
            CommandClass::Critical
        );
        assert_eq!(
            BleTaskCommand::AllowPairing { host_id: HostId(1) }.class(),
            CommandClass::Critical
        );
        assert_eq!(
            StatusTaskCommand {
                status: BridgeStatus {
                    active_target: None,
                    pairable_host: None,
                },
                snapshot: StatusSnapshot::empty(),
                management: None,
            }
            .class(),
            CommandClass::BestEffort
        );
        assert_eq!(
            StatusTaskCommand {
                status: BridgeStatus {
                    active_target: None,
                    pairable_host: None,
                },
                snapshot: StatusSnapshot::empty(),
                management: Some(ManagementTaskResponse {
                    destination: ManagementDestination::Wired,
                    response: ManagementResponse {
                        request_id: 1,
                        result: ManagementResult::Ok,
                        payload: ManagementResponsePayload::Status(ManagementStatus::empty(4)),
                    },
                }),
            }
            .class(),
            CommandClass::Critical
        );
    }

    #[test]
    fn default_runtime_capacities_cover_worst_case_target_switch_commands() {
        assert_eq!(
            DEFAULT_RUNTIME_CAPACITIES,
            RuntimeCapacities {
                hosts: STORED_HOSTS_MAX,
                usb_interfaces: 8,
                bridge_actions: RUNTIME_BRIDGE_ACTION_CAPACITY,
                commands: RUNTIME_COMMAND_CAPACITY,
                ble_events: 2,
                input_queue: 16,
                ble_command_queue: RUNTIME_BLE_COMMAND_QUEUE_CAPACITY,
                usb_command_queue: RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
                storage_command_queue: RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
                status_command_queue: RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
            }
        );

        let mut runtime = DefaultBridgeRuntime::new(0);
        let mut commands = RuntimeCommandVec::new();

        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::HostConnected { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::HostSecurityChanged {
                    host_id: HostId(1),
                    encrypted: true,
                    bonded: true,
                    bond: None,
                }),
                &mut commands,
            )
            .unwrap();
        for report in [
            ReportKind::Keyboard,
            ReportKind::Mouse,
            ReportKind::Consumer,
            ReportKind::KeyboardOutput,
        ] {
            runtime
                .handle_default_input(
                    RuntimeInput::BridgeEvent(BridgeEvent::CccdChanged {
                        host_id: HostId(1),
                        report,
                        enabled: true,
                    }),
                    &mut commands,
                )
                .unwrap();
        }
        for id in 1..=RUNTIME_USB_INTERFACES_MAX as u8 {
            runtime
                .handle_default_input(
                    RuntimeInput::UsbHidInterfaceConnected {
                        interface_id: InterfaceId(id),
                        device_id: DeviceId(id),
                        led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
                    },
                    &mut commands,
                )
                .unwrap();
        }
        commands.clear();

        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(2) }),
                &mut commands,
            )
            .unwrap();

        assert_eq!(commands.len(), 14);
        assert!(matches!(
            commands[0],
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                reason: NotifyReason::TargetSwitchRelease,
                ..
            })
        ));
        assert!(matches!(
            commands[1],
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                reason: NotifyReason::TargetSwitchRelease,
                ..
            })
        ));
        assert!(matches!(
            commands[2],
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                reason: NotifyReason::TargetSwitchRelease,
                ..
            })
        ));
        assert!(matches!(
            commands[3],
            RuntimeCommand::BleCommand(BleTaskCommand::ActivateInput { host_id: HostId(2) })
        ));
        assert!(matches!(
            commands[4],
            RuntimeCommand::UsbKeyboardLedWrite { .. }
        ));
        assert!(
            commands[4..12]
                .iter()
                .all(|command| matches!(command, RuntimeCommand::UsbKeyboardLedWrite { .. }))
        );
        assert!(matches!(
            commands[12],
            RuntimeCommand::PersistStorage {
                priority: StoragePersistPriority::Lazy,
                ..
            }
        ));
        assert!(matches!(commands[13], RuntimeCommand::StatusChanged(_)));

        let mut queues = DefaultRuntimeCommandQueues::new();
        queues.dispatch_from(commands.as_slice()).unwrap();

        assert_eq!(queues.ble.len(), 4);
        assert_eq!(queues.usb.len(), RUNTIME_USB_INTERFACES_MAX);
        assert_eq!(queues.storage.len(), 1);
        assert_eq!(queues.status.len(), 1);
        assert_eq!(queues.usb[0].device_id, DeviceId(1));

        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::EnterPairingMode { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::ClearHost { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        assert_eq!(commands.len(), 15);
        let mut queues = DefaultRuntimeCommandQueues::new();
        queues.dispatch_from(commands.as_slice()).unwrap();
        assert_eq!(queues.ble.len(), 5);
        assert_eq!(queues.usb.len(), RUNTIME_USB_INTERFACES_MAX);
        assert_eq!(queues.storage.len(), 1);
        assert_eq!(queues.status.len(), 1);
    }

    #[test]
    fn status_snapshots_are_monotonic_latest_state_values() {
        let mut runtime = BridgeRuntime::<2, 2>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::HostConnected { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        let connected = commands
            .iter()
            .find_map(|command| match command {
                RuntimeCommand::StatusChanged(snapshot) => Some(*snapshot),
                _ => None,
            })
            .unwrap();
        assert_eq!(connected.connected_hosts, 1);

        runtime
            .handle_input::<8, 8, 2>(
                RuntimeInput::BridgeEvent(BridgeEvent::HostDisconnected { host_id: HostId(1) }),
                &mut commands,
            )
            .unwrap();
        let disconnected = commands
            .iter()
            .find_map(|command| match command {
                RuntimeCommand::StatusChanged(snapshot) => Some(*snapshot),
                _ => None,
            })
            .unwrap();
        assert!(disconnected.sequence > connected.sequence);
        assert_eq!(disconnected.connected_hosts, 0);
    }

    #[test]
    fn storage_queue_covers_real_host_pairing_and_cccd_burst() {
        let mut commands = RuntimeCommandVec::new();
        for generation in 1..=4 {
            commands
                .push(RuntimeCommand::PersistStorage {
                    state: StorageState::new(generation),
                    priority: StoragePersistPriority::Normal,
                })
                .unwrap();
        }

        let mut queues = DefaultRuntimeCommandQueues::new();
        queues.dispatch_from(commands.as_slice()).unwrap();

        assert_eq!(queues.storage.len(), 4);
        assert_eq!(queues.storage[3].state.generation, 4);
        assert_eq!(queues.storage[3].priority, StoragePersistPriority::Normal);
    }

    #[test]
    fn default_runtime_accepts_eight_usb_interfaces() {
        let mut runtime = DefaultBridgeRuntime::new(0);
        let mut commands = RuntimeCommandVec::new();

        for interface in 1..=RUNTIME_USB_INTERFACES_MAX as u8 {
            runtime
                .handle_default_input(
                    RuntimeInput::UsbHidInterfaceConnected {
                        interface_id: InterfaceId(interface),
                        device_id: DeviceId(interface),
                        led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
                    },
                    &mut commands,
                )
                .unwrap();
        }

        assert_eq!(
            runtime.handle_default_input(
                RuntimeInput::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(99),
                    device_id: DeviceId(99),
                    led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
                },
                &mut commands,
            ),
            Err(RuntimeError::UsbHidInterfaceCapacity)
        );
    }

    #[test]
    fn runtime_command_dispatch_reports_target_queue_capacity_errors() {
        let commands = [
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }),
            RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }),
        ];
        let mut queues = RuntimeCommandQueues::<1, 1, 1, 1>::new();

        assert_eq!(
            queues.dispatch_from(&commands),
            Err(RuntimeDispatchError::BleQueueCapacity)
        );
    }

    #[test]
    fn usb_led_command_rejects_reused_interface_for_a_different_device() {
        let command = UsbTaskCommand {
            interface_id: InterfaceId(3),
            device_id: DeviceId(7),
            bytes: KeyboardLedOutputReport::boot_keyboard()
                .build(crate::input::KeyboardLedState::empty())
                .unwrap(),
        };
        assert!(command.matches_target(InterfaceId(3), DeviceId(7)));
        assert!(!command.matches_target(InterfaceId(3), DeviceId(8)));
        assert!(!command.matches_target(InterfaceId(4), DeviceId(7)));
    }

    fn ready_runtime() -> BridgeRuntime<2, 1> {
        let mut runtime = BridgeRuntime::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();
        runtime
            .handle_event::<8, 8>(
                BridgeEvent::SwitchTarget { target: HostId(1) },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<8, 8>(
                BridgeEvent::HostConnected { host_id: HostId(1) },
                &mut commands,
            )
            .unwrap();
        runtime
            .handle_event::<8, 8>(
                BridgeEvent::HostSecurityChanged {
                    host_id: HostId(1),
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut commands,
            )
            .unwrap();
        for report in [
            ReportKind::Keyboard,
            ReportKind::Mouse,
            ReportKind::Consumer,
            ReportKind::KeyboardOutput,
        ] {
            runtime
                .handle_event::<8, 8>(
                    BridgeEvent::CccdChanged {
                        host_id: HostId(1),
                        report,
                        enabled: true,
                    },
                    &mut commands,
                )
                .unwrap();
        }
        runtime
    }

    fn keyboard_input(key: KeyUsage) -> crate::input::StandardInputFrame {
        let mut frame = KeyboardFrame::new(ModifierState::empty());
        frame.push_key(key).unwrap();
        crate::input::StandardInputFrame {
            device_id: DeviceId(1),
            interface_id: InterfaceId(1),
            keyboard: Some(frame),
            mouse: None,
            consumer: None,
        }
    }

    fn consumer_input(active: Option<u16>) -> crate::input::StandardInputFrame {
        crate::input::StandardInputFrame {
            device_id: DeviceId(1),
            interface_id: InterfaceId(2),
            keyboard: None,
            mouse: None,
            consumer: Some(crate::input::ConsumerFrame {
                active: active.map(crate::input::ConsumerUsage),
            }),
        }
    }
}
