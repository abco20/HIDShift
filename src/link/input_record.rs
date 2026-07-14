use crate::ids::{DeviceId, InterfaceId};

use super::{MAX_HID_REPORT_SIZE, MotionCumulative};

/// Owned, fixed-capacity HID report at the ESP-NOW input boundary.
///
/// The same representation is used by the critical transition journal and
/// by the Device-side writer, while transport-specific packet metadata stays
/// outside the USB HID layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputReportRecord {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    pub sequence: u32,
    pub e2e_sequence: u32,
    pub motion: MotionCumulative,
    report_len: u16,
    report: [u8; MAX_HID_REPORT_SIZE],
}

impl InputReportRecord {
    pub fn new(
        device_id: DeviceId,
        interface_id: InterfaceId,
        sequence: u32,
        e2e_sequence: u32,
        report: &[u8],
    ) -> Option<Self> {
        Self::new_with_motion(
            device_id,
            interface_id,
            sequence,
            e2e_sequence,
            MotionCumulative::zero(),
            report,
        )
    }

    pub fn new_with_motion(
        device_id: DeviceId,
        interface_id: InterfaceId,
        sequence: u32,
        e2e_sequence: u32,
        motion: MotionCumulative,
        report: &[u8],
    ) -> Option<Self> {
        if report.len() > MAX_HID_REPORT_SIZE {
            return None;
        }
        let mut bytes = [0; MAX_HID_REPORT_SIZE];
        bytes[..report.len()].copy_from_slice(report);
        Some(Self {
            device_id,
            interface_id,
            sequence,
            e2e_sequence,
            motion,
            report_len: report.len() as u16,
            report: bytes,
        })
    }

    pub fn report(&self) -> &[u8] {
        &self.report[..self.report_len as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_record_rejects_oversized_reports_without_partial_state() {
        let oversized = [0xaa; MAX_HID_REPORT_SIZE + 1];
        assert!(InputReportRecord::new(DeviceId(1), InterfaceId(2), 3, 4, &oversized).is_none());
    }
}
