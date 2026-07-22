use heapless::Vec;

use super::message::{
    ActivateProfile, CAPABILITY_DYNAMIC_PROFILE, CAPABILITY_FALLBACK_PROFILE,
    CAPABILITY_PROFILE_FLASH_CACHE, CAPABILITY_STANDARD_WIRED_HID, CAPABILITY_USB_STATE_REPORTING,
    ProfileBegin, ProfileChunk, ProfileChunkData, ProfileResult, RECORD_ACTIVATE_PROFILE,
    RECORD_FORCE_FALLBACK, RECORD_HEARTBEAT, RECORD_HELLO, RECORD_HELLO_ACK, RECORD_LINK_RESET,
    RECORD_PROFILE_BEGIN, RECORD_PROFILE_CHUNK, RECORD_PROFILE_COMMIT, RECORD_PROFILE_RESULT,
    RECORD_STANDARD_INPUT_REPORT, RECORD_STANDARD_OUTPUT_REPORT, RECORD_STANDARD_RELEASE_ALL,
    RECORD_USB_STATE,
};
use super::{
    Hello, InterchipRole, ReceiveDisposition, Record, RecordIter, ReliableReceiver, ReliableSender,
    RetransmitAction, SPI_CELL_LEN, SPI_CELL_PAYLOAD_LEN, SPI_PROTOCOL_VERSION, SpiCell,
    StandardInputReport, StandardOutputReport, UsbState, encode_records,
};

const DEVICE_CAPABILITIES: u32 =
    CAPABILITY_FALLBACK_PROFILE | CAPABILITY_STANDARD_WIRED_HID | CAPABILITY_USB_STATE_REPORTING;
const RETRANSMIT_TIMEOUT_MS: u64 = 5;
const MAX_RETRANSMIT_ATTEMPTS: u8 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceLinkEvent {
    StandardInput(StandardInputReport),
    ReleaseAll,
    ForceFallback { operation_id: u32 },
    ActivateProfile(ActivateProfile),
    ProfileBegin(ProfileBegin),
    ProfileChunk(ProfileChunkData),
    ProfileCommit { transfer_id: u32 },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeviceLinkDiagnostics {
    pub valid_cells: u32,
    pub standard_reports_received: u32,
    pub releases_received: u32,
    pub malformed_cells: u32,
    pub duplicate_cells: u32,
    pub sequence_gaps: u32,
    pub event_overflows: u32,
}

pub struct DeviceLink {
    sender: ReliableSender,
    receiver: ReliableReceiver,
    next_cell: Option<SpiCell>,
    host_compatible: bool,
    usb_state: UsbState,
    diagnostics: DeviceLinkDiagnostics,
    capabilities: u32,
    active_profile_hash: u32,
}

impl DeviceLink {
    pub fn new(session_id: u32, usb_state: UsbState) -> Self {
        Self::with_capabilities(session_id, usb_state, DEVICE_CAPABILITIES, 0)
    }

    pub fn new_with_profile_storage(
        session_id: u32,
        usb_state: UsbState,
        active_profile_hash: u32,
    ) -> Self {
        Self::with_capabilities(
            session_id,
            usb_state,
            DEVICE_CAPABILITIES | CAPABILITY_DYNAMIC_PROFILE | CAPABILITY_PROFILE_FLASH_CACHE,
            active_profile_hash,
        )
    }

    fn with_capabilities(
        session_id: u32,
        usb_state: UsbState,
        capabilities: u32,
        active_profile_hash: u32,
    ) -> Self {
        let mut link = Self {
            sender: ReliableSender::new(nonzero_session(session_id)),
            receiver: ReliableReceiver::new(),
            next_cell: None,
            host_compatible: false,
            usb_state,
            diagnostics: DeviceLinkDiagnostics::default(),
            capabilities,
            active_profile_hash,
        };
        link.next_cell = link.queue_hello(0);
        link
    }

    pub const fn host_compatible(&self) -> bool {
        self.host_compatible
    }

    pub const fn diagnostics(&self) -> DeviceLinkDiagnostics {
        self.diagnostics
    }

    pub fn update_usb_state(&mut self, state: UsbState, now_ms: u64) {
        self.usb_state = state;
        if self.host_compatible {
            self.next_cell = self.queue_usb_state(now_ms);
        }
    }

    pub fn queue_standard_output(&mut self, report: StandardOutputReport, now_ms: u64) -> bool {
        if !self.host_compatible || self.next_cell.is_some() {
            return false;
        }
        let data = report.encode();
        self.next_cell = self.queue_record(RECORD_STANDARD_OUTPUT_REPORT, &data, now_ms);
        self.next_cell.is_some()
    }

    pub fn queue_profile_result(&mut self, result: ProfileResult, now_ms: u64) -> bool {
        if !self.host_compatible || self.next_cell.is_some() {
            return false;
        }
        self.next_cell = self.queue_record(RECORD_PROFILE_RESULT, &result.encode(), now_ms);
        self.next_cell.is_some()
    }

    pub fn next_transaction(&mut self, now_ms: u64) -> [u8; SPI_CELL_LEN] {
        self.sender
            .set_cumulative_ack(self.receiver.cumulative_ack());
        let cell = self.next_cell.take().or_else(|| {
            match self.sender.poll_retransmit(
                now_ms,
                RETRANSMIT_TIMEOUT_MS,
                MAX_RETRANSMIT_ATTEMPTS,
            ) {
                RetransmitAction::Send(cell) => Some(cell),
                RetransmitAction::LinkResetRequired => {
                    let session = nonzero_session(self.sender.session_id().wrapping_add(1));
                    self.sender.reset_session(session);
                    self.host_compatible = false;
                    self.queue_record(RECORD_LINK_RESET, &[], now_ms)
                }
                RetransmitAction::Idle => None,
            }
        });
        let mut cell = cell.unwrap_or_else(|| SpiCell::empty(self.sender.session_id()));
        cell.header.cumulative_ack = self.receiver.cumulative_ack();
        cell.encode().unwrap_or([0; SPI_CELL_LEN])
    }

    pub fn handle_transaction<const EVENTS: usize>(
        &mut self,
        bytes: &[u8; SPI_CELL_LEN],
        now_ms: u64,
        events: &mut Vec<DeviceLinkEvent, EVENTS>,
    ) {
        let cell = match SpiCell::decode(bytes) {
            Ok(cell) => cell,
            Err(_) => {
                self.diagnostics.malformed_cells =
                    self.diagnostics.malformed_cells.saturating_add(1);
                return;
            }
        };
        self.diagnostics.valid_cells = self.diagnostics.valid_cells.saturating_add(1);
        self.sender.acknowledge(cell.header.cumulative_ack);
        if self.receiver.session_id() != Some(cell.header.session_id) {
            self.host_compatible = false;
            if !contains_compatible_host_hello(&cell) {
                self.receiver.reset_session(cell.header.session_id);
                return;
            }
        } else if !self.host_compatible && !contains_compatible_host_hello(&cell) {
            return;
        }
        match self.receiver.receive(&cell) {
            ReceiveDisposition::Accepted { .. } => {}
            ReceiveDisposition::Duplicate { .. } => {
                self.diagnostics.duplicate_cells =
                    self.diagnostics.duplicate_cells.saturating_add(1);
                return;
            }
            ReceiveDisposition::Gap { .. } => {
                self.diagnostics.sequence_gaps = self.diagnostics.sequence_gaps.saturating_add(1);
                return;
            }
            ReceiveDisposition::Empty | ReceiveDisposition::SessionChanged => return,
        }

        let mut records = RecordIter::new(cell.payload(), cell.header.record_count);
        for record in records.by_ref() {
            let Ok(record) = record else {
                self.mark_malformed();
                return;
            };
            match record.record_type {
                RECORD_HELLO => {
                    let Ok(hello) = Hello::decode(record.data) else {
                        self.mark_malformed();
                        continue;
                    };
                    self.host_compatible = hello.role == InterchipRole::Host
                        && hello.protocol_version == SPI_PROTOCOL_VERSION;
                    if self.host_compatible {
                        self.next_cell = self.queue_hello_ack_and_usb_state(now_ms);
                    }
                }
                RECORD_HEARTBEAT => {}
                RECORD_LINK_RESET => {
                    self.host_compatible = false;
                    self.next_cell = self.queue_hello(now_ms);
                }
                RECORD_FORCE_FALLBACK => {
                    let Ok(raw_operation_id) = <[u8; 4]>::try_from(record.data) else {
                        self.mark_malformed();
                        continue;
                    };
                    self.push_event(
                        events,
                        DeviceLinkEvent::ForceFallback {
                            operation_id: u32::from_le_bytes(raw_operation_id),
                        },
                    );
                    self.next_cell = self.queue_usb_state(now_ms);
                }
                RECORD_ACTIVATE_PROFILE if self.host_compatible => {
                    match ActivateProfile::decode(record.data) {
                        Ok(activate) => {
                            self.push_event(events, DeviceLinkEvent::ActivateProfile(activate))
                        }
                        Err(_) => self.mark_malformed(),
                    }
                }
                RECORD_PROFILE_BEGIN if self.host_compatible => {
                    match ProfileBegin::decode(record.data) {
                        Ok(begin) => self.push_event(events, DeviceLinkEvent::ProfileBegin(begin)),
                        Err(_) => self.mark_malformed(),
                    }
                }
                RECORD_PROFILE_CHUNK if self.host_compatible => {
                    match ProfileChunk::decode(record.data) {
                        Ok(chunk) => match ProfileChunkData::from_borrowed(chunk) {
                            Ok(chunk) => {
                                self.push_event(events, DeviceLinkEvent::ProfileChunk(chunk))
                            }
                            Err(_) => self.mark_malformed(),
                        },
                        Err(_) => self.mark_malformed(),
                    }
                }
                RECORD_PROFILE_COMMIT if self.host_compatible => {
                    let Ok(bytes) = <[u8; 4]>::try_from(record.data) else {
                        self.mark_malformed();
                        continue;
                    };
                    self.push_event(
                        events,
                        DeviceLinkEvent::ProfileCommit {
                            transfer_id: u32::from_le_bytes(bytes),
                        },
                    );
                }
                RECORD_STANDARD_INPUT_REPORT if self.host_compatible => {
                    match StandardInputReport::decode(record.data) {
                        Ok(report) => {
                            self.diagnostics.standard_reports_received =
                                self.diagnostics.standard_reports_received.saturating_add(1);
                            self.push_event(events, DeviceLinkEvent::StandardInput(report));
                        }
                        Err(_) => self.mark_malformed(),
                    }
                }
                RECORD_STANDARD_RELEASE_ALL if self.host_compatible => {
                    if record.data.is_empty() {
                        self.diagnostics.releases_received =
                            self.diagnostics.releases_received.saturating_add(1);
                        self.push_event(events, DeviceLinkEvent::ReleaseAll);
                    } else {
                        self.mark_malformed();
                    }
                }
                _ => {}
            }
        }
        if records.finish().is_err() {
            self.mark_malformed();
        }
    }

    fn push_event<const EVENTS: usize>(
        &mut self,
        events: &mut Vec<DeviceLinkEvent, EVENTS>,
        event: DeviceLinkEvent,
    ) {
        if events.push(event).is_err() {
            self.diagnostics.event_overflows = self.diagnostics.event_overflows.saturating_add(1);
        }
    }

    fn mark_malformed(&mut self) {
        self.diagnostics.malformed_cells = self.diagnostics.malformed_cells.saturating_add(1);
    }

    fn queue_hello(&mut self, now_ms: u64) -> Option<SpiCell> {
        let hello = self.device_hello().encode();
        self.queue_record(RECORD_HELLO, &hello, now_ms)
    }

    fn queue_hello_ack_and_usb_state(&mut self, now_ms: u64) -> Option<SpiCell> {
        let hello = self.device_hello().encode();
        let state = self.usb_state.encode();
        self.queue_records(
            &[
                Record {
                    record_type: RECORD_HELLO_ACK,
                    flags: 0,
                    data: &hello,
                },
                Record {
                    record_type: RECORD_USB_STATE,
                    flags: 0,
                    data: &state,
                },
            ],
            now_ms,
        )
    }

    fn queue_usb_state(&mut self, now_ms: u64) -> Option<SpiCell> {
        let state = self.usb_state.encode();
        self.queue_record(RECORD_USB_STATE, &state, now_ms)
    }

    fn queue_record(&mut self, record_type: u8, data: &[u8], now_ms: u64) -> Option<SpiCell> {
        self.queue_records(
            &[Record {
                record_type,
                flags: 0,
                data,
            }],
            now_ms,
        )
    }

    fn queue_records(&mut self, records: &[Record<'_>], now_ms: u64) -> Option<SpiCell> {
        let mut payload = [0u8; SPI_CELL_PAYLOAD_LEN];
        let (length, count) = encode_records(records, &mut payload).ok()?;
        self.sender
            .queue(&payload[..length as usize], count, now_ms)
            .ok()
    }

    const fn device_hello(&self) -> Hello {
        Hello {
            role: InterchipRole::Device,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: self.capabilities,
            active_profile_hash: self.active_profile_hash,
        }
    }
}

fn contains_compatible_host_hello(cell: &SpiCell) -> bool {
    let mut records = RecordIter::new(cell.payload(), cell.header.record_count);
    let found = records.by_ref().any(|record| {
        let Ok(record) = record else {
            return false;
        };
        record.record_type == RECORD_HELLO
            && Hello::decode(record.data).is_ok_and(|hello| {
                hello.role == InterchipRole::Host && hello.protocol_version == SPI_PROTOCOL_VERSION
            })
    });
    found && records.finish().is_ok()
}

const fn nonzero_session(value: u32) -> u32 {
    if value == 0 { 1 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::{Keyboard6KroReport, StandardHidReport};

    fn fallback_state() -> UsbState {
        UsbState {
            attached: true,
            configured: false,
            fallback_active: true,
            healthy: true,
            active_profile_hash: 0,
            error_code: 0,
        }
    }

    fn host_cell(sender: &mut ReliableSender, record_type: u8, data: &[u8]) -> [u8; SPI_CELL_LEN] {
        let mut payload = [0; SPI_CELL_PAYLOAD_LEN];
        let (length, count) = encode_records(
            &[Record {
                record_type,
                flags: 0,
                data,
            }],
            &mut payload,
        )
        .unwrap();
        sender
            .queue(&payload[..length as usize], count, 0)
            .unwrap()
            .encode()
            .unwrap()
    }

    #[test]
    fn hello_ack_includes_current_usb_state() {
        let mut device = DeviceLink::new(2, fallback_state());
        let mut host = ReliableSender::new(1);
        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: DEVICE_CAPABILITIES,
            active_profile_hash: 0,
        }
        .encode();
        let cell = host_cell(&mut host, RECORD_HELLO, &hello);
        let mut events = Vec::<_, 2>::new();
        device.handle_transaction(&cell, 0, &mut events);
        assert!(device.host_compatible());

        let response = SpiCell::decode(&device.next_transaction(1)).unwrap();
        let records: Vec<_, 2> = RecordIter::new(response.payload(), response.header.record_count)
            .map(Result::unwrap)
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].record_type, RECORD_HELLO_ACK);
        assert_eq!(UsbState::decode(records[1].data), Ok(fallback_state()));
    }

    #[test]
    fn duplicate_standard_input_is_delivered_once() {
        let mut device = DeviceLink::new(2, fallback_state());
        let mut host = ReliableSender::new(1);
        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: DEVICE_CAPABILITIES,
            active_profile_hash: 0,
        }
        .encode();
        let mut events = Vec::<_, 4>::new();
        device.handle_transaction(&host_cell(&mut host, RECORD_HELLO, &hello), 0, &mut events);
        let report = StandardInputReport {
            flags: 0,
            sequence: 7,
            report: StandardHidReport::Keyboard(Keyboard6KroReport::from_bytes([
                0, 0, 4, 0, 0, 0, 0, 0,
            ])),
        };
        let (wire, length) = report.encode();
        let cell = host_cell(
            &mut host,
            RECORD_STANDARD_INPUT_REPORT,
            &wire[..length as usize],
        );
        device.handle_transaction(&cell, 1, &mut events);
        device.handle_transaction(&cell, 2, &mut events);

        assert_eq!(events.as_slice(), &[DeviceLinkEvent::StandardInput(report)]);
        assert_eq!(device.diagnostics().standard_reports_received, 1);
        assert_eq!(device.diagnostics().duplicate_cells, 1);
    }

    #[test]
    fn new_host_session_must_repeat_hello_before_commands_are_acked() {
        let mut device = DeviceLink::new(2, fallback_state());
        let mut original_host = ReliableSender::new(1);
        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: DEVICE_CAPABILITIES,
            active_profile_hash: 0,
        }
        .encode();
        let mut events = Vec::<_, 4>::new();
        device.handle_transaction(
            &host_cell(&mut original_host, RECORD_HELLO, &hello),
            0,
            &mut events,
        );
        assert!(device.host_compatible());

        let mut restarted_host = ReliableSender::new(9);
        let report = StandardInputReport {
            flags: 0,
            sequence: 1,
            report: StandardHidReport::Keyboard(Keyboard6KroReport::from_bytes([
                0, 0, 4, 0, 0, 0, 0, 0,
            ])),
        };
        let (wire, length) = report.encode();
        device.handle_transaction(
            &host_cell(
                &mut restarted_host,
                RECORD_STANDARD_INPUT_REPORT,
                &wire[..length as usize],
            ),
            1,
            &mut events,
        );

        assert!(!device.host_compatible());
        assert!(events.is_empty());
        assert_eq!(device.receiver.cumulative_ack(), 0);

        restarted_host.reset_session(9);
        device.handle_transaction(
            &host_cell(&mut restarted_host, RECORD_HELLO, &hello),
            2,
            &mut events,
        );
        assert!(device.host_compatible());
    }

    #[test]
    fn link_reset_requires_a_new_hello_before_more_commands_are_acked() {
        let mut device = DeviceLink::new(2, fallback_state());
        let mut host = ReliableSender::new(1);
        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: DEVICE_CAPABILITIES,
            active_profile_hash: 0,
        }
        .encode();
        let mut events = Vec::<_, 4>::new();
        device.handle_transaction(&host_cell(&mut host, RECORD_HELLO, &hello), 0, &mut events);
        device.handle_transaction(
            &host_cell(&mut host, RECORD_LINK_RESET, &[]),
            1,
            &mut events,
        );
        assert!(!device.host_compatible());
        let ack_before = device.receiver.cumulative_ack();

        device.handle_transaction(
            &host_cell(&mut host, RECORD_STANDARD_RELEASE_ALL, &[]),
            2,
            &mut events,
        );

        assert!(events.is_empty());
        assert_eq!(device.receiver.cumulative_ack(), ack_before);
    }

    #[test]
    fn standard_output_is_sent_only_after_compatible_hello() {
        let mut device = DeviceLink::new(2, fallback_state());
        let output = StandardOutputReport::new(1, &[0x02]).unwrap();
        assert!(!device.queue_standard_output(output, 0));

        let mut host = ReliableSender::new(1);
        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: DEVICE_CAPABILITIES,
            active_profile_hash: 0,
        }
        .encode();
        let mut events = Vec::<_, 1>::new();
        device.handle_transaction(&host_cell(&mut host, RECORD_HELLO, &hello), 0, &mut events);
        // Send the mandatory HELLO_ACK/USB_STATE response first.
        let _ = device.next_transaction(1);
        assert!(device.queue_standard_output(output, 2));
        let cell = SpiCell::decode(&device.next_transaction(2)).unwrap();
        let record = RecordIter::new(cell.payload(), cell.header.record_count)
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(record.record_type, RECORD_STANDARD_OUTPUT_REPORT);
        assert_eq!(StandardOutputReport::decode(record.data), Ok(output));
    }

    #[test]
    fn profile_records_preserve_chunk_offsets_and_payloads() {
        let mut device = DeviceLink::new_with_profile_storage(2, fallback_state(), 0x1122_3344);
        let mut host = ReliableSender::new(1);
        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: CAPABILITY_DYNAMIC_PROFILE,
            active_profile_hash: 0,
        }
        .encode();
        let mut events = Vec::<_, 4>::new();
        device.handle_transaction(&host_cell(&mut host, RECORD_HELLO, &hello), 0, &mut events);

        let chunk = ProfileChunk {
            transfer_id: 9,
            offset: 96,
            data: &[1, 2, 3, 4],
        };
        let mut encoded = [0; 104];
        let length = chunk.encode(&mut encoded).unwrap();
        device.handle_transaction(
            &host_cell(&mut host, RECORD_PROFILE_CHUNK, &encoded[..length]),
            1,
            &mut events,
        );

        let received = events
            .iter()
            .find_map(|event| match event {
                DeviceLinkEvent::ProfileChunk(chunk) => Some(chunk.as_borrowed()),
                _ => None,
            })
            .unwrap();
        assert_eq!(received, chunk);
    }

    #[test]
    fn activation_is_delivered_only_after_a_compatible_hello() {
        let mut device = DeviceLink::new_with_profile_storage(2, fallback_state(), 0);
        let mut host = ReliableSender::new(1);
        let activate = ActivateProfile {
            operation_id: 12,
            profile_hash: 0x1122_3344,
        };
        let mut events = Vec::<_, 2>::new();
        device.handle_transaction(
            &host_cell(&mut host, RECORD_ACTIVATE_PROFILE, &activate.encode()),
            0,
            &mut events,
        );
        assert!(events.is_empty());

        let hello = Hello {
            role: InterchipRole::Host,
            protocol_version: SPI_PROTOCOL_VERSION,
            firmware_major: 0,
            firmware_minor: 2,
            capabilities: CAPABILITY_DYNAMIC_PROFILE,
            active_profile_hash: 0,
        }
        .encode();
        // The unacknowledged pre-HELLO record remains sender-pending. A real
        // Host link starts a fresh session before its handshake.
        host.reset_session(1);
        device.handle_transaction(&host_cell(&mut host, RECORD_HELLO, &hello), 1, &mut events);
        device.handle_transaction(
            &host_cell(&mut host, RECORD_ACTIVATE_PROFILE, &activate.encode()),
            2,
            &mut events,
        );
        assert_eq!(
            events.as_slice(),
            &[DeviceLinkEvent::ActivateProfile(activate)]
        );
    }
}
