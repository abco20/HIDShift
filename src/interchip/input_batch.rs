use super::SPI_CELL_PAYLOAD_LEN;
use super::message::{
    RECORD_RAW_ENDPOINT_IN, RECORD_STANDARD_INPUT_REPORT, RawEndpointReport, StandardInputReport,
};
use super::record::RECORD_HEADER_LEN;

/// Matches the Device firmware's fixed per-cell event capacity.
pub const INPUT_REPORT_BATCH_MAX_REPORTS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputReport {
    Raw(RawEndpointReport),
    Standard(StandardInputReport),
}

impl InputReport {
    fn encode(self) -> (u8, [u8; super::message::RAW_ENDPOINT_MAX_WIRE_LEN], usize) {
        let mut data = [0; super::message::RAW_ENDPOINT_MAX_WIRE_LEN];
        match self {
            Self::Raw(report) => {
                let report_data = report.data();
                data[0] = report.endpoint_address;
                data[2..4].copy_from_slice(&report.packet_sequence.to_le_bytes());
                data[4..6].copy_from_slice(&(report_data.len() as u16).to_le_bytes());
                data[6..6 + report_data.len()].copy_from_slice(report_data);
                (RECORD_RAW_ENDPOINT_IN, data, 6 + report_data.len())
            }
            Self::Standard(report) => {
                let (encoded, length) = report.encode();
                data[..usize::from(length)].copy_from_slice(&encoded[..usize::from(length)]);
                (RECORD_STANDARD_INPUT_REPORT, data, usize::from(length))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputReportBatch {
    payload: [u8; SPI_CELL_PAYLOAD_LEN],
    payload_len: u8,
    report_count: u8,
}

impl InputReportBatch {
    pub fn new(report: InputReport) -> Self {
        let mut batch = Self {
            payload: [0; SPI_CELL_PAYLOAD_LEN],
            payload_len: 0,
            report_count: 0,
        };
        // Both input report encodings are individually smaller than a cell.
        let (record_type, data, data_len) = report.encode();
        batch.push_encoded(record_type, &data[..data_len]);
        batch
    }

    /// Adds a report only when the complete record still fits in one cell.
    /// The rejected report is returned unchanged so the caller can preserve
    /// ordering in a pending slot.
    pub fn try_push(&mut self, report: InputReport) -> Result<(), InputReport> {
        if usize::from(self.report_count) >= INPUT_REPORT_BATCH_MAX_REPORTS {
            return Err(report);
        }
        let (record_type, data, data_len) = report.encode();
        let record_len = align4(RECORD_HEADER_LEN + data_len);
        let offset = usize::from(self.payload_len);
        if offset + record_len > SPI_CELL_PAYLOAD_LEN {
            return Err(report);
        }

        self.push_encoded(record_type, &data[..data_len]);
        Ok(())
    }

    fn push_encoded(&mut self, record_type: u8, data: &[u8]) {
        let record_len = align4(RECORD_HEADER_LEN + data.len());
        let offset = usize::from(self.payload_len);
        self.payload[offset] = record_type;
        self.payload[offset + 1] = 0;
        self.payload[offset + 2..offset + 4].copy_from_slice(&(data.len() as u16).to_le_bytes());
        let message = offset + RECORD_HEADER_LEN;
        self.payload[message..message + data.len()].copy_from_slice(data);
        self.payload_len = (offset + record_len) as u8;
        self.report_count += 1;
    }

    #[cfg(test)]
    const fn len(&self) -> usize {
        self.report_count as usize
    }

    pub fn encoded(&self) -> (&[u8], u8) {
        (
            &self.payload[..usize::from(self.payload_len)],
            self.report_count,
        )
    }
}

const fn align4(value: usize) -> usize {
    (value + 3) & !3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interchip::RecordIter;
    use crate::reports::{MouseReport, StandardHidReport};

    fn standard_mouse(sequence: u16, x: i8) -> StandardInputReport {
        StandardInputReport {
            flags: 0,
            sequence,
            report: StandardHidReport::Mouse(MouseReport::from_bytes([0, x as u8, 0, 0, 0])),
        }
    }

    #[test]
    fn four_standard_reports_share_one_cell_in_order() {
        let reports = [
            standard_mouse(1, 1),
            standard_mouse(2, 2),
            standard_mouse(3, 3),
            standard_mouse(4, 4),
        ];
        let mut batch = InputReportBatch::new(InputReport::Standard(reports[0]));
        for report in &reports[1..] {
            batch.try_push(InputReport::Standard(*report)).unwrap();
        }

        let (payload, count) = batch.encoded();
        let decoded: heapless::Vec<_, INPUT_REPORT_BATCH_MAX_REPORTS> =
            RecordIter::new(payload, count)
                .map(|record| {
                    let record = record.unwrap();
                    assert_eq!(record.record_type, RECORD_STANDARD_INPUT_REPORT);
                    StandardInputReport::decode(record.data).unwrap()
                })
                .collect();

        assert_eq!(decoded.as_slice(), &reports);
    }

    #[test]
    fn raw_and_standard_reports_can_share_a_cell_without_reordering() {
        let raw = RawEndpointReport::new(0x82, 9, &[1, 2, 3]).unwrap();
        let standard = standard_mouse(10, -4);
        let mut batch = InputReportBatch::new(InputReport::Raw(raw));
        batch.try_push(InputReport::Standard(standard)).unwrap();

        let (payload, count) = batch.encoded();
        let records: heapless::Vec<_, INPUT_REPORT_BATCH_MAX_REPORTS> =
            RecordIter::new(payload, count).collect();
        assert_eq!(
            records[0].as_ref().unwrap().record_type,
            RECORD_RAW_ENDPOINT_IN
        );
        assert_eq!(
            records[1].as_ref().unwrap().record_type,
            RECORD_STANDARD_INPUT_REPORT
        );
        assert_eq!(
            RawEndpointReport::decode(records[0].as_ref().unwrap().data),
            Ok(raw)
        );
        assert_eq!(
            StandardInputReport::decode(records[1].as_ref().unwrap().data),
            Ok(standard)
        );
    }

    #[test]
    fn report_that_does_not_fit_is_returned_for_ordered_retry() {
        let large = RawEndpointReport::new(0x81, 1, &[0x5a; 64]).unwrap();
        let next = RawEndpointReport::new(0x82, 2, &[0xa5; 64]).unwrap();
        let mut batch = InputReportBatch::new(InputReport::Raw(large));

        assert_eq!(
            batch.try_push(InputReport::Raw(next)),
            Err(InputReport::Raw(next))
        );
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn fixed_event_capacity_returns_the_fifth_report() {
        let first = RawEndpointReport::new(0x81, 1, &[]).unwrap();
        let mut batch = InputReportBatch::new(InputReport::Raw(first));
        for sequence in 2..=4 {
            batch
                .try_push(InputReport::Raw(
                    RawEndpointReport::new(0x81, sequence, &[]).unwrap(),
                ))
                .unwrap();
        }
        let fifth = RawEndpointReport::new(0x81, 5, &[]).unwrap();

        assert_eq!(
            batch.try_push(InputReport::Raw(fifth)),
            Err(InputReport::Raw(fifth))
        );
        assert_eq!(batch.len(), INPUT_REPORT_BATCH_MAX_REPORTS);
    }
}
