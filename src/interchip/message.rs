use crate::reports::{
    CONSUMER_REPORT_LEN, ConsumerReport, KEYBOARD_REPORT_LEN, Keyboard6KroReport, MOUSE_REPORT_LEN,
    MouseReport, StandardHidReport,
};

pub const RECORD_HELLO: u8 = 0x01;
pub const RECORD_HELLO_ACK: u8 = 0x02;
pub const RECORD_HEARTBEAT: u8 = 0x03;
pub const RECORD_LINK_RESET: u8 = 0x04;
pub const RECORD_PROFILE_BEGIN: u8 = 0x10;
pub const RECORD_PROFILE_CHUNK: u8 = 0x11;
pub const RECORD_PROFILE_COMMIT: u8 = 0x12;
pub const RECORD_PROFILE_RESULT: u8 = 0x13;
pub const RECORD_ACTIVATE_PROFILE: u8 = 0x14;
pub const RECORD_FORCE_FALLBACK: u8 = 0x15;
pub const RECORD_USB_STATE: u8 = 0x20;
pub const RECORD_CONTROL_REQUEST: u8 = 0x21;
pub const RECORD_CONTROL_RESPONSE: u8 = 0x22;
pub const RECORD_CONTROL_CANCEL: u8 = 0x23;
pub const RECORD_RAW_ENDPOINT_IN: u8 = 0x24;
pub const RECORD_RAW_ENDPOINT_OUT: u8 = 0x25;
pub const RECORD_STANDARD_INPUT_REPORT: u8 = 0x26;
pub const RECORD_STANDARD_OUTPUT_REPORT: u8 = 0x27;
pub const RECORD_STANDARD_RELEASE_ALL: u8 = 0x28;
pub const RECORD_GET_DIAGNOSTICS: u8 = 0x30;
pub const RECORD_DIAGNOSTICS: u8 = 0x31;
pub const RECORD_TEST_FAULT: u8 = 0x32;

pub const HELLO_WIRE_LEN: usize = 12;
pub const USB_STATE_WIRE_LEN: usize = 8;
pub const STANDARD_INPUT_HEADER_LEN: usize = 5;
pub const STANDARD_INPUT_MAX_LEN: usize = STANDARD_INPUT_HEADER_LEN + KEYBOARD_REPORT_LEN;
pub const STANDARD_OUTPUT_MAX_DATA_LEN: usize = 8;
pub const STANDARD_OUTPUT_WIRE_LEN: usize = 2 + STANDARD_OUTPUT_MAX_DATA_LEN;
pub const PROFILE_BEGIN_WIRE_LEN: usize = 16;
pub const PROFILE_CHUNK_HEADER_LEN: usize = 8;
pub const PROFILE_CHUNK_MAX_DATA_LEN: usize = 96;
pub const PROFILE_RESULT_WIRE_LEN: usize = 12;
pub const ACTIVATE_PROFILE_WIRE_LEN: usize = 8;
pub const RAW_ENDPOINT_HEADER_LEN: usize = 6;
pub const RAW_ENDPOINT_MAX_DATA_LEN: usize = 64;
pub const RAW_ENDPOINT_MAX_WIRE_LEN: usize = RAW_ENDPOINT_HEADER_LEN + RAW_ENDPOINT_MAX_DATA_LEN;
pub const CONTROL_REQUEST_HEADER_LEN: usize = 14;
pub const CONTROL_RESPONSE_HEADER_LEN: usize = 7;
pub const CONTROL_DATA_MAX_LEN: usize = 256;
pub const CONTROL_REQUEST_MAX_WIRE_LEN: usize = CONTROL_REQUEST_HEADER_LEN + CONTROL_DATA_MAX_LEN;
pub const CONTROL_RESPONSE_MAX_WIRE_LEN: usize = CONTROL_RESPONSE_HEADER_LEN + CONTROL_DATA_MAX_LEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum InterchipRole {
    Host = 1,
    Device = 2,
}

pub const CAPABILITY_DYNAMIC_PROFILE: u32 = 1 << 0;
pub const CAPABILITY_FALLBACK_PROFILE: u32 = 1 << 1;
pub const CAPABILITY_CONTROL_FORWARDING: u32 = 1 << 2;
pub const CAPABILITY_ENDPOINT_IN: u32 = 1 << 3;
pub const CAPABILITY_ENDPOINT_OUT: u32 = 1 << 4;
pub const CAPABILITY_PROFILE_FLASH_CACHE: u32 = 1 << 5;
pub const CAPABILITY_STANDARD_WIRED_HID: u32 = 1 << 6;
pub const CAPABILITY_USB_STATE_REPORTING: u32 = 1 << 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hello {
    pub role: InterchipRole,
    pub protocol_version: u8,
    pub firmware_major: u8,
    pub firmware_minor: u8,
    pub capabilities: u32,
    pub active_profile_hash: u32,
}

impl Hello {
    pub const fn encode(self) -> [u8; HELLO_WIRE_LEN] {
        let mut bytes = [0; HELLO_WIRE_LEN];
        bytes[0] = self.role as u8;
        bytes[1] = self.protocol_version;
        bytes[2] = self.firmware_major;
        bytes[3] = self.firmware_minor;
        let capabilities = self.capabilities.to_le_bytes();
        bytes[4] = capabilities[0];
        bytes[5] = capabilities[1];
        bytes[6] = capabilities[2];
        bytes[7] = capabilities[3];
        let profile = self.active_profile_hash.to_le_bytes();
        bytes[8] = profile[0];
        bytes[9] = profile[1];
        bytes[10] = profile[2];
        bytes[11] = profile[3];
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let [
            role,
            protocol_version,
            firmware_major,
            firmware_minor,
            c0,
            c1,
            c2,
            c3,
            h0,
            h1,
            h2,
            h3,
        ] = bytes
        else {
            return Err(MessageError::InvalidLength);
        };
        let role = match *role {
            1 => InterchipRole::Host,
            2 => InterchipRole::Device,
            _ => return Err(MessageError::InvalidRole),
        };
        Ok(Self {
            role,
            protocol_version: *protocol_version,
            firmware_major: *firmware_major,
            firmware_minor: *firmware_minor,
            capabilities: u32::from_le_bytes([*c0, *c1, *c2, *c3]),
            active_profile_hash: u32::from_le_bytes([*h0, *h1, *h2, *h3]),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileBegin {
    pub transfer_id: u32,
    pub total_length: u32,
    pub crc32: u32,
    pub profile_hash: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawEndpointReport {
    pub endpoint_address: u8,
    pub packet_sequence: u16,
    length: u8,
    data: [u8; RAW_ENDPOINT_MAX_DATA_LEN],
}

impl RawEndpointReport {
    pub fn new(
        endpoint_address: u8,
        packet_sequence: u16,
        data: &[u8],
    ) -> Result<Self, MessageError> {
        if data.len() > RAW_ENDPOINT_MAX_DATA_LEN {
            return Err(MessageError::InvalidLength);
        }
        let mut report = Self {
            endpoint_address,
            packet_sequence,
            length: data.len() as u8,
            data: [0; RAW_ENDPOINT_MAX_DATA_LEN],
        };
        report.data[..data.len()].copy_from_slice(data);
        Ok(report)
    }

    pub const fn data(&self) -> &[u8] {
        self.data.split_at(self.length as usize).0
    }

    pub fn encode(self, out: &mut [u8]) -> Result<usize, MessageError> {
        let length = RAW_ENDPOINT_HEADER_LEN + self.data().len();
        if out.len() < length {
            return Err(MessageError::InvalidLength);
        }
        out[0] = self.endpoint_address;
        out[1] = 0;
        out[2..4].copy_from_slice(&self.packet_sequence.to_le_bytes());
        out[4..6].copy_from_slice(&(self.data().len() as u16).to_le_bytes());
        out[6..length].copy_from_slice(self.data());
        Ok(length)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        if bytes.len() < RAW_ENDPOINT_HEADER_LEN
            || bytes[1] != 0
            || usize::from(read_u16(&bytes[4..6])) != bytes.len() - RAW_ENDPOINT_HEADER_LEN
        {
            return Err(MessageError::InvalidLength);
        }
        Self::new(bytes[0], read_u16(&bytes[2..4]), &bytes[6..])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorControlRequest {
    pub request_id: u32,
    pub setup_packet: [u8; 8],
    length: u16,
    data: [u8; CONTROL_DATA_MAX_LEN],
}

impl MirrorControlRequest {
    pub fn new(request_id: u32, setup_packet: [u8; 8], data: &[u8]) -> Result<Self, MessageError> {
        if data.len() > CONTROL_DATA_MAX_LEN {
            return Err(MessageError::InvalidLength);
        }
        let mut request = Self {
            request_id,
            setup_packet,
            length: data.len() as u16,
            data: [0; CONTROL_DATA_MAX_LEN],
        };
        request.data[..data.len()].copy_from_slice(data);
        Ok(request)
    }

    pub const fn data(&self) -> &[u8] {
        self.data.split_at(self.length as usize).0
    }

    pub fn encode(self, out: &mut [u8]) -> Result<usize, MessageError> {
        let length = CONTROL_REQUEST_HEADER_LEN + self.data().len();
        if out.len() < length {
            return Err(MessageError::InvalidLength);
        }
        out[..4].copy_from_slice(&self.request_id.to_le_bytes());
        out[4..12].copy_from_slice(&self.setup_packet);
        out[12..14].copy_from_slice(&(self.data().len() as u16).to_le_bytes());
        out[14..length].copy_from_slice(self.data());
        Ok(length)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        if bytes.len() < CONTROL_REQUEST_HEADER_LEN
            || usize::from(read_u16(&bytes[12..14])) != bytes.len() - CONTROL_REQUEST_HEADER_LEN
        {
            return Err(MessageError::InvalidLength);
        }
        let mut setup_packet = [0; 8];
        setup_packet.copy_from_slice(&bytes[4..12]);
        Self::new(read_u32(&bytes[..4]), setup_packet, &bytes[14..])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ControlStatus {
    Success = 0,
    Stall = 1,
    Timeout = 2,
    Disconnected = 3,
    Unsupported = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorControlResponse {
    pub request_id: u32,
    pub status: ControlStatus,
    length: u16,
    data: [u8; CONTROL_DATA_MAX_LEN],
}

impl MirrorControlResponse {
    pub fn new(request_id: u32, status: ControlStatus, data: &[u8]) -> Result<Self, MessageError> {
        if data.len() > CONTROL_DATA_MAX_LEN {
            return Err(MessageError::InvalidLength);
        }
        let mut response = Self {
            request_id,
            status,
            length: data.len() as u16,
            data: [0; CONTROL_DATA_MAX_LEN],
        };
        response.data[..data.len()].copy_from_slice(data);
        Ok(response)
    }

    pub const fn data(&self) -> &[u8] {
        self.data.split_at(self.length as usize).0
    }

    pub fn encode(self, out: &mut [u8]) -> Result<usize, MessageError> {
        let length = CONTROL_RESPONSE_HEADER_LEN + self.data().len();
        if out.len() < length {
            return Err(MessageError::InvalidLength);
        }
        out[..4].copy_from_slice(&self.request_id.to_le_bytes());
        out[4] = self.status as u8;
        out[5..7].copy_from_slice(&(self.data().len() as u16).to_le_bytes());
        out[7..length].copy_from_slice(self.data());
        Ok(length)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        if bytes.len() < CONTROL_RESPONSE_HEADER_LEN
            || usize::from(read_u16(&bytes[5..7])) != bytes.len() - CONTROL_RESPONSE_HEADER_LEN
        {
            return Err(MessageError::InvalidLength);
        }
        let status = match bytes[4] {
            0 => ControlStatus::Success,
            1 => ControlStatus::Stall,
            2 => ControlStatus::Timeout,
            3 => ControlStatus::Disconnected,
            4 => ControlStatus::Unsupported,
            _ => return Err(MessageError::InvalidStatus),
        };
        Self::new(read_u32(&bytes[..4]), status, &bytes[7..])
    }
}

impl ProfileBegin {
    pub const fn encode(self) -> [u8; PROFILE_BEGIN_WIRE_LEN] {
        let transfer = self.transfer_id.to_le_bytes();
        let length = self.total_length.to_le_bytes();
        let crc = self.crc32.to_le_bytes();
        let hash = self.profile_hash.to_le_bytes();
        [
            transfer[0],
            transfer[1],
            transfer[2],
            transfer[3],
            length[0],
            length[1],
            length[2],
            length[3],
            crc[0],
            crc[1],
            crc[2],
            crc[3],
            hash[0],
            hash[1],
            hash[2],
            hash[3],
        ]
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        if bytes.len() != PROFILE_BEGIN_WIRE_LEN {
            return Err(MessageError::InvalidLength);
        }
        Ok(Self {
            transfer_id: read_u32(&bytes[0..4]),
            total_length: read_u32(&bytes[4..8]),
            crc32: read_u32(&bytes[8..12]),
            profile_hash: read_u32(&bytes[12..16]),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileChunk<'a> {
    pub transfer_id: u32,
    pub offset: u32,
    pub data: &'a [u8],
}

impl<'a> ProfileChunk<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, MessageError> {
        if !(PROFILE_CHUNK_HEADER_LEN..=PROFILE_CHUNK_HEADER_LEN + PROFILE_CHUNK_MAX_DATA_LEN)
            .contains(&bytes.len())
        {
            return Err(MessageError::InvalidLength);
        }
        Ok(Self {
            transfer_id: read_u32(&bytes[0..4]),
            offset: read_u32(&bytes[4..8]),
            data: &bytes[PROFILE_CHUNK_HEADER_LEN..],
        })
    }

    pub fn encode(self, out: &mut [u8]) -> Result<usize, MessageError> {
        let length = PROFILE_CHUNK_HEADER_LEN + self.data.len();
        if self.data.len() > PROFILE_CHUNK_MAX_DATA_LEN || out.len() < length {
            return Err(MessageError::InvalidLength);
        }
        out[0..4].copy_from_slice(&self.transfer_id.to_le_bytes());
        out[4..8].copy_from_slice(&self.offset.to_le_bytes());
        out[8..length].copy_from_slice(self.data);
        Ok(length)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileChunkData {
    transfer_id: u32,
    offset: u32,
    length: u8,
    data: [u8; PROFILE_CHUNK_MAX_DATA_LEN],
}

impl ProfileChunkData {
    pub fn from_borrowed(chunk: ProfileChunk<'_>) -> Result<Self, MessageError> {
        if chunk.data.len() > PROFILE_CHUNK_MAX_DATA_LEN {
            return Err(MessageError::InvalidLength);
        }
        let mut data = [0; PROFILE_CHUNK_MAX_DATA_LEN];
        data[..chunk.data.len()].copy_from_slice(chunk.data);
        Ok(Self {
            transfer_id: chunk.transfer_id,
            offset: chunk.offset,
            length: chunk.data.len() as u8,
            data,
        })
    }

    pub const fn transfer_id(&self) -> u32 {
        self.transfer_id
    }

    pub fn as_borrowed(&self) -> ProfileChunk<'_> {
        ProfileChunk {
            transfer_id: self.transfer_id,
            offset: self.offset,
            data: &self.data[..self.length as usize],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProfileResultStatus {
    Accepted = 0,
    AlreadyStored = 1,
    InvalidImage = 2,
    Unsupported = 3,
    ResourceExhausted = 4,
    StorageError = 5,
    Busy = 6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileResult {
    pub transfer_id: u32,
    pub profile_hash: u32,
    pub status: ProfileResultStatus,
    pub reject_reason: u8,
    pub detail: u16,
}

impl ProfileResult {
    pub const fn encode(self) -> [u8; PROFILE_RESULT_WIRE_LEN] {
        let transfer = self.transfer_id.to_le_bytes();
        let hash = self.profile_hash.to_le_bytes();
        let detail = self.detail.to_le_bytes();
        [
            transfer[0],
            transfer[1],
            transfer[2],
            transfer[3],
            hash[0],
            hash[1],
            hash[2],
            hash[3],
            self.status as u8,
            self.reject_reason,
            detail[0],
            detail[1],
        ]
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        if bytes.len() != PROFILE_RESULT_WIRE_LEN {
            return Err(MessageError::InvalidLength);
        }
        let status = match bytes[8] {
            0 => ProfileResultStatus::Accepted,
            1 => ProfileResultStatus::AlreadyStored,
            2 => ProfileResultStatus::InvalidImage,
            3 => ProfileResultStatus::Unsupported,
            4 => ProfileResultStatus::ResourceExhausted,
            5 => ProfileResultStatus::StorageError,
            6 => ProfileResultStatus::Busy,
            _ => return Err(MessageError::InvalidStatus),
        };
        Ok(Self {
            transfer_id: read_u32(&bytes[0..4]),
            profile_hash: read_u32(&bytes[4..8]),
            status,
            reject_reason: bytes[9],
            detail: u16::from_le_bytes([bytes[10], bytes[11]]),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivateProfile {
    pub operation_id: u32,
    pub profile_hash: u32,
}

impl ActivateProfile {
    pub const fn encode(self) -> [u8; ACTIVATE_PROFILE_WIRE_LEN] {
        let operation = self.operation_id.to_le_bytes();
        let profile = self.profile_hash.to_le_bytes();
        [
            operation[0],
            operation[1],
            operation[2],
            operation[3],
            profile[0],
            profile[1],
            profile[2],
            profile[3],
        ]
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        if bytes.len() != ACTIVATE_PROFILE_WIRE_LEN {
            return Err(MessageError::InvalidLength);
        }
        Ok(Self {
            operation_id: read_u32(&bytes[..4]),
            profile_hash: read_u32(&bytes[4..]),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbState {
    pub attached: bool,
    pub configured: bool,
    pub fallback_active: bool,
    pub healthy: bool,
    pub active_profile_hash: u32,
    pub error_code: u16,
}

impl UsbState {
    pub const fn encode(self) -> [u8; USB_STATE_WIRE_LEN] {
        let mut flags = 0u8;
        if self.attached {
            flags |= 1 << 0;
        }
        if self.configured {
            flags |= 1 << 1;
        }
        if self.fallback_active {
            flags |= 1 << 2;
        }
        if self.healthy {
            flags |= 1 << 3;
        }
        let hash = self.active_profile_hash.to_le_bytes();
        let error = self.error_code.to_le_bytes();
        [
            flags, 0, hash[0], hash[1], hash[2], hash[3], error[0], error[1],
        ]
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let [flags, reserved, h0, h1, h2, h3, e0, e1] = bytes else {
            return Err(MessageError::InvalidLength);
        };
        if *reserved != 0 || *flags & !0x0f != 0 {
            return Err(MessageError::InvalidFlags);
        }
        Ok(Self {
            attached: *flags & (1 << 0) != 0,
            configured: *flags & (1 << 1) != 0,
            fallback_active: *flags & (1 << 2) != 0,
            healthy: *flags & (1 << 3) != 0,
            active_profile_hash: u32::from_le_bytes([*h0, *h1, *h2, *h3]),
            error_code: u16::from_le_bytes([*e0, *e1]),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StandardInputReport {
    pub flags: u8,
    pub sequence: u16,
    pub report: StandardHidReport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StandardOutputReport {
    pub kind: u8,
    pub length: u8,
    pub data: [u8; STANDARD_OUTPUT_MAX_DATA_LEN],
}

impl StandardOutputReport {
    pub fn new(kind: u8, data: &[u8]) -> Result<Self, StandardOutputReportError> {
        if data.len() > STANDARD_OUTPUT_MAX_DATA_LEN {
            return Err(StandardOutputReportError::DataTooLong);
        }
        let mut report = Self {
            kind,
            length: data.len() as u8,
            data: [0; STANDARD_OUTPUT_MAX_DATA_LEN],
        };
        report.data[..data.len()].copy_from_slice(data);
        Ok(report)
    }

    pub const fn data(&self) -> &[u8] {
        self.data.split_at(self.length as usize).0
    }

    pub const fn encode(self) -> [u8; STANDARD_OUTPUT_WIRE_LEN] {
        let mut bytes = [0; STANDARD_OUTPUT_WIRE_LEN];
        bytes[0] = self.kind;
        bytes[1] = self.length;
        let mut index = 0;
        while index < STANDARD_OUTPUT_MAX_DATA_LEN {
            bytes[index + 2] = self.data[index];
            index += 1;
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StandardOutputReportError> {
        if bytes.len() != STANDARD_OUTPUT_WIRE_LEN {
            return Err(StandardOutputReportError::InvalidLength);
        }
        let length = bytes[1] as usize;
        if length > STANDARD_OUTPUT_MAX_DATA_LEN
            || bytes[2 + length..].iter().any(|byte| *byte != 0)
        {
            return Err(StandardOutputReportError::InvalidLength);
        }
        let mut data = [0; STANDARD_OUTPUT_MAX_DATA_LEN];
        data.copy_from_slice(&bytes[2..]);
        Ok(Self {
            kind: bytes[0],
            length: length as u8,
            data,
        })
    }
}

impl StandardInputReport {
    pub fn encode(self) -> ([u8; STANDARD_INPUT_MAX_LEN], u8) {
        let mut bytes = [0; STANDARD_INPUT_MAX_LEN];
        bytes[1] = self.flags;
        bytes[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        let data: &[u8] = match &self.report {
            StandardHidReport::Keyboard(report) => {
                bytes[0] = 1;
                report.as_bytes()
            }
            StandardHidReport::Mouse(report) => {
                bytes[0] = 2;
                report.as_bytes()
            }
            StandardHidReport::Consumer(report) => {
                bytes[0] = 3;
                report.as_bytes()
            }
        };
        bytes[4] = data.len() as u8;
        bytes[STANDARD_INPUT_HEADER_LEN..STANDARD_INPUT_HEADER_LEN + data.len()]
            .copy_from_slice(data);
        (bytes, (STANDARD_INPUT_HEADER_LEN + data.len()) as u8)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StandardInputReportError> {
        if bytes.len() < STANDARD_INPUT_HEADER_LEN {
            return Err(StandardInputReportError::InvalidLength);
        }
        let data_len = bytes[4] as usize;
        if bytes.len() != STANDARD_INPUT_HEADER_LEN + data_len {
            return Err(StandardInputReportError::InvalidLength);
        }
        let data = &bytes[STANDARD_INPUT_HEADER_LEN..];
        let report = match bytes[0] {
            1 if data_len == KEYBOARD_REPORT_LEN => {
                let mut raw = [0; KEYBOARD_REPORT_LEN];
                raw.copy_from_slice(data);
                StandardHidReport::Keyboard(Keyboard6KroReport::from_bytes(raw))
            }
            2 if data_len == MOUSE_REPORT_LEN => {
                let mut raw = [0; MOUSE_REPORT_LEN];
                raw.copy_from_slice(data);
                StandardHidReport::Mouse(MouseReport::from_bytes(raw))
            }
            3 if data_len == CONSUMER_REPORT_LEN => {
                let mut raw = [0; CONSUMER_REPORT_LEN];
                raw.copy_from_slice(data);
                StandardHidReport::Consumer(ConsumerReport::from_usage_id(u16::from_le_bytes(raw)))
            }
            1..=3 => return Err(StandardInputReportError::InvalidReportLength),
            _ => return Err(StandardInputReportError::InvalidKind),
        };
        Ok(Self {
            flags: bytes[1],
            sequence: u16::from_le_bytes([bytes[2], bytes[3]]),
            report,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageError {
    InvalidLength,
    InvalidRole,
    InvalidFlags,
    InvalidStatus,
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardInputReportError {
    InvalidLength,
    InvalidKind,
    InvalidReportLength,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardOutputReportError {
    DataTooLong,
    InvalidLength,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips_without_native_layout_assumptions() {
        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: 1,
            firmware_major: 2,
            firmware_minor: 3,
            capabilities: 0x1234_5678,
            active_profile_hash: 0xaabb_ccdd,
        };
        assert_eq!(Hello::decode(&hello.encode()), Ok(hello));
    }

    #[test]
    fn usb_state_round_trips_and_rejects_reserved_bits() {
        let state = UsbState {
            attached: true,
            configured: true,
            fallback_active: true,
            healthy: true,
            active_profile_hash: 0x1234_5678,
            error_code: 9,
        };
        assert_eq!(UsbState::decode(&state.encode()), Ok(state));

        let mut invalid = state.encode();
        invalid[0] |= 0x80;
        assert_eq!(UsbState::decode(&invalid), Err(MessageError::InvalidFlags));
    }

    #[test]
    fn every_standard_report_round_trips() {
        let reports = [
            StandardHidReport::Keyboard(Keyboard6KroReport::from_bytes([1, 0, 4, 5, 0, 0, 0, 0])),
            StandardHidReport::Mouse(MouseReport::from_bytes([1, 2, 3, 4, 5])),
            StandardHidReport::Consumer(ConsumerReport::from_usage_id(0x00e9)),
        ];
        for report in reports {
            let message = StandardInputReport {
                flags: 0x80,
                sequence: 42,
                report,
            };
            let (bytes, len) = message.encode();
            assert_eq!(
                StandardInputReport::decode(&bytes[..len as usize]),
                Ok(message)
            );
        }
    }

    #[test]
    fn standard_output_report_round_trips_with_zeroed_tail() {
        let report = StandardOutputReport::new(1, &[0x05]).unwrap();
        assert_eq!(report.data(), &[0x05]);
        assert_eq!(StandardOutputReport::decode(&report.encode()), Ok(report));

        let mut noncanonical = report.encode();
        noncanonical[9] = 1;
        assert_eq!(
            StandardOutputReport::decode(&noncanonical),
            Err(StandardOutputReportError::InvalidLength)
        );
    }

    #[test]
    fn kind_specific_length_is_enforced() {
        let invalid = [1, 0, 0, 0, 1, 0];
        assert_eq!(
            StandardInputReport::decode(&invalid),
            Err(StandardInputReportError::InvalidReportLength)
        );
    }

    #[test]
    fn profile_transfer_messages_round_trip() {
        let begin = ProfileBegin {
            transfer_id: 7,
            total_length: 16_384,
            crc32: 0x1234_5678,
            profile_hash: 0xaabb_ccdd,
        };
        assert_eq!(ProfileBegin::decode(&begin.encode()), Ok(begin));

        let chunk = ProfileChunk {
            transfer_id: 7,
            offset: 96,
            data: &[1, 2, 3],
        };
        let mut encoded = [0; 104];
        let length = chunk.encode(&mut encoded).unwrap();
        assert_eq!(ProfileChunk::decode(&encoded[..length]), Ok(chunk));

        let result = ProfileResult {
            transfer_id: 7,
            profile_hash: 0xaabb_ccdd,
            status: ProfileResultStatus::Accepted,
            reject_reason: 0,
            detail: 42,
        };
        assert_eq!(ProfileResult::decode(&result.encode()), Ok(result));

        let activate = ActivateProfile {
            operation_id: 19,
            profile_hash: 0xaabb_ccdd,
        };
        assert_eq!(ActivateProfile::decode(&activate.encode()), Ok(activate));
    }

    #[test]
    fn raw_endpoint_report_preserves_endpoint_sequence_and_payload() {
        let report = RawEndpointReport::new(0x82, 0xfffe, &[0x10; 64]).unwrap();
        let mut encoded = [0; RAW_ENDPOINT_MAX_WIRE_LEN];
        let length = report.encode(&mut encoded).unwrap();
        assert_eq!(RawEndpointReport::decode(&encoded[..length]), Ok(report));
        assert_eq!(report.data(), &[0x10; 64]);

        encoded[1] = 1;
        assert_eq!(
            RawEndpointReport::decode(&encoded[..length]),
            Err(MessageError::InvalidLength)
        );
    }

    #[test]
    fn mirror_control_messages_preserve_setup_status_and_data() {
        let request =
            MirrorControlRequest::new(0x1122_3344, [0xa1, 1, 0x10, 3, 1, 0, 17, 0], &[]).unwrap();
        let mut request_bytes = [0; CONTROL_REQUEST_MAX_WIRE_LEN];
        let request_len = request.encode(&mut request_bytes).unwrap();
        assert_eq!(
            MirrorControlRequest::decode(&request_bytes[..request_len]),
            Ok(request)
        );

        let response =
            MirrorControlResponse::new(request.request_id, ControlStatus::Success, &[0x10; 17])
                .unwrap();
        let mut response_bytes = [0; CONTROL_RESPONSE_MAX_WIRE_LEN];
        let response_len = response.encode(&mut response_bytes).unwrap();
        assert_eq!(
            MirrorControlResponse::decode(&response_bytes[..response_len]),
            Ok(response)
        );
    }

    #[test]
    fn mirror_control_messages_reject_oversize_and_unknown_status() {
        assert_eq!(
            MirrorControlRequest::new(1, [0; 8], &[0; CONTROL_DATA_MAX_LEN + 1]),
            Err(MessageError::InvalidLength)
        );
        assert_eq!(
            MirrorControlResponse::new(1, ControlStatus::Success, &[0; CONTROL_DATA_MAX_LEN + 1]),
            Err(MessageError::InvalidLength)
        );
        assert_eq!(
            MirrorControlResponse::decode(&[1, 0, 0, 0, 5, 0, 0]),
            Err(MessageError::InvalidStatus)
        );
    }

    #[test]
    fn owned_profile_chunk_rejects_oversized_data_without_panicking() {
        let data = [0; PROFILE_CHUNK_MAX_DATA_LEN + 1];
        assert_eq!(
            ProfileChunkData::from_borrowed(ProfileChunk {
                transfer_id: 1,
                offset: 0,
                data: &data,
            }),
            Err(MessageError::InvalidLength)
        );
    }
}
