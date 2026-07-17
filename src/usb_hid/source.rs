use crate::ids::{DeviceId, InterfaceId};

pub const USB_DEVICE_STRING_MAX_LEN: usize = 64;
pub const USB_HID_REPORT_DESCRIPTOR_MAX_LEN: usize = 1024;
pub const USB_HID_REPORT_MAX_LEN: usize = 256;

pub type UsbDeviceString = heapless::String<USB_DEVICE_STRING_MAX_LEN>;
pub type UsbHidReportDescriptorBytes = heapless::Vec<u8, USB_HID_REPORT_DESCRIPTOR_MAX_LEN>;
pub type UsbHidReportBytes = heapless::Vec<u8, USB_HID_REPORT_MAX_LEN>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHidSourceError {
    DeviceStringTooLong,
    ReportDescriptorTooLong,
    ReportTooLong,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbHidDeviceIdentity {
    pub device_id: DeviceId,
    pub vendor_id: u16,
    pub product_id: u16,
    pub version: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub manufacturer: UsbDeviceString,
    pub product: UsbDeviceString,
    pub serial_number: UsbDeviceString,
}

impl UsbHidDeviceIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device_id: DeviceId,
        vendor_id: u16,
        product_id: u16,
        version: u16,
        device_class: u8,
        device_subclass: u8,
        device_protocol: u8,
        manufacturer: &str,
        product: &str,
        serial_number: &str,
    ) -> Result<Self, UsbHidSourceError> {
        Ok(Self {
            device_id,
            vendor_id,
            product_id,
            version,
            device_class,
            device_subclass,
            device_protocol,
            manufacturer: device_string(manufacturer)?,
            product: device_string(product)?,
            serial_number: device_string(serial_number)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbHidInterfaceSnapshot {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
    report_descriptor: UsbHidReportDescriptorBytes,
}

impl UsbHidInterfaceSnapshot {
    pub fn new(
        device_id: DeviceId,
        interface_id: InterfaceId,
        interface_number: u8,
        alternate_setting: u8,
        interface_subclass: u8,
        interface_protocol: u8,
        report_descriptor: &[u8],
    ) -> Result<Self, UsbHidSourceError> {
        Ok(Self {
            device_id,
            interface_id,
            interface_number,
            alternate_setting,
            interface_subclass,
            interface_protocol,
            report_descriptor: descriptor_bytes(report_descriptor)?,
        })
    }

    pub fn report_descriptor(&self) -> &[u8] {
        self.report_descriptor.as_slice()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbHidInputReport<'a> {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    bytes: &'a [u8],
}

impl<'a> UsbHidInputReport<'a> {
    pub fn new(
        device_id: DeviceId,
        interface_id: InterfaceId,
        bytes: &'a [u8],
    ) -> Result<Self, UsbHidSourceError> {
        if bytes.len() > USB_HID_REPORT_MAX_LEN {
            return Err(UsbHidSourceError::ReportTooLong);
        }
        Ok(Self {
            device_id,
            interface_id,
            bytes,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }

    pub fn to_owned(self) -> Result<OwnedUsbHidInputReport, UsbHidSourceError> {
        OwnedUsbHidInputReport::new(self.device_id, self.interface_id, self.bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedUsbHidInputReport {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    bytes: UsbHidReportBytes,
}

impl OwnedUsbHidInputReport {
    pub fn new(
        device_id: DeviceId,
        interface_id: InterfaceId,
        bytes: &[u8],
    ) -> Result<Self, UsbHidSourceError> {
        Ok(Self {
            device_id,
            interface_id,
            bytes: report_bytes(bytes)?,
        })
    }

    pub fn as_borrowed(&self) -> UsbHidInputReport<'_> {
        UsbHidInputReport {
            device_id: self.device_id,
            interface_id: self.interface_id,
            bytes: self.bytes.as_slice(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHidSourceEvent<'a> {
    DeviceAttached(&'a UsbHidDeviceIdentity),
    InterfaceAttached(&'a UsbHidInterfaceSnapshot),
    InputReport(UsbHidInputReport<'a>),
    InterfaceRemoved {
        device_id: DeviceId,
        interface_id: InterfaceId,
    },
    DeviceRemoved {
        device_id: DeviceId,
    },
}

fn device_string(value: &str) -> Result<UsbDeviceString, UsbHidSourceError> {
    let mut result = UsbDeviceString::new();
    result
        .push_str(value)
        .map_err(|_| UsbHidSourceError::DeviceStringTooLong)?;
    Ok(result)
}

fn descriptor_bytes(value: &[u8]) -> Result<UsbHidReportDescriptorBytes, UsbHidSourceError> {
    let mut result = UsbHidReportDescriptorBytes::new();
    result
        .extend_from_slice(value)
        .map_err(|_| UsbHidSourceError::ReportDescriptorTooLong)?;
    Ok(result)
}

fn report_bytes(value: &[u8]) -> Result<UsbHidReportBytes, UsbHidSourceError> {
    let mut result = UsbHidReportBytes::new();
    result
        .extend_from_slice(value)
        .map_err(|_| UsbHidSourceError::ReportTooLong)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_boundary_preserves_device_identity_without_storage_coupling() {
        let identity = UsbHidDeviceIdentity::new(
            DeviceId(2),
            0x046d,
            0xc547,
            0x1203,
            0,
            0,
            0,
            "Logitech",
            "USB Receiver",
            "receiver-1",
        )
        .unwrap();

        assert_eq!(identity.device_id, DeviceId(2));
        assert_eq!((identity.vendor_id, identity.product_id), (0x046d, 0xc547));
        assert_eq!(identity.version, 0x1203);
        assert_eq!(identity.manufacturer.as_str(), "Logitech");
        assert_eq!(identity.product.as_str(), "USB Receiver");
        assert_eq!(identity.serial_number.as_str(), "receiver-1");
    }

    #[test]
    fn source_boundary_does_not_remap_report_ids_or_descriptor_bytes() {
        let descriptor = [
            0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x07, 0x75, 0x08, 0x95, 0x03, 0x81,
            0x02, 0xc0,
        ];
        let interface =
            UsbHidInterfaceSnapshot::new(DeviceId(2), InterfaceId(5), 3, 0, 0, 0, &descriptor)
                .unwrap();
        let report_bytes = [0x07, 0xaa, 0xbb];
        let report = UsbHidInputReport::new(DeviceId(2), InterfaceId(5), &report_bytes).unwrap();

        assert_eq!(interface.report_descriptor(), descriptor);
        assert_eq!(report.bytes(), &[0x07, 0xaa, 0xbb]);
        assert_eq!(report.to_owned().unwrap().as_borrowed(), report);
    }

    #[test]
    fn capacity_failures_do_not_create_truncated_source_data() {
        let descriptor = [0; USB_HID_REPORT_DESCRIPTOR_MAX_LEN + 1];
        assert_eq!(
            UsbHidInterfaceSnapshot::new(DeviceId(1), InterfaceId(1), 0, 0, 0, 0, &descriptor,),
            Err(UsbHidSourceError::ReportDescriptorTooLong)
        );

        let report = [0; USB_HID_REPORT_MAX_LEN + 1];
        assert_eq!(
            UsbHidInputReport::new(DeviceId(1), InterfaceId(1), &report),
            Err(UsbHidSourceError::ReportTooLong)
        );
    }
}
