use crate::ids::{DeviceId, InterfaceId};

use super::{InputLane, InputReportRecord, MAX_HID_REPORT_SIZE, MotionCumulative};

pub const INPUT_SNAPSHOT_RECORD_HEADER_LEN: usize = 13;
/// Number of ordered critical transitions repeated in realtime state frames.
///
/// Three records recover a complete short tap even when both the press and
/// release broadcasts are lost: the next critical or cumulative-motion frame
/// still carries both transitions. Keeping the window deliberately small is
/// important because ESP-NOW callback tails grow when every mouse frame is
/// padded to the maximum 250-byte action-frame payload.
pub const REALTIME_CRITICAL_JOURNAL_CAPACITY: usize = 3;
const INPUT_SNAPSHOT_MOTION_EXTENSION_LEN: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    Empty,
    BufferTooSmall,
    InvalidLength,
    ReportTooLarge,
    TooManyRecords,
    InvalidLane,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodedInputSnapshot {
    pub record_count: u8,
    pub records_len: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputSnapshotRecord<'a> {
    pub lane: InputLane,
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    pub sequence: u32,
    pub e2e_sequence: u32,
    pub motion: MotionCumulative,
    pub report: &'a [u8],
}

/// Validated iterator over the records carried by one input snapshot.
///
/// The outer bridge message owns the record count. Validating the complete
/// stream before constructing this iterator prevents a truncated recovery
/// record from being partially applied to USB HID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputSnapshotRecords<'a> {
    bytes: &'a [u8],
    remaining: u8,
    offset: usize,
}

impl<'a> InputSnapshotRecords<'a> {
    pub fn new(record_count: u8, bytes: &'a [u8]) -> Result<Self, SnapshotError> {
        if record_count == 0 {
            return Err(SnapshotError::Empty);
        }
        let mut offset = 0usize;
        for _ in 0..record_count {
            let base_end = offset
                .checked_add(INPUT_SNAPSHOT_RECORD_HEADER_LEN)
                .ok_or(SnapshotError::InvalidLength)?;
            let header = bytes
                .get(offset..base_end)
                .ok_or(SnapshotError::InvalidLength)?;
            let lane = decode_lane(header[0])?;
            let header_end = base_end
                .checked_add(motion_extension_len(lane))
                .ok_or(SnapshotError::InvalidLength)?;
            if header_end > bytes.len() {
                return Err(SnapshotError::InvalidLength);
            }
            let report_len = u16::from_le_bytes([header[11], header[12]]) as usize;
            if report_len > MAX_HID_REPORT_SIZE {
                return Err(SnapshotError::ReportTooLarge);
            }
            offset = header_end
                .checked_add(report_len)
                .ok_or(SnapshotError::InvalidLength)?;
            if offset > bytes.len() {
                return Err(SnapshotError::InvalidLength);
            }
        }
        if offset != bytes.len() {
            return Err(SnapshotError::InvalidLength);
        }
        Ok(Self {
            bytes,
            remaining: record_count,
            offset: 0,
        })
    }
}

impl<'a> Iterator for InputSnapshotRecords<'a> {
    type Item = InputSnapshotRecord<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let base_end = self.offset + INPUT_SNAPSHOT_RECORD_HEADER_LEN;
        let header = self.bytes.get(self.offset..base_end)?;
        let lane = decode_lane(header[0]).ok()?;
        let header_end = base_end + motion_extension_len(lane);
        let motion = if lane == InputLane::Motion {
            let extension = self.bytes.get(base_end..header_end)?;
            MotionCumulative {
                x: i32::from_le_bytes(extension[0..4].try_into().ok()?),
                y: i32::from_le_bytes(extension[4..8].try_into().ok()?),
                wheel: i32::from_le_bytes(extension[8..12].try_into().ok()?),
                pan: i32::from_le_bytes(extension[12..16].try_into().ok()?),
            }
        } else {
            MotionCumulative::zero()
        };
        let report_len = u16::from_le_bytes([header[11], header[12]]) as usize;
        let report_end = header_end + report_len;
        let report = self.bytes.get(header_end..report_end)?;
        self.offset = report_end;
        self.remaining -= 1;
        Some(InputSnapshotRecord {
            lane,
            device_id: DeviceId(header[1]),
            interface_id: InterfaceId(header[2]),
            sequence: u32::from_le_bytes([header[3], header[4], header[5], header[6]]),
            e2e_sequence: u32::from_le_bytes([header[7], header[8], header[9], header[10]]),
            motion,
            report,
        })
    }
}

const fn motion_extension_len(lane: InputLane) -> usize {
    match lane {
        InputLane::Critical => 0,
        InputLane::Motion => INPUT_SNAPSHOT_MOTION_EXTENSION_LEN,
    }
}

fn decode_lane(value: u8) -> Result<InputLane, SnapshotError> {
    match value {
        1 => Ok(InputLane::Motion),
        2 => Ok(InputLane::Critical),
        _ => Err(SnapshotError::InvalidLane),
    }
}

fn encoded_record_len(lane: InputLane, report_len: usize) -> usize {
    INPUT_SNAPSHOT_RECORD_HEADER_LEN + motion_extension_len(lane) + report_len
}

/// Fixed-size rolling transition journal used to build self-healing input
/// snapshots. Each new snapshot carries the newest suffix which fits in the
/// requested radio budget; no packet identity is retained or retransmitted.
pub struct InputSnapshotHistory<const CAPACITY: usize> {
    entries: [Option<InputReportRecord>; CAPACITY],
    start: usize,
    len: usize,
}

impl<const CAPACITY: usize> InputSnapshotHistory<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; CAPACITY],
            start: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, report: InputReportRecord) {
        if CAPACITY == 0 {
            return;
        }
        if self.len < CAPACITY {
            let index = (self.start + self.len) % CAPACITY;
            self.entries[index] = Some(report);
            self.len += 1;
        } else {
            self.entries[self.start] = Some(report);
            self.start = (self.start + 1) % CAPACITY;
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
        self.start = 0;
        self.len = 0;
    }

    /// Encode the largest chronological suffix which fits `target_len`.
    /// A single oversized newest report is still emitted when it fits `out`,
    /// allowing the wire fragmenter to handle uncommon reports over 212 B.
    pub fn encode_recent(
        &self,
        out: &mut [u8],
        target_len: usize,
    ) -> Result<EncodedInputSnapshot, SnapshotError> {
        self.encode_state(out, target_len, None)
    }

    /// Encode recent critical transitions and, when present, the latest
    /// cumulative motion in one broadcast state frame. Motion is always the
    /// final record; older critical transitions are trimmed first.
    pub fn encode_state(
        &self,
        out: &mut [u8],
        target_len: usize,
        motion: Option<&InputReportRecord>,
    ) -> Result<EncodedInputSnapshot, SnapshotError> {
        if self.is_empty() && motion.is_none() {
            return Err(SnapshotError::Empty);
        }
        let budget = target_len.min(out.len());
        let motion_len = motion.map_or(0, |report| {
            encoded_record_len(InputLane::Motion, report.report().len())
        });
        if motion_len > out.len() {
            return Err(SnapshotError::BufferTooSmall);
        }
        let critical_budget = budget.saturating_sub(motion_len);
        let mut selected = 0usize;
        let mut selected_len = 0usize;
        for reverse in 0..self.len {
            let logical = self.len - 1 - reverse;
            let report = self.entry(logical).ok_or(SnapshotError::InvalidLength)?;
            let record_len = encoded_record_len(InputLane::Critical, report.report().len());
            if selected == 0 && motion.is_none() && record_len > critical_budget {
                if record_len > out.len() {
                    return Err(SnapshotError::BufferTooSmall);
                }
                selected = 1;
                selected_len = record_len;
                break;
            }
            if selected_len + record_len > critical_budget {
                break;
            }
            selected += 1;
            selected_len += record_len;
        }
        if selected == 0 && motion.is_none() {
            return Err(SnapshotError::BufferTooSmall);
        }
        let record_count = u8::try_from(selected + usize::from(motion.is_some()))
            .map_err(|_| SnapshotError::TooManyRecords)?;
        let total_len = selected_len
            .checked_add(motion_len)
            .ok_or(SnapshotError::InvalidLength)?;
        if total_len > out.len() {
            return Err(SnapshotError::BufferTooSmall);
        }
        let mut offset = 0usize;
        for logical in self.len - selected..self.len {
            let report = self.entry(logical).ok_or(SnapshotError::InvalidLength)?;
            offset = encode_record(out, offset, InputLane::Critical, report)?;
        }
        if let Some(motion) = motion {
            offset = encode_record(out, offset, InputLane::Motion, motion)?;
        }
        Ok(EncodedInputSnapshot {
            record_count,
            records_len: offset,
        })
    }

    fn entry(&self, logical: usize) -> Option<&InputReportRecord> {
        if logical >= self.len || CAPACITY == 0 {
            return None;
        }
        self.entries[(self.start + logical) % CAPACITY].as_ref()
    }
}

fn encode_record(
    out: &mut [u8],
    offset: usize,
    lane: InputLane,
    report: &InputReportRecord,
) -> Result<usize, SnapshotError> {
    let report_bytes = report.report();
    let header_end = offset + INPUT_SNAPSHOT_RECORD_HEADER_LEN + motion_extension_len(lane);
    let record_end = header_end + report_bytes.len();
    let record = out
        .get_mut(offset..record_end)
        .ok_or(SnapshotError::BufferTooSmall)?;
    record[0] = lane as u8;
    record[1] = report.device_id.0;
    record[2] = report.interface_id.0;
    record[3..7].copy_from_slice(&report.sequence.to_le_bytes());
    record[7..11].copy_from_slice(&report.e2e_sequence.to_le_bytes());
    record[11..13].copy_from_slice(&(report_bytes.len() as u16).to_le_bytes());
    if lane == InputLane::Motion {
        record[13..17].copy_from_slice(&report.motion.x.to_le_bytes());
        record[17..21].copy_from_slice(&report.motion.y.to_le_bytes());
        record[21..25].copy_from_slice(&report.motion.wheel.to_le_bytes());
        record[25..29].copy_from_slice(&report.motion.pan.to_le_bytes());
    }
    record[header_end - offset..].copy_from_slice(report_bytes);
    Ok(record_end)
}

impl<const CAPACITY: usize> Default for InputSnapshotHistory<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DeviceId, InterfaceId};
    use crate::link::{InputReportRecord, InputSequenceDecision, InputSequenceWindow};

    fn report(sequence: u32, bytes: &[u8]) -> InputReportRecord {
        InputReportRecord::new(DeviceId(1), InterfaceId(2), sequence, sequence + 100, bytes)
            .unwrap()
    }

    #[test]
    fn next_snapshot_recovers_a_dropped_keyboard_transition_without_packet_retransmission() {
        let mut history = InputSnapshotHistory::<8>::new();
        history.push(report(10, &[1, 0, 0, 4, 0, 0, 0, 0, 0]));

        let mut dropped = [0; 224];
        history.encode_recent(&mut dropped, 224).unwrap();

        history.push(report(11, &[1, 0, 0, 0, 0, 0, 0, 0, 0]));
        let mut delivered = [0; 224];
        let encoded = history.encode_recent(&mut delivered, 224).unwrap();
        let records =
            InputSnapshotRecords::new(encoded.record_count, &delivered[..encoded.records_len])
                .unwrap();

        let recovered: heapless::Vec<_, 8> = records.map(|record| record.sequence).collect();
        assert_eq!(recovered.as_slice(), &[10, 11]);
    }

    #[test]
    fn duplicate_recovery_snapshots_do_not_reapply_keyboard_events() {
        let mut history = InputSnapshotHistory::<8>::new();
        history.push(report(42, &[1, 0, 0, 4, 0, 0, 0, 0, 0]));
        let mut payload = [0; 224];
        let encoded = history.encode_recent(&mut payload, 224).unwrap();
        let mut received = InputSequenceWindow::new();

        let mut applied = 0;
        for _ in 0..2 {
            for record in
                InputSnapshotRecords::new(encoded.record_count, &payload[..encoded.records_len])
                    .unwrap()
            {
                if received.observe_forward_only(record.sequence) == InputSequenceDecision::New {
                    applied += 1;
                }
            }
        }
        assert_eq!(applied, 1);
    }

    #[test]
    fn snapshot_keeps_the_newest_suffix_inside_one_espnow_payload() {
        let mut history = InputSnapshotHistory::<16>::new();
        for sequence in 1..=16 {
            history.push(report(sequence, &[sequence as u8; 32]));
        }
        let mut payload = [0; 224];
        let encoded = history.encode_recent(&mut payload, 224).unwrap();
        assert!(encoded.records_len <= 224);
        let sequences: heapless::Vec<_, 16> =
            InputSnapshotRecords::new(encoded.record_count, &payload[..encoded.records_len])
                .unwrap()
                .map(|record| record.sequence)
                .collect();
        assert_eq!(sequences.last(), Some(&16));
        assert!(sequences[0] > 1);
    }

    #[test]
    fn malformed_snapshot_record_stream_is_rejected_before_hid_delivery() {
        let truncated = [1, 2, 3];
        assert_eq!(
            InputSnapshotRecords::new(1, &truncated),
            Err(SnapshotError::InvalidLength)
        );
    }

    #[test]
    fn motion_state_frame_carries_cumulative_axes_and_recent_critical_history() {
        let mut history = InputSnapshotHistory::<8>::new();
        history.push(report(10, &[1, 0, 0, 4, 0, 0, 0, 0, 0]));
        let motion = InputReportRecord::new_with_motion(
            DeviceId(1),
            InterfaceId(3),
            20,
            120,
            crate::link::MotionCumulative {
                x: 101,
                y: -202,
                wheel: 3,
                pan: -4,
            },
            &[2, 0, 1, 0],
        )
        .unwrap();
        let mut payload = [0; 224];
        let encoded = history
            .encode_state(&mut payload, 224, Some(&motion))
            .unwrap();
        let records: heapless::Vec<_, 8> =
            InputSnapshotRecords::new(encoded.record_count, &payload[..encoded.records_len])
                .unwrap()
                .collect();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].lane, crate::link::InputLane::Critical);
        assert_eq!(records[0].sequence, 10);
        assert_eq!(records[1].lane, crate::link::InputLane::Motion);
        assert_eq!(records[1].sequence, 20);
        assert_eq!(records[1].motion, motion.motion);
        assert_eq!(records[1].report, &[2, 0, 1, 0]);
    }

    #[test]
    fn motion_is_kept_when_only_a_suffix_of_critical_history_fits() {
        let mut history = InputSnapshotHistory::<16>::new();
        for sequence in 1..=16 {
            history.push(report(sequence, &[sequence as u8; 32]));
        }
        let motion = InputReportRecord::new_with_motion(
            DeviceId(1),
            InterfaceId(3),
            90,
            190,
            crate::link::MotionCumulative {
                x: 90,
                y: 0,
                wheel: 0,
                pan: 0,
            },
            &[2, 0],
        )
        .unwrap();
        let mut payload = [0; 224];
        let encoded = history
            .encode_state(&mut payload, 224, Some(&motion))
            .unwrap();
        let records: heapless::Vec<_, 16> =
            InputSnapshotRecords::new(encoded.record_count, &payload[..encoded.records_len])
                .unwrap()
                .collect();

        assert!(encoded.records_len <= 224);
        assert!(records.len() < 17);
        assert_eq!(records.last().unwrap().lane, crate::link::InputLane::Motion);
        assert_eq!(records.last().unwrap().sequence, 90);
    }

    #[test]
    fn realtime_journal_recovers_two_lost_transitions_in_a_compact_motion_frame() {
        let mut history = InputSnapshotHistory::<REALTIME_CRITICAL_JOURNAL_CAPACITY>::new();
        history.push(report(10, &[1, 0, 0, 4, 0, 0, 0, 0, 0]));
        history.push(report(11, &[1, 0, 0, 0, 0, 0, 0, 0, 0]));
        history.push(report(12, &[3, 0, 0]));
        let motion = InputReportRecord::new_with_motion(
            DeviceId(1),
            InterfaceId(3),
            20,
            120,
            crate::link::MotionCumulative {
                x: 7,
                y: 0,
                wheel: 0,
                pan: 0,
            },
            &[2, 0, 7, 0, 0, 0, 0, 0],
        )
        .unwrap();
        let mut payload = [0; 224];
        let encoded = history
            .encode_state(&mut payload, 224, Some(&motion))
            .unwrap();
        let records: heapless::Vec<_, 4> =
            InputSnapshotRecords::new(encoded.record_count, &payload[..encoded.records_len])
                .unwrap()
                .collect();

        assert_eq!(
            records
                .iter()
                .map(|record| record.sequence)
                .collect::<heapless::Vec<_, 4>>()
                .as_slice(),
            &[10, 11, 12, 20]
        );
        assert!(encoded.records_len <= 100, "{}", encoded.records_len);
    }
}
