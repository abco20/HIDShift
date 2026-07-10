#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ConsumerUsage(pub u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsumerFrame {
    pub active: Option<ConsumerUsage>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConsumerState {
    pub active: Option<ConsumerUsage>,
}

impl ConsumerState {
    pub const fn new() -> Self {
        Self { active: None }
    }

    pub fn apply_frame(&mut self, frame: ConsumerFrame) {
        self.active = frame.active;
    }
}
