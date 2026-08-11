//! Capacity-checked routing from bridge commands to task-owned queues.
//!
//! A batch is classified and validated before any queue is mutated, so a
//! capacity error leaves the previous batch intact for retry or diagnostics.

#[cfg(feature = "dual-s3-wired")]
use crate::management::ManagementDestination;
use crate::management::ManagementResponsePayload;

use super::{
    BleTaskCommand, ManagementTaskResponse, RuntimeCommand, RuntimeEffect, StatusSnapshot,
    StatusTaskCommand, StorageTaskCommand, UsbHostTaskCommand,
};
#[cfg(feature = "dual-s3-wired")]
use super::{DeviceTaskCommand, RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDispatchError {
    BleQueueCapacity,
    #[cfg(feature = "dual-s3-wired")]
    DeviceQueueCapacity,
    UsbQueueCapacity,
    StorageQueueCapacity,
    StatusQueueCapacity,
    EffectQueueCapacity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCommandQueues<
    const BLE: usize,
    const USB_HOST: usize,
    const STORAGE: usize,
    const STATUS: usize,
> {
    pub ble: heapless::Vec<BleTaskCommand, BLE>,
    #[cfg(feature = "dual-s3-wired")]
    pub device: heapless::Vec<DeviceTaskCommand, RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY>,
    pub usb_host: heapless::Vec<UsbHostTaskCommand, USB_HOST>,
    pub storage: heapless::Vec<StorageTaskCommand, STORAGE>,
    pub status: heapless::Vec<StatusTaskCommand, STATUS>,
    pub effects: heapless::Vec<RuntimeEffect, STATUS>,
}

impl<const BLE: usize, const USB_HOST: usize, const STORAGE: usize, const STATUS: usize>
    RuntimeCommandQueues<BLE, USB_HOST, STORAGE, STATUS>
{
    pub const fn new() -> Self {
        Self {
            ble: heapless::Vec::new(),
            #[cfg(feature = "dual-s3-wired")]
            device: heapless::Vec::new(),
            usb_host: heapless::Vec::new(),
            storage: heapless::Vec::new(),
            status: heapless::Vec::new(),
            effects: heapless::Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.ble.clear();
        #[cfg(feature = "dual-s3-wired")]
        self.device.clear();
        self.usb_host.clear();
        self.storage.clear();
        self.status.clear();
        self.effects.clear();
    }

    pub fn dispatch_from(
        &mut self,
        commands: &[RuntimeCommand],
    ) -> Result<(), RuntimeDispatchError> {
        self.validate_capacity(commands)?;
        self.clear();
        for command in commands {
            // Capacity was checked for every lane before mutating any queue.
            // No push can fail unless classification and dispatch diverge.
            self.dispatch_one(command)?;
        }
        Ok(())
    }

    fn validate_capacity(&self, commands: &[RuntimeCommand]) -> Result<(), RuntimeDispatchError> {
        let mut ble = 0usize;
        #[cfg(feature = "dual-s3-wired")]
        let mut device = 0usize;
        let mut usb = 0usize;
        let mut storage = 0usize;
        let mut status = 0usize;
        let mut effects = 0usize;
        for command in commands {
            match command {
                RuntimeCommand::BleCommand(_) => ble += 1,
                #[cfg(feature = "dual-s3-wired")]
                RuntimeCommand::DeviceCommand(_) => device += 1,
                RuntimeCommand::UsbKeyboardLedWrite { .. } => usb += 1,
                #[cfg(feature = "dual-s3-wired")]
                RuntimeCommand::UsbMirrorEndpointOut { .. }
                | RuntimeCommand::UsbMirrorControlRequest { .. } => usb += 1,
                RuntimeCommand::PersistStorage { .. } => storage += 1,
                RuntimeCommand::StatusChanged(_) => status += 1,
                RuntimeCommand::ManagementResponse { destination, .. } => {
                    #[cfg(feature = "dual-s3-wired")]
                    if matches!(destination, ManagementDestination::WiredHid) {
                        device += 1;
                    } else {
                        status += 1;
                    }
                    #[cfg(not(feature = "dual-s3-wired"))]
                    {
                        let _ = destination;
                        status += 1;
                    }
                }
                RuntimeCommand::ApplyEffect(_) => effects += 1,
            }
        }
        if ble > BLE {
            return Err(RuntimeDispatchError::BleQueueCapacity);
        }
        #[cfg(feature = "dual-s3-wired")]
        if device > RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY {
            return Err(RuntimeDispatchError::DeviceQueueCapacity);
        }
        if usb > USB_HOST {
            return Err(RuntimeDispatchError::UsbQueueCapacity);
        }
        if storage > STORAGE {
            return Err(RuntimeDispatchError::StorageQueueCapacity);
        }
        if status > STATUS {
            return Err(RuntimeDispatchError::StatusQueueCapacity);
        }
        if effects > STATUS {
            return Err(RuntimeDispatchError::EffectQueueCapacity);
        }
        Ok(())
    }

    fn dispatch_one(&mut self, command: &RuntimeCommand) -> Result<(), RuntimeDispatchError> {
        match command {
            RuntimeCommand::BleCommand(command) => self
                .ble
                .push(*command)
                .map_err(|_| RuntimeDispatchError::BleQueueCapacity),
            #[cfg(feature = "dual-s3-wired")]
            RuntimeCommand::DeviceCommand(command) => self
                .device
                .push(*command)
                .map_err(|_| RuntimeDispatchError::DeviceQueueCapacity),
            RuntimeCommand::UsbKeyboardLedWrite {
                interface_id,
                device_id,
                bytes,
            } => self
                .usb_host
                .push(UsbHostTaskCommand::KeyboardLedWrite {
                    interface_id: *interface_id,
                    device_id: *device_id,
                    bytes: *bytes,
                })
                .map_err(|_| RuntimeDispatchError::UsbQueueCapacity),
            #[cfg(feature = "dual-s3-wired")]
            RuntimeCommand::UsbMirrorEndpointOut { device_id, report } => self
                .usb_host
                .push(UsbHostTaskCommand::MirrorEndpointOut {
                    device_id: *device_id,
                    report: *report,
                })
                .map_err(|_| RuntimeDispatchError::UsbQueueCapacity),
            #[cfg(feature = "dual-s3-wired")]
            RuntimeCommand::UsbMirrorControlRequest { device_id, request } => self
                .usb_host
                .push(UsbHostTaskCommand::MirrorControlRequest {
                    device_id: *device_id,
                    request: *request,
                })
                .map_err(|_| RuntimeDispatchError::UsbQueueCapacity),
            RuntimeCommand::PersistStorage { state, priority } => self
                .storage
                .push(StorageTaskCommand::Persist {
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
            } => {
                #[cfg(feature = "dual-s3-wired")]
                if matches!(destination, ManagementDestination::WiredHid) {
                    // A management command can intentionally change the USB
                    // presentation. Deliver its response before any queued
                    // activation command invalidates this HID connection.
                    return self
                        .device
                        .insert(0, DeviceTaskCommand::ManagementResponse(*response))
                        .map_err(|_| RuntimeDispatchError::DeviceQueueCapacity);
                }
                self.status
                    .push(StatusTaskCommand {
                        status: match response.payload {
                            ManagementResponsePayload::Status(status) => {
                                crate::bridge::BridgeStatus {
                                    active_target: status.active_host,
                                    pairable_host: status.pairing_host,
                                }
                            }
                            _ => crate::bridge::BridgeStatus {
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
                    .map_err(|_| RuntimeDispatchError::StatusQueueCapacity)
            }
            RuntimeCommand::ApplyEffect(effect) => self
                .effects
                .push(*effect)
                .map_err(|_| RuntimeDispatchError::EffectQueueCapacity),
        }
    }
}

impl<const BLE: usize, const USB_HOST: usize, const STORAGE: usize, const STATUS: usize> Default
    for RuntimeCommandQueues<BLE, USB_HOST, STORAGE, STATUS>
{
    fn default() -> Self {
        Self::new()
    }
}
