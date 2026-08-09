/// Fixed-capacity CR/LF-delimited line accumulator for byte-stream adapters.
///
/// Oversized lines are discarded through their delimiter so a trailing fragment
/// cannot be mistaken for a valid protocol message.
pub struct SerialLineBuffer<const CAPACITY: usize> {
    bytes: [u8; CAPACITY],
    len: usize,
    discarding_oversized_line: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletedSerialLine<const CAPACITY: usize> {
    len: usize,
}

impl<const CAPACITY: usize> Default for SerialLineBuffer<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAPACITY: usize> SerialLineBuffer<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            len: 0,
            discarding_oversized_line: false,
        }
    }

    /// Returns a completed-line token. Call [`Self::line`] before pushing
    /// another byte to read the completed contents.
    pub fn push(&mut self, byte: u8) -> Option<CompletedSerialLine<CAPACITY>> {
        if matches!(byte, b'\n' | b'\r') {
            if self.discarding_oversized_line {
                self.discarding_oversized_line = false;
                self.len = 0;
                return None;
            }
            let len = self.len;
            self.len = 0;
            return Some(CompletedSerialLine { len });
        }
        if self.discarding_oversized_line {
            return None;
        }
        if self.len == self.bytes.len() {
            self.len = 0;
            self.discarding_oversized_line = true;
            return None;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        None
    }

    pub fn line(&self, completed: CompletedSerialLine<CAPACITY>) -> &[u8] {
        &self.bytes[..completed.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_multiple_lines_from_one_input_chunk() {
        let mut buffer = SerialLineBuffer::<8>::new();
        let mut line_index = 0;
        for byte in b"first\nsecond\r" {
            if let Some(completed) = buffer.push(*byte) {
                let line = buffer.line(completed);
                match line_index {
                    0 => assert_eq!(line, b"first"),
                    1 => assert_eq!(line, b"second"),
                    _ => panic!("unexpected extra line"),
                }
                line_index += 1;
            }
        }
        assert_eq!(line_index, 2);
    }

    #[test]
    fn discards_oversized_line_through_its_delimiter() {
        let mut buffer = SerialLineBuffer::<3>::new();
        for byte in b"oversized\n" {
            assert_eq!(buffer.push(*byte), None);
        }
        for byte in b"ok" {
            assert_eq!(buffer.push(*byte), None);
        }
        let completed = buffer.push(b'\n').expect("line should complete");
        assert_eq!(buffer.line(completed), b"ok");
    }
}
