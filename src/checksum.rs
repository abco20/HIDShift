pub fn crc16_ccitt_false(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in bytes {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finalize()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub const fn new() -> Self {
        Self { state: 0xffff_ffff }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (self.state & 1).wrapping_neg();
                self.state = (self.state >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }

    pub const fn finalize(self) -> u32 {
        !self.state
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ccitt_false_matches_standard_check_value() {
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29b1);
    }

    #[test]
    fn ieee_crc32_matches_standard_check_value() {
        assert_eq!(crc32_ieee(b"123456789"), 0xcbf4_3926);
    }
}
