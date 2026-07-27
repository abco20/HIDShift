use heapless::{Deque, String};

pub const LOG_RING_BYTE_BUDGET: usize = 8 * 1024;
pub const LOG_MESSAGE_CAPACITY: usize = 160;
// Keep the complete fixed-capacity value below the per-node 8 KiB budget,
// including sequence/timestamp/metadata and heapless container bookkeeping.
pub const LOG_RING_CAPACITY: usize = 44;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub sequence: u32,
    pub timestamp_ms: u64,
    pub level: LogLevel,
    pub message: String<LOG_MESSAGE_CAPACITY>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogWrite {
    pub sequence: u32,
    pub truncated: bool,
    pub evicted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRing {
    entries: Deque<LogEntry, LOG_RING_CAPACITY>,
    next_sequence: u32,
    evicted: u32,
    stream_dropped: u32,
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

impl LogRing {
    pub const fn new() -> Self {
        Self {
            entries: Deque::new(),
            next_sequence: 0,
            evicted: 0,
            stream_dropped: 0,
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &LogEntry> {
        self.entries.iter()
    }

    pub const fn evicted(&self) -> u32 {
        self.evicted
    }

    pub const fn stream_dropped(&self) -> u32 {
        self.stream_dropped
    }

    pub fn mark_stream_dropped(&mut self) {
        self.stream_dropped = self.stream_dropped.saturating_add(1);
    }

    pub fn push(&mut self, timestamp_ms: u64, level: LogLevel, message: &str) -> LogWrite {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let mut owned = String::new();
        let mut truncated = false;
        for character in message.chars() {
            if owned.push(character).is_err() {
                truncated = true;
                break;
            }
        }
        let evicted = if self.entries.is_full() {
            self.entries.pop_front();
            self.evicted = self.evicted.saturating_add(1);
            true
        } else {
            false
        };
        let _ = self.entries.push_back(LogEntry {
            sequence,
            timestamp_ms,
            level,
            message: owned,
        });
        LogWrite {
            sequence,
            truncated,
            evicted,
        }
    }

    pub fn after(&self, sequence: u32) -> impl Iterator<Item = &LogEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.sequence.wrapping_sub(sequence) < (u32::MAX / 2))
            .filter(move |entry| entry.sequence != sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_ring_evicts_oldest_without_flash_or_heap_allocation() {
        let mut ring = LogRing::new();
        for index in 0..=LOG_RING_CAPACITY {
            ring.push(index as u64, LogLevel::Info, "event");
        }
        assert_eq!(ring.entries().count(), LOG_RING_CAPACITY);
        assert_eq!(ring.entries().next().unwrap().sequence, 1);
        assert_eq!(ring.evicted(), 1);
    }

    #[test]
    fn utf8_messages_truncate_only_at_character_boundaries() {
        let mut ring = LogRing::new();
        let write = ring.push(0, LogLevel::Warn, &"あ".repeat(100));
        assert!(write.truncated);
        assert!(
            ring.entries()
                .next()
                .unwrap()
                .message
                .is_char_boundary(ring.entries().next().unwrap().message.len())
        );
    }

    #[test]
    fn live_stream_backpressure_is_counted_separately_from_history() {
        let mut ring = LogRing::new();
        ring.push(0, LogLevel::Error, "kept");
        ring.mark_stream_dropped();
        assert_eq!(ring.entries().count(), 1);
        assert_eq!(ring.stream_dropped(), 1);
    }
}
