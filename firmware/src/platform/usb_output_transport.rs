use embassy_usb_driver::host::{PipeError, UsbHostAllocator, UsbPipe, pipe};
use embassy_usb_driver::{Direction as UsbDirection, EndpointAddress, EndpointInfo, EndpointType};
use embassy_usb_host::class::hid::HidError;
use embassy_usb_host::control::SetupPacket;
use embassy_usb_host::handler::EnumerationInfo;
use hidshift::usb_hid::host_interface::HidInterfaceInfo;
use hidshift::usb_hid::output::KeyboardLedOutputBytes;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbKeyboardLedWriteError {
    NoInterface,
    NoPipe,
    Transfer,
}

impl From<HidError> for UsbKeyboardLedWriteError {
    fn from(error: HidError) -> Self {
        match error {
            HidError::Transfer(_) => Self::Transfer,
            HidError::NoInterface => Self::NoInterface,
            HidError::NoPipe => Self::NoPipe,
        }
    }
}

pub struct UsbKeyboardLedWriter<'d, A: UsbHostAllocator<'d>> {
    transport: UsbKeyboardLedTransport<'d, A>,
    interface: u8,
}

pub struct UsbRawReportWriter<'d, A: UsbHostAllocator<'d>> {
    transport: UsbKeyboardLedTransport<'d, A>,
    interface: u8,
}

enum UsbKeyboardLedTransport<'d, A: UsbHostAllocator<'d>> {
    Control(A::Pipe<pipe::Control, pipe::InOut>),
    Interrupt(A::Pipe<pipe::Interrupt, pipe::Out>),
}

impl<'d, A: UsbHostAllocator<'d>> UsbKeyboardLedWriter<'d, A> {
    pub fn new_for_interface(
        alloc: &A,
        info: HidInterfaceInfo,
        enum_info: &EnumerationInfo,
    ) -> Result<Self, UsbKeyboardLedWriteError> {
        let transport = if info.interrupt_out_ep != 0 {
            let endpoint = EndpointInfo {
                addr: EndpointAddress::from_parts(
                    (info.interrupt_out_ep & 0x0f) as usize,
                    UsbDirection::Out,
                ),
                ep_type: EndpointType::Interrupt,
                max_packet_size: info.interrupt_out_mps,
                interval_ms: info.interrupt_out_interval_ms,
            };
            UsbKeyboardLedTransport::Interrupt(
                alloc
                    .alloc_pipe::<pipe::Interrupt, pipe::Out>(
                        enum_info.device_address,
                        &endpoint,
                        enum_info.split(),
                    )
                    .map_err(|_| UsbKeyboardLedWriteError::NoPipe)?,
            )
        } else {
            let endpoint = EndpointInfo {
                addr: EndpointAddress::from_parts(0, UsbDirection::In),
                ep_type: EndpointType::Control,
                max_packet_size: enum_info.device_desc.max_packet_size0 as u16,
                interval_ms: 0,
            };
            UsbKeyboardLedTransport::Control(
                alloc
                    .alloc_pipe::<pipe::Control, pipe::InOut>(
                        enum_info.device_address,
                        &endpoint,
                        enum_info.split(),
                    )
                    .map_err(|_| UsbKeyboardLedWriteError::NoPipe)?,
            )
        };

        Ok(Self {
            transport,
            interface: info.interface_number,
        })
    }

    pub async fn write_leds(
        &mut self,
        bytes: KeyboardLedOutputBytes,
    ) -> Result<(), UsbKeyboardLedWriteError> {
        match &mut self.transport {
            UsbKeyboardLedTransport::Control(pipe) => {
                let (report_id, payload) = bytes.control_set_report();
                let setup =
                    set_report_setup_packet(self.interface, report_id, payload.len() as u16);
                pipe.control_out(&setup.to_bytes(), payload)
                    .await
                    .map_err(|_err: PipeError| UsbKeyboardLedWriteError::Transfer)
            }
            UsbKeyboardLedTransport::Interrupt(pipe) => pipe
                .request_out(bytes.as_slice(), true)
                .await
                .map_err(|_err: PipeError| UsbKeyboardLedWriteError::Transfer),
        }
    }
}

impl<'d, A: UsbHostAllocator<'d>> UsbRawReportWriter<'d, A> {
    pub fn new_for_interface(
        alloc: &A,
        info: HidInterfaceInfo,
        enum_info: &EnumerationInfo,
        prefer_interrupt_out: bool,
    ) -> Result<Self, UsbKeyboardLedWriteError> {
        let transport = if prefer_interrupt_out && info.interrupt_out_ep != 0 {
            let endpoint = EndpointInfo {
                addr: EndpointAddress::from_parts(
                    (info.interrupt_out_ep & 0x0f) as usize,
                    UsbDirection::Out,
                ),
                ep_type: EndpointType::Interrupt,
                max_packet_size: info.interrupt_out_mps,
                interval_ms: info.interrupt_out_interval_ms,
            };
            UsbKeyboardLedTransport::Interrupt(
                alloc
                    .alloc_pipe::<pipe::Interrupt, pipe::Out>(
                        enum_info.device_address,
                        &endpoint,
                        enum_info.split(),
                    )
                    .map_err(|_| UsbKeyboardLedWriteError::NoPipe)?,
            )
        } else {
            let endpoint = EndpointInfo {
                addr: EndpointAddress::from_parts(0, UsbDirection::In),
                ep_type: EndpointType::Control,
                max_packet_size: enum_info.device_desc.max_packet_size0 as u16,
                interval_ms: 0,
            };
            UsbKeyboardLedTransport::Control(
                alloc
                    .alloc_pipe::<pipe::Control, pipe::InOut>(
                        enum_info.device_address,
                        &endpoint,
                        enum_info.split(),
                    )
                    .map_err(|_| UsbKeyboardLedWriteError::NoPipe)?,
            )
        };
        Ok(Self {
            transport,
            interface: info.interface_number,
        })
    }

    pub async fn write_report(
        &mut self,
        report_type: u8,
        report_id: u8,
        payload: &[u8],
    ) -> Result<(), UsbKeyboardLedWriteError> {
        match &mut self.transport {
            UsbKeyboardLedTransport::Control(pipe) => {
                let setup = SetupPacket::class_interface_out(
                    0x09,
                    (report_type as u16) << 8 | report_id as u16,
                    self.interface as u16,
                    payload.len() as u16,
                );
                pipe.control_out(&setup.to_bytes(), payload)
                    .await
                    .map_err(|_err: PipeError| UsbKeyboardLedWriteError::Transfer)
            }
            UsbKeyboardLedTransport::Interrupt(pipe) => pipe
                .request_out(payload, true)
                .await
                .map_err(|_err: PipeError| UsbKeyboardLedWriteError::Transfer),
        }
    }
}

fn set_report_setup_packet(interface: u8, report_id: u8, payload_len: u16) -> SetupPacket {
    const SET_REPORT: u8 = 0x09;
    const OUTPUT_REPORT_TYPE: u16 = 2 << 8;
    SetupPacket::class_interface_out(
        SET_REPORT,
        OUTPUT_REPORT_TYPE | report_id as u16,
        interface as u16,
        payload_len,
    )
}
