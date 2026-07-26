pub const HSMI_MAGIC: [u8; 4] = *b"HSMI";
pub const HSMI_VERSION: u16 = 1;
pub const HSMI_HEADER_LEN: usize = 32;
pub const HSMI_MAX_SIZE: usize = 16 * 1024;
pub const STRING_RECORD_HEADER_LEN: usize = 5;
pub const HID_REPORT_RECORD_HEADER_LEN: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorImageHeader {
    pub magic: [u8; 4],
    pub version: u16,
    pub header_length: u16,
    pub total_length: u32,
    pub image_crc32: u32,
    pub flags: u32,
    pub device_length: u16,
    pub configuration_length: u16,
    pub bos_length: u16,
    pub string_count: u8,
    pub hid_report_count: u8,
    pub reserved: [u8; 4],
}

impl MirrorImageHeader {
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HSMI_HEADER_LEN {
            return None;
        }
        Some(Self {
            magic: bytes[0..4].try_into().ok()?,
            version: read_u16(&bytes[4..6]),
            header_length: read_u16(&bytes[6..8]),
            total_length: read_u32(&bytes[8..12]),
            image_crc32: read_u32(&bytes[12..16]),
            flags: read_u32(&bytes[16..20]),
            device_length: read_u16(&bytes[20..22]),
            configuration_length: read_u16(&bytes[22..24]),
            bos_length: read_u16(&bytes[24..26]),
            string_count: bytes[26],
            hid_report_count: bytes[27],
            reserved: bytes[28..32].try_into().ok()?,
        })
    }

    fn encode(self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.magic);
        write_u16(&mut out[4..6], self.version);
        write_u16(&mut out[6..8], self.header_length);
        write_u32(&mut out[8..12], self.total_length);
        write_u32(&mut out[12..16], self.image_crc32);
        write_u32(&mut out[16..20], self.flags);
        write_u16(&mut out[20..22], self.device_length);
        write_u16(&mut out[22..24], self.configuration_length);
        write_u16(&mut out[24..26], self.bos_length);
        out[26] = self.string_count;
        out[27] = self.hid_report_count;
        out[28..32].copy_from_slice(&self.reserved);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StringRecord<'a> {
    pub index: u8,
    pub lang_id: u16,
    pub descriptor: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidReportRecord<'a> {
    pub interface_number: u8,
    pub descriptor: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorImage<'a> {
    pub header: MirrorImageHeader,
    pub device_descriptor: &'a [u8],
    pub configuration_descriptor: &'a [u8],
    pub bos_descriptor: &'a [u8],
    pub(crate) string_records: &'a [u8],
    pub(crate) hid_report_records: &'a [u8],
}

impl<'a> MirrorImage<'a> {
    pub fn strings(&self) -> StringRecords<'a> {
        StringRecords {
            remaining: self.string_records,
            count: self.header.string_count,
        }
    }

    pub fn hid_reports(&self) -> HidReportRecords<'a> {
        HidReportRecords {
            remaining: self.hid_report_records,
            count: self.header.hid_report_count,
        }
    }

    pub fn string_table(&self) -> StringDescriptorTable<'a> {
        StringDescriptorTable {
            records: self.string_records,
            count: self.header.string_count,
        }
    }

    pub fn hid_report_table(&self) -> HidReportDescriptorTable<'a> {
        HidReportDescriptorTable {
            records: self.hid_report_records,
            count: self.header.hid_report_count,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StringDescriptorTable<'a> {
    records: &'a [u8],
    count: u8,
}

impl<'a> StringDescriptorTable<'a> {
    pub fn get(self, index: u8, lang_id: u16) -> Option<&'a [u8]> {
        (StringRecords {
            remaining: self.records,
            count: self.count,
        })
        .find(|record| record.index == index && record.lang_id == lang_id)
        .map(|record| record.descriptor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidReportDescriptorTable<'a> {
    records: &'a [u8],
    count: u8,
}

impl<'a> HidReportDescriptorTable<'a> {
    pub fn get(self, interface_number: u8) -> Option<&'a [u8]> {
        (HidReportRecords {
            remaining: self.records,
            count: self.count,
        })
        .find(|record| record.interface_number == interface_number)
        .map(|record| record.descriptor)
    }
}

pub struct StringRecords<'a> {
    remaining: &'a [u8],
    count: u8,
}

impl<'a> Iterator for StringRecords<'a> {
    type Item = StringRecord<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count == 0 || self.remaining.len() < STRING_RECORD_HEADER_LEN {
            return None;
        }
        let length = read_u16(&self.remaining[3..5]) as usize;
        let end = STRING_RECORD_HEADER_LEN + length;
        if end > self.remaining.len() {
            self.count = 0;
            return None;
        }
        let record = StringRecord {
            index: self.remaining[0],
            lang_id: read_u16(&self.remaining[1..3]),
            descriptor: &self.remaining[STRING_RECORD_HEADER_LEN..end],
        };
        self.remaining = &self.remaining[end..];
        self.count -= 1;
        Some(record)
    }
}

pub struct HidReportRecords<'a> {
    remaining: &'a [u8],
    count: u8,
}

impl<'a> Iterator for HidReportRecords<'a> {
    type Item = HidReportRecord<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count == 0 || self.remaining.len() < HID_REPORT_RECORD_HEADER_LEN {
            return None;
        }
        let length = read_u16(&self.remaining[1..3]) as usize;
        let end = HID_REPORT_RECORD_HEADER_LEN + length;
        if end > self.remaining.len() {
            self.count = 0;
            return None;
        }
        let record = HidReportRecord {
            interface_number: self.remaining[0],
            descriptor: &self.remaining[HID_REPORT_RECORD_HEADER_LEN..end],
        };
        self.remaining = &self.remaining[end..];
        self.count -= 1;
        Some(record)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MirrorImageSource<'a> {
    pub flags: u32,
    pub device_descriptor: &'a [u8],
    pub configuration_descriptor: &'a [u8],
    pub bos_descriptor: &'a [u8],
    pub strings: &'a [StringRecord<'a>],
    pub hid_reports: &'a [HidReportRecord<'a>],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorImageEncodeError {
    TooLarge,
    OutputTooSmall,
    LengthOverflow,
    TooManyRecords,
}

pub fn serialize_mirror_image(
    source: MirrorImageSource<'_>,
    out: &mut [u8],
) -> Result<usize, MirrorImageEncodeError> {
    let strings_length = source.strings.iter().try_fold(0usize, |total, record| {
        total
            .checked_add(STRING_RECORD_HEADER_LEN)
            .and_then(|value| value.checked_add(record.descriptor.len()))
            .ok_or(MirrorImageEncodeError::LengthOverflow)
    })?;
    let reports_length = source
        .hid_reports
        .iter()
        .try_fold(0usize, |total, record| {
            total
                .checked_add(HID_REPORT_RECORD_HEADER_LEN)
                .and_then(|value| value.checked_add(record.descriptor.len()))
                .ok_or(MirrorImageEncodeError::LengthOverflow)
        })?;
    let total_length = HSMI_HEADER_LEN
        .checked_add(source.device_descriptor.len())
        .and_then(|value| value.checked_add(source.configuration_descriptor.len()))
        .and_then(|value| value.checked_add(source.bos_descriptor.len()))
        .and_then(|value| value.checked_add(strings_length))
        .and_then(|value| value.checked_add(reports_length))
        .ok_or(MirrorImageEncodeError::LengthOverflow)?;
    if total_length > HSMI_MAX_SIZE {
        return Err(MirrorImageEncodeError::TooLarge);
    }
    if out.len() < total_length {
        return Err(MirrorImageEncodeError::OutputTooSmall);
    }
    let device_length = u16::try_from(source.device_descriptor.len())
        .map_err(|_| MirrorImageEncodeError::LengthOverflow)?;
    let configuration_length = u16::try_from(source.configuration_descriptor.len())
        .map_err(|_| MirrorImageEncodeError::LengthOverflow)?;
    let bos_length = u16::try_from(source.bos_descriptor.len())
        .map_err(|_| MirrorImageEncodeError::LengthOverflow)?;
    let string_count =
        u8::try_from(source.strings.len()).map_err(|_| MirrorImageEncodeError::TooManyRecords)?;
    let hid_report_count = u8::try_from(source.hid_reports.len())
        .map_err(|_| MirrorImageEncodeError::TooManyRecords)?;

    out[..total_length].fill(0);
    let header = MirrorImageHeader {
        magic: HSMI_MAGIC,
        version: HSMI_VERSION,
        header_length: HSMI_HEADER_LEN as u16,
        total_length: total_length as u32,
        image_crc32: 0,
        flags: source.flags,
        device_length,
        configuration_length,
        bos_length,
        string_count,
        hid_report_count,
        reserved: [0; 4],
    };
    header.encode(&mut out[..HSMI_HEADER_LEN]);
    let mut offset = HSMI_HEADER_LEN;
    for descriptor in [
        source.device_descriptor,
        source.configuration_descriptor,
        source.bos_descriptor,
    ] {
        out[offset..offset + descriptor.len()].copy_from_slice(descriptor);
        offset += descriptor.len();
    }
    for record in source.strings {
        out[offset] = record.index;
        write_u16(&mut out[offset + 1..offset + 3], record.lang_id);
        write_u16(
            &mut out[offset + 3..offset + 5],
            u16::try_from(record.descriptor.len())
                .map_err(|_| MirrorImageEncodeError::LengthOverflow)?,
        );
        offset += STRING_RECORD_HEADER_LEN;
        out[offset..offset + record.descriptor.len()].copy_from_slice(record.descriptor);
        offset += record.descriptor.len();
    }
    for record in source.hid_reports {
        out[offset] = record.interface_number;
        write_u16(
            &mut out[offset + 1..offset + 3],
            u16::try_from(record.descriptor.len())
                .map_err(|_| MirrorImageEncodeError::LengthOverflow)?,
        );
        offset += HID_REPORT_RECORD_HEADER_LEN;
        out[offset..offset + record.descriptor.len()].copy_from_slice(record.descriptor);
        offset += record.descriptor.len();
    }
    let crc = mirror_image_crc32(&out[..total_length]);
    write_u32(&mut out[12..16], crc);
    Ok(total_length)
}

pub(crate) fn mirror_image_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let byte = if (12..16).contains(&index) { 0 } else { byte };
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn write_u16(out: &mut [u8], value: u16) {
    out.copy_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut [u8], value: u32) {
    out.copy_from_slice(&value.to_le_bytes());
}
