use heapless::Deque;
use hidshift::fallback::{
    CONSUMER_REPORT_DESCRIPTOR, KEYBOARD_REPORT_DESCRIPTOR, MOUSE_REPORT_DESCRIPTOR,
};
use hidshift::interchip::{DeviceLinkEvent, StandardOutputReport};
use hidshift::reports::{ConsumerReport, Keyboard6KroReport, MouseReport, StandardHidReport};
use usb_device::class_prelude::*;

const USB_CLASS_HID: u8 = 0x03;
const HID_DESCRIPTOR: u8 = 0x21;
const HID_REPORT_DESCRIPTOR: u8 = 0x22;
const HID_GET_REPORT: u8 = 0x01;
const HID_GET_IDLE: u8 = 0x02;
const HID_GET_PROTOCOL: u8 = 0x03;
const HID_SET_REPORT: u8 = 0x09;
const HID_SET_IDLE: u8 = 0x0a;
const HID_SET_PROTOCOL: u8 = 0x0b;
const HID_REPORT_TYPE_INPUT: u8 = 1;
const HID_REPORT_TYPE_OUTPUT: u8 = 2;
const PENDING_REPORT_CAPACITY: usize = 16;

pub struct FallbackUsb<'a, B: UsbBus> {
    pub keyboard: RawHidInterface<'a, B>,
    pub mouse: RawHidInterface<'a, B>,
    pub consumer: RawHidInterface<'a, B>,
    pending: Deque<StandardHidReport, PENDING_REPORT_CAPACITY>,
    pending_led: Option<u8>,
    pub dropped_reports: u32,
}

impl<'a, B: UsbBus> FallbackUsb<'a, B> {
    pub fn new(alloc: &'a UsbBusAllocator<B>) -> Self {
        Self {
            keyboard: RawHidInterface::new(alloc, KEYBOARD_REPORT_DESCRIPTOR, 8, Some(1), 1, 1, 1),
            mouse: RawHidInterface::new(alloc, MOUSE_REPORT_DESCRIPTOR, 5, None, 1, 1, 2),
            consumer: RawHidInterface::new(alloc, CONSUMER_REPORT_DESCRIPTOR, 2, None, 1, 0, 0),
            pending: Deque::new(),
            pending_led: None,
            dropped_reports: 0,
        }
    }

    pub fn enqueue_link_event(&mut self, event: DeviceLinkEvent) {
        match event {
            DeviceLinkEvent::StandardInput(report) => self.enqueue(report.report),
            DeviceLinkEvent::ReleaseAll => {
                self.enqueue(StandardHidReport::Keyboard(Keyboard6KroReport::release()));
                self.enqueue(StandardHidReport::Mouse(MouseReport::release_buttons()));
                self.enqueue(StandardHidReport::Consumer(ConsumerReport::release()));
            }
            DeviceLinkEvent::ForceFallback { .. } => {}
            DeviceLinkEvent::ProfileBegin(_)
            | DeviceLinkEvent::ProfileChunk(_)
            | DeviceLinkEvent::ProfileCommit { .. } => {}
        }
    }

    pub fn service(&mut self) {
        self.capture_keyboard_output();
        let Some(report) = self.pending.front().copied() else {
            return;
        };
        let result = match report {
            StandardHidReport::Keyboard(report) => self.keyboard.push_input(report.as_bytes()),
            StandardHidReport::Mouse(report) => self.mouse.push_input(report.as_bytes()),
            StandardHidReport::Consumer(report) => self.consumer.push_input(report.as_bytes()),
        };
        match result {
            Ok(()) => {
                self.pending.pop_front();
            }
            Err(UsbError::WouldBlock) | Err(UsbError::InvalidState) => {}
            Err(_) => {
                self.pending.pop_front();
                self.dropped_reports = self.dropped_reports.saturating_add(1);
            }
        }
    }

    pub fn take_keyboard_output(&mut self) -> Option<StandardOutputReport> {
        self.capture_keyboard_output();
        let leds = self.pending_led.take()?;
        StandardOutputReport::new(1, &[leds]).ok()
    }

    pub fn restore_keyboard_output(&mut self, report: StandardOutputReport) {
        if let [leds] = report.data() {
            self.pending_led = Some(*leds);
        }
    }

    fn enqueue(&mut self, report: StandardHidReport) {
        if self.pending.push_back(report).is_err() {
            self.dropped_reports = self.dropped_reports.saturating_add(1);
        }
    }

    fn capture_keyboard_output(&mut self) {
        let mut output = [0u8; 1];
        if self.keyboard.pull_output(&mut output) == Ok(1) {
            self.pending_led = Some(output[0]);
        }
    }
}

pub struct RawHidInterface<'a, B: UsbBus> {
    interface: InterfaceNumber,
    input: EndpointIn<'a, B>,
    output: Option<EndpointOut<'a, B>>,
    report_descriptor: &'static [u8],
    subclass: u8,
    boot_protocol: u8,
    protocol_mode: u8,
    idle_rate: u8,
    last_input: [u8; 8],
    last_input_len: u8,
    control_output: Option<([u8; 8], u8)>,
}

impl<'a, B: UsbBus> RawHidInterface<'a, B> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        alloc: &'a UsbBusAllocator<B>,
        report_descriptor: &'static [u8],
        input_packet_size: u16,
        output_packet_size: Option<u16>,
        poll_ms: u8,
        subclass: u8,
        boot_protocol: u8,
    ) -> Self {
        Self {
            interface: alloc.interface(),
            input: alloc.interrupt(input_packet_size, poll_ms),
            output: output_packet_size.map(|size| alloc.interrupt(size, poll_ms)),
            report_descriptor,
            subclass,
            boot_protocol,
            protocol_mode: 1,
            idle_rate: 0,
            last_input: [0; 8],
            last_input_len: 0,
            control_output: None,
        }
    }

    fn push_input(&mut self, report: &[u8]) -> usb_device::Result<()> {
        let report = if self.boot_protocol == 2 && self.protocol_mode == 0 {
            report.get(..3).ok_or(UsbError::BufferOverflow)?
        } else {
            report
        };
        if report.len() > self.last_input.len() {
            return Err(UsbError::BufferOverflow);
        }
        self.input.write(report)?;
        self.last_input.fill(0);
        self.last_input[..report.len()].copy_from_slice(report);
        self.last_input_len = report.len() as u8;
        Ok(())
    }

    fn pull_output(&mut self, output: &mut [u8]) -> usb_device::Result<usize> {
        if let Some(endpoint) = &self.output {
            match endpoint.read(output) {
                Ok(length) => return Ok(length),
                Err(UsbError::WouldBlock) => {}
                Err(error) => return Err(error),
            }
        }
        let Some((data, length)) = self.control_output.take() else {
            return Err(UsbError::WouldBlock);
        };
        let length = usize::from(length);
        if output.len() < length {
            return Err(UsbError::BufferOverflow);
        }
        output[..length].copy_from_slice(&data[..length]);
        Ok(length)
    }
}

impl<B: UsbBus> UsbClass<B> for RawHidInterface<'_, B> {
    fn get_configuration_descriptors(
        &self,
        writer: &mut DescriptorWriter,
    ) -> usb_device::Result<()> {
        writer.interface(
            self.interface,
            USB_CLASS_HID,
            self.subclass,
            self.boot_protocol,
        )?;
        writer.write(
            HID_DESCRIPTOR,
            &[
                0x11,
                0x01,
                0,
                1,
                HID_REPORT_DESCRIPTOR,
                self.report_descriptor.len() as u8,
                (self.report_descriptor.len() >> 8) as u8,
            ],
        )?;
        writer.endpoint(&self.input)?;
        if let Some(output) = &self.output {
            writer.endpoint(output)?;
        }
        Ok(())
    }

    fn control_in(&mut self, transfer: ControlIn<B>) {
        let request = transfer.request();
        if request.recipient != usb_device::control::Recipient::Interface
            || request.index != u8::from(self.interface) as u16
        {
            return;
        }
        match (request.request_type, request.request) {
            (
                usb_device::control::RequestType::Standard,
                usb_device::control::Request::GET_DESCRIPTOR,
            ) => match (request.value >> 8) as u8 {
                HID_REPORT_DESCRIPTOR => {
                    let _ = transfer.accept_with_static(self.report_descriptor);
                }
                HID_DESCRIPTOR => {
                    let descriptor = [
                        9,
                        HID_DESCRIPTOR,
                        0x11,
                        0x01,
                        0,
                        1,
                        HID_REPORT_DESCRIPTOR,
                        self.report_descriptor.len() as u8,
                        (self.report_descriptor.len() >> 8) as u8,
                    ];
                    let _ = transfer.accept_with(&descriptor);
                }
                _ => {}
            },
            (usb_device::control::RequestType::Class, HID_GET_REPORT)
                if (request.value >> 8) as u8 == HID_REPORT_TYPE_INPUT =>
            {
                let _ = transfer.accept_with(&self.last_input[..self.last_input_len as usize]);
            }
            (usb_device::control::RequestType::Class, HID_GET_IDLE) => {
                let _ = transfer.accept_with(&[self.idle_rate]);
            }
            (usb_device::control::RequestType::Class, HID_GET_PROTOCOL)
                if self.boot_protocol != 0 =>
            {
                let _ = transfer.accept_with(&[self.protocol_mode]);
            }
            _ => {}
        }
    }

    fn control_out(&mut self, transfer: ControlOut<B>) {
        let request = transfer.request();
        if request.recipient != usb_device::control::Recipient::Interface
            || request.index != u8::from(self.interface) as u16
        {
            return;
        }
        match (request.request_type, request.request) {
            (usb_device::control::RequestType::Class, HID_SET_IDLE) => {
                self.idle_rate = (request.value >> 8) as u8;
                let _ = transfer.accept();
            }
            (usb_device::control::RequestType::Class, HID_SET_PROTOCOL)
                if self.boot_protocol != 0 && request.value <= 1 =>
            {
                self.protocol_mode = request.value as u8;
                let _ = transfer.accept();
            }
            (usb_device::control::RequestType::Class, HID_SET_REPORT)
                if (request.value >> 8) as u8 == HID_REPORT_TYPE_OUTPUT
                    && transfer.data().len() <= 8 =>
            {
                let mut data = [0u8; 8];
                let length = transfer.data().len();
                data[..length].copy_from_slice(transfer.data());
                self.control_output = Some((data, length as u8));
                let _ = transfer.accept();
            }
            _ => {
                let _ = transfer.reject();
            }
        }
    }
}
