#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputSequenceDecision {
    New,
    Duplicate,
    Stale,
}

/// Bounded replay/duplicate detection for the transport sequence space.
/// Broadcast frames may arrive a few positions out of order, so unlike the
/// input state window this accepts the most recent 32 sequence numbers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayWindow {
    highest: Option<u32>,
    seen: u32,
}

impl ReplayWindow {
    pub const fn new() -> Self {
        Self {
            highest: None,
            seen: 0,
        }
    }

    pub fn observe(&mut self, sequence: u32) -> InputSequenceDecision {
        let Some(highest) = self.highest else {
            self.highest = Some(sequence);
            self.seen = 1;
            return InputSequenceDecision::New;
        };
        let forward = sequence.wrapping_sub(highest);
        if forward == 0 {
            return InputSequenceDecision::Duplicate;
        }
        if forward < 0x8000_0000 {
            self.highest = Some(sequence);
            self.seen = if forward >= 32 {
                1
            } else {
                (self.seen << forward) | 1
            };
            return InputSequenceDecision::New;
        }
        let behind = highest.wrapping_sub(sequence);
        if behind >= 32 {
            return InputSequenceDecision::Stale;
        }
        let mask = 1u32 << behind;
        if self.seen & mask != 0 {
            InputSequenceDecision::Duplicate
        } else {
            self.seen |= mask;
            InputSequenceDecision::New
        }
    }

    pub const fn reset(&mut self) {
        self.highest = None;
        self.seen = 0;
    }
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// Forward-only de-duplication for self-healing broadcast state frames.
///
/// A later snapshot supersedes every older one. Accepting a late frame after
/// newer state was applied could replay a released key or old cumulative
/// motion, so it is deliberately classified as stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputSequenceWindow {
    highest: Option<u32>,
}

impl InputSequenceWindow {
    pub const fn new() -> Self {
        Self { highest: None }
    }

    pub fn observe_forward_only(&mut self, sequence: u32) -> InputSequenceDecision {
        let Some(highest) = self.highest else {
            self.highest = Some(sequence);
            return InputSequenceDecision::New;
        };
        let forward = sequence.wrapping_sub(highest);
        if forward == 0 {
            InputSequenceDecision::Duplicate
        } else if forward < 0x8000_0000 {
            self.highest = Some(sequence);
            InputSequenceDecision::New
        } else {
            InputSequenceDecision::Stale
        }
    }

    pub const fn reset(&mut self) {
        self.highest = None;
    }
}

impl Default for InputSequenceWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_and_late_broadcast_state_are_not_reapplied() {
        let mut window = InputSequenceWindow::new();
        assert_eq!(window.observe_forward_only(10), InputSequenceDecision::New);
        assert_eq!(
            window.observe_forward_only(10),
            InputSequenceDecision::Duplicate
        );
        assert_eq!(window.observe_forward_only(12), InputSequenceDecision::New);
        assert_eq!(
            window.observe_forward_only(11),
            InputSequenceDecision::Stale
        );
    }

    #[test]
    fn reset_accepts_a_new_sender_session_sequence() {
        let mut window = InputSequenceWindow::new();
        assert_eq!(window.observe_forward_only(42), InputSequenceDecision::New);
        window.reset();
        assert_eq!(window.observe_forward_only(1), InputSequenceDecision::New);
    }

    #[test]
    fn replay_window_accepts_small_reordering_but_rejects_duplicates_and_old_frames() {
        let mut window = ReplayWindow::new();
        assert_eq!(window.observe(10), InputSequenceDecision::New);
        assert_eq!(window.observe(12), InputSequenceDecision::New);
        assert_eq!(window.observe(11), InputSequenceDecision::New);
        assert_eq!(window.observe(11), InputSequenceDecision::Duplicate);
        assert_eq!(window.observe(10), InputSequenceDecision::Duplicate);
        assert_eq!(window.observe(10), InputSequenceDecision::Duplicate);
        assert_eq!(window.observe(100), InputSequenceDecision::New);
        assert_eq!(window.observe(10), InputSequenceDecision::Stale);
    }
}
