use super::cell::SPI_CELL_PAYLOAD_LEN;

pub const RECORD_HEADER_LEN: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Record<'a> {
    pub record_type: u8,
    pub flags: u8,
    pub data: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordRef<'a> {
    pub record_type: u8,
    pub flags: u8,
    pub data: &'a [u8],
}

pub fn encode_records(
    records: &[Record<'_>],
    out: &mut [u8; SPI_CELL_PAYLOAD_LEN],
) -> Result<(u16, u8), RecordCodecError> {
    if records.len() > u8::MAX as usize {
        return Err(RecordCodecError::TooManyRecords);
    }
    out.fill(0);
    let mut offset = 0usize;
    for record in records {
        if record.data.len() > u16::MAX as usize {
            return Err(RecordCodecError::RecordTooLong);
        }
        let raw_len = RECORD_HEADER_LEN + record.data.len();
        let padded_len = align4(raw_len).ok_or(RecordCodecError::RecordTooLong)?;
        if offset + padded_len > out.len() {
            return Err(RecordCodecError::PayloadCapacity);
        }
        out[offset] = record.record_type;
        out[offset + 1] = record.flags;
        out[offset + 2..offset + 4].copy_from_slice(&(record.data.len() as u16).to_le_bytes());
        out[offset + 4..offset + 4 + record.data.len()].copy_from_slice(record.data);
        offset += padded_len;
    }
    Ok((offset as u16, records.len() as u8))
}

#[derive(Clone, Debug)]
pub struct RecordIter<'a> {
    bytes: &'a [u8],
    offset: usize,
    remaining: u8,
    failed: bool,
}

impl<'a> RecordIter<'a> {
    pub const fn new(bytes: &'a [u8], record_count: u8) -> Self {
        Self {
            bytes,
            offset: 0,
            remaining: record_count,
            failed: false,
        }
    }

    pub fn finish(self) -> Result<(), RecordCodecError> {
        if self.failed {
            return Err(RecordCodecError::MalformedRecord);
        }
        if self.remaining != 0 || self.offset != self.bytes.len() {
            return Err(RecordCodecError::RecordCountMismatch);
        }
        Ok(())
    }
}

impl<'a> Iterator for RecordIter<'a> {
    type Item = Result<RecordRef<'a>, RecordCodecError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        if self.offset + RECORD_HEADER_LEN > self.bytes.len() {
            self.failed = true;
            return Some(Err(RecordCodecError::MalformedRecord));
        }
        let start = self.offset;
        let data_len = u16::from_le_bytes([self.bytes[start + 2], self.bytes[start + 3]]) as usize;
        let raw_len = match RECORD_HEADER_LEN.checked_add(data_len) {
            Some(length) => length,
            None => {
                self.failed = true;
                return Some(Err(RecordCodecError::MalformedRecord));
            }
        };
        let Some(padded_len) = align4(raw_len) else {
            self.failed = true;
            return Some(Err(RecordCodecError::MalformedRecord));
        };
        if start + padded_len > self.bytes.len() {
            self.failed = true;
            return Some(Err(RecordCodecError::MalformedRecord));
        }
        if self.bytes[start + raw_len..start + padded_len]
            .iter()
            .any(|byte| *byte != 0)
        {
            self.failed = true;
            return Some(Err(RecordCodecError::NonZeroPadding));
        }
        self.offset += padded_len;
        self.remaining -= 1;
        Some(Ok(RecordRef {
            record_type: self.bytes[start],
            flags: self.bytes[start + 1],
            data: &self.bytes[start + RECORD_HEADER_LEN..start + raw_len],
        }))
    }
}

fn align4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|value| value & !3)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordCodecError {
    TooManyRecords,
    RecordTooLong,
    PayloadCapacity,
    MalformedRecord,
    NonZeroPadding,
    RecordCountMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_records_round_trip_with_zero_padding() {
        let records = [
            Record {
                record_type: 1,
                flags: 2,
                data: &[1, 2, 3],
            },
            Record {
                record_type: 0x26,
                flags: 0,
                data: &[4, 5, 6, 7],
            },
        ];
        let mut payload = [0; SPI_CELL_PAYLOAD_LEN];
        let (length, count) = encode_records(&records, &mut payload).unwrap();
        let mut iter = RecordIter::new(&payload[..length as usize], count);
        assert_eq!(
            iter.next().unwrap().unwrap(),
            RecordRef {
                record_type: 1,
                flags: 2,
                data: &[1, 2, 3]
            }
        );
        assert_eq!(iter.next().unwrap().unwrap().data, &[4, 5, 6, 7]);
        assert!(iter.next().is_none());
        assert_eq!(iter.finish(), Ok(()));
    }

    #[test]
    fn nonzero_padding_and_trailing_records_are_rejected() {
        let mut payload = [0; SPI_CELL_PAYLOAD_LEN];
        let (length, _) = encode_records(
            &[Record {
                record_type: 1,
                flags: 0,
                data: &[9],
            }],
            &mut payload,
        )
        .unwrap();
        payload[5] = 1;
        let mut iter = RecordIter::new(&payload[..length as usize], 1);
        assert_eq!(iter.next(), Some(Err(RecordCodecError::NonZeroPadding)));

        let iter = RecordIter::new(&[1, 0, 0, 0], 0);
        assert_eq!(iter.finish(), Err(RecordCodecError::RecordCountMismatch));
    }
}
