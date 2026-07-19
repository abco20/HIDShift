use embassy_usb_driver::host::{PipeError, UsbHostAllocator, UsbPipe, pipe};
use embassy_usb_driver::{Direction as UsbDirection, EndpointAddress, EndpointInfo, EndpointType};
use embassy_usb_host::class::hid::HidError;
use embassy_usb_host::control::SetupPacket;
use embassy_usb_host::handler::EnumerationInfo;
use hidshift::usb_hid::host_interface::HidInterfaceInfo;

pub struct UsbHidReader<'d, A: UsbHostAllocator<'d>> {
    in_ch: A::Pipe<pipe::Interrupt, pipe::In>,
}

impl<'d, A: UsbHostAllocator<'d>> UsbHidReader<'d, A> {
    pub fn new(
        alloc: &A,
        interface: HidInterfaceInfo,
        enum_info: &EnumerationInfo,
    ) -> Result<Self, HidError> {
        let in_ch = alloc
            .alloc_pipe::<pipe::Interrupt, pipe::In>(
                enum_info.device_address,
                &interrupt_in_endpoint_info(interface),
                enum_info.split(),
            )
            .map_err(|_| HidError::NoPipe)?;
        Ok(Self { in_ch })
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, HidError> {
        self.in_ch.request_in(buf).await.map_err(Into::into)
    }
}

pub struct UsbHidControl<'d, A: UsbHostAllocator<'d>> {
    ctrl_ch: A::Pipe<pipe::Control, pipe::InOut>,
    interface: u8,
    report_descriptor_len: u16,
}

impl<'d, A: UsbHostAllocator<'d>> UsbHidControl<'d, A> {
    pub fn new(
        alloc: &A,
        interface: HidInterfaceInfo,
        enum_info: &EnumerationInfo,
    ) -> Result<Self, HidError> {
        let ctrl_ch = alloc
            .alloc_pipe::<pipe::Control, pipe::InOut>(
                enum_info.device_address,
                &control_endpoint_info(enum_info),
                enum_info.split(),
            )
            .map_err(|_| HidError::NoPipe)?;
        Ok(Self {
            ctrl_ch,
            interface: interface.interface_number,
            report_descriptor_len: interface.report_descriptor_len,
        })
    }

    pub async fn fetch_report_descriptor<'a>(
        &mut self,
        buf: &'a mut [u8],
    ) -> Result<&'a [u8], HidError> {
        let len = (self.report_descriptor_len as usize).min(buf.len()) as u16;
        let setup = SetupPacket::get_hid_report_descriptor(self.interface, len);
        let n = self
            .ctrl_ch
            .control_in(&setup.to_bytes(), &mut buf[..len as usize])
            .await?;
        Ok(&buf[..n])
    }

    pub async fn set_idle(&mut self, report_id: u8, idle_duration: u8) -> Result<(), HidError> {
        let value = (idle_duration as u16) << 8 | report_id as u16;
        let setup = SetupPacket::class_interface_out(0x0A, value, self.interface as u16, 0);
        match self.ctrl_ch.control_out(&setup.to_bytes(), &[]).await {
            Ok(_) | Err(PipeError::Stall) => Ok(()),
            Err(error) => Err(HidError::Transfer(error)),
        }
    }

    pub async fn set_protocol(&mut self, protocol: u8) -> Result<(), HidError> {
        let setup =
            SetupPacket::class_interface_out(0x0B, protocol as u16, self.interface as u16, 0);
        self.ctrl_ch.control_out(&setup.to_bytes(), &[]).await?;
        Ok(())
    }

    pub async fn ensure_protocol(&mut self, protocol: u8) -> Result<(), HidError> {
        let setup = SetupPacket::class_interface_in(0x03, 0, self.interface as u16, 1);
        let mut current = [0u8; 1];
        if matches!(
            self.ctrl_ch
                .control_in(&setup.to_bytes(), &mut current)
                .await,
            Ok(1)
        ) && current[0] == protocol
        {
            return Ok(());
        }
        self.set_protocol(protocol).await
    }
}

fn control_endpoint_info(enum_info: &EnumerationInfo) -> EndpointInfo {
    EndpointInfo {
        addr: EndpointAddress::from_parts(0, UsbDirection::In),
        ep_type: EndpointType::Control,
        max_packet_size: enum_info.device_desc.max_packet_size0 as u16,
        interval_ms: 0,
    }
}

fn interrupt_in_endpoint_info(interface: HidInterfaceInfo) -> EndpointInfo {
    EndpointInfo {
        addr: EndpointAddress::from_parts(
            (interface.interrupt_in_ep & 0x0F) as usize,
            UsbDirection::In,
        ),
        ep_type: EndpointType::Interrupt,
        max_packet_size: interface.interrupt_in_mps,
        interval_ms: interface.interrupt_in_interval_ms,
    }
}
