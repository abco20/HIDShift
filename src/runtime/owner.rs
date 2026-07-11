use super::{
    BridgeRuntime, DEFAULT_RUNTIME_CAPACITIES, DefaultRuntimeCommandQueues,
    RUNTIME_BLE_COMMAND_QUEUE_CAPACITY, RUNTIME_BLE_EVENT_CAPACITY, RUNTIME_BRIDGE_ACTION_CAPACITY,
    RUNTIME_COMMAND_CAPACITY, RUNTIME_HOSTS_MAX, RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
    RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY, RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    RUNTIME_USB_INTERFACES_MAX, RuntimeCapacities, RuntimeCommand, RuntimeCommandQueues,
    RuntimeDispatchError, RuntimeError, RuntimeInput, message::RuntimeInputMessage,
};

pub type DefaultRuntimeOwner = RuntimeOwner<
    RUNTIME_HOSTS_MAX,
    RUNTIME_USB_INTERFACES_MAX,
    RUNTIME_COMMAND_CAPACITY,
    RUNTIME_BRIDGE_ACTION_CAPACITY,
    RUNTIME_BLE_EVENT_CAPACITY,
    RUNTIME_BLE_COMMAND_QUEUE_CAPACITY,
    RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
    RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeOwner<
    const HOSTS: usize,
    const USB_KEYBOARDS: usize,
    const COMMANDS: usize,
    const ACTIONS: usize,
    const EVENTS: usize,
    const BLE: usize,
    const USB: usize,
    const STORAGE: usize,
    const STATUS: usize,
> {
    runtime: BridgeRuntime<HOSTS, USB_KEYBOARDS>,
    commands: heapless::Vec<RuntimeCommand, COMMANDS>,
    queues: RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>,
}

impl<
    const HOSTS: usize,
    const USB_KEYBOARDS: usize,
    const COMMANDS: usize,
    const ACTIONS: usize,
    const EVENTS: usize,
    const BLE: usize,
    const USB: usize,
    const STORAGE: usize,
    const STATUS: usize,
> RuntimeOwner<HOSTS, USB_KEYBOARDS, COMMANDS, ACTIONS, EVENTS, BLE, USB, STORAGE, STATUS>
{
    pub const fn new(storage_generation: u32) -> Self {
        Self {
            runtime: BridgeRuntime::new(storage_generation),
            commands: heapless::Vec::new(),
            queues: RuntimeCommandQueues::new(),
        }
    }

    pub const fn from_runtime(runtime: BridgeRuntime<HOSTS, USB_KEYBOARDS>) -> Self {
        Self {
            runtime,
            commands: heapless::Vec::new(),
            queues: RuntimeCommandQueues::new(),
        }
    }

    pub const fn runtime(&self) -> &BridgeRuntime<HOSTS, USB_KEYBOARDS> {
        &self.runtime
    }

    pub const fn commands(&self) -> &heapless::Vec<RuntimeCommand, COMMANDS> {
        &self.commands
    }

    pub const fn queues(&self) -> &RuntimeCommandQueues<BLE, USB, STORAGE, STATUS> {
        &self.queues
    }

    pub fn process_input(
        &mut self,
        input: RuntimeInput<'_>,
    ) -> Result<&RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>, RuntimeOwnerError> {
        let mut next_runtime = self.runtime.clone();
        let mut next_commands = heapless::Vec::new();
        let mut next_queues = RuntimeCommandQueues::new();
        next_runtime
            .handle_input_in_place::<COMMANDS, ACTIONS, EVENTS>(input, &mut next_commands)?;
        next_queues.dispatch_from(next_commands.as_slice())?;
        next_runtime.observe_outbox_usage(
            next_queues.ble.len(),
            next_queues.usb.len(),
            next_queues.storage.len(),
            next_queues.status.len(),
        );
        self.runtime = next_runtime;
        self.commands = next_commands;
        self.queues = next_queues;
        Ok(&self.queues)
    }

    pub fn process_message(
        &mut self,
        message: &RuntimeInputMessage,
    ) -> Result<&RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>, RuntimeOwnerError> {
        self.process_input(message.as_runtime_input())
    }

    pub fn staged_message(&self, message: &RuntimeInputMessage) -> Result<Self, RuntimeOwnerError> {
        let mut next = self.clone();
        next.process_message_in_place(message)?;
        Ok(next)
    }

    fn process_message_in_place(
        &mut self,
        message: &RuntimeInputMessage,
    ) -> Result<(), RuntimeOwnerError> {
        let mut next_commands = heapless::Vec::new();
        let mut next_queues = RuntimeCommandQueues::new();
        self.runtime
            .handle_input_in_place::<COMMANDS, ACTIONS, EVENTS>(
                message.as_runtime_input(),
                &mut next_commands,
            )?;
        next_queues.dispatch_from(next_commands.as_slice())?;
        self.runtime.observe_outbox_usage(
            next_queues.ble.len(),
            next_queues.usb.len(),
            next_queues.storage.len(),
            next_queues.status.len(),
        );
        self.commands = next_commands;
        self.queues = next_queues;
        Ok(())
    }

    pub fn mark_host_disconnected_for_quiesce(&mut self, host_id: crate::ids::HostId) {
        self.runtime.mark_host_disconnected_for_quiesce(host_id);
    }

    pub fn prepare_for_quiesce(&mut self) -> Result<(), RuntimeOwnerError> {
        self.runtime.prepare_for_quiesce()?;
        Ok(())
    }

    pub fn observe_transport_metrics(
        &mut self,
        runtime_input_depth: usize,
        mouse: crate::mouse_accumulator::MouseAccumulatorStats,
        status_updates_dropped: u32,
    ) {
        self.runtime
            .observe_transport_metrics(runtime_input_depth, mouse, status_updates_dropped);
    }

    pub fn into_inner(self) -> BridgeRuntime<HOSTS, USB_KEYBOARDS> {
        self.runtime
    }
}

impl DefaultRuntimeOwner {
    pub fn default_capacities(&self) -> RuntimeCapacities {
        DEFAULT_RUNTIME_CAPACITIES
    }

    pub fn default_queues(&self) -> &DefaultRuntimeCommandQueues {
        &self.queues
    }
}

impl<
    const HOSTS: usize,
    const USB_KEYBOARDS: usize,
    const COMMANDS: usize,
    const ACTIONS: usize,
    const EVENTS: usize,
    const BLE: usize,
    const USB: usize,
    const STORAGE: usize,
    const STATUS: usize,
> Default
    for RuntimeOwner<HOSTS, USB_KEYBOARDS, COMMANDS, ACTIONS, EVENTS, BLE, USB, STORAGE, STATUS>
{
    fn default() -> Self {
        Self::new(0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeOwnerError {
    Runtime(RuntimeError),
    Dispatch(RuntimeDispatchError),
}

impl From<RuntimeError> for RuntimeOwnerError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<RuntimeDispatchError> for RuntimeOwnerError {
    fn from(error: RuntimeDispatchError) -> Self {
        Self::Dispatch(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble::BleHidAttribute;
    use crate::bridge::{BridgeError, BridgeEvent, BridgeStatus};
    use crate::ids::{DeviceId, HostId, InterfaceId};
    use crate::input::KeyboardLedState;
    use crate::reports::ReportKind;
    use crate::storage::StorageState;
    use crate::usb_hid::output::KeyboardLedOutputReport;

    type TestOwner = RuntimeOwner<2, 1, 8, 8, 2, 8, 8, 2, 8>;

    #[test]
    fn owner_processes_runtime_input_into_task_outboxes() {
        let mut owner = ready_owner();

        let queues = owner
            .process_input(RuntimeInput::UsbHidInterfaceConnected {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
            })
            .unwrap();

        assert_eq!(queues.usb.len(), 1);
        assert_eq!(queues.usb[0].device_id, DeviceId(7));
        assert_eq!(queues.ble.len(), 0);
        assert_eq!(queues.storage.len(), 0);
        assert_eq!(queues.status.len(), 0);
    }

    #[test]
    fn owner_clears_stale_outboxes_between_inputs() {
        let mut owner = TestOwner::new(0);

        let queues = owner
            .process_input(RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget {
                target: HostId(1),
            }))
            .unwrap();
        assert_eq!(queues.storage.len(), 1);
        assert_eq!(queues.status.len(), 1);

        let queues = owner
            .process_input(RuntimeInput::BridgeEvent(BridgeEvent::HostConnected {
                host_id: HostId(1),
            }))
            .unwrap();
        assert_eq!(queues.storage.len(), 0);
        assert_eq!(queues.status.len(), 1);
        assert_eq!(
            queues.status[0].status,
            BridgeStatus {
                active_target: Some(HostId(1)),
                pairable_host: None,
            }
        );
    }

    #[test]
    fn owner_reports_dispatch_errors_at_task_boundary() {
        let mut owner = RuntimeOwner::<2, 1, 8, 8, 2, 8, 8, 0, 8>::new(0);
        let before = owner.clone();

        let err = owner
            .process_input(RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget {
                target: HostId(1),
            }))
            .unwrap_err();

        assert_eq!(
            err,
            RuntimeOwnerError::Dispatch(RuntimeDispatchError::StorageQueueCapacity)
        );
        assert_eq!(owner, before);
    }

    #[test]
    fn action_and_command_capacity_errors_roll_back_the_owner() {
        let mut action_limited = RuntimeOwner::<2, 1, 16, 0, 2, 16, 16, 16, 16>::new(0);
        let before = action_limited.clone();
        assert!(matches!(
            action_limited.process_input(RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget {
                target: HostId(1)
            })),
            Err(RuntimeOwnerError::Runtime(RuntimeError::Bridge(
                BridgeError::ActionCapacity
            )))
        ));
        assert_eq!(action_limited, before);

        let mut command_limited = RuntimeOwner::<2, 1, 0, 16, 2, 16, 16, 16, 16>::new(0);
        let before = command_limited.clone();
        assert_eq!(
            command_limited.process_input(RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget {
                target: HostId(1)
            })),
            Err(RuntimeOwnerError::Runtime(RuntimeError::CommandCapacity))
        );
        assert_eq!(command_limited, before);
    }

    #[test]
    fn every_task_outbox_capacity_error_rolls_back_runtime_state() {
        let mut status_limited = RuntimeOwner::<2, 1, 16, 16, 2, 16, 16, 16, 0>::new(0);
        let before = status_limited.clone();
        assert!(matches!(
            status_limited.process_input(RuntimeInput::BridgeEvent(BridgeEvent::HostConnected {
                host_id: HostId(1)
            })),
            Err(RuntimeOwnerError::Dispatch(
                RuntimeDispatchError::StatusQueueCapacity
            ))
        ));
        assert_eq!(status_limited, before);

        let mut usb_limited = RuntimeOwner::<2, 1, 16, 16, 2, 16, 0, 16, 16>::new(0);
        usb_limited
            .process_input(RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget {
                target: HostId(1),
            }))
            .unwrap();
        let before = usb_limited.clone();
        assert!(matches!(
            usb_limited.process_input(RuntimeInput::UsbHidInterfaceConnected {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
            }),
            Err(RuntimeOwnerError::Dispatch(
                RuntimeDispatchError::UsbQueueCapacity
            ))
        ));
        assert_eq!(usb_limited, before);

        let mut setup = RuntimeOwner::<2, 1, 16, 16, 2, 16, 16, 16, 16>::new(0);
        for event in [
            BridgeEvent::SwitchTarget { target: HostId(1) },
            BridgeEvent::HostConnected { host_id: HostId(1) },
            BridgeEvent::HostSecurityChanged {
                host_id: HostId(1),
                encrypted: true,
                bonded: true,
                bond: None,
            },
            BridgeEvent::CccdChanged {
                host_id: HostId(1),
                report: ReportKind::Keyboard,
                enabled: true,
            },
        ] {
            setup
                .process_input(RuntimeInput::BridgeEvent(event))
                .unwrap();
        }
        let mut ble_limited =
            RuntimeOwner::<2, 1, 16, 16, 2, 0, 16, 16, 16>::from_runtime(setup.into_inner());
        let before = ble_limited.clone();
        let mut keyboard = crate::input::KeyboardFrame::new(crate::input::ModifierState::empty());
        keyboard.push_key(crate::input::KeyUsage(4)).unwrap();
        assert!(matches!(
            ble_limited.process_input(RuntimeInput::BridgeEvent(BridgeEvent::InputFrame(
                crate::input::InputFrame::Standard(crate::input::StandardInputFrame {
                    device_id: DeviceId(7),
                    keyboard: Some(keyboard),
                    mouse: None,
                    consumer: None,
                })
            ))),
            Err(RuntimeOwnerError::Dispatch(
                RuntimeDispatchError::BleQueueCapacity
            ))
        ));
        assert_eq!(ble_limited, before);
    }

    #[test]
    fn quiesce_disconnect_only_clears_ble_session_state() {
        let mut owner = TestOwner::new(0);

        owner
            .process_input(RuntimeInput::BridgeEvent(BridgeEvent::HostConnected {
                host_id: HostId(1),
            }))
            .unwrap();
        assert!(
            owner
                .runtime()
                .bridge()
                .state()
                .hosts
                .host(HostId(1))
                .unwrap()
                .connected
        );
        let commands = owner.commands().clone();
        let queues = owner.queues().clone();

        owner.mark_host_disconnected_for_quiesce(HostId(1));
        assert!(
            !owner
                .runtime()
                .bridge()
                .state()
                .hosts
                .host(HostId(1))
                .unwrap()
                .connected
        );
        assert_eq!(owner.commands(), &commands);
        assert_eq!(owner.queues(), &queues);
    }

    #[test]
    fn owner_keeps_ble_gatt_adaptation_out_of_firmware_tasks() {
        let mut owner = ready_owner_with_usb();

        let queues = owner
            .process_message(&RuntimeInputMessage::BleHostEvent {
                host_id: HostId(1),
                event: crate::runtime::message::RuntimeBleHostEvent::GattWrite {
                    attribute: BleHidAttribute::BootKeyboardOutputReport,
                    data: crate::runtime::message::RuntimeBleGattWrite::from_slice(&[0b0000_0010])
                        .unwrap(),
                },
            })
            .unwrap();

        assert_eq!(queues.usb.len(), 1);
        assert_eq!(
            queues.usb[0].bytes,
            KeyboardLedOutputReport::boot_keyboard()
                .build(KeyboardLedState::CAPS_LOCK)
                .unwrap()
        );
        assert_eq!(queues.ble.len(), 0);
        assert_eq!(queues.storage.len(), 0);
    }

    #[test]
    fn owner_accepts_owned_storage_restore_message() {
        let mut owner = TestOwner::new(0);
        let mut storage = StorageState::new(22);
        storage.last_active_host = Some(HostId(2));

        let queues = owner
            .process_message(&RuntimeInputMessage::RestoreStorage(storage))
            .unwrap();

        assert_eq!(queues.ble.len(), 0);
        assert_eq!(queues.usb.len(), 0);
        assert_eq!(queues.storage.len(), 0);
        assert_eq!(queues.status.len(), 0);
        assert_eq!(owner.runtime().storage_generation(), 22);
        assert_eq!(
            owner.runtime().bridge().state().hosts.active_target(),
            Some(HostId(2))
        );
    }

    fn ready_owner() -> TestOwner {
        let mut owner = TestOwner::new(0);
        owner
            .process_input(RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget {
                target: HostId(1),
            }))
            .unwrap();
        owner
            .process_input(RuntimeInput::BridgeEvent(BridgeEvent::HostConnected {
                host_id: HostId(1),
            }))
            .unwrap();
        owner
            .process_input(RuntimeInput::BridgeEvent(
                BridgeEvent::HostSecurityChanged {
                    host_id: HostId(1),
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
            ))
            .unwrap();
        for report in [
            ReportKind::Keyboard,
            ReportKind::Mouse,
            ReportKind::Consumer,
            ReportKind::KeyboardOutput,
        ] {
            owner
                .process_input(RuntimeInput::BridgeEvent(BridgeEvent::CccdChanged {
                    host_id: HostId(1),
                    report,
                    enabled: true,
                }))
                .unwrap();
        }
        owner
    }

    fn ready_owner_with_usb() -> TestOwner {
        let mut owner = ready_owner();
        owner
            .process_input(RuntimeInput::UsbHidInterfaceConnected {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
            })
            .unwrap();
        owner
    }
}
