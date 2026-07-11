use crate::ids::HostId;
use crate::reports::BleMouseReport;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PendingMouseState {
    buttons: u8,
    x: i32,
    y: i32,
    wheel: i32,
    pan: i32,
    pending: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MouseAccumulatorStats {
    pub reports_coalesced: u32,
    pub movement_saturated: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MouseReportAccumulator<const HOSTS: usize> {
    hosts: [PendingMouseState; HOSTS],
    stats: MouseAccumulatorStats,
}

impl<const HOSTS: usize> MouseReportAccumulator<HOSTS> {
    pub const fn new() -> Self {
        Self {
            hosts: [PendingMouseState {
                buttons: 0,
                x: 0,
                y: 0,
                wheel: 0,
                pan: 0,
                pending: false,
            }; HOSTS],
            stats: MouseAccumulatorStats {
                reports_coalesced: 0,
                movement_saturated: 0,
            },
        }
    }

    pub fn push(&mut self, host_id: HostId, report: BleMouseReport) -> bool {
        let Some(index) = host_index::<HOSTS>(host_id) else {
            return false;
        };
        let bytes = report.as_bytes();
        let state = &mut self.hosts[index];
        // Button transitions are ordered events and must never be coalesced
        // into movement that happened under the previous button state.
        if bytes[0] != state.buttons {
            return false;
        }
        if state.pending {
            self.stats.reports_coalesced = self.stats.reports_coalesced.saturating_add(1);
        }
        state.x = saturating_add(state.x, bytes[1] as i8 as i32, &mut self.stats);
        state.y = saturating_add(state.y, bytes[2] as i8 as i32, &mut self.stats);
        state.wheel = saturating_add(state.wheel, bytes[3] as i8 as i32, &mut self.stats);
        state.pan = saturating_add(state.pan, bytes[4] as i8 as i32, &mut self.stats);
        state.pending = true;
        true
    }

    pub fn set_buttons(&mut self, host_id: HostId, buttons: u8) -> bool {
        let Some(index) = host_index::<HOSTS>(host_id) else {
            return false;
        };
        self.hosts[index].buttons = buttons;
        true
    }

    pub fn buttons(&self, host_id: HostId) -> Option<u8> {
        host_index::<HOSTS>(host_id).map(|index| self.hosts[index].buttons)
    }

    pub fn take_next(&mut self, host_id: HostId) -> Option<BleMouseReport> {
        let index = host_index::<HOSTS>(host_id)?;
        let state = &mut self.hosts[index];
        if !state.pending {
            return None;
        }
        let x = take_axis(&mut state.x);
        let y = take_axis(&mut state.y);
        let wheel = take_axis(&mut state.wheel);
        let pan = take_axis(&mut state.pan);
        state.pending = state.x != 0 || state.y != 0 || state.wheel != 0 || state.pan != 0;
        Some(BleMouseReport::from_bytes([
            state.buttons,
            x as u8,
            y as u8,
            wheel as u8,
            pan as u8,
        ]))
    }

    pub fn discard(&mut self, host_id: HostId) {
        if let Some(index) = host_index::<HOSTS>(host_id) {
            self.hosts[index] = PendingMouseState::default();
        }
    }

    pub fn discard_all(&mut self) {
        self.hosts.fill(PendingMouseState::default());
    }

    pub const fn stats(&self) -> MouseAccumulatorStats {
        self.stats
    }

    pub fn is_pending(&self, host_id: HostId) -> bool {
        host_index::<HOSTS>(host_id).is_some_and(|index| self.hosts[index].pending)
    }
}

impl<const HOSTS: usize> Default for MouseReportAccumulator<HOSTS> {
    fn default() -> Self {
        Self::new()
    }
}

fn host_index<const HOSTS: usize>(host_id: HostId) -> Option<usize> {
    let index = host_id.0.checked_sub(1)? as usize;
    (index < HOSTS).then_some(index)
}

fn saturating_add(current: i32, delta: i32, stats: &mut MouseAccumulatorStats) -> i32 {
    match current.checked_add(delta) {
        Some(value) => value,
        None => {
            stats.movement_saturated = stats.movement_saturated.saturating_add(1);
            current.saturating_add(delta)
        }
    }
}

fn take_axis(value: &mut i32) -> i8 {
    let part = (*value).clamp(i8::MIN as i32, i8::MAX as i32) as i8;
    *value -= part as i32;
    part
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_rate_movement_uses_one_fixed_accumulator_and_preserves_sum() {
        let mut accumulator = MouseReportAccumulator::<4>::new();
        accumulator.set_buttons(HostId(1), 1);
        for _ in 0..1_000 {
            assert!(accumulator.push(HostId(1), BleMouseReport::from_bytes([1, 1, 255, 1, 0])));
        }
        let mut x = 0i32;
        let mut y = 0i32;
        let mut wheel = 0i32;
        while let Some(report) = accumulator.take_next(HostId(1)) {
            let bytes = report.as_bytes();
            assert_eq!(bytes[0], 1);
            x += bytes[1] as i8 as i32;
            y += bytes[2] as i8 as i32;
            wheel += bytes[3] as i8 as i32;
        }
        assert_eq!((x, y, wheel), (1_000, -1_000, 1_000));
        assert_eq!(accumulator.stats().reports_coalesced, 999);
    }

    #[test]
    fn discard_prevents_old_target_movement_from_reaching_a_new_session() {
        let mut accumulator = MouseReportAccumulator::<4>::new();
        accumulator.push(HostId(1), BleMouseReport::from_bytes([0, 10, 0, 0, 0]));
        accumulator.discard(HostId(1));
        assert_eq!(accumulator.take_next(HostId(1)), None);
    }

    #[test]
    fn button_changes_are_rejected_instead_of_rewriting_pending_movement() {
        let mut accumulator = MouseReportAccumulator::<4>::new();
        assert!(accumulator.push(HostId(1), BleMouseReport::from_bytes([0, 100, 0, 0, 0])));

        assert!(!accumulator.push(HostId(1), BleMouseReport::from_bytes([1, 0, 0, 0, 0])));
        assert_eq!(
            accumulator.take_next(HostId(1)).unwrap().as_bytes(),
            &[0, 100, 0, 0, 0]
        );
    }

    #[test]
    fn movement_after_an_ordered_press_keeps_pressed_buttons() {
        let mut accumulator = MouseReportAccumulator::<4>::new();
        assert!(accumulator.set_buttons(HostId(1), 1));
        assert!(accumulator.push(HostId(1), BleMouseReport::from_bytes([1, 4, 0, 0, 0])));

        assert_eq!(
            accumulator.take_next(HostId(1)).unwrap().as_bytes(),
            &[1, 4, 0, 0, 0]
        );
    }
}
