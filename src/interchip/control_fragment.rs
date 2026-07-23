use super::{ControlStatus, MirrorControlRequest, MirrorControlResponse};
use crate::interchip::message::{CONTROL_DATA_MAX_LEN, MessageError};

pub const CONTROL_FRAGMENT_FIRST: u8 = 1 << 0;
pub const CONTROL_FRAGMENT_LAST: u8 = 1 << 1;
// Record headers and four-byte alignment must also fit the 110-byte Cell payload.
pub const CONTROL_FRAGMENT_DATA_MAX_LEN: usize = 86;
pub const CONTROL_REQUEST_FRAGMENT_HEADER_LEN: usize = 18;
pub const CONTROL_RESPONSE_FRAGMENT_HEADER_LEN: usize = 10;
pub const CONTROL_REQUEST_FRAGMENT_MAX_WIRE_LEN: usize =
    CONTROL_REQUEST_FRAGMENT_HEADER_LEN + CONTROL_FRAGMENT_DATA_MAX_LEN;
pub const CONTROL_RESPONSE_FRAGMENT_MAX_WIRE_LEN: usize =
    CONTROL_RESPONSE_FRAGMENT_HEADER_LEN + CONTROL_FRAGMENT_DATA_MAX_LEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlRequestFragment {
    pub request_id: u32,
    pub setup_packet: [u8; 8],
    pub offset: u16,
    pub total_length: u16,
    pub flags: u8,
    length: u8,
    data: [u8; CONTROL_FRAGMENT_DATA_MAX_LEN],
}

impl ControlRequestFragment {
    pub fn from_request(
        request: MirrorControlRequest,
        offset: usize,
    ) -> Result<Self, ControlFragmentError> {
        fragment_bounds(request.data().len(), offset)?;
        let end = request
            .data()
            .len()
            .min(offset + CONTROL_FRAGMENT_DATA_MAX_LEN);
        let mut fragment = Self {
            request_id: request.request_id,
            setup_packet: request.setup_packet,
            offset: offset as u16,
            total_length: request.data().len() as u16,
            flags: fragment_flags(offset, end, request.data().len()),
            length: (end - offset) as u8,
            data: [0; CONTROL_FRAGMENT_DATA_MAX_LEN],
        };
        fragment.data[..end - offset].copy_from_slice(&request.data()[offset..end]);
        Ok(fragment)
    }

    pub const fn data(&self) -> &[u8] {
        self.data.split_at(self.length as usize).0
    }

    pub fn encode(self, out: &mut [u8]) -> Result<usize, MessageError> {
        let length = CONTROL_REQUEST_FRAGMENT_HEADER_LEN + self.data().len();
        if out.len() < length {
            return Err(MessageError::InvalidLength);
        }
        out[..4].copy_from_slice(&self.request_id.to_le_bytes());
        out[4] = self.flags;
        out[5] = 0;
        out[6..8].copy_from_slice(&self.offset.to_le_bytes());
        out[8..10].copy_from_slice(&self.total_length.to_le_bytes());
        out[10..18].copy_from_slice(&self.setup_packet);
        out[18..length].copy_from_slice(self.data());
        Ok(length)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ControlFragmentError> {
        if bytes.len() < CONTROL_REQUEST_FRAGMENT_HEADER_LEN
            || bytes[5] != 0
            || bytes.len() - CONTROL_REQUEST_FRAGMENT_HEADER_LEN > CONTROL_FRAGMENT_DATA_MAX_LEN
        {
            return Err(ControlFragmentError::Malformed);
        }
        let offset = read_u16(&bytes[6..8]);
        let total_length = read_u16(&bytes[8..10]);
        validate_fragment(
            bytes[4],
            offset,
            total_length,
            bytes.len() - CONTROL_REQUEST_FRAGMENT_HEADER_LEN,
        )?;
        let mut setup_packet = [0; 8];
        setup_packet.copy_from_slice(&bytes[10..18]);
        let mut fragment = Self {
            request_id: read_u32(&bytes[..4]),
            setup_packet,
            offset,
            total_length,
            flags: bytes[4],
            length: (bytes.len() - CONTROL_REQUEST_FRAGMENT_HEADER_LEN) as u8,
            data: [0; CONTROL_FRAGMENT_DATA_MAX_LEN],
        };
        fragment.data[..usize::from(fragment.length)].copy_from_slice(&bytes[18..]);
        Ok(fragment)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlResponseFragment {
    pub request_id: u32,
    pub status: ControlStatus,
    pub offset: u16,
    pub total_length: u16,
    pub flags: u8,
    length: u8,
    data: [u8; CONTROL_FRAGMENT_DATA_MAX_LEN],
}

impl ControlResponseFragment {
    pub fn from_response(
        response: MirrorControlResponse,
        offset: usize,
    ) -> Result<Self, ControlFragmentError> {
        fragment_bounds(response.data().len(), offset)?;
        let end = response
            .data()
            .len()
            .min(offset + CONTROL_FRAGMENT_DATA_MAX_LEN);
        let mut fragment = Self {
            request_id: response.request_id,
            status: response.status,
            offset: offset as u16,
            total_length: response.data().len() as u16,
            flags: fragment_flags(offset, end, response.data().len()),
            length: (end - offset) as u8,
            data: [0; CONTROL_FRAGMENT_DATA_MAX_LEN],
        };
        fragment.data[..end - offset].copy_from_slice(&response.data()[offset..end]);
        Ok(fragment)
    }

    pub const fn data(&self) -> &[u8] {
        self.data.split_at(self.length as usize).0
    }

    pub fn encode(self, out: &mut [u8]) -> Result<usize, MessageError> {
        let length = CONTROL_RESPONSE_FRAGMENT_HEADER_LEN + self.data().len();
        if out.len() < length {
            return Err(MessageError::InvalidLength);
        }
        out[..4].copy_from_slice(&self.request_id.to_le_bytes());
        out[4] = self.status as u8;
        out[5] = self.flags;
        out[6..8].copy_from_slice(&self.offset.to_le_bytes());
        out[8..10].copy_from_slice(&self.total_length.to_le_bytes());
        out[10..length].copy_from_slice(self.data());
        Ok(length)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ControlFragmentError> {
        if bytes.len() < CONTROL_RESPONSE_FRAGMENT_HEADER_LEN
            || bytes.len() - CONTROL_RESPONSE_FRAGMENT_HEADER_LEN > CONTROL_FRAGMENT_DATA_MAX_LEN
        {
            return Err(ControlFragmentError::Malformed);
        }
        let status = match bytes[4] {
            0 => ControlStatus::Success,
            1 => ControlStatus::Stall,
            2 => ControlStatus::Timeout,
            3 => ControlStatus::Disconnected,
            4 => ControlStatus::Unsupported,
            _ => return Err(ControlFragmentError::Malformed),
        };
        let offset = read_u16(&bytes[6..8]);
        let total_length = read_u16(&bytes[8..10]);
        validate_fragment(
            bytes[5],
            offset,
            total_length,
            bytes.len() - CONTROL_RESPONSE_FRAGMENT_HEADER_LEN,
        )?;
        let mut fragment = Self {
            request_id: read_u32(&bytes[..4]),
            status,
            offset,
            total_length,
            flags: bytes[5],
            length: (bytes.len() - CONTROL_RESPONSE_FRAGMENT_HEADER_LEN) as u8,
            data: [0; CONTROL_FRAGMENT_DATA_MAX_LEN],
        };
        fragment.data[..usize::from(fragment.length)].copy_from_slice(&bytes[10..]);
        Ok(fragment)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlFragmentError {
    Malformed,
    OutOfOrder,
    TooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlRequestAssembler {
    request_id: Option<u32>,
    setup_packet: [u8; 8],
    total_length: u16,
    next_offset: u16,
    data: [u8; CONTROL_DATA_MAX_LEN],
}

impl ControlRequestAssembler {
    pub const fn new() -> Self {
        Self {
            request_id: None,
            setup_packet: [0; 8],
            total_length: 0,
            next_offset: 0,
            data: [0; CONTROL_DATA_MAX_LEN],
        }
    }

    pub fn push(
        &mut self,
        fragment: ControlRequestFragment,
    ) -> Result<Option<MirrorControlRequest>, ControlFragmentError> {
        if fragment.flags & CONTROL_FRAGMENT_FIRST != 0 {
            self.request_id = Some(fragment.request_id);
            self.setup_packet = fragment.setup_packet;
            self.total_length = fragment.total_length;
            self.next_offset = 0;
        }
        if self.request_id != Some(fragment.request_id)
            || self.setup_packet != fragment.setup_packet
            || self.total_length != fragment.total_length
            || self.next_offset != fragment.offset
        {
            self.reset();
            return Err(ControlFragmentError::OutOfOrder);
        }
        let end = usize::from(fragment.offset) + fragment.data().len();
        self.data[usize::from(fragment.offset)..end].copy_from_slice(fragment.data());
        self.next_offset = end as u16;
        if fragment.flags & CONTROL_FRAGMENT_LAST == 0 {
            return Ok(None);
        }
        let request = MirrorControlRequest::new(
            fragment.request_id,
            fragment.setup_packet,
            &self.data[..end],
        )
        .map_err(|_| ControlFragmentError::Malformed)?;
        self.reset();
        Ok(Some(request))
    }

    pub fn reset(&mut self) {
        self.request_id = None;
        self.total_length = 0;
        self.next_offset = 0;
    }
}

impl Default for ControlRequestAssembler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlResponseAssembler {
    request_id: Option<u32>,
    status: ControlStatus,
    total_length: u16,
    next_offset: u16,
    data: [u8; CONTROL_DATA_MAX_LEN],
}

impl ControlResponseAssembler {
    pub const fn new() -> Self {
        Self {
            request_id: None,
            status: ControlStatus::Success,
            total_length: 0,
            next_offset: 0,
            data: [0; CONTROL_DATA_MAX_LEN],
        }
    }

    pub fn push(
        &mut self,
        fragment: ControlResponseFragment,
    ) -> Result<Option<MirrorControlResponse>, ControlFragmentError> {
        if fragment.flags & CONTROL_FRAGMENT_FIRST != 0 {
            self.request_id = Some(fragment.request_id);
            self.status = fragment.status;
            self.total_length = fragment.total_length;
            self.next_offset = 0;
        }
        if self.request_id != Some(fragment.request_id)
            || self.status != fragment.status
            || self.total_length != fragment.total_length
            || self.next_offset != fragment.offset
        {
            self.reset();
            return Err(ControlFragmentError::OutOfOrder);
        }
        let end = usize::from(fragment.offset) + fragment.data().len();
        self.data[usize::from(fragment.offset)..end].copy_from_slice(fragment.data());
        self.next_offset = end as u16;
        if fragment.flags & CONTROL_FRAGMENT_LAST == 0 {
            return Ok(None);
        }
        let response =
            MirrorControlResponse::new(fragment.request_id, fragment.status, &self.data[..end])
                .map_err(|_| ControlFragmentError::Malformed)?;
        self.reset();
        Ok(Some(response))
    }

    pub fn reset(&mut self) {
        self.request_id = None;
        self.total_length = 0;
        self.next_offset = 0;
    }
}

impl Default for ControlResponseAssembler {
    fn default() -> Self {
        Self::new()
    }
}

fn fragment_bounds(total_length: usize, offset: usize) -> Result<(), ControlFragmentError> {
    if total_length > CONTROL_DATA_MAX_LEN || offset > total_length {
        return Err(ControlFragmentError::TooLarge);
    }
    if total_length != 0 && offset == total_length {
        return Err(ControlFragmentError::OutOfOrder);
    }
    Ok(())
}

fn fragment_flags(offset: usize, end: usize, total_length: usize) -> u8 {
    u8::from(offset == 0) * CONTROL_FRAGMENT_FIRST
        | u8::from(end == total_length) * CONTROL_FRAGMENT_LAST
}

fn validate_fragment(
    flags: u8,
    offset: u16,
    total_length: u16,
    data_length: usize,
) -> Result<(), ControlFragmentError> {
    if flags & !(CONTROL_FRAGMENT_FIRST | CONTROL_FRAGMENT_LAST) != 0
        || usize::from(total_length) > CONTROL_DATA_MAX_LEN
        || usize::from(offset) + data_length > usize::from(total_length)
        || (flags & CONTROL_FRAGMENT_FIRST != 0) != (offset == 0)
        || (flags & CONTROL_FRAGMENT_LAST != 0)
            != (usize::from(offset) + data_length == usize::from(total_length))
    {
        return Err(ControlFragmentError::Malformed);
    }
    Ok(())
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_response_round_trip_across_three_fragments() {
        let request_data = [0x5a; CONTROL_DATA_MAX_LEN];
        let request =
            MirrorControlRequest::new(7, [1, 2, 3, 4, 5, 6, 7, 8], &request_data).unwrap();
        let mut request_assembler = ControlRequestAssembler::new();
        let mut offset = 0;
        let mut completed = None;
        while offset < request.data().len() {
            let fragment = ControlRequestFragment::from_request(request, offset).unwrap();
            let mut wire = [0; CONTROL_REQUEST_FRAGMENT_MAX_WIRE_LEN];
            let length = fragment.encode(&mut wire).unwrap();
            let decoded = ControlRequestFragment::decode(&wire[..length]).unwrap();
            offset += decoded.data().len();
            completed = request_assembler.push(decoded).unwrap().or(completed);
        }
        assert_eq!(completed, Some(request));

        let response =
            MirrorControlResponse::new(7, ControlStatus::Success, &request_data).unwrap();
        let mut response_assembler = ControlResponseAssembler::new();
        let mut offset = 0;
        let mut completed = None;
        while offset < response.data().len() {
            let fragment = ControlResponseFragment::from_response(response, offset).unwrap();
            let mut wire = [0; CONTROL_RESPONSE_FRAGMENT_MAX_WIRE_LEN];
            let length = fragment.encode(&mut wire).unwrap();
            let decoded = ControlResponseFragment::decode(&wire[..length]).unwrap();
            offset += decoded.data().len();
            completed = response_assembler.push(decoded).unwrap().or(completed);
        }
        assert_eq!(completed, Some(response));
    }

    #[test]
    fn out_of_order_fragment_discards_partial_message() {
        let request = MirrorControlRequest::new(1, [0; 8], &[0xaa; 200]).unwrap();
        let first = ControlRequestFragment::from_request(request, 0).unwrap();
        let third = ControlRequestFragment::from_request(request, 176).unwrap();
        let mut assembler = ControlRequestAssembler::new();
        assert_eq!(assembler.push(first), Ok(None));
        assert_eq!(assembler.push(third), Err(ControlFragmentError::OutOfOrder));
    }
}
