use heapless::{Deque, Vec};
use hidshift::interchip::{
    ControlStatus, MirrorControlRequest, MirrorControlResponse, StandardOutputReport,
};
use hidshift::mirror::{MIRROR_ENDPOINTS_MAX, MirrorControlForwarder, UsbDevicePlan};
use hidshift::reports::{ConsumerReport, Keyboard6KroReport, MouseReport, StandardHidReport};
use usb_device::UsbDirection;
use usb_device::class_prelude::*;
use usb_device::control::{Recipient, Request, RequestType};
use usb_device::endpoint::{EndpointAddress, EndpointType};

const RAW_PACKET_MAX_LEN: usize = 64;
const RAW_QUEUE_CAPACITY: usize = 16;
const FALLBACK_INTERFACE_COUNT: usize = 3;
const HID_GET_REPORT: u8 = 0x01;
const HID_GET_IDLE: u8 = 0x02;
const HID_GET_PROTOCOL: u8 = 0x03;
const HID_SET_REPORT: u8 = 0x09;
const HID_SET_IDLE: u8 = 0x0a;
const HID_SET_PROTOCOL: u8 = 0x0b;
const HID_REPORT_TYPE_INPUT: u8 = 1;
const HID_REPORT_TYPE_OUTPUT: u8 = 2;

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
    fallback: bool,
    pending_standard_output: Option<StandardOutputReport>,
    fallback_idle: [u8; FALLBACK_INTERFACE_COUNT],
    fallback_protocol: [u8; FALLBACK_INTERFACE_COUNT],
    fallback_last_input: [Option<RawPacket>; FALLBACK_INTERFACE_COUNT],
    control_forwarder: MirrorControlForwarder,
    pending_control_request: Option<MirrorControlRequest>,
    pending_control_response: Option<MirrorControlResponse>,
    pending_control_direction: Option<UsbDirection>,
    pub dropped_packets: u32,
}

impl<'a, B: UsbBus> DynamicUsb<'a, B> {
    pub fn new(
        alloc: &'a UsbBusAllocator<B>,
        plan: UsbDevicePlan<'static>,
        fallback: bool,
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
            plan,
            inputs,
            outputs,
            pending_in: Deque::new(),
            pending_out: Deque::new(),
            fallback,
            pending_standard_output: None,
            fallback_idle: [0; FALLBACK_INTERFACE_COUNT],
            fallback_protocol: [1; FALLBACK_INTERFACE_COUNT],
            fallback_last_input: [None; FALLBACK_INTERFACE_COUNT],
            control_forwarder: MirrorControlForwarder::new(),
            pending_control_request: None,
            pending_control_response: None,
            pending_control_direction: None,
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

    pub fn take_standard_output(&mut self) -> Option<StandardOutputReport> {
        self.pending_standard_output.take()
    }

    pub fn restore_standard_output(&mut self, report: StandardOutputReport) {
        if self.pending_standard_output.is_none() {
            self.pending_standard_output = Some(report);
        }
    }

    pub fn enqueue_standard_report(&mut self, report: StandardHidReport) {
        if !self.fallback {
            self.drop_standard_report();
            return;
        }
        let (interface, endpoint, data): (usize, u8, &[u8]) = match &report {
            StandardHidReport::Keyboard(report) => (0, 0x81, report.as_bytes()),
            StandardHidReport::Mouse(report) => {
                let bytes = report.as_bytes();
                let data = if self.fallback_protocol[1] == 0 {
                    &bytes[..3]
                } else {
                    bytes
                };
                (1, 0x82, data)
            }
            StandardHidReport::Consumer(report) => (2, 0x83, report.as_bytes()),
        };
        match RawPacket::new(endpoint, data) {
            Ok(packet) => {
                self.fallback_last_input[interface] = Some(packet);
                if self.pending_in.push_back(packet).is_err() {
                    self.dropped_packets = self.dropped_packets.saturating_add(1);
                }
            }
            Err(_) => self.dropped_packets = self.dropped_packets.saturating_add(1),
        }
    }

    pub fn release_all_standard(&mut self) {
        for report in [
            StandardHidReport::Keyboard(Keyboard6KroReport::release()),
            StandardHidReport::Mouse(MouseReport::release_buttons()),
            StandardHidReport::Consumer(ConsumerReport::release()),
        ] {
            self.enqueue_standard_report(report);
        }
    }

    pub fn take_control_request(&mut self) -> Option<MirrorControlRequest> {
        self.pending_control_request.take()
    }

    pub fn restore_control_request(&mut self, request: MirrorControlRequest) {
        if self.pending_control_request.is_none() {
            self.pending_control_request = Some(request);
        }
    }

    pub fn enqueue_control_response(&mut self, response: MirrorControlResponse) {
        if self.pending_control_response.is_none() {
            self.pending_control_response = Some(response);
        } else {
            self.dropped_packets = self.dropped_packets.saturating_add(1);
        }
    }

    pub fn restore_output(&mut self, packet: RawPacket) {
        if self.pending_out.push_front(packet).is_err() {
            self.dropped_packets = self.dropped_packets.saturating_add(1);
        }
    }

    pub const fn plan(&self) -> &UsbDevicePlan<'static> {
        &self.plan
    }

    pub const fn is_fallback(&self) -> bool {
        self.fallback
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
                        if self.fallback && output.address == 0x01 {
                            self.pending_standard_output =
                                StandardOutputReport::new(1, packet.data()).ok();
                        } else {
                            let _ = self.pending_out.push_back(packet);
                        }
                    }
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    self.dropped_packets = self.dropped_packets.saturating_add(1);
                }
            }
        }
    }

    pub fn service_control(&mut self, usb_device: &mut usb_device::device::UsbDevice<'_, B>) {
        let now_ms = now_ms();
        if let Some(response) = self.pending_control_response.take() {
            let completed = self.control_forwarder.complete(response, now_ms);
            if let Ok(response) = completed {
                let result = match response.status {
                    ControlStatus::Success => match self.pending_control_direction.take() {
                        Some(UsbDirection::In) => usb_device.complete_control_in(response.data()),
                        Some(UsbDirection::Out) => usb_device.complete_control_out(),
                        None => usb_device.reject_deferred_control(),
                    },
                    _ => {
                        self.pending_control_direction = None;
                        usb_device.reject_deferred_control()
                    }
                };
                if result.is_err() {
                    self.dropped_packets = self.dropped_packets.saturating_add(1);
                }
            }
        }
        if self.control_forwarder.expire(now_ms).is_some() {
            self.pending_control_direction = None;
            self.pending_control_request = None;
            let _ = usb_device.reject_deferred_control();
        }
    }

    fn defer_control(&mut self, request: Request, data: &[u8]) -> Result<(), DynamicUsbError> {
        let setup = setup_packet(request);
        let forwarded = self
            .control_forwarder
            .begin(setup, data, now_ms())
            .map_err(|_| DynamicUsbError::QueueFull)?;
        self.pending_control_request = Some(forwarded);
        self.pending_control_direction = Some(request.direction);
        Ok(())
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
            && request.request == Request::GET_DESCRIPTOR
        {
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
                Recipient::Interface if descriptor_index == 0 => {
                    self.plan.interfaces.iter().find_map(|interface| {
                        if interface.interface_number != request.index as u8 {
                            return None;
                        }
                        match descriptor_type {
                            0x21 => Some(interface.hid_descriptor),
                            0x22 => Some(interface.report_descriptor),
                            _ => None,
                        }
                    })
                }
                _ => None,
            };
            if let Some(descriptor) = descriptor {
                let _ = transfer.accept_with_static(descriptor);
            }
            return;
        }
        if self.fallback
            && request.request_type == RequestType::Class
            && request.recipient == Recipient::Interface
        {
            if request.index > u16::from(u8::MAX) {
                return;
            }
            let interface = usize::from(request.index as u8);
            if interface >= FALLBACK_INTERFACE_COUNT {
                return;
            }
            match request.request {
                HID_GET_REPORT if (request.value >> 8) as u8 == HID_REPORT_TYPE_INPUT => {
                    if let Some(report) = self.fallback_last_input[interface] {
                        let _ = transfer.accept_with(report.data());
                    } else {
                        let zeroes = [0; 8];
                        let length = match interface {
                            0 => 8,
                            1 if self.fallback_protocol[1] == 0 => 3,
                            1 => 5,
                            _ => 2,
                        };
                        let _ = transfer.accept_with(&zeroes[..length]);
                    }
                    return;
                }
                HID_GET_IDLE => {
                    let _ = transfer.accept_with(&[self.fallback_idle[interface]]);
                    return;
                }
                HID_GET_PROTOCOL if interface < 2 => {
                    let _ = transfer.accept_with(&[self.fallback_protocol[interface]]);
                    return;
                }
                _ => {}
            }
        }
        if should_forward_control(request) && self.defer_control(request, &[]).is_ok() {
            let _ = transfer.defer();
        }
    }

    fn control_out(&mut self, transfer: ControlOut<B>) {
        let request = *transfer.request();
        if self.fallback
            && request.request_type == RequestType::Class
            && request.recipient == Recipient::Interface
        {
            if request.index > u16::from(u8::MAX) {
                let _ = transfer.reject();
                return;
            }
            let interface = usize::from(request.index as u8);
            if interface >= FALLBACK_INTERFACE_COUNT {
                return;
            }
            match request.request {
                HID_SET_REPORT
                    if interface == 0 && (request.value >> 8) as u8 == HID_REPORT_TYPE_OUTPUT =>
                {
                    match StandardOutputReport::new(1, transfer.data()) {
                        Ok(report) => {
                            self.pending_standard_output = Some(report);
                            let _ = transfer.accept();
                        }
                        Err(_) => {
                            let _ = transfer.reject();
                        }
                    }
                    return;
                }
                HID_SET_IDLE => {
                    self.fallback_idle[interface] = (request.value >> 8) as u8;
                    let _ = transfer.accept();
                    return;
                }
                HID_SET_PROTOCOL if interface < 2 && request.value <= 1 => {
                    self.fallback_protocol[interface] = request.value as u8;
                    let _ = transfer.accept();
                    return;
                }
                _ => {
                    let _ = transfer.reject();
                    return;
                }
            }
        }
        if should_forward_control(request) {
            let data = transfer.data();
            if self.defer_control(request, data).is_ok() {
                let _ = transfer.defer();
            } else {
                let _ = transfer.reject();
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

    fn reset(&mut self) {
        let _ = self.control_forwarder.cancel();
        self.pending_control_request = None;
        self.pending_control_response = None;
        self.pending_control_direction = None;
        self.pending_standard_output = None;
        self.fallback_idle.fill(0);
        self.fallback_protocol.fill(1);
        self.fallback_last_input.fill(None);
    }
}

fn should_forward_control(request: Request) -> bool {
    request.request_type != RequestType::Standard
}

fn setup_packet(request: Request) -> [u8; 8] {
    let direction = match request.direction {
        UsbDirection::Out => 0,
        UsbDirection::In => 0x80,
    };
    let value = request.value.to_le_bytes();
    let index = request.index.to_le_bytes();
    let length = request.length.to_le_bytes();
    [
        direction | ((request.request_type as u8) << 5) | request.recipient as u8,
        request.request,
        value[0],
        value[1],
        index[0],
        index[1],
        length[0],
        length[1],
    ]
}

fn now_ms() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis()
}
