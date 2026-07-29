#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeartbeatMonitor {
    last_heartbeat: u32,
    missed_intervals: u8,
    allowed_missed_intervals: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeartbeatGroup<const TASKS: usize> {
    monitors: [HeartbeatMonitor; TASKS],
}

impl<const TASKS: usize> HeartbeatGroup<TASKS> {
    pub fn new(initial_heartbeats: [u32; TASKS], allowed_missed_intervals: u8) -> Self {
        Self {
            monitors: core::array::from_fn(|index| {
                HeartbeatMonitor::new(initial_heartbeats[index], allowed_missed_intervals)
            }),
        }
    }

    pub fn should_feed_watchdog(
        &mut self,
        heartbeats: [u32; TASKS],
        required: [bool; TASKS],
    ) -> bool {
        let mut healthy = true;
        for index in 0..TASKS {
            let task_healthy = self.monitors[index].should_feed_watchdog(heartbeats[index]);
            if required[index] && !task_healthy {
                healthy = false;
            }
        }
        healthy
    }
}

impl HeartbeatMonitor {
    pub const fn new(initial_heartbeat: u32, allowed_missed_intervals: u8) -> Self {
        Self {
            last_heartbeat: initial_heartbeat,
            missed_intervals: 0,
            allowed_missed_intervals,
        }
    }

    pub fn should_feed_watchdog(&mut self, heartbeat: u32) -> bool {
        if heartbeat != self.last_heartbeat {
            self.last_heartbeat = heartbeat;
            self.missed_intervals = 0;
            return true;
        }

        self.missed_intervals = self.missed_intervals.saturating_add(1);
        self.missed_intervals <= self.allowed_missed_intervals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_progress_keeps_the_watchdog_fed() {
        let mut monitor = HeartbeatMonitor::new(0, 2);

        assert!(monitor.should_feed_watchdog(1));
        assert!(monitor.should_feed_watchdog(2));
        assert!(monitor.should_feed_watchdog(3));
    }

    #[test]
    fn stalled_heartbeat_stops_feeding_after_the_grace_period() {
        let mut monitor = HeartbeatMonitor::new(7, 2);

        assert!(monitor.should_feed_watchdog(7));
        assert!(monitor.should_feed_watchdog(7));
        assert!(!monitor.should_feed_watchdog(7));
        assert!(monitor.should_feed_watchdog(8));
    }

    #[test]
    fn heartbeat_group_requires_every_selected_task_to_progress() {
        let mut group = HeartbeatGroup::new([0, 10], 1);

        assert!(group.should_feed_watchdog([1, 11], [true, true]));
        assert!(group.should_feed_watchdog([2, 11], [true, true]));
        assert!(!group.should_feed_watchdog([3, 11], [true, true]));
        assert!(group.should_feed_watchdog([4, 11], [true, false]));
        assert!(group.should_feed_watchdog([5, 12], [true, true]));
    }
}
