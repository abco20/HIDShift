use super::image::{
    HID_REPORT_RECORD_HEADER_LEN, HSMI_HEADER_LEN, HSMI_MAGIC, HSMI_MAX_SIZE, HSMI_VERSION,
    MirrorImage, MirrorImageHeader, STRING_RECORD_HEADER_LEN, mirror_image_crc32,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorImageParseError {
    ImageTooLarge,
    Truncated,
    InvalidMagic,
    UnsupportedVersion,
    InvalidHeaderLength,
    InvalidTotalLength,
    InvalidReservedBytes,
    InvalidRecordLength,
    CrcMismatch,
    TrailingBytes,
}

pub fn parse_mirror_image(bytes: &[u8]) -> Result<MirrorImage<'_>, MirrorImageParseError> {
    if bytes.len() > HSMI_MAX_SIZE {
        return Err(MirrorImageParseError::ImageTooLarge);
    }
    let header = MirrorImageHeader::decode(bytes).ok_or(MirrorImageParseError::Truncated)?;
    if header.magic != HSMI_MAGIC {
        return Err(MirrorImageParseError::InvalidMagic);
    }
    if header.version != HSMI_VERSION {
        return Err(MirrorImageParseError::UnsupportedVersion);
    }
    if usize::from(header.header_length) != HSMI_HEADER_LEN {
        return Err(MirrorImageParseError::InvalidHeaderLength);
    }
    let total_length = usize::try_from(header.total_length)
        .map_err(|_| MirrorImageParseError::InvalidTotalLength)?;
    if !(HSMI_HEADER_LEN..=HSMI_MAX_SIZE).contains(&total_length) {
        return Err(MirrorImageParseError::InvalidTotalLength);
    }
    if bytes.len() < total_length {
        return Err(MirrorImageParseError::Truncated);
    }
    if bytes.len() > total_length {
        return Err(MirrorImageParseError::TrailingBytes);
    }
    if header.reserved != [0; 4] {
        return Err(MirrorImageParseError::InvalidReservedBytes);
    }
    if mirror_image_crc32(bytes) != header.image_crc32 {
        return Err(MirrorImageParseError::CrcMismatch);
    }

    let mut offset = HSMI_HEADER_LEN;
    let device_descriptor = take(bytes, &mut offset, usize::from(header.device_length))?;
    let configuration_descriptor =
        take(bytes, &mut offset, usize::from(header.configuration_length))?;
    let bos_descriptor = take(bytes, &mut offset, usize::from(header.bos_length))?;

    let string_start = offset;
    for _ in 0..header.string_count {
        let record_header = take(bytes, &mut offset, STRING_RECORD_HEADER_LEN)?;
        let descriptor_length = u16::from_le_bytes([record_header[3], record_header[4]]) as usize;
        take(bytes, &mut offset, descriptor_length)?;
    }
    let string_records = &bytes[string_start..offset];

    let report_start = offset;
    for _ in 0..header.hid_report_count {
        let record_header = take(bytes, &mut offset, HID_REPORT_RECORD_HEADER_LEN)?;
        let descriptor_length = u16::from_le_bytes([record_header[1], record_header[2]]) as usize;
        take(bytes, &mut offset, descriptor_length)?;
    }
    let hid_report_records = &bytes[report_start..offset];
    if offset != total_length {
        return Err(MirrorImageParseError::TrailingBytes);
    }

    Ok(MirrorImage {
        header,
        device_descriptor,
        configuration_descriptor,
        bos_descriptor,
        string_records,
        hid_report_records,
    })
}

fn take<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    length: usize,
) -> Result<&'a [u8], MirrorImageParseError> {
    let end = offset
        .checked_add(length)
        .ok_or(MirrorImageParseError::InvalidRecordLength)?;
    let value = bytes
        .get(*offset..end)
        .ok_or(MirrorImageParseError::Truncated)?;
    *offset = end;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::image::{
        HidReportRecord, MirrorImageSource, StringRecord, serialize_mirror_image,
    };

    const DEVICE: [u8; 18] = [
        18, 1, 0x00, 0x02, 0, 0, 0, 64, 0xfe, 0xca, 1, 0x40, 0, 1, 1, 2, 3, 1,
    ];
    const CONFIG: [u8; 9] = [9, 2, 9, 0, 0, 1, 0, 0x80, 50];
    const STRING: [u8; 4] = [4, 3, b'A', 0];
    const REPORT: [u8; 2] = [0x05, 0x01];

    fn encoded() -> ([u8; 256], usize) {
        let strings = [StringRecord {
            index: 1,
            lang_id: 0x0409,
            descriptor: &STRING,
        }];
        let reports = [HidReportRecord {
            interface_number: 0,
            descriptor: &REPORT,
        }];
        let mut out = [0; 256];
        let length = serialize_mirror_image(
            MirrorImageSource {
                flags: 7,
                device_descriptor: &DEVICE,
                configuration_descriptor: &CONFIG,
                bos_descriptor: &[],
                strings: &strings,
                hid_reports: &reports,
            },
            &mut out,
        )
        .unwrap();
        (out, length)
    }

    #[test]
    fn image_serialization_and_parsing_round_trip() {
        let (bytes, length) = encoded();
        let parsed = parse_mirror_image(&bytes[..length]).unwrap();

        assert_eq!(parsed.header.flags, 7);
        assert_eq!(parsed.device_descriptor, DEVICE);
        assert_eq!(parsed.configuration_descriptor, CONFIG);
        assert_eq!(parsed.strings().next().unwrap().descriptor, STRING);
        assert_eq!(parsed.hid_reports().next().unwrap().descriptor, REPORT);
        assert_eq!(
            parsed.string_table().get(1, 0x0409),
            Some(STRING.as_slice())
        );
        assert_eq!(parsed.hid_report_table().get(0), Some(REPORT.as_slice()));
    }

    #[test]
    fn crc_truncation_and_trailing_bytes_are_rejected() {
        let (mut bytes, length) = encoded();
        assert_eq!(
            parse_mirror_image(&bytes[..length - 1]),
            Err(MirrorImageParseError::Truncated)
        );

        bytes[20] ^= 1;
        assert_eq!(
            parse_mirror_image(&bytes[..length]),
            Err(MirrorImageParseError::CrcMismatch)
        );

        let (mut bytes, length) = encoded();
        bytes[length] = 0xaa;
        assert_eq!(
            parse_mirror_image(&bytes[..length + 1]),
            Err(MirrorImageParseError::TrailingBytes)
        );
    }
}
