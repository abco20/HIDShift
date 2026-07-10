bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct KeyboardLedState: u8 {
        const NUM_LOCK = 1 << 0;
        const CAPS_LOCK = 1 << 1;
        const SCROLL_LOCK = 1 << 2;
    }
}
