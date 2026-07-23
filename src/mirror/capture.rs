use crate::checksum::{Crc32, crc32_ieee};
use crate::mirror::{
    HidReportRecord, MirrorImageEncodeError, MirrorImageSource, MirrorRejectReason, StringRecord,
    serialize_mirror_image, validate_mirror_image,
};
use crate::output_target::{MirrorStableId, MirrorStableIdError};

#[derive(Clone, Copy, Debug)]
pub struct MirrorCaptureSource<'a> {
    pub flags: u32,
    pub device_descriptor: &'a [u8],
    pub configuration_descriptor: &'a [u8],
    pub bos_descriptor: &'a [u8],
    pub strings: &'a [StringRecord<'a>],
    pub hid_reports: &'a [HidReportRecord<'a>],
    pub port_path: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapturedMirrorProfile {
    pub stable_id: MirrorStableId,
    pub profile_hash: u32,
    pub image_length: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorCaptureError {
    InvalidDeviceDescriptor,
    StableIdentity(MirrorStableIdError),
    Encode(MirrorImageEncodeError),
    Rejected(MirrorRejectReason),
}

pub fn capture_mirror_profile(
    source: MirrorCaptureSource<'_>,
    out: &mut [u8],
) -> Result<CapturedMirrorProfile, MirrorCaptureError> {
    if source.device_descriptor.len() != 18 || source.device_descriptor[1] != 1 {
        return Err(MirrorCaptureError::InvalidDeviceDescriptor);
    }
    let vendor_id = u16::from_le_bytes([source.device_descriptor[8], source.device_descriptor[9]]);
    let product_id =
        u16::from_le_bytes([source.device_descriptor[10], source.device_descriptor[11]]);
    let serial_index = source.device_descriptor[16];
    let serial_hash = source
        .strings
        .iter()
        .find(|record| record.index == serial_index && serial_index != 0)
        .map(|record| crc32_ieee(record.descriptor));

    let mut fingerprint = Crc32::new();
    fingerprint.update(source.device_descriptor);
    fingerprint.update(source.configuration_descriptor);
    fingerprint.update(source.bos_descriptor);
    for report in source.hid_reports {
        fingerprint.update(&[report.interface_number]);
        fingerprint.update(report.descriptor);
    }
    let stable_id = MirrorStableId::new(
        vendor_id,
        product_id,
        serial_hash,
        fingerprint.finalize(),
        source.port_path,
    )
    .map_err(MirrorCaptureError::StableIdentity)?;

    let image_length = serialize_mirror_image(
        MirrorImageSource {
            flags: source.flags,
            device_descriptor: source.device_descriptor,
            configuration_descriptor: source.configuration_descriptor,
            bos_descriptor: source.bos_descriptor,
            strings: source.strings,
            hid_reports: source.hid_reports,
        },
        out,
    )
    .map_err(MirrorCaptureError::Encode)?;
    validate_mirror_image(&out[..image_length]).map_err(MirrorCaptureError::Rejected)?;

    Ok(CapturedMirrorProfile {
        stable_id,
        profile_hash: crc32_ieee(&out[..image_length]),
        image_length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::parse_mirror_image;

    #[test]
    fn fixture_capture_rebuilds_a_valid_profile_and_stable_identity() {
        let fixture = include_bytes!("../../e2e/fixtures/mirror/composite-a.hsmi");
        let parsed = parse_mirror_image(fixture).unwrap();
        let strings: std::vec::Vec<_> = parsed.strings().collect();
        let reports: std::vec::Vec<_> = parsed.hid_reports().collect();
        let mut output = [0; crate::mirror::HSMI_MAX_SIZE];
        let captured = capture_mirror_profile(
            MirrorCaptureSource {
                flags: parsed.header.flags,
                device_descriptor: parsed.device_descriptor,
                configuration_descriptor: parsed.configuration_descriptor,
                bos_descriptor: parsed.bos_descriptor,
                strings: &strings,
                hid_reports: &reports,
                port_path: &[2],
            },
            &mut output,
        )
        .unwrap();

        assert_eq!(captured.stable_id.vendor_id, 0xcafe);
        assert_eq!(captured.stable_id.product_id, 0x4001);
        assert!(captured.stable_id.serial_hash.is_some());
        assert!(validate_mirror_image(&output[..captured.image_length]).is_ok());
    }

    #[test]
    fn non_serial_device_requires_a_port_path() {
        let fixture = include_bytes!("../../e2e/fixtures/mirror/composite-a.hsmi");
        let parsed = parse_mirror_image(fixture).unwrap();
        let mut device = <[u8; 18]>::try_from(parsed.device_descriptor).unwrap();
        device[16] = 0;
        let reports: std::vec::Vec<_> = parsed.hid_reports().collect();
        let mut output = [0; crate::mirror::HSMI_MAX_SIZE];

        assert_eq!(
            capture_mirror_profile(
                MirrorCaptureSource {
                    flags: 0,
                    device_descriptor: &device,
                    configuration_descriptor: parsed.configuration_descriptor,
                    bos_descriptor: parsed.bos_descriptor,
                    strings: &[],
                    hid_reports: &reports,
                    port_path: &[],
                },
                &mut output,
            ),
            Err(MirrorCaptureError::StableIdentity(
                MirrorStableIdError::MissingPhysicalLocation
            ))
        );
    }
}
