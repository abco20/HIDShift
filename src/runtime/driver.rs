#[cfg(feature = "dual-s3-wired")]
use super::DeviceTaskCommand;
use super::message::RuntimeInputMessage;
use super::owner::{RuntimeOwner, RuntimeOwnerError};
use super::{
    BleTaskCommand, RuntimeCommandQueues, StatusTaskCommand, StorageTaskCommand, UsbHostTaskCommand,
};

pub trait RuntimeTaskSink {
    type Error;

    fn reserve_batch<
        const BLE: usize,
        const USB: usize,
        const STORAGE: usize,
        const STATUS: usize,
    >(
        &mut self,
        queues: &RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>,
    ) -> Result<(), (RuntimeTaskKind, Self::Error)>;

    fn send_ble(&mut self, command: BleTaskCommand) -> Result<(), Self::Error>;
    #[cfg(feature = "dual-s3-wired")]
    fn send_device(&mut self, command: DeviceTaskCommand) -> Result<(), Self::Error>;
    fn send_usb_host(&mut self, command: UsbHostTaskCommand) -> Result<(), Self::Error>;
    fn send_storage(&mut self, command: StorageTaskCommand) -> Result<(), Self::Error>;
    fn send_status(&mut self, command: StatusTaskCommand) -> Result<(), Self::Error>;
    fn apply_effect(&mut self, effect: super::RuntimeEffect);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTaskKind {
    Ble,
    #[cfg(feature = "dual-s3-wired")]
    Device,
    UsbHost,
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
    let next_owner = owner.staged_message(message)?;
    let queues = next_owner.queues();
    sink.reserve_batch(queues)
        .map_err(|(task, error)| RuntimeDriverError::Sink { task, error })?;
    dispatch_runtime_queues(queues, sink)?;
    *owner = next_owner;
    for effect in owner.queues().effects.iter().copied() {
        sink.apply_effect(effect);
    }
    Ok(())
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
    #[cfg(feature = "dual-s3-wired")]
    for command in queues.device.iter().copied() {
        sink.send_device(command)
            .map_err(|error| RuntimeDriverError::Sink {
                task: RuntimeTaskKind::Device,
                error,
            })?;
    }
    for command in queues.usb_host.iter().copied() {
        sink.send_usb_host(command)
            .map_err(|error| RuntimeDriverError::Sink {
                task: RuntimeTaskKind::UsbHost,
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
    use crate::runtime::RuntimeEffect;
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

    #[cfg(feature = "dual-s3-wired")]
    #[test]
    fn drive_runtime_message_routes_wired_input_only_to_device_sink() {
        let mut owner = ready_owner();
        let mut sink = RecordingSink::default();
        for message in [
            RuntimeInputMessage::BridgeEvent(BridgeEvent::SelectOutputTarget {
                target: crate::output_target::OutputTarget::Wired,
            }),
            RuntimeInputMessage::DeviceUsbState(crate::interchip::UsbState {
                attached: true,
                configured: true,
                fallback_active: true,
                healthy: true,
                active_profile_hash: 0,
                error_code: 0,
            }),
        ] {
            drive_runtime_message(&mut owner, &message, &mut sink).unwrap();
        }
        sink.ble.clear();
        sink.device.clear();

        drive_runtime_message(
            &mut owner,
            &RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(InputFrame::Standard(
                keyboard_input(DeviceId(7), KeyUsage(0x04)),
            ))),
            &mut sink,
        )
        .unwrap();

        assert!(sink.ble.is_empty());
        assert!(matches!(
            sink.device.as_slice(),
            [DeviceTaskCommand::StandardReport {
                report: crate::reports::StandardHidReport::Keyboard(report),
                reason: NotifyReason::Input,
            }] if report.as_bytes()[2] == 0x04
        ));
    }

    #[test]
    fn drive_runtime_message_surfaces_sink_task_failures() {
        let mut owner = ready_owner();
        let mut sink = RecordingSink {
            fail_task: Some(RuntimeTaskKind::UsbHost),
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
                task: RuntimeTaskKind::UsbHost,
                error: SinkError::Rejected,
            }
        );
    }

    #[test]
    fn batch_reservation_prevents_ble_side_effect_when_usb_is_full() {
        let mut owner = ready_owner_with_usb();
        let mut setup_sink = RecordingSink::default();
        for message in [
            RuntimeInputMessage::BridgeEvent(BridgeEvent::HostConnected { host_id: HostId(2) }),
            RuntimeInputMessage::BridgeEvent(BridgeEvent::HostSecurityChanged {
                host_id: HostId(2),
                encrypted: true,
                bonded: true,
                bond: None,
            }),
            RuntimeInputMessage::BridgeEvent(BridgeEvent::CccdChanged {
                host_id: HostId(2),
                report: ReportKind::Keyboard,
                enabled: true,
            }),
            RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(InputFrame::Standard(
                keyboard_input(DeviceId(7), KeyUsage(0x04)),
            ))),
        ] {
            drive_runtime_message(&mut owner, &message, &mut setup_sink).unwrap();
        }
        let before = owner.clone();
        let mut failing_sink = RecordingSink {
            fail_task: Some(RuntimeTaskKind::UsbHost),
            ..RecordingSink::default()
        };

        let error = drive_runtime_message(
            &mut owner,
            &RuntimeInputMessage::BridgeEvent(BridgeEvent::SwitchTarget { target: HostId(2) }),
            &mut failing_sink,
        )
        .unwrap_err();

        assert_eq!(
            error,
            RuntimeDriverError::Sink {
                task: RuntimeTaskKind::UsbHost,
                error: SinkError::Rejected,
            }
        );
        assert!(failing_sink.ble.is_empty());
        assert_eq!(owner, before);
    }

    #[test]
    fn log_level_effect_runs_only_after_successful_commit() {
        let message = RuntimeInputMessage::ManagementRequest {
            destination: crate::management::ManagementDestination::Wired,
            request: crate::management::ManagementRequest {
                request_id: 9,
                command: crate::management::ManagementCommand::SetSetting {
                    id: crate::settings::SettingId::LogLevel,
                    target: crate::settings::SettingTarget::Global,
                    value: 3,
                },
            },
            now_ms: 0,
        };
        let mut owner = ready_owner();
        let before = owner.clone();
        let mut failed = RecordingSink {
            fail_task: Some(RuntimeTaskKind::Storage),
            ..RecordingSink::default()
        };
        assert!(drive_runtime_message(&mut owner, &message, &mut failed).is_err());
        assert!(failed.effects.is_empty());
        assert_eq!(owner, before);

        let mut success = RecordingSink::default();
        drive_runtime_message(&mut owner, &message, &mut success).unwrap();
        assert_eq!(success.effects.as_slice(), &[RuntimeEffect::SetLogLevel(3)]);

        let mut same_value = RecordingSink::default();
        drive_runtime_message(&mut owner, &message, &mut same_value).unwrap();
        assert!(same_value.effects.is_empty());
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
        #[cfg(feature = "dual-s3-wired")]
        device: heapless::Vec<DeviceTaskCommand, 8>,
        usb: heapless::Vec<UsbHostTaskCommand, 8>,
        storage: heapless::Vec<StorageTaskCommand, 8>,
        status: heapless::Vec<StatusTaskCommand, 8>,
        fail_task: Option<RuntimeTaskKind>,
        effects: heapless::Vec<RuntimeEffect, 8>,
    }

    impl RuntimeTaskSink for RecordingSink {
        type Error = SinkError;

        fn reserve_batch<
            const BLE: usize,
            const USB: usize,
            const STORAGE: usize,
            const STATUS: usize,
        >(
            &mut self,
            queues: &RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>,
        ) -> Result<(), (RuntimeTaskKind, Self::Error)> {
            let rejected = match self.fail_task {
                Some(RuntimeTaskKind::Ble) => !queues.ble.is_empty(),
                #[cfg(feature = "dual-s3-wired")]
                Some(RuntimeTaskKind::Device) => !queues.device.is_empty(),
                Some(RuntimeTaskKind::UsbHost) => !queues.usb_host.is_empty(),
                Some(RuntimeTaskKind::Storage) => !queues.storage.is_empty(),
                Some(RuntimeTaskKind::Status) => !queues.status.is_empty(),
                None => false,
            };
            if rejected {
                Err((self.fail_task.unwrap(), SinkError::Rejected))
            } else {
                Ok(())
            }
        }

        fn send_ble(&mut self, command: BleTaskCommand) -> Result<(), Self::Error> {
            if self.fail_task == Some(RuntimeTaskKind::Ble) {
                return Err(SinkError::Rejected);
            }
            self.ble.push(command).unwrap();
            Ok(())
        }

        #[cfg(feature = "dual-s3-wired")]
        fn send_device(&mut self, command: DeviceTaskCommand) -> Result<(), Self::Error> {
            if self.fail_task == Some(RuntimeTaskKind::Device) {
                return Err(SinkError::Rejected);
            }
            self.device.push(command).unwrap();
            Ok(())
        }

        fn send_usb_host(&mut self, command: UsbHostTaskCommand) -> Result<(), Self::Error> {
            if self.fail_task == Some(RuntimeTaskKind::UsbHost) {
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

        fn apply_effect(&mut self, effect: RuntimeEffect) {
            self.effects.push(effect).unwrap();
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
            interface_id: InterfaceId(device_id.0),
            keyboard: Some(frame),
            mouse: None,
            consumer: None,
        }
    }
}
