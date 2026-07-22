use heapless::{Deque, Vec};
use hidshift::mirror::{MIRROR_ENDPOINTS_MAX, UsbDevicePlan};
use usb_device::class_prelude::*;
use usb_device::control::{Recipient, Request, RequestType};
use usb_device::endpoint::{EndpointAddress, EndpointType};

const RAW_PACKET_MAX_LEN: usize = 64;
const RAW_QUEUE_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawPacket {
    endpoint_address: u8,
    length: u8,
    data: [u8; RAW_PACKET_MAX_LEN],
}

impl RawPacket {
    pub fn new(endpoint_address: u8, data: &[u8]) -> Result<Self, DynamicUsbError> {
        if data.len() > RAW_PACKET_MAX_LEN {
            return Err(DynamicUsbError::PacketTooLarge);
        }
        let mut packet = Self {
            endpoint_address,
            length: data.len() as u8,
            data: [0; RAW_PACKET_MAX_LEN],
        };
        packet.data[..data.len()].copy_from_slice(data);
        Ok(packet)
    }

    pub const fn endpoint_address(&self) -> u8 {
        self.endpoint_address
    }

    pub const fn data(&self) -> &[u8] {
        self.data.split_at(self.length as usize).0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicUsbError {
    EndpointAllocation,
    EndpointCapacity,
    UnknownEndpoint,
    PacketTooLarge,
    QueueFull,
}

struct InputEndpoint<'a, B: UsbBus> {
    address: u8,
    endpoint: EndpointIn<'a, B>,
}

struct OutputEndpoint<'a, B: UsbBus> {
    address: u8,
    endpoint: EndpointOut<'a, B>,
}

/// Runtime HID class backed entirely by a validated MirrorImage plan.
///
/// Standard descriptors are intercepted before `usb-device` generates its own
/// descriptors, preserving the source bytes and endpoint addresses exactly.
pub struct DynamicUsb<'a, B: UsbBus> {
    plan: UsbDevicePlan<'static>,
    inputs: Vec<InputEndpoint<'a, B>, MIRROR_ENDPOINTS_MAX>,
    outputs: Vec<OutputEndpoint<'a, B>, MIRROR_ENDPOINTS_MAX>,
    pending_in: Deque<RawPacket, RAW_QUEUE_CAPACITY>,
    pending_out: Deque<RawPacket, RAW_QUEUE_CAPACITY>,
    configuration_value: u8,
    configured: bool,
    pub dropped_packets: u32,
}

impl<'a, B: UsbBus> DynamicUsb<'a, B> {
    pub fn new(
        alloc: &'a UsbBusAllocator<B>,
        plan: UsbDevicePlan<'static>,
    ) -> Result<Self, DynamicUsbError> {
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for endpoint in &plan.endpoints {
            let address = EndpointAddress::from(endpoint.address);
            if endpoint.address & 0x80 != 0 {
                let allocated: EndpointIn<'a, B> = alloc
                    .alloc(
                        Some(address),
                        EndpointType::Interrupt,
                        endpoint.max_packet_size,
                        endpoint.interval,
                    )
                    .map_err(|_| DynamicUsbError::EndpointAllocation)?;
                inputs
                    .push(InputEndpoint {
                        address: endpoint.address,
                        endpoint: allocated,
                    })
                    .map_err(|_| DynamicUsbError::EndpointCapacity)?;
            } else {
                let allocated: EndpointOut<'a, B> = alloc
                    .alloc(
                        Some(address),
                        EndpointType::Interrupt,
                        endpoint.max_packet_size,
                        endpoint.interval,
                    )
                    .map_err(|_| DynamicUsbError::EndpointAllocation)?;
                outputs
                    .push(OutputEndpoint {
                        address: endpoint.address,
                        endpoint: allocated,
                    })
                    .map_err(|_| DynamicUsbError::EndpointCapacity)?;
            }
        }
        Ok(Self {
            configuration_value: plan.configuration_descriptor[5],
            plan,
            inputs,
            outputs,
            pending_in: Deque::new(),
            pending_out: Deque::new(),
            configured: false,
            dropped_packets: 0,
        })
    }

    pub fn enqueue_input(&mut self, packet: RawPacket) -> Result<(), DynamicUsbError> {
        if !self
            .inputs
            .iter()
            .any(|endpoint| endpoint.address == packet.endpoint_address())
        {
            return Err(DynamicUsbError::UnknownEndpoint);
        }
        self.pending_in
            .push_back(packet)
            .map_err(|_| DynamicUsbError::QueueFull)
    }

    pub fn take_output(&mut self) -> Option<RawPacket> {
        self.pending_out.pop_front()
    }

    pub const fn configured(&self) -> bool {
        self.configured
    }

    pub const fn plan(&self) -> &UsbDevicePlan<'static> {
        &self.plan
    }

    pub fn drop_standard_report(&mut self) {
        self.dropped_packets = self.dropped_packets.saturating_add(1);
    }

    pub fn service(&mut self) {
        if let Some(packet) = self.pending_in.front().copied() {
            let result = self
                .inputs
                .iter_mut()
                .find(|endpoint| endpoint.address == packet.endpoint_address())
                .ok_or(UsbError::InvalidEndpoint)
                .and_then(|endpoint| endpoint.endpoint.write(packet.data()).map(|_| ()));
            match result {
                Ok(()) => {
                    self.pending_in.pop_front();
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    self.pending_in.pop_front();
                    self.dropped_packets = self.dropped_packets.saturating_add(1);
                }
            }
        }

        for output in &mut self.outputs {
            if self.pending_out.is_full() {
                break;
            }
            let mut data = [0; RAW_PACKET_MAX_LEN];
            match output.endpoint.read(&mut data) {
                Ok(length) => {
                    if let Ok(packet) = RawPacket::new(output.address, &data[..length]) {
                        let _ = self.pending_out.push_back(packet);
                    }
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    self.dropped_packets = self.dropped_packets.saturating_add(1);
                }
            }
        }
    }

}

impl<B: UsbBus> UsbClass<B> for DynamicUsb<'_, B> {
    fn get_configuration_descriptors(
        &self,
        _writer: &mut DescriptorWriter,
    ) -> usb_device::Result<()> {
        // The original raw Configuration Descriptor is returned by control_in.
        Ok(())
    }

    fn control_in(&mut self, transfer: ControlIn<B>) {
        let request = *transfer.request();
        if request.request_type == RequestType::Standard
            && request.recipient == Recipient::Device
            && request.request == Request::GET_CONFIGURATION
        {
            let value = if self.configured {
                self.configuration_value
            } else {
                0
            };
            let _ = transfer.accept_with(&[value]);
            return;
        }
        if request.request_type != RequestType::Standard
            || request.request != Request::GET_DESCRIPTOR
        {
            return;
        }
        let descriptor_type = (request.value >> 8) as u8;
        let descriptor_index = request.value as u8;
        if request.recipient == Recipient::Device && descriptor_type == 0x01 {
            let _ = transfer.accept_with(&self.plan.device_descriptor);
            return;
        }
        let descriptor = match request.recipient {
            Recipient::Device => match (descriptor_type, descriptor_index) {
                (0x02, 0) => Some(self.plan.configuration_descriptor),
                (0x03, index) => self.plan.strings.get(index, request.index),
                (0x0f, 0) if !self.plan.bos_descriptor.is_empty() => {
                    Some(self.plan.bos_descriptor)
                }
                _ => None,
            },
            Recipient::Interface if descriptor_index == 0 => self.plan.interfaces.iter().find_map(
                |interface| {
                    if interface.interface_number != request.index as u8 {
                        return None;
                    }
                    match descriptor_type {
                        0x21 => Some(interface.hid_descriptor),
                        0x22 => Some(interface.report_descriptor),
                        _ => None,
                    }
                },
            ),
            _ => None,
        };
        if let Some(descriptor) = descriptor {
            let _ = transfer.accept_with_static(descriptor);
        }
    }

    fn control_out(&mut self, transfer: ControlOut<B>) {
        let request = *transfer.request();
        if request.request_type == RequestType::Standard
            && request.recipient == Recipient::Device
            && request.request == Request::SET_CONFIGURATION
        {
            match request.value as u8 {
                0 => {
                    self.configured = false;
                    let _ = transfer.accept();
                }
                value if value == self.configuration_value => {
                    self.configured = true;
                    let _ = transfer.accept();
                }
                _ => {
                    let _ = transfer.reject();
                }
            }
        }
    }

    fn get_alt_setting(&mut self, interface: InterfaceNumber) -> Option<u8> {
        let number = u8::from(interface);
        self.plan
            .interfaces
            .iter()
            .any(|candidate| candidate.interface_number == number)
            .then_some(0)
    }

    fn set_alt_setting(&mut self, interface: InterfaceNumber, alternative: u8) -> bool {
        alternative == 0 && self.get_alt_setting(interface).is_some()
    }
}
