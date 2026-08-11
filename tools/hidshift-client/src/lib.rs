//! Transport-independent client support for HIDShift management frontends.
//!
//! The firmware owns the wire schema in `hidshift::management`. This crate owns
//! client-side request correlation and transport framing, so CLI, WebHID, and
//! Web Bluetooth do not each grow their own protocol implementation.

use hidshift::{
    HostId, MANAGEMENT_EVENT_LEN, MANAGEMENT_HID_EVENT_PACKET_LEN, MANAGEMENT_HID_EVENT_REPORT_ID,
    MANAGEMENT_HID_REQUEST_PACKET_LEN, MANAGEMENT_HID_REQUEST_REPORT_ID,
    MANAGEMENT_HID_RESPONSE_PACKET_LEN, MANAGEMENT_HID_RESPONSE_REPORT_ID, MANAGEMENT_REQUEST_LEN,
    MANAGEMENT_RESPONSE_LEN, ManagementCommand, ManagementEvent, ManagementOutputTarget,
    ManagementOutputTargetStatus, ManagementProtocolError, ManagementRequest, ManagementResponse,
    ManagementStatus,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidManagementFrame {
    Response([u8; MANAGEMENT_RESPONSE_LEN]),
    Event([u8; MANAGEMENT_EVENT_LEN]),
}

pub const fn is_management_hid_identity(
    vendor_id: u16,
    product_id: u16,
    usage_page: u16,
    usage: u16,
) -> bool {
    vendor_id == hidshift::fallback::FALLBACK_USB_VENDOR_ID
        && product_id == hidshift::fallback::FALLBACK_USB_PRODUCT_ID
        && usage_page == hidshift::MANAGEMENT_HID_USAGE_PAGE
        && usage == hidshift::MANAGEMENT_HID_USAGE
}

pub fn encode_hid_request(request: PendingRequest) -> [u8; MANAGEMENT_HID_REQUEST_PACKET_LEN] {
    let mut packet = [0; MANAGEMENT_HID_REQUEST_PACKET_LEN];
    packet[0] = MANAGEMENT_HID_REQUEST_REPORT_ID;
    packet[1..].copy_from_slice(&request.encode());
    packet
}

pub fn decode_hid_input(report_id: u8, data: &[u8]) -> Option<HidManagementFrame> {
    match (report_id, data.len()) {
        (MANAGEMENT_HID_RESPONSE_REPORT_ID, MANAGEMENT_RESPONSE_LEN) => {
            Some(HidManagementFrame::Response(data.try_into().ok()?))
        }
        (MANAGEMENT_HID_EVENT_REPORT_ID, MANAGEMENT_EVENT_LEN) => {
            Some(HidManagementFrame::Event(data.try_into().ok()?))
        }
        _ => None,
    }
}

pub fn decode_hid_input_packet(bytes: &[u8]) -> Option<HidManagementFrame> {
    match bytes.len() {
        MANAGEMENT_HID_RESPONSE_PACKET_LEN | MANAGEMENT_HID_EVENT_PACKET_LEN => {
            decode_hid_input(bytes[0], &bytes[1..])
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientError {
    Protocol(ManagementProtocolError),
    UnexpectedRequestId { expected: u8, actual: u8 },
    RequestAlreadyPending,
    NoPendingRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveTargetNotification {
    ThisComputer,
    OtherComputer(HostId),
    Wired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientTarget {
    Wired,
    Ble(HostId),
}

/// Tracks an event stream against authoritative status snapshots. Connection
/// setup deliberately establishes a baseline without producing stale desktop
/// notifications.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClientSessionTracker {
    local_host: Option<HostId>,
    active_target: Option<ClientTarget>,
    last_event_sequence: Option<u16>,
    synchronized: bool,
    refresh_pending: bool,
}

impl ClientSessionTracker {
    pub const fn new(local_host: Option<HostId>) -> Self {
        Self {
            local_host,
            active_target: None,
            last_event_sequence: None,
            synchronized: false,
            refresh_pending: false,
        }
    }

    pub fn disconnected(&mut self) {
        self.synchronized = false;
        self.refresh_pending = false;
        self.last_event_sequence = None;
    }

    pub fn set_local_host(&mut self, host_id: Option<HostId>) {
        self.local_host = host_id;
    }

    pub fn accept_event(&mut self, bytes: &[u8]) -> Result<bool, ClientError> {
        let ManagementEvent::StatusChanged { sequence } =
            ManagementEvent::decode(bytes).map_err(ClientError::Protocol)?;
        if self.last_event_sequence == Some(sequence) {
            return Ok(false);
        }
        self.last_event_sequence = Some(sequence);
        self.refresh_pending = true;
        Ok(true)
    }

    pub const fn refresh_pending(&self) -> bool {
        self.refresh_pending
    }

    pub fn observe_status(&mut self, status: ManagementStatus) -> Option<ActiveTargetNotification> {
        self.observe_target(status.active_host.map(ClientTarget::Ble))
    }

    pub fn observe_output_status(
        &mut self,
        status: ManagementOutputTargetStatus,
    ) -> Option<ActiveTargetNotification> {
        self.observe_target(status.active.map(|target| match target {
            ManagementOutputTarget::Wired => ClientTarget::Wired,
            ManagementOutputTarget::Ble(host) => ClientTarget::Ble(host),
        }))
    }

    fn observe_target(&mut self, target: Option<ClientTarget>) -> Option<ActiveTargetNotification> {
        self.refresh_pending = false;
        let previous = self.active_target;
        self.active_target = target;
        if !self.synchronized {
            self.synchronized = true;
            return None;
        }
        if previous == self.active_target {
            return None;
        }
        self.active_target.map(|target| match target {
            ClientTarget::Wired => ActiveTargetNotification::Wired,
            ClientTarget::Ble(host) if self.local_host == Some(host) => {
                ActiveTargetNotification::ThisComputer
            }
            ClientTarget::Ble(host) => ActiveTargetNotification::OtherComputer(host),
        })
    }
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

    /// Accepts an asynchronous transport notification when it belongs to the
    /// in-flight request. Notifications for another request ID are stale or
    /// unsolicited and leave the current request pending.
    pub fn accept_notification(
        &mut self,
        bytes: &[u8],
    ) -> Result<Option<ManagementResponse>, ClientError> {
        let pending = self.pending.ok_or(ClientError::NoPendingRequest)?;
        if bytes
            .get(1)
            .copied()
            .is_some_and(|request_id| request_id != pending.request().request_id)
        {
            return Ok(None);
        }
        let response = pending.accept(bytes)?;
        self.pending = None;
        Ok(Some(response))
    }

    pub fn cancel(&mut self) -> Option<PendingRequest> {
        self.pending.take()
    }

    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// UART text framing for lab diagnostics and hardware E2E only. Production
/// frontends intentionally do not enable this feature.
#[cfg(feature = "debug-serial")]
pub mod debug_serial {
    use super::PendingRequest;
    use hidshift::{MANAGEMENT_REQUEST_LEN, MANAGEMENT_RESPONSE_LEN};

    pub const SERIAL_PREFIX: &[u8] = b"@HIDSHIFT:";
    pub const SERIAL_RESPONSE_LINE_LEN: usize = SERIAL_PREFIX.len() + MANAGEMENT_RESPONSE_LEN * 2;
    pub const SERIAL_EVENT_PREFIX: &[u8] = hidshift::MANAGEMENT_SERIAL_EVENT_PREFIX.as_bytes();
    pub const SERIAL_EVENT_LINE_LEN: usize =
        SERIAL_EVENT_PREFIX.len() + hidshift::MANAGEMENT_EVENT_LEN * 2;
    const SERIAL_MANAGEMENT_LINE_LEN: usize = if SERIAL_RESPONSE_LINE_LEN > SERIAL_EVENT_LINE_LEN {
        SERIAL_RESPONSE_LINE_LEN
    } else {
        SERIAL_EVENT_LINE_LEN
    };

    #[derive(Debug, Default)]
    pub struct SerialResponseDecoder {
        frames: SerialManagementDecoder,
    }

    impl SerialResponseDecoder {
        pub fn push(&mut self, bytes: &[u8]) -> Vec<[u8; MANAGEMENT_RESPONSE_LEN]> {
            self.frames
                .push(bytes)
                .into_iter()
                .filter_map(|frame| match frame {
                    SerialManagementFrame::Response(response) => Some(response),
                    SerialManagementFrame::Event(_) => None,
                })
                .collect()
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum SerialManagementFrame {
        Response([u8; MANAGEMENT_RESPONSE_LEN]),
        Event([u8; hidshift::MANAGEMENT_EVENT_LEN]),
    }

    #[derive(Debug, Default)]
    pub struct SerialManagementDecoder {
        line: Vec<u8>,
        discard_until_newline: bool,
    }

    impl SerialManagementDecoder {
        pub fn push(&mut self, bytes: &[u8]) -> Vec<SerialManagementFrame> {
            let mut frames = Vec::new();
            for &byte in bytes {
                if byte == b'\n' || byte == b'\r' {
                    if !self.discard_until_newline {
                        if let Some(response) = decode_serial_response_line(&self.line) {
                            frames.push(SerialManagementFrame::Response(response));
                        } else if let Some(event) = decode_serial_event_line(&self.line) {
                            frames.push(SerialManagementFrame::Event(event));
                        }
                    }
                    self.line.clear();
                    self.discard_until_newline = false;
                } else if !self.discard_until_newline {
                    if self.line.len() < SERIAL_MANAGEMENT_LINE_LEN {
                        self.line.push(byte);
                    } else {
                        self.line.clear();
                        self.discard_until_newline = true;
                    }
                }
            }
            frames
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

    pub fn decode_serial_event_line(line: &[u8]) -> Option<[u8; hidshift::MANAGEMENT_EVENT_LEN]> {
        let line = trim_ascii(line);
        let encoded = line.strip_prefix(SERIAL_EVENT_PREFIX)?;
        if encoded.len() != hidshift::MANAGEMENT_EVENT_LEN * 2 {
            return None;
        }
        let mut event = [0u8; hidshift::MANAGEMENT_EVENT_LEN];
        for (index, output) in event.iter_mut().enumerate() {
            *output = (hex_nibble(encoded[index * 2])? << 4) | hex_nibble(encoded[index * 2 + 1])?;
        }
        Some(event)
    }

    pub(crate) const fn hex_digit(value: u8) -> u8 {
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
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "debug-serial")]
    use super::debug_serial::*;
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

    #[cfg(feature = "debug-serial")]
    fn serial_response_line(request_id: u8) -> Vec<u8> {
        let mut line = SERIAL_PREFIX.to_vec();
        for byte in response(request_id) {
            line.push(hex_digit(byte >> 4));
            line.push(hex_digit(byte & 0x0f));
        }
        line.push(b'\n');
        line
    }

    #[cfg(feature = "debug-serial")]
    fn serial_event_line(sequence: u16) -> Vec<u8> {
        let mut line = SERIAL_EVENT_PREFIX.to_vec();
        for byte in (ManagementEvent::StatusChanged { sequence }).encode() {
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
    fn stale_notification_is_ignored_until_the_expected_response_arrives() {
        let mut client = ManagementClient::new(7);
        client.begin(ManagementCommand::GetStatus).unwrap();

        let mut malformed_stale = [0xff; MANAGEMENT_RESPONSE_LEN];
        malformed_stale[1] = 6;
        assert_eq!(client.accept_notification(&malformed_stale), Ok(None));
        assert!(client.is_pending());
        assert_eq!(
            client.accept_notification(&response(7)),
            Ok(Some(ManagementResponse::decode(&response(7)).unwrap()))
        );
        assert!(!client.is_pending());
    }

    #[cfg(feature = "debug-serial")]
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

    #[cfg(feature = "debug-serial")]
    #[test]
    fn oversized_or_malformed_serial_lines_are_discarded_and_decoder_recovers() {
        let mut decoder = SerialResponseDecoder::default();
        let mut input = vec![b'x'; SERIAL_RESPONSE_LINE_LEN + 10];
        input.extend_from_slice(b"\n@HIDSHIFT:not-hex\n");
        input.extend_from_slice(&serial_response_line(1));
        assert_eq!(decoder.push(&input), vec![response(1)]);
    }

    #[cfg(feature = "debug-serial")]
    #[test]
    fn serial_management_decoder_preserves_events_mixed_with_responses_and_logs() {
        let mut decoder = SerialManagementDecoder::default();
        let event = serial_event_line(0x1234);
        let response = serial_response_line(7);
        let split = event.len() - 3;

        assert!(decoder.push(b"firmware: switched\r\n").is_empty());
        assert!(decoder.push(&event[..split]).is_empty());
        let mut remainder = event[split..].to_vec();
        remainder.extend_from_slice(&response);

        assert_eq!(
            decoder.push(&remainder),
            vec![
                SerialManagementFrame::Event(
                    (ManagementEvent::StatusChanged { sequence: 0x1234 }).encode()
                ),
                SerialManagementFrame::Response(self::response(7)),
            ]
        );
    }

    #[cfg(feature = "debug-serial")]
    #[test]
    fn response_only_decoder_ignores_unsolicited_serial_events() {
        let mut decoder = SerialResponseDecoder::default();
        let mut input = serial_event_line(4);
        input.extend_from_slice(&serial_response_line(8));
        assert_eq!(decoder.push(&input), vec![response(8)]);
    }

    #[cfg(feature = "debug-serial")]
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
        assert!(
            line.starts_with(
                format!(
                    "@HIDSHIFT:{:02x}2a030103",
                    hidshift::MANAGEMENT_PROTOCOL_VERSION
                )
                .as_bytes()
            )
        );
    }

    #[test]
    fn hid_request_and_input_reports_keep_transport_ids_outside_protocol_bytes() {
        let mut client = ManagementClient::new(0x2a);
        let pending = client.begin(ManagementCommand::GetStatus).unwrap();
        let packet = encode_hid_request(pending);
        assert_eq!(packet[0], MANAGEMENT_HID_REQUEST_REPORT_ID);
        assert_eq!(&packet[1..], &pending.encode());

        let response = response(0x2a);
        assert_eq!(
            decode_hid_input(MANAGEMENT_HID_RESPONSE_REPORT_ID, &response),
            Some(HidManagementFrame::Response(response))
        );
        let event = ManagementEvent::StatusChanged { sequence: 9 }.encode();
        assert_eq!(
            decode_hid_input(MANAGEMENT_HID_EVENT_REPORT_ID, &event),
            Some(HidManagementFrame::Event(event))
        );
    }

    #[test]
    fn hid_input_rejects_wrong_report_lengths_and_unknown_ids() {
        assert_eq!(
            decode_hid_input(MANAGEMENT_HID_RESPONSE_REPORT_ID, &[0; 19]),
            None
        );
        assert_eq!(decode_hid_input(0xff, &[0; MANAGEMENT_EVENT_LEN]), None);
        assert_eq!(decode_hid_input_packet(&[0; 64]), None);
    }

    #[test]
    fn management_hid_identity_excludes_other_collections_on_the_composite_device() {
        assert!(is_management_hid_identity(
            hidshift::fallback::FALLBACK_USB_VENDOR_ID,
            hidshift::fallback::FALLBACK_USB_PRODUCT_ID,
            hidshift::MANAGEMENT_HID_USAGE_PAGE,
            hidshift::MANAGEMENT_HID_USAGE,
        ));
        assert!(!is_management_hid_identity(
            hidshift::fallback::FALLBACK_USB_VENDOR_ID,
            hidshift::fallback::FALLBACK_USB_PRODUCT_ID,
            0x01,
            0x06,
        ));
    }

    #[test]
    fn event_refreshes_authoritative_status_without_notifying_initial_sync() {
        let mut tracker = ClientSessionTracker::new(Some(HostId(2)));
        let mut status = ManagementStatus::empty(4);
        status.active_host = Some(HostId(1));
        assert_eq!(tracker.observe_status(status), None);

        assert!(
            tracker
                .accept_event(&ManagementEvent::StatusChanged { sequence: u16::MAX }.encode())
                .unwrap()
        );
        assert!(tracker.refresh_pending());
        status.active_host = Some(HostId(2));
        assert_eq!(
            tracker.observe_status(status),
            Some(ActiveTargetNotification::ThisComputer)
        );

        assert!(
            !tracker
                .accept_event(&ManagementEvent::StatusChanged { sequence: u16::MAX }.encode())
                .unwrap()
        );
        assert!(
            tracker
                .accept_event(&ManagementEvent::StatusChanged { sequence: 0 }.encode())
                .unwrap()
        );
        status.active_host = Some(HostId(3));
        assert_eq!(
            tracker.observe_status(status),
            Some(ActiveTargetNotification::OtherComputer(HostId(3)))
        );
    }

    #[test]
    fn reconnect_uses_a_new_silent_baseline() {
        let mut tracker = ClientSessionTracker::new(Some(HostId(1)));
        let mut status = ManagementStatus::empty(4);
        status.active_host = Some(HostId(1));
        tracker.observe_status(status);
        tracker.disconnected();
        status.active_host = Some(HostId(2));
        assert_eq!(tracker.observe_status(status), None);
    }

    #[test]
    fn dual_s3_tracker_distinguishes_wired_from_ble_destinations() {
        let mut tracker = ClientSessionTracker::new(Some(HostId(1)));
        let mut status = hidshift::ManagementOutputTargetStatus {
            selected: hidshift::ManagementOutputTarget::Ble(HostId(1)),
            active: Some(hidshift::ManagementOutputTarget::Ble(HostId(1))),
            availability: hidshift::OutputTargetAvailability::Ready,
            wired_ready: true,
            ready_ble_mask: 1,
            effective_presentation: hidshift::ManagementUsbPresentationKind::Fallback,
            mirror_configured: false,
            operation_id: 0,
        };
        assert_eq!(tracker.observe_output_status(status), None);
        status.selected = hidshift::ManagementOutputTarget::Wired;
        status.active = Some(hidshift::ManagementOutputTarget::Wired);
        assert_eq!(
            tracker.observe_output_status(status),
            Some(ActiveTargetNotification::Wired)
        );
    }
}
