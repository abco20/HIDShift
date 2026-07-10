use crate::reports::{
    BLE_HID_NOTIFICATIONS_PER_REPORT_MAX, BleHidCharacteristic, BleHidNotification,
    BleHidNotificationError, BleHidReport, notifications_for_input_report,
};
use crate::runtime::BleTaskCommand;

pub trait BleNotificationSink {
    type Error;

    fn send_notification(
        &mut self,
        characteristic: BleHidCharacteristic,
        value: &[u8],
    ) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleTypedNotification {
    KeyboardInputReport([u8; 8]),
    MouseInputReport([u8; 5]),
    ConsumerInputReport([u8; 2]),
}

impl BleTypedNotification {
    pub const fn characteristic(&self) -> BleHidCharacteristic {
        match self {
            Self::KeyboardInputReport(_) => BleHidCharacteristic::KeyboardInputReport,
            Self::MouseInputReport(_) => BleHidCharacteristic::MouseInputReport,
            Self::ConsumerInputReport(_) => BleHidCharacteristic::ConsumerInputReport,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleTypedNotificationError {
    LengthMismatch {
        characteristic: BleHidCharacteristic,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleNotificationDispatchError<E> {
    Plan(BleHidNotificationError),
    Sink {
        characteristic: BleHidCharacteristic,
        error: E,
    },
}

pub fn dispatch_ble_task_command<S>(
    command: BleTaskCommand,
    sink: &mut S,
) -> Result<(), BleNotificationDispatchError<S::Error>>
where
    S: BleNotificationSink,
{
    match command {
        BleTaskCommand::Notify { report, .. } => dispatch_input_report_notifications(report, sink),
        BleTaskCommand::AllowPairing { .. }
        | BleTaskCommand::RejectPairing { .. }
        | BleTaskCommand::ClearBond { .. } => Ok(()),
    }
}

pub fn dispatch_input_report_notifications<S>(
    report: BleHidReport,
    sink: &mut S,
) -> Result<(), BleNotificationDispatchError<S::Error>>
where
    S: BleNotificationSink,
{
    let mut notifications =
        heapless::Vec::<BleHidNotification, BLE_HID_NOTIFICATIONS_PER_REPORT_MAX>::new();
    notifications_for_input_report(report, &mut notifications)
        .map_err(BleNotificationDispatchError::Plan)?;

    for notification in notifications.iter() {
        sink.send_notification(notification.characteristic, notification.as_slice())
            .map_err(|error| BleNotificationDispatchError::Sink {
                characteristic: notification.characteristic,
                error,
            })?;
    }

    Ok(())
}

pub fn typed_notification(
    notification: &BleHidNotification,
) -> Result<BleTypedNotification, BleTypedNotificationError> {
    match notification.characteristic {
        BleHidCharacteristic::KeyboardInputReport => Ok(BleTypedNotification::KeyboardInputReport(
            notification_array(notification)?,
        )),
        BleHidCharacteristic::MouseInputReport => Ok(BleTypedNotification::MouseInputReport(
            notification_array(notification)?,
        )),
        BleHidCharacteristic::ConsumerInputReport => Ok(BleTypedNotification::ConsumerInputReport(
            notification_array(notification)?,
        )),
    }
}

fn notification_array<const N: usize>(
    notification: &BleHidNotification,
) -> Result<[u8; N], BleTypedNotificationError> {
    let bytes = notification.as_slice();
    if bytes.len() != N {
        return Err(BleTypedNotificationError::LengthMismatch {
            characteristic: notification.characteristic,
        });
    }
    let mut out = [0; N];
    out.copy_from_slice(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::NotifyReason;
    use crate::ids::HostId;
    use crate::reports::{BleConsumerReport, BleKeyboard6KroReport};

    #[derive(Default)]
    struct RecordingSink {
        notifications: heapless::Vec<(BleHidCharacteristic, heapless::Vec<u8, 9>), 4>,
        fail_on: Option<BleHidCharacteristic>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum SinkError {
        Rejected,
    }

    impl BleNotificationSink for RecordingSink {
        type Error = SinkError;

        fn send_notification(
            &mut self,
            characteristic: BleHidCharacteristic,
            value: &[u8],
        ) -> Result<(), Self::Error> {
            if self.fail_on == Some(characteristic) {
                return Err(SinkError::Rejected);
            }
            let mut bytes = heapless::Vec::<u8, 9>::new();
            bytes.extend_from_slice(value).unwrap();
            self.notifications.push((characteristic, bytes)).unwrap();
            Ok(())
        }
    }

    #[test]
    fn keyboard_command_dispatches_one_report_notification() {
        let mut sink = RecordingSink::default();

        dispatch_ble_task_command(
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            },
            &mut sink,
        )
        .unwrap();

        assert_eq!(sink.notifications.len(), 1);
        assert_eq!(
            sink.notifications[0].0,
            BleHidCharacteristic::KeyboardInputReport
        );
    }

    #[test]
    fn consumer_command_dispatches_single_notification() {
        let mut sink = RecordingSink::default();

        dispatch_input_report_notifications(
            BleHidReport::Consumer(BleConsumerReport::release()),
            &mut sink,
        )
        .unwrap();

        assert_eq!(sink.notifications.len(), 1);
        assert_eq!(
            sink.notifications[0].0,
            BleHidCharacteristic::ConsumerInputReport
        );
    }

    #[test]
    fn sink_failures_are_tagged_with_characteristic() {
        let mut sink = RecordingSink {
            fail_on: Some(BleHidCharacteristic::KeyboardInputReport),
            ..RecordingSink::default()
        };

        let err = dispatch_input_report_notifications(
            BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
            &mut sink,
        )
        .unwrap_err();

        assert_eq!(
            err,
            BleNotificationDispatchError::Sink {
                characteristic: BleHidCharacteristic::KeyboardInputReport,
                error: SinkError::Rejected,
            }
        );
    }

    #[test]
    fn typed_notification_maps_keyboard_report_characteristic_and_length() {
        let notification = BleHidNotification {
            characteristic: BleHidCharacteristic::KeyboardInputReport,
            len: 8,
            bytes: [0; 8],
        };

        assert_eq!(
            typed_notification(&notification),
            Ok(BleTypedNotification::KeyboardInputReport([0; 8]))
        );
    }

    #[test]
    fn typed_notification_rejects_unexpected_payload_length() {
        let notification = BleHidNotification {
            characteristic: BleHidCharacteristic::KeyboardInputReport,
            len: 7,
            bytes: [0; 8],
        };

        assert_eq!(
            typed_notification(&notification),
            Err(BleTypedNotificationError::LengthMismatch {
                characteristic: BleHidCharacteristic::KeyboardInputReport
            })
        );
    }
}
