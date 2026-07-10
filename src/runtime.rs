use crate::ble::{BleHostAdapterError, BleHostAdapterEvent, bridge_events_from_ble_host_event};
use crate::bridge::{Bridge, BridgeAction, BridgeError, BridgeEvent, BridgeStatus, NotifyReason};
use crate::ids::{DeviceId, HostId, InterfaceId};
use crate::reports::BleHidReport;
use crate::storage::{
    STORED_HOSTS_MAX, StorageError, StoragePersistPriority, StorageState, StoredBond,
};
use crate::target_control::ButtonIntent;
use crate::usb_hid::output::{
    KeyboardLedOutputBytes, KeyboardLedOutputError, KeyboardLedOutputReport,
};

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
pub const RUNTIME_BRIDGE_ACTION_CAPACITY: usize = 12;
pub const RUNTIME_COMMAND_CAPACITY: usize = 12;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeInput<'a> {
    BridgeEvent(BridgeEvent),
    ButtonIntent {
        intent: ButtonIntent,
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
    RestoreStorage(&'a StorageState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeRuntime<const HOSTS: usize, const USB_INTERFACES: usize> {
    bridge: Bridge<HOSTS>,
    usb_interfaces: [Option<UsbHidInterfaceRuntimeState>; USB_INTERFACES],
    storage_generation: u32,
    pairing_mode: Option<PairingModeState>,
}

impl<const HOSTS: usize, const USB_INTERFACES: usize> BridgeRuntime<HOSTS, USB_INTERFACES> {
    pub const fn new(storage_generation: u32) -> Self {
        Self {
            bridge: Bridge::new(),
            usb_interfaces: [None; USB_INTERFACES],
            storage_generation,
            pairing_mode: None,
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

    pub fn restore_storage_state<const COMMANDS: usize>(
        &mut self,
        storage: &StorageState,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        let mut actions = heapless::Vec::<BridgeAction, 1>::new();
        self.bridge.restore_storage_state(storage, &mut actions)?;
        self.storage_generation = storage.generation;
        self.pairing_mode = None;
        Ok(())
    }

    pub fn handle_input<const COMMANDS: usize, const ACTIONS: usize, const EVENTS: usize>(
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
            RuntimeInput::Tick { now_ms } => {
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
        self.upsert_usb_hid_interface(interface_id, device_id, led_output)?;
        self.handle_event::<COMMANDS, ACTIONS>(
            BridgeEvent::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                keyboard_led_sink: led_output.is_some(),
            },
            commands,
        )
    }

    pub fn unregister_usb_device<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        interface_id: InterfaceId,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        if let Some(index) = self.usb_hid_interface_index(interface_id) {
            self.usb_interfaces[index] = None;
        }
        self.handle_event::<COMMANDS, ACTIONS>(
            BridgeEvent::UsbDeviceRemoved { interface_id },
            commands,
        )
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
        match intent {
            ButtonIntent::NextConnectedTarget => {
                let Some(target) = self.bridge.state().hosts.next_connected_target() else {
                    commands.clear();
                    return Ok(());
                };
                self.handle_bridge_event::<COMMANDS, ACTIONS>(
                    BridgeEvent::SwitchTarget { target },
                    commands,
                )
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

    fn handle_tick<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        let Some(pairing_mode) = self.pairing_mode else {
            commands.clear();
            return Ok(());
        };
        if now_ms < pairing_mode.deadline_ms {
            commands.clear();
            return Ok(());
        }
        self.pairing_mode = None;
        self.handle_bridge_event::<COMMANDS, ACTIONS>(
            BridgeEvent::PairingModeExpired {
                host_id: pairing_mode.host_id,
            },
            commands,
        )
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
        self.bridge.handle_event(event, &mut actions)?;

        for action in actions {
            self.dispatch_bridge_action(action, persist_priority, commands)?;
        }

        Ok(())
    }

    fn observe_bridge_event(&mut self, event: &BridgeEvent) {
        match event {
            BridgeEvent::EnterPairingMode { host_id } => {
                if self.pairing_mode.map(|state| state.host_id) != Some(*host_id) {
                    self.pairing_mode = Some(PairingModeState {
                        host_id: *host_id,
                        deadline_ms: self
                            .pairing_mode
                            .map(|state| state.deadline_ms)
                            .unwrap_or(PAIRING_MODE_TIMEOUT_MS),
                    });
                }
            }
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
            } => push_command(
                commands,
                RuntimeCommand::BleCommand(BleTaskCommand::Notify {
                    host_id,
                    report,
                    reason,
                }),
            ),
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
            BridgeAction::PersistProfiles => {
                self.storage_generation = self.storage_generation.wrapping_add(1);
                let state = self.bridge.storage_state(self.storage_generation)?;
                push_command(
                    commands,
                    RuntimeCommand::PersistStorage {
                        state,
                        priority: persist_priority.unwrap_or(StoragePersistPriority::Normal),
                    },
                )
            }
            BridgeAction::StatusChanged(status) => {
                if status.pairable_host.is_none() {
                    self.pairing_mode = None;
                }
                push_command(commands, RuntimeCommand::StatusChanged(status))
            }
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
}

pub const PAIRING_MODE_TIMEOUT_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingModeState {
    pub host_id: HostId,
    pub deadline_ms: u64,
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
    StatusChanged(BridgeStatus),
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
}

impl BleTaskCommand {
    pub const fn lane(self) -> BleCommandLane {
        match self {
            Self::Notify { .. } => BleCommandLane::Notify,
            Self::AllowPairing { .. } | Self::RejectPairing { .. } | Self::ClearBond { .. } => {
                BleCommandLane::Control
            }
        }
    }

    pub const fn class(self) -> CommandClass {
        match self {
            Self::Notify { reason, .. } => match reason {
                NotifyReason::Input
                | NotifyReason::TargetSwitchRelease
                | NotifyReason::UsbDeviceRemovedRelease
                | NotifyReason::SafetyRelease => CommandClass::Critical,
            },
            Self::AllowPairing { .. } | Self::RejectPairing { .. } | Self::ClearBond { .. } => {
                CommandClass::Critical
            }
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
}

impl StatusTaskCommand {
    pub const fn class(self) -> CommandClass {
        CommandClass::BestEffort
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDispatchError {
    BleQueueCapacity,
    UsbQueueCapacity,
    StorageQueueCapacity,
    StatusQueueCapacity,
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
        }
    }

    pub fn clear(&mut self) {
        self.ble.clear();
        self.usb.clear();
        self.storage.clear();
        self.status.clear();
    }

    pub fn dispatch_from(
        &mut self,
        commands: &[RuntimeCommand],
    ) -> Result<(), RuntimeDispatchError> {
        self.clear();
        for command in commands {
            self.dispatch_one(command)?;
        }
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
                .push(StatusTaskCommand { status: *status })
                .map_err(|_| RuntimeDispatchError::StatusQueueCapacity),
        }
    }
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
        BridgeEvent::HostSecurityChanged { .. } | BridgeEvent::ClearHost { .. } => {
            Some(StoragePersistPriority::Critical)
        }
        BridgeEvent::CccdChanged { .. } => Some(StoragePersistPriority::Normal),
        BridgeEvent::SwitchTarget { .. } => Some(StoragePersistPriority::Lazy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble::{BleHidAttribute, BleHostAdapterEvent};
    use crate::input::{KeyUsage, KeyboardFrame, KeyboardLedState, ModifierState};
    use crate::reports::{BleKeyboard6KroReport, ReportKind};
    use crate::storage::{FixedName, StoredHostProfile};

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
        assert!(commands.is_empty());
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
                RuntimeCommand::StatusChanged(BridgeStatus {
                    active_target: Some(HostId(2)),
                    pairable_host: None,
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

        assert!(commands.iter().any(|command| {
            matches!(
                command,
                RuntimeCommand::StatusChanged(BridgeStatus {
                    active_target: Some(HostId(3)),
                    ..
                })
            )
        }));
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
            CommandClass::Critical
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
            }
            .class(),
            CommandClass::BestEffort
        );
    }

    #[test]
    fn default_runtime_capacities_cover_worst_case_target_switch_commands() {
        assert_eq!(
            DEFAULT_RUNTIME_CAPACITIES,
            RuntimeCapacities {
                hosts: STORED_HOSTS_MAX,
                usb_interfaces: 8,
                bridge_actions: 12,
                commands: 12,
                ble_events: 2,
                input_queue: 16,
                ble_command_queue: 12,
                usb_command_queue: 12,
                storage_command_queue: 12,
                status_command_queue: 12,
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
        runtime
            .handle_default_input(
                RuntimeInput::UsbHidInterfaceConnected {
                    interface_id: InterfaceId(1),
                    device_id: DeviceId(7),
                    led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
                },
                &mut commands,
            )
            .unwrap();
        commands.clear();

        runtime
            .handle_default_input(
                RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(2) }),
                &mut commands,
            )
            .unwrap();

        assert_eq!(commands.len(), 6);
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
            RuntimeCommand::UsbKeyboardLedWrite { .. }
        ));
        assert!(matches!(
            commands[4],
            RuntimeCommand::PersistStorage {
                priority: StoragePersistPriority::Lazy,
                ..
            }
        ));
        assert!(matches!(commands[5], RuntimeCommand::StatusChanged(_)));

        let mut queues = DefaultRuntimeCommandQueues::new();
        queues.dispatch_from(commands.as_slice()).unwrap();

        assert_eq!(queues.ble.len(), 3);
        assert_eq!(queues.usb.len(), 1);
        assert_eq!(queues.storage.len(), 1);
        assert_eq!(queues.status.len(), 1);
        assert_eq!(queues.usb[0].device_id, DeviceId(7));
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
            keyboard: Some(frame),
            mouse: None,
            consumer: None,
        }
    }
}
