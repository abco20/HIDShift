use crate::input::ConsumerUsage;

pub const CONSUMER_REPORT_ID: u8 = 3;
pub const CONSUMER_REPORT_LEN: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsumerReport {
    bytes: [u8; CONSUMER_REPORT_LEN],
}

impl ConsumerReport {
    pub const fn from_usage(usage: ConsumerUsage) -> Self {
        Self {
            bytes: usage.0.to_le_bytes(),
        }
    }

    pub const fn from_usage_id(usage_id: u16) -> Self {
        Self {
            bytes: usage_id.to_le_bytes(),
        }
    }

    pub const fn release() -> Self {
        Self {
            bytes: [0; CONSUMER_REPORT_LEN],
        }
    }

    pub const fn as_bytes(&self) -> &[u8; CONSUMER_REPORT_LEN] {
        &self.bytes
    }
}

pub type BleConsumerReport = ConsumerReport;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_report_uses_little_endian_usage_id() {
        assert_eq!(
            BleConsumerReport::from_usage(ConsumerUsage(0x00e9)).as_bytes(),
            &[0xe9, 0x00]
        );
    }
}
