use crate::ids::{DeviceId, InterfaceId, ReportId};
use crate::input::InputError;

pub const MAX_VENDOR_REPORT_SIZE: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidDirection {
    Input,
    Output,
    Feature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VendorHidFrame {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    pub usage_page: u16,
    pub report_id: Option<ReportId>,
    pub direction: HidDirection,
    payload: heapless::Vec<u8, MAX_VENDOR_REPORT_SIZE>,
}

impl VendorHidFrame {
    pub fn new(
        device_id: DeviceId,
        interface_id: InterfaceId,
        usage_page: u16,
        report_id: Option<ReportId>,
        direction: HidDirection,
        payload: &[u8],
    ) -> Result<Self, InputError> {
        let mut frame = Self {
            device_id,
            interface_id,
            usage_page,
            report_id,
            direction,
            payload: heapless::Vec::new(),
        };
        frame
            .payload
            .extend_from_slice(payload)
            .map_err(|_| InputError::VendorPayloadCapacity)?;
        Ok(frame)
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}
