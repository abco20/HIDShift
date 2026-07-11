//! Transport-independent client support for HIDShift management frontends.
//!
//! The firmware owns the wire schema in `hidshift::management`. This crate owns
//! client-side request correlation and stream framing, so CLI, Web Bluetooth,
//! and Web Serial do not each grow their own protocol implementation.

use hidshift::{
    MANAGEMENT_REQUEST_LEN, MANAGEMENT_RESPONSE_LEN, ManagementCommand, ManagementProtocolError,
    ManagementRequest, ManagementResponse,
};

pub const SERIAL_PREFIX: &[u8] = b"@HIDSHIFT:";
pub const SERIAL_RESPONSE_LINE_LEN: usize = SERIAL_PREFIX.len() + MANAGEMENT_RESPONSE_LEN * 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientError {
    Protocol(ManagementProtocolError),
    UnexpectedRequestId { expected: u8, actual: u8 },
    RequestAlreadyPending,
    NoPendingRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingRequest {
    request: ManagementRequest,
}

impl PendingRequest {
    pub const fn request(self) -> ManagementRequest {
        self.request
    }

    pub fn encode(self) -> [u8; MANAGEMENT_REQUEST_LEN] {
        self.request.encode()
    }

    pub fn accept(self, bytes: &[u8]) -> Result<ManagementResponse, ClientError> {
        let response = ManagementResponse::decode(bytes).map_err(ClientError::Protocol)?;
        if response.request_id != self.request.request_id {
            return Err(ClientError::UnexpectedRequestId {
                expected: self.request.request_id,
                actual: response.request_id,
            });
        }
        Ok(response)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementClient {
    next_request_id: u8,
    pending: Option<PendingRequest>,
}

impl ManagementClient {
    pub const fn new(initial_request_id: u8) -> Self {
        Self {
            next_request_id: initial_request_id,
            pending: None,
        }
    }

    pub fn begin(&mut self, command: ManagementCommand) -> Result<PendingRequest, ClientError> {
        if self.pending.is_some() {
            return Err(ClientError::RequestAlreadyPending);
        }
        let request = PendingRequest {
            request: ManagementRequest {
                request_id: self.next_request_id,
                command,
            },
        };
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.pending = Some(request);
        Ok(request)
    }

    pub fn accept(&mut self, bytes: &[u8]) -> Result<ManagementResponse, ClientError> {
        let pending = self.pending.ok_or(ClientError::NoPendingRequest)?;
        let response = pending.accept(bytes)?;
        self.pending = None;
        Ok(response)
    }

    pub fn cancel(&mut self) -> Option<PendingRequest> {
        self.pending.take()
    }

    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[derive(Debug, Default)]
pub struct SerialResponseDecoder {
    line: Vec<u8>,
    discard_until_newline: bool,
}

impl SerialResponseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<[u8; MANAGEMENT_RESPONSE_LEN]> {
        let mut responses = Vec::new();
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                if !self.discard_until_newline
                    && let Some(response) = decode_serial_response_line(&self.line)
                {
                    responses.push(response);
                }
                self.line.clear();
                self.discard_until_newline = false;
            } else if !self.discard_until_newline {
                if self.line.len() < SERIAL_RESPONSE_LINE_LEN {
                    self.line.push(byte);
                } else {
                    self.line.clear();
                    self.discard_until_newline = true;
                }
            }
        }
        responses
    }
}

pub fn encode_serial_request(request: PendingRequest) -> Vec<u8> {
    let mut line = Vec::with_capacity(SERIAL_PREFIX.len() + MANAGEMENT_REQUEST_LEN * 2 + 1);
    line.extend_from_slice(SERIAL_PREFIX);
    for byte in request.encode() {
        line.push(hex_digit(byte >> 4));
        line.push(hex_digit(byte & 0x0f));
    }
    line.push(b'\n');
    line
}

pub fn decode_serial_response_line(line: &[u8]) -> Option<[u8; MANAGEMENT_RESPONSE_LEN]> {
    let line = trim_ascii(line);
    let encoded = line.strip_prefix(SERIAL_PREFIX)?;
    if encoded.len() != MANAGEMENT_RESPONSE_LEN * 2 {
        return None;
    }
    let mut response = [0u8; MANAGEMENT_RESPONSE_LEN];
    for (index, output) in response.iter_mut().enumerate() {
        *output = (hex_nibble(encoded[index * 2])? << 4) | hex_nibble(encoded[index * 2 + 1])?;
    }
    Some(response)
}

const fn hex_digit(value: u8) -> u8 {
    if value < 10 {
        b'0' + value
    } else {
        b'a' + value - 10
    }
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidshift::{HostId, ManagementResponsePayload, ManagementResult, ManagementStatus};

    fn response(request_id: u8) -> [u8; MANAGEMENT_RESPONSE_LEN] {
        ManagementResponse {
            request_id,
            result: ManagementResult::Ok,
            payload: ManagementResponsePayload::Status(ManagementStatus::empty(4)),
        }
        .encode()
    }

    fn serial_response_line(request_id: u8) -> Vec<u8> {
        let mut line = SERIAL_PREFIX.to_vec();
        for byte in response(request_id) {
            line.push(hex_digit(byte >> 4));
            line.push(hex_digit(byte & 0x0f));
        }
        line.push(b'\n');
        line
    }

    #[test]
    fn request_ids_wrap_without_reusing_an_in_flight_request() {
        let mut client = ManagementClient::new(255);
        let first = client.begin(ManagementCommand::GetStatus).unwrap();
        assert_eq!(first.request().request_id, 255);
        assert_eq!(
            client.begin(ManagementCommand::GetStatus),
            Err(ClientError::RequestAlreadyPending)
        );
        client.accept(&response(255)).unwrap();
        assert_eq!(
            client
                .begin(ManagementCommand::SelectHost(HostId(2)))
                .unwrap()
                .request()
                .request_id,
            0
        );
    }

    #[test]
    fn mismatched_response_does_not_consume_pending_request() {
        let mut client = ManagementClient::new(7);
        client.begin(ManagementCommand::GetStatus).unwrap();
        assert_eq!(
            client.accept(&response(8)),
            Err(ClientError::UnexpectedRequestId {
                expected: 7,
                actual: 8
            })
        );
        assert!(client.is_pending());
        assert!(client.accept(&response(7)).is_ok());
    }

    #[test]
    fn serial_decoder_handles_fragmented_and_coalesced_input_with_logs() {
        let mut decoder = SerialResponseDecoder::default();
        assert!(decoder.push(b"firmware: log\r\n@HID").is_empty());
        let first = serial_response_line(1);
        let second = serial_response_line(2);
        let mut remainder = first[4..].to_vec();
        remainder.extend_from_slice(&second);
        let responses = decoder.push(&remainder);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0], response(1));
        assert_eq!(responses[1], response(2));
    }

    #[test]
    fn oversized_or_malformed_serial_lines_are_discarded_and_decoder_recovers() {
        let mut decoder = SerialResponseDecoder::default();
        let mut input = vec![b'x'; SERIAL_RESPONSE_LINE_LEN + 10];
        input.extend_from_slice(b"\n@HIDSHIFT:not-hex\n");
        input.extend_from_slice(&serial_response_line(1));
        assert_eq!(decoder.push(&input), vec![response(1)]);
    }

    #[test]
    fn serial_request_uses_shared_protocol_encoding() {
        let mut client = ManagementClient::new(0x2a);
        let request = client
            .begin(ManagementCommand::StartPairing(HostId(3)))
            .unwrap();
        let line = encode_serial_request(request);
        assert_eq!(
            line.len(),
            SERIAL_PREFIX.len() + MANAGEMENT_REQUEST_LEN * 2 + 1
        );
        assert!(line.starts_with(b"@HIDSHIFT:012a030103"));
    }
}
