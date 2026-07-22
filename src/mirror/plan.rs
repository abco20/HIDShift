use heapless::Vec;

use super::image::{HidReportDescriptorTable, StringDescriptorTable};

pub const MIRROR_HID_INTERFACES_MAX: usize = 4;
pub const MIRROR_ENDPOINTS_MAX: usize = 8;

pub const USB_DESCRIPTOR_DEVICE: u8 = 0x01;
pub const USB_DESCRIPTOR_CONFIGURATION: u8 = 0x02;
pub const USB_DESCRIPTOR_STRING: u8 = 0x03;
pub const USB_DESCRIPTOR_BOS: u8 = 0x0f;
pub const USB_DESCRIPTOR_HID: u8 = 0x21;
pub const USB_DESCRIPTOR_HID_REPORT: u8 = 0x22;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointPlan {
    pub interface_number: u8,
    pub address: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidInterfacePlan<'a> {
    pub interface_number: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub hid_descriptor: &'a [u8],
    pub report_descriptor: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbDevicePlan<'a> {
    pub device_descriptor: [u8; 18],
    pub configuration_descriptor: &'a [u8],
    pub bos_descriptor: &'a [u8],
    pub strings: StringDescriptorTable<'a>,
    pub hid_reports: HidReportDescriptorTable<'a>,
    pub interfaces: Vec<HidInterfacePlan<'a>, MIRROR_HID_INTERFACES_MAX>,
    pub endpoints: Vec<EndpointPlan, MIRROR_ENDPOINTS_MAX>,
}

impl<'a> UsbDevicePlan<'a> {
    pub fn device_descriptor(&self, descriptor_type: u8, index: u8, lang_id: u16) -> Option<&[u8]> {
        match (descriptor_type, index) {
            (USB_DESCRIPTOR_DEVICE, 0) => Some(&self.device_descriptor),
            (USB_DESCRIPTOR_CONFIGURATION, 0) => Some(self.configuration_descriptor),
            (USB_DESCRIPTOR_STRING, index) => self.strings.get(index, lang_id),
            (USB_DESCRIPTOR_BOS, 0) if !self.bos_descriptor.is_empty() => Some(self.bos_descriptor),
            _ => None,
        }
    }

    pub fn interface_descriptor(&self, interface_number: u8, descriptor_type: u8) -> Option<&[u8]> {
        let interface = self
            .interfaces
            .iter()
            .find(|interface| interface.interface_number == interface_number)?;
        match descriptor_type {
            USB_DESCRIPTOR_HID => Some(interface.hid_descriptor),
            USB_DESCRIPTOR_HID_REPORT => Some(interface.report_descriptor),
            _ => None,
        }
    }
}
