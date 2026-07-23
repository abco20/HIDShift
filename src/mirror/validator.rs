use heapless::Vec;

use super::image::MirrorImage;
use super::parser::{MirrorImageParseError, parse_mirror_image};
use super::plan::{
    EndpointPlan, HidInterfacePlan, MIRROR_ENDPOINTS_MAX, MIRROR_HID_INTERFACES_MAX, UsbDevicePlan,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MirrorRejectReason {
    None = 0,
    MalformedImage = 1,
    InvalidDescriptorLength = 2,
    MultipleConfigurations = 3,
    NonHidInterface = 4,
    UnsupportedAlternateSetting = 5,
    UnsupportedEndpointType = 6,
    PacketTooLarge = 7,
    DuplicateEndpointAddress = 8,
    EndpointResourceExhausted = 9,
    MissingReportDescriptor = 10,
    ImageTooLarge = 11,
    UnsupportedUsbVersion = 12,
    RemoteWakeUnsupported = 13,
    StorageFailure = 14,
}

pub fn validate_mirror_image(bytes: &[u8]) -> Result<UsbDevicePlan<'_>, MirrorRejectReason> {
    let image = parse_mirror_image(bytes).map_err(map_parse_error)?;
    validate_parsed_image(&image)
}

fn validate_parsed_image<'a>(
    image: &MirrorImage<'a>,
) -> Result<UsbDevicePlan<'a>, MirrorRejectReason> {
    let device: [u8; 18] = image
        .device_descriptor
        .try_into()
        .map_err(|_| MirrorRejectReason::InvalidDescriptorLength)?;
    if device[0] != 18 || device[1] != 0x01 {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    if device[17] != 1 {
        return Err(MirrorRejectReason::MultipleConfigurations);
    }
    let usb_version = u16::from_le_bytes([device[2], device[3]]);
    if !(0x0100..=0x0210).contains(&usb_version) {
        return Err(MirrorRejectReason::UnsupportedUsbVersion);
    }
    if !matches!(device[7], 8 | 16 | 32 | 64) {
        return Err(MirrorRejectReason::PacketTooLarge);
    }

    validate_bos(image.bos_descriptor)?;
    validate_strings(image)?;
    let reports = collect_reports(image)?;
    validate_configuration(image, device, &reports)
}

fn validate_bos(descriptor: &[u8]) -> Result<(), MirrorRejectReason> {
    if descriptor.is_empty() {
        return Ok(());
    }
    if descriptor.len() < 5
        || descriptor[0] != 5
        || descriptor[1] != 0x0f
        || usize::from(u16::from_le_bytes([descriptor[2], descriptor[3]])) != descriptor.len()
    {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    walk_descriptors(descriptor, 0)?;
    Ok(())
}

fn validate_strings(image: &MirrorImage<'_>) -> Result<(), MirrorRejectReason> {
    for record in image.strings() {
        let descriptor = record.descriptor;
        if descriptor.len() < 2
            || descriptor.len() % 2 != 0
            || usize::from(descriptor[0]) != descriptor.len()
            || descriptor[1] != 0x03
        {
            return Err(MirrorRejectReason::InvalidDescriptorLength);
        }
        if image
            .strings()
            .filter(|other| other.index == record.index && other.lang_id == record.lang_id)
            .count()
            != 1
        {
            return Err(MirrorRejectReason::MalformedImage);
        }
    }
    Ok(())
}

fn collect_reports<'a>(
    image: &MirrorImage<'a>,
) -> Result<Vec<(u8, &'a [u8]), MIRROR_HID_INTERFACES_MAX>, MirrorRejectReason> {
    let mut reports = Vec::new();
    for report in image.hid_reports() {
        if report.descriptor.is_empty() {
            return Err(MirrorRejectReason::MissingReportDescriptor);
        }
        if reports
            .iter()
            .any(|(interface_number, _)| *interface_number == report.interface_number)
        {
            return Err(MirrorRejectReason::MalformedImage);
        }
        reports
            .push((report.interface_number, report.descriptor))
            .map_err(|_| MirrorRejectReason::EndpointResourceExhausted)?;
    }
    Ok(reports)
}

fn validate_configuration<'a>(
    image: &MirrorImage<'a>,
    device_descriptor: [u8; 18],
    reports: &Vec<(u8, &'a [u8]), MIRROR_HID_INTERFACES_MAX>,
) -> Result<UsbDevicePlan<'a>, MirrorRejectReason> {
    let config = image.configuration_descriptor;
    if config.len() < 9 || config[0] != 9 || config[1] != 0x02 {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    if usize::from(u16::from_le_bytes([config[2], config[3]])) != config.len() {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    let mut interfaces = Vec::<HidInterfacePlan<'a>, MIRROR_HID_INTERFACES_MAX>::new();
    let mut endpoints = Vec::<EndpointPlan, MIRROR_ENDPOINTS_MAX>::new();
    let mut endpoint_addresses = Vec::<u8, MIRROR_ENDPOINTS_MAX>::new();
    let mut current_interface: Option<ParsedInterface<'a>> = None;
    let mut offset = 9;
    while offset < config.len() {
        let length = descriptor_length(config, offset)?;
        let descriptor = &config[offset..offset + length];
        match descriptor[1] {
            0x02 => return Err(MirrorRejectReason::MultipleConfigurations),
            0x04 => {
                finish_interface(&mut interfaces, reports, current_interface.take())?;
                if length < 9 {
                    return Err(MirrorRejectReason::InvalidDescriptorLength);
                }
                if descriptor[3] != 0 {
                    return Err(MirrorRejectReason::UnsupportedAlternateSetting);
                }
                if descriptor[5] != 0x03 {
                    return Err(MirrorRejectReason::NonHidInterface);
                }
                if interfaces
                    .iter()
                    .any(|plan| plan.interface_number == descriptor[2])
                {
                    return Err(MirrorRejectReason::MalformedImage);
                }
                current_interface = Some(ParsedInterface {
                    number: descriptor[2],
                    subclass: descriptor[6],
                    protocol: descriptor[7],
                    expected_endpoints: descriptor[4],
                    actual_endpoints: 0,
                    hid_descriptor: None,
                });
            }
            0x21 => {
                let Some(current) = current_interface.as_mut() else {
                    return Err(MirrorRejectReason::MalformedImage);
                };
                if length < 9 || descriptor[5] == 0 {
                    return Err(MirrorRejectReason::InvalidDescriptorLength);
                }
                let mut hid_offset = 6;
                let mut declared_report = None;
                for _ in 0..descriptor[5] {
                    if hid_offset + 3 > descriptor.len() {
                        return Err(MirrorRejectReason::InvalidDescriptorLength);
                    }
                    if descriptor[hid_offset] == 0x22 {
                        declared_report = Some(u16::from_le_bytes([
                            descriptor[hid_offset + 1],
                            descriptor[hid_offset + 2],
                        ]));
                    }
                    hid_offset += 3;
                }
                if declared_report.is_none() || current.hid_descriptor.is_some() {
                    return Err(MirrorRejectReason::InvalidDescriptorLength);
                }
                current.hid_descriptor = Some(descriptor);
            }
            0x05 => {
                let Some(current) = current_interface.as_mut() else {
                    return Err(MirrorRejectReason::MalformedImage);
                };
                if length < 7 {
                    return Err(MirrorRejectReason::InvalidDescriptorLength);
                }
                if descriptor[3] & 0x03 != 0x03 {
                    return Err(MirrorRejectReason::UnsupportedEndpointType);
                }
                let address = descriptor[2];
                if address & 0x0f == 0 || address & 0x70 != 0 {
                    return Err(MirrorRejectReason::MalformedImage);
                }
                if endpoint_addresses.contains(&address) {
                    return Err(MirrorRejectReason::DuplicateEndpointAddress);
                }
                let max_packet_size = u16::from_le_bytes([descriptor[4], descriptor[5]]);
                if max_packet_size == 0 || max_packet_size > 64 {
                    return Err(MirrorRejectReason::PacketTooLarge);
                }
                let direction_count = endpoints
                    .iter()
                    .filter(|endpoint| endpoint.address & 0x80 == address & 0x80)
                    .count();
                if direction_count >= 4 {
                    return Err(MirrorRejectReason::EndpointResourceExhausted);
                }
                endpoint_addresses
                    .push(address)
                    .map_err(|_| MirrorRejectReason::EndpointResourceExhausted)?;
                endpoints
                    .push(EndpointPlan {
                        interface_number: current.number,
                        address,
                        max_packet_size,
                        interval: descriptor[6],
                    })
                    .map_err(|_| MirrorRejectReason::EndpointResourceExhausted)?;
                current.actual_endpoints += 1;
            }
            _ => {}
        }
        offset += length;
    }
    finish_interface(&mut interfaces, reports, current_interface)?;
    if interfaces.len() != usize::from(config[4]) || interfaces.is_empty() {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    if reports.len() != interfaces.len() {
        return Err(MirrorRejectReason::MissingReportDescriptor);
    }

    Ok(UsbDevicePlan {
        device_descriptor,
        configuration_descriptor: config,
        bos_descriptor: image.bos_descriptor,
        strings: image.string_table(),
        hid_reports: image.hid_report_table(),
        interfaces,
        endpoints,
    })
}

fn finish_interface<'a>(
    interfaces: &mut Vec<HidInterfacePlan<'a>, MIRROR_HID_INTERFACES_MAX>,
    reports: &Vec<(u8, &'a [u8]), MIRROR_HID_INTERFACES_MAX>,
    current: Option<ParsedInterface<'a>>,
) -> Result<(), MirrorRejectReason> {
    let Some(current) = current else {
        return Ok(());
    };
    if usize::from(current.expected_endpoints) != current.actual_endpoints {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    let report = reports
        .iter()
        .find(|(interface_number, _)| *interface_number == current.number)
        .map(|(_, descriptor)| *descriptor)
        .ok_or(MirrorRejectReason::MissingReportDescriptor)?;
    let hid_descriptor = current
        .hid_descriptor
        .ok_or(MirrorRejectReason::InvalidDescriptorLength)?;
    let report_length = u16::from_le_bytes([hid_descriptor[7], hid_descriptor[8]]);
    if report_length != report.len() as u16 {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    interfaces
        .push(HidInterfacePlan {
            interface_number: current.number,
            subclass: current.subclass,
            protocol: current.protocol,
            hid_descriptor,
            report_descriptor: report,
        })
        .map_err(|_| MirrorRejectReason::EndpointResourceExhausted)
}

struct ParsedInterface<'a> {
    number: u8,
    subclass: u8,
    protocol: u8,
    expected_endpoints: u8,
    actual_endpoints: usize,
    hid_descriptor: Option<&'a [u8]>,
}

fn descriptor_length(bytes: &[u8], offset: usize) -> Result<usize, MirrorRejectReason> {
    if offset + 2 > bytes.len() {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    let length = usize::from(bytes[offset]);
    if length < 2 || offset + length > bytes.len() {
        return Err(MirrorRejectReason::InvalidDescriptorLength);
    }
    Ok(length)
}

fn walk_descriptors(bytes: &[u8], mut offset: usize) -> Result<(), MirrorRejectReason> {
    while offset < bytes.len() {
        offset += descriptor_length(bytes, offset)?;
    }
    Ok(())
}

const fn map_parse_error(error: MirrorImageParseError) -> MirrorRejectReason {
    match error {
        MirrorImageParseError::ImageTooLarge => MirrorRejectReason::ImageTooLarge,
        MirrorImageParseError::InvalidHeaderLength | MirrorImageParseError::InvalidRecordLength => {
            MirrorRejectReason::InvalidDescriptorLength
        }
        MirrorImageParseError::UnsupportedVersion => MirrorRejectReason::MalformedImage,
        _ => MirrorRejectReason::MalformedImage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::image::{HidReportRecord, MirrorImageSource, serialize_mirror_image};

    const DEVICE: [u8; 18] = [
        18, 1, 0x00, 0x02, 0, 0, 0, 64, 0xfe, 0xca, 1, 0x40, 0, 1, 0, 0, 0, 1,
    ];
    const REPORT: [u8; 2] = [0x05, 0x01];
    const CONFIG: [u8; 34] = [
        9, 2, 34, 0, 1, 1, 0, 0x80, 50, // configuration
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 2, 0, // HID
        7, 5, 0x81, 3, 8, 0, 1, // interrupt IN
    ];

    fn encode<'a>(
        config: &'a [u8],
        reports: &'a [HidReportRecord<'a>],
        out: &'a mut [u8],
    ) -> &'a [u8] {
        let length = serialize_mirror_image(
            MirrorImageSource {
                flags: 0,
                device_descriptor: &DEVICE,
                configuration_descriptor: config,
                bos_descriptor: &[],
                strings: &[],
                hid_reports: reports,
            },
            out,
        )
        .unwrap();
        &out[..length]
    }

    #[test]
    fn valid_hid_image_builds_endpoint_plan_without_rewriting_addresses() {
        let reports = [HidReportRecord {
            interface_number: 0,
            descriptor: &REPORT,
        }];
        let mut out = [0; 256];
        let plan = validate_mirror_image(encode(&CONFIG, &reports, &mut out)).unwrap();

        assert_eq!(plan.interfaces.len(), 1);
        assert_eq!(plan.interfaces[0].report_descriptor, REPORT);
        assert_eq!(plan.endpoints.len(), 1);
        assert_eq!(plan.endpoints[0].address, 0x81);
        assert_eq!(plan.endpoints[0].max_packet_size, 8);
        assert_eq!(plan.interface_descriptor(0, 0x21), Some(&CONFIG[18..27]));
        assert_eq!(plan.interface_descriptor(0, 0x22), Some(REPORT.as_slice()));
        assert_eq!(plan.device_descriptor(0x02, 0, 0), Some(CONFIG.as_slice()));
    }

    #[test]
    fn duplicate_endpoint_across_interfaces_is_rejected_explicitly() {
        let config = [
            9, 2, 59, 0, 2, 1, 0, 0x80, 50, // configuration
            9, 4, 0, 0, 1, 3, 1, 1, 0, // interface 0
            9, 0x21, 0x11, 0x01, 0, 1, 0x22, 2, 0, // HID
            7, 5, 0x81, 3, 8, 0, 1, // endpoint 0x81
            9, 4, 1, 0, 1, 3, 0, 0, 0, // interface 1
            9, 0x21, 0x11, 0x01, 0, 1, 0x22, 2, 0, // HID
            7, 5, 0x81, 3, 64, 0, 1, // duplicate endpoint 0x81
        ];
        let reports = [
            HidReportRecord {
                interface_number: 0,
                descriptor: &REPORT,
            },
            HidReportRecord {
                interface_number: 1,
                descriptor: &REPORT,
            },
        ];
        let mut out = [0; 256];

        assert_eq!(
            validate_mirror_image(encode(&config, &reports, &mut out)),
            Err(MirrorRejectReason::DuplicateEndpointAddress)
        );
    }

    #[test]
    fn missing_report_descriptor_is_rejected() {
        let mut out = [0; 256];
        assert_eq!(
            validate_mirror_image(encode(&CONFIG, &[], &mut out)),
            Err(MirrorRejectReason::MissingReportDescriptor)
        );
    }

    #[test]
    fn endpoint_type_packet_size_and_remote_wakeup_are_validated() {
        let reports = [HidReportRecord {
            interface_number: 0,
            descriptor: &REPORT,
        }];
        let mut out = [0; 256];

        let mut bulk = CONFIG;
        bulk[30] = 2;
        assert_eq!(
            validate_mirror_image(encode(&bulk, &reports, &mut out)),
            Err(MirrorRejectReason::UnsupportedEndpointType)
        );

        let mut packet = CONFIG;
        packet[31] = 65;
        assert_eq!(
            validate_mirror_image(encode(&packet, &reports, &mut out)),
            Err(MirrorRejectReason::PacketTooLarge)
        );

        let mut wake = CONFIG;
        wake[7] |= 0x20;
        let plan = validate_mirror_image(encode(&wake, &reports, &mut out)).unwrap();
        assert!(plan.supports_remote_wakeup());
    }

    #[test]
    fn unsupported_interface_variants_are_rejected() {
        let reports = [HidReportRecord {
            interface_number: 0,
            descriptor: &REPORT,
        }];
        let mut out = [0; 256];

        let mut alternate = CONFIG;
        alternate[12] = 1;
        assert_eq!(
            validate_mirror_image(encode(&alternate, &reports, &mut out)),
            Err(MirrorRejectReason::UnsupportedAlternateSetting)
        );

        let mut non_hid = CONFIG;
        non_hid[14] = 0xff;
        assert_eq!(
            validate_mirror_image(encode(&non_hid, &reports, &mut out)),
            Err(MirrorRejectReason::NonHidInterface)
        );
    }

    #[test]
    fn image_size_and_descriptor_lengths_are_bounded() {
        let oversized = [0; super::super::image::HSMI_MAX_SIZE + 1];
        assert_eq!(
            validate_mirror_image(&oversized),
            Err(MirrorRejectReason::ImageTooLarge)
        );

        let reports = [HidReportRecord {
            interface_number: 0,
            descriptor: &REPORT,
        }];
        let mut malformed = CONFIG;
        malformed[9] = 8;
        let mut out = [0; 256];
        assert_eq!(
            validate_mirror_image(encode(&malformed, &reports, &mut out)),
            Err(MirrorRejectReason::InvalidDescriptorLength)
        );
    }
}
