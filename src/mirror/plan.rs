use heapless::Vec;

use super::image::{HidReportDescriptorTable, StringDescriptorTable};

pub const MIRROR_HID_INTERFACES_MAX: usize = 4;
pub const MIRROR_ENDPOINTS_MAX: usize = 8;

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
