use heapless::Vec;

#[cfg(feature = "usb-host")]
use embassy_usb_driver::host::{PipeError, UsbHostAllocator, UsbPipe, pipe};
#[cfg(feature = "usb-host")]
use embassy_usb_driver::{Direction as UsbDirection, EndpointAddress, EndpointInfo, EndpointType};
#[cfg(feature = "usb-host")]
use embassy_usb_host::class::hid::HidError;
#[cfg(feature = "usb-host")]
use embassy_usb_host::control::SetupPacket;
#[cfg(feature = "usb-host")]
use embassy_usb_host::handler::EnumerationInfo;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidInterfaceInfo {
    pub interface_number: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
    pub interrupt_in_ep: u8,
    pub interrupt_in_mps: u16,
    pub interrupt_in_interval_ms: u8,
    pub interrupt_out_ep: u8,
    pub interrupt_out_mps: u16,
    pub interrupt_out_interval_ms: u8,
    pub report_descriptor_len: u16,
}

impl HidInterfaceInfo {
    pub const fn supports_set_protocol(self) -> bool {
        self.interface_subclass == 0x01
            && (self.interface_protocol == 0x01 || self.interface_protocol == 0x02)
    }

    pub const fn boot_keyboard_led_fallback_allowed(self) -> bool {
        self.interface_subclass == 0x01 && self.interface_protocol == 0x01
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidInterfaceLookupError {
    InvalidDescriptor,
    TooManyInterfaces,
}

pub fn config_descriptor_has_interface_class(config_desc: &[u8], interface_class: u8) -> bool {
    if config_desc.len() < 9 || config_desc[1] != 0x02 {
        return false;
    }
    let total_len = u16::from_le_bytes([config_desc[2], config_desc[3]]) as usize;
    if total_len > config_desc.len() {
        return false;
    }

    let mut offset = config_desc[0] as usize;
    while offset + 2 <= total_len {
        let len = config_desc[offset] as usize;
        if len < 2 || offset + len > total_len {
            return false;
        }
        let desc = &config_desc[offset..offset + len];
        if desc[1] == 0x04 && len >= 9 && desc[5] == interface_class {
            return true;
        }
        offset += len;
    }
    false
}

pub fn find_hid_interfaces<const N: usize>(
    config_desc: &[u8],
) -> Result<Vec<HidInterfaceInfo, N>, HidInterfaceLookupError> {
    let mut infos = Vec::new();
    if config_desc.len() < 9 || config_desc[1] != 0x02 {
        return Err(HidInterfaceLookupError::InvalidDescriptor);
    }
    let total_len = u16::from_le_bytes([config_desc[2], config_desc[3]]) as usize;
    if total_len > config_desc.len() {
        return Err(HidInterfaceLookupError::InvalidDescriptor);
    }

    let mut offset = config_desc[0] as usize;
    let mut current: Option<HidInterfaceInfo> = None;

    while offset + 2 <= total_len {
        let len = config_desc[offset] as usize;
        let dtype = config_desc[offset + 1];
        if len < 2 || offset + len > total_len {
            return Err(HidInterfaceLookupError::InvalidDescriptor);
        }
        let desc = &config_desc[offset..offset + len];

        match dtype {
            0x04 => {
                if let Some(info) = current.take() {
                    infos
                        .push(info)
                        .map_err(|_| HidInterfaceLookupError::TooManyInterfaces)?;
                }
                if len >= 9 && desc[5] == 0x03 {
                    current = Some(HidInterfaceInfo {
                        interface_number: desc[2],
                        interface_subclass: desc[6],
                        interface_protocol: desc[7],
                        interrupt_in_ep: 0,
                        interrupt_in_mps: 0,
                        interrupt_in_interval_ms: 0,
                        interrupt_out_ep: 0,
                        interrupt_out_mps: 0,
                        interrupt_out_interval_ms: 0,
                        report_descriptor_len: 0,
                    });
                } else {
                    current = None;
                }
            }
            0x21 => {
                if let Some(info) = current.as_mut()
                    && len >= 9
                {
                    info.report_descriptor_len = u16::from_le_bytes([desc[7], desc[8]]);
                }
            }
            0x05 => {
                if let Some(info) = current.as_mut()
                    && len >= 7
                {
                    let endpoint_address = desc[2];
                    let attributes = desc[3] & 0x03;
                    if attributes == 0x03 {
                        if endpoint_address & 0x80 != 0 {
                            info.interrupt_in_ep = endpoint_address;
                            info.interrupt_in_mps = u16::from_le_bytes([desc[4], desc[5]]);
                            info.interrupt_in_interval_ms = desc[6];
                        } else {
                            info.interrupt_out_ep = endpoint_address;
                            info.interrupt_out_mps = u16::from_le_bytes([desc[4], desc[5]]);
                            info.interrupt_out_interval_ms = desc[6];
                        }
                    }
                }
            }
            _ => {}
        }

        offset += len;
    }

    if let Some(info) = current.take() {
        infos
            .push(info)
            .map_err(|_| HidInterfaceLookupError::TooManyInterfaces)?;
    }

    infos.retain(|info| info.interrupt_in_ep != 0);

    Ok(infos)
}

#[cfg(feature = "usb-host")]
pub struct UsbHidReader<'d, A: UsbHostAllocator<'d>> {
    in_ch: A::Pipe<pipe::Interrupt, pipe::In>,
}

#[cfg(feature = "usb-host")]
impl<'d, A: UsbHostAllocator<'d>> UsbHidReader<'d, A> {
    pub fn new(
        alloc: &A,
        interface: HidInterfaceInfo,
        enum_info: &EnumerationInfo,
    ) -> Result<Self, HidError> {
        let device_address = enum_info.device_address;
        let split = enum_info.split();

        let in_ep_info = interrupt_in_endpoint_info(interface);
        let in_ch = alloc
            .alloc_pipe::<pipe::Interrupt, pipe::In>(device_address, &in_ep_info, split)
            .map_err(|_| HidError::NoPipe)?;

        Ok(Self { in_ch })
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, HidError> {
        let n = self.in_ch.request_in(buf).await?;
        Ok(n)
    }
}

#[cfg(feature = "usb-host")]
pub struct UsbHidControl<'d, A: UsbHostAllocator<'d>> {
    ctrl_ch: A::Pipe<pipe::Control, pipe::InOut>,
    interface: u8,
    report_descriptor_len: u16,
}

#[cfg(feature = "usb-host")]
impl<'d, A: UsbHostAllocator<'d>> UsbHidControl<'d, A> {
    pub fn new(
        alloc: &A,
        interface: HidInterfaceInfo,
        enum_info: &EnumerationInfo,
    ) -> Result<Self, HidError> {
        let ctrl_ep_info = control_endpoint_info(enum_info);
        let ctrl_ch = alloc
            .alloc_pipe::<pipe::Control, pipe::InOut>(
                enum_info.device_address,
                &ctrl_ep_info,
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
            Err(e) => Err(HidError::Transfer(e)),
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

#[cfg(feature = "usb-host")]
fn control_endpoint_info(enum_info: &EnumerationInfo) -> EndpointInfo {
    EndpointInfo {
        addr: EndpointAddress::from_parts(0, UsbDirection::In),
        ep_type: EndpointType::Control,
        max_packet_size: enum_info.device_desc.max_packet_size0 as u16,
        interval_ms: 0,
    }
}

#[cfg(feature = "usb-host")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_multiple_hid_interfaces_in_composite_configuration() {
        let desc_bytes = [
            9, 2, 66, 0, 2, 1, 0, 160, 101, 9, 4, 0, 0, 1, 3, 1, 1, 0, 9, 33, 16, 1, 0, 1, 34, 63,
            0, 7, 5, 129, 3, 8, 0, 1, 9, 4, 1, 0, 2, 3, 1, 0, 0, 9, 33, 16, 1, 0, 1, 34, 39, 0, 7,
            5, 131, 3, 64, 0, 1, 7, 5, 3, 3, 64, 0, 1,
        ];

        let interfaces = find_hid_interfaces::<4>(&desc_bytes).unwrap();

        assert_eq!(interfaces.len(), 2);
        assert_eq!(interfaces[0].interrupt_in_interval_ms, 1);
        assert_eq!(interfaces[1].interrupt_in_interval_ms, 1);
        assert_eq!(
            interfaces[0],
            HidInterfaceInfo {
                interface_number: 0,
                interface_subclass: 1,
                interface_protocol: 1,
                interrupt_in_ep: 0x81,
                interrupt_in_mps: 8,
                interrupt_in_interval_ms: 1,
                interrupt_out_ep: 0,
                interrupt_out_mps: 0,
                interrupt_out_interval_ms: 0,
                report_descriptor_len: 63,
            }
        );
        assert_eq!(
            interfaces[1],
            HidInterfaceInfo {
                interface_number: 1,
                interface_subclass: 1,
                interface_protocol: 0,
                interrupt_in_ep: 0x83,
                interrupt_in_mps: 64,
                interrupt_in_interval_ms: 1,
                interrupt_out_ep: 0x03,
                interrupt_out_mps: 64,
                interrupt_out_interval_ms: 1,
                report_descriptor_len: 39,
            }
        );
    }

    #[test]
    fn detects_interface_class_without_class_control_requests() {
        let hid_mouse_config = [
            9, 2, 34, 0, 1, 1, 0, 160, 50, 9, 4, 0, 0, 1, 3, 1, 2, 0, 9, 33, 17, 1, 0, 1, 34, 52,
            0, 7, 5, 129, 3, 4, 0, 10,
        ];
        let hub_config = [
            9, 2, 25, 0, 1, 1, 0, 224, 0, 9, 4, 0, 0, 1, 9, 0, 0, 0, 7, 5, 129, 3, 1, 0, 12,
        ];

        assert!(config_descriptor_has_interface_class(
            &hid_mouse_config,
            0x03
        ));
        assert!(!config_descriptor_has_interface_class(
            &hid_mouse_config,
            0x09
        ));
        assert!(config_descriptor_has_interface_class(&hub_config, 0x09));
    }

    #[test]
    fn rejects_more_hid_interfaces_than_capacity() {
        let desc_bytes = [
            9, 2, 66, 0, 2, 1, 0, 160, 101, 9, 4, 0, 0, 1, 3, 1, 1, 0, 9, 33, 16, 1, 0, 1, 34, 63,
            0, 7, 5, 129, 3, 8, 0, 1, 9, 4, 1, 0, 2, 3, 1, 0, 0, 9, 33, 16, 1, 0, 1, 34, 39, 0, 7,
            5, 131, 3, 64, 0, 1, 7, 5, 3, 3, 64, 0, 1,
        ];

        assert_eq!(
            find_hid_interfaces::<1>(&desc_bytes),
            Err(HidInterfaceLookupError::TooManyInterfaces)
        );
    }

    #[test]
    fn set_protocol_is_only_supported_by_boot_interfaces() {
        let mut interface = HidInterfaceInfo {
            interface_number: 0,
            interface_subclass: 0,
            interface_protocol: 0,
            interrupt_in_ep: 0x81,
            interrupt_in_mps: 8,
            interrupt_in_interval_ms: 1,
            interrupt_out_ep: 0,
            interrupt_out_mps: 0,
            interrupt_out_interval_ms: 0,
            report_descriptor_len: 32,
        };

        assert!(!interface.supports_set_protocol());
        interface.interface_subclass = 1;
        assert!(!interface.supports_set_protocol());
        interface.interface_protocol = 1;
        assert!(interface.supports_set_protocol());
        interface.interface_protocol = 2;
        assert!(interface.supports_set_protocol());
    }
}
