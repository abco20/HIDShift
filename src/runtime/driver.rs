use super::message::RuntimeInputMessage;
use super::owner::{RuntimeOwner, RuntimeOwnerError};
use super::{
    BleTaskCommand, RuntimeCommandQueues, StatusTaskCommand, StorageTaskCommand, UsbTaskCommand,
};

pub trait RuntimeTaskSink {
    type Error;

    fn send_ble(&mut self, command: BleTaskCommand) -> Result<(), Self::Error>;
    fn send_usb(&mut self, command: UsbTaskCommand) -> Result<(), Self::Error>;
    fn send_storage(&mut self, command: StorageTaskCommand) -> Result<(), Self::Error>;
    fn send_status(&mut self, command: StatusTaskCommand) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTaskKind {
    Ble,
    Usb,
    Storage,
    Status,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDriverError<E> {
    Owner(RuntimeOwnerError),
    Sink { task: RuntimeTaskKind, error: E },
}

impl<E> From<RuntimeOwnerError> for RuntimeDriverError<E> {
    fn from(error: RuntimeOwnerError) -> Self {
        Self::Owner(error)
    }
}

pub fn drive_runtime_message<
    S,
    const HOSTS: usize,
    const USB_KEYBOARDS: usize,
    const COMMANDS: usize,
    const ACTIONS: usize,
    const EVENTS: usize,
    const BLE: usize,
    const USB: usize,
    const STORAGE: usize,
    const STATUS: usize,
>(
    owner: &mut RuntimeOwner<
        HOSTS,
        USB_KEYBOARDS,
        COMMANDS,
        ACTIONS,
        EVENTS,
        BLE,
        USB,
        STORAGE,
        STATUS,
    >,
    message: &RuntimeInputMessage,
    sink: &mut S,
) -> Result<(), RuntimeDriverError<S::Error>>
where
    S: RuntimeTaskSink,
{
    let queues = owner.process_message(message)?;
    dispatch_runtime_queues(queues, sink)
}

pub fn dispatch_runtime_queues<
    S,
    const BLE: usize,
    const USB: usize,
    const STORAGE: usize,
    const STATUS: usize,
>(
    queues: &RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>,
    sink: &mut S,
) -> Result<(), RuntimeDriverError<S::Error>>
where
    S: RuntimeTaskSink,
{
    for command in queues.ble.iter().copied() {
        sink.send_ble(command)
            .map_err(|error| RuntimeDriverError::Sink {
                task: RuntimeTaskKind::Ble,
                error,
            })?;
    }
    for command in queues.usb.iter().copied() {
        sink.send_usb(command)
            .map_err(|error| RuntimeDriverError::Sink {
                task: RuntimeTaskKind::Usb,
                error,
            })?;
    }
    for command in queues.storage.iter().cloned() {
        sink.send_storage(command)
            .map_err(|error| RuntimeDriverError::Sink {
                task: RuntimeTaskKind::Storage,
                error,
            })?;
    }
    for command in queues.status.iter().copied() {
        sink.send_status(command)
            .map_err(|error| RuntimeDriverError::Sink {
                task: RuntimeTaskKind::Status,
                error,
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{BridgeEvent, NotifyReason};
    use crate::ids::{DeviceId, HostId, InterfaceId};
    use crate::input::{InputFrame, KeyUsage, KeyboardFrame, ModifierState, StandardInputFrame};
    use crate::reports::{BleHidReport, ReportKind};
    use crate::runtime::message::RuntimeBleGattWrite;
    use crate::runtime::message::RuntimeBleHostEvent;
    use crate::usb_hid::output::KeyboardLedOutputReport;

    type TestOwner = RuntimeOwner<2, 1, 8, 8, 2, 8, 8, 2, 8>;

    #[test]
    fn drive_runtime_message_dispatches_owner_outboxes_to_sink() {
        let mut owner = ready_owner();
        let mut sink = RecordingSink::default();

        drive_runtime_message(
            &mut owner,
            &RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(InputFrame::Standard(
                keyboard_input(DeviceId(7), KeyUsage(0x04)),
            ))),
            &mut sink,
        )
        .unwrap();

        assert_eq!(sink.ble.len(), 1);
        assert_eq!(sink.usb.len(), 0);
        assert_eq!(sink.storage.len(), 0);
        assert_eq!(sink.status.len(), 0);
        assert!(matches!(
            sink.ble[0],
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(_),
                reason: NotifyReason::Input,
            }
        ));
    }

    #[test]
    fn drive_runtime_message_surfaces_sink_task_failures() {
        let mut owner = ready_owner();
        let mut sink = RecordingSink {
            fail_task: Some(RuntimeTaskKind::Usb),
            ..RecordingSink::default()
        };

        let err = drive_runtime_message(
            &mut owner,
            &RuntimeInputMessage::UsbHidInterfaceConnected {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
            },
            &mut sink,
        )
        .unwrap_err();

        assert_eq!(
            err,
            RuntimeDriverError::Sink {
                task: RuntimeTaskKind::Usb,
                error: SinkError::Rejected,
            }
        );
    }

    #[test]
    fn drive_runtime_message_accepts_owned_ble_message_boundary() {
        let mut owner = ready_owner_with_usb();
        let mut sink = RecordingSink::default();

        drive_runtime_message(
            &mut owner,
            &RuntimeInputMessage::BleHostEvent {
                host_id: HostId(1),
                event: RuntimeBleHostEvent::GattWrite {
                    attribute: crate::ble::BleHidAttribute::BootKeyboardOutputReport,
                    data: RuntimeBleGattWrite::from_slice(&[0b0000_0010]).unwrap(),
                },
            },
            &mut sink,
        )
        .unwrap();

        assert_eq!(sink.ble.len(), 0);
        assert_eq!(sink.usb.len(), 1);
        assert_eq!(sink.storage.len(), 0);
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum SinkError {
        Rejected,
    }

    #[derive(Default)]
    struct RecordingSink {
        ble: heapless::Vec<BleTaskCommand, 8>,
        usb: heapless::Vec<UsbTaskCommand, 8>,
        storage: heapless::Vec<StorageTaskCommand, 8>,
        status: heapless::Vec<StatusTaskCommand, 8>,
        fail_task: Option<RuntimeTaskKind>,
    }

    impl RuntimeTaskSink for RecordingSink {
        type Error = SinkError;

        fn send_ble(&mut self, command: BleTaskCommand) -> Result<(), Self::Error> {
            if self.fail_task == Some(RuntimeTaskKind::Ble) {
                return Err(SinkError::Rejected);
            }
            self.ble.push(command).unwrap();
            Ok(())
        }

        fn send_usb(&mut self, command: UsbTaskCommand) -> Result<(), Self::Error> {
            if self.fail_task == Some(RuntimeTaskKind::Usb) {
                return Err(SinkError::Rejected);
            }
            self.usb.push(command).unwrap();
            Ok(())
        }

        fn send_storage(&mut self, command: StorageTaskCommand) -> Result<(), Self::Error> {
            if self.fail_task == Some(RuntimeTaskKind::Storage) {
                return Err(SinkError::Rejected);
            }
            self.storage.push(command).unwrap();
            Ok(())
        }

        fn send_status(&mut self, command: StatusTaskCommand) -> Result<(), Self::Error> {
            if self.fail_task == Some(RuntimeTaskKind::Status) {
                return Err(SinkError::Rejected);
            }
            self.status.push(command).unwrap();
            Ok(())
        }
    }

    fn ready_owner() -> TestOwner {
        let mut owner = TestOwner::new(0);
        let mut sink = RecordingSink::default();
        for message in [
            RuntimeInputMessage::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(1) }),
            RuntimeInputMessage::BridgeEvent(BridgeEvent::HostConnected { host_id: HostId(1) }),
            RuntimeInputMessage::BridgeEvent(BridgeEvent::HostSecurityChanged {
                host_id: HostId(1),
                encrypted: true,
                bonded: true,
                bond: None,
            }),
        ] {
            drive_runtime_message(&mut owner, &message, &mut sink).unwrap();
        }
        for report in [
            ReportKind::Keyboard,
            ReportKind::Mouse,
            ReportKind::Consumer,
            ReportKind::KeyboardOutput,
        ] {
            drive_runtime_message(
                &mut owner,
                &RuntimeInputMessage::BridgeEvent(BridgeEvent::CccdChanged {
                    host_id: HostId(1),
                    report,
                    enabled: true,
                }),
                &mut sink,
            )
            .unwrap();
        }
        owner
    }

    fn ready_owner_with_usb() -> TestOwner {
        let mut owner = ready_owner();
        let mut sink = RecordingSink::default();
        drive_runtime_message(
            &mut owner,
            &RuntimeInputMessage::UsbHidInterfaceConnected {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                led_output: Some(KeyboardLedOutputReport::boot_keyboard()),
            },
            &mut sink,
        )
        .unwrap();
        owner
    }

    fn keyboard_input(device_id: DeviceId, key: KeyUsage) -> StandardInputFrame {
        let mut frame = KeyboardFrame::new(ModifierState::empty());
        frame.push_key(key).unwrap();
        StandardInputFrame {
            device_id,
            keyboard: Some(frame),
            mouse: None,
            consumer: None,
        }
    }
}
