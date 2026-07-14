use crate::ids::{DeviceId, InterfaceId};
use heapless::Vec;

use super::{InputLane, MAX_HID_REPORT_SIZE, MotionCumulative};

/// Delivery policy for a report travelling over the realtime input lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputDeliveryClass {
    /// A state transition or vendor report must retain ordering.
    Critical,
    /// A movement-only report may be replaced by a newer report for the same
    /// interface while the radio is busy.
    Motion,
}

/// Why a report could not be admitted to the fixed-capacity scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputSchedulerError {
    CriticalQueueFull,
    MotionQueueDisabled,
    MotionQueueUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledInput {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    pub ingress_us: u64,
    pub sequence: u32,
    pub e2e_sequence: u32,
    pub motion: MotionCumulative,
    pub report: Vec<u8, MAX_HID_REPORT_SIZE>,
    class: InputDeliveryClass,
    order: u32,
}

impl ScheduledInput {
    pub fn new(
        device_id: DeviceId,
        interface_id: InterfaceId,
        ingress_us: u64,
        sequence: u32,
        e2e_sequence: u32,
        report: &[u8],
        class: InputDeliveryClass,
    ) -> Option<Self> {
        Some(Self {
            device_id,
            interface_id,
            ingress_us,
            sequence,
            e2e_sequence,
            motion: MotionCumulative::zero(),
            report: Vec::from_slice(report).ok()?,
            class,
            order: 0,
        })
    }

    pub const fn class(&self) -> InputDeliveryClass {
        self.class
    }

    pub const fn lane(&self) -> InputLane {
        match self.class {
            InputDeliveryClass::Motion => InputLane::Motion,
            InputDeliveryClass::Critical => InputLane::Critical,
        }
    }

    pub const fn with_motion(mut self, motion: MotionCumulative) -> Self {
        self.motion = motion;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputSchedulerStats {
    pub queued_critical: u32,
    pub queued_motion: u32,
    pub coalesced_motion: u32,
    pub dropped_motion: u32,
    pub rejected_critical: u32,
}

/// Fixed-capacity input scheduler used immediately before radio submission.
///
/// Critical reports are never displaced by movement. Motion is kept per
/// interface and the oldest motion slot is replaced when all slots are busy.
/// This keeps a busy radio from turning stale mouse movement into latency for
/// a later keyboard transition.
pub struct InputScheduler<const CRITICAL: usize, const MOTION: usize> {
    critical: [Option<ScheduledInput>; CRITICAL],
    motion: [Option<ScheduledInput>; MOTION],
    next_order: u32,
    stats: InputSchedulerStats,
}

impl<const CRITICAL: usize, const MOTION: usize> InputScheduler<CRITICAL, MOTION> {
    pub const fn new() -> Self {
        Self {
            critical: [const { None }; CRITICAL],
            motion: [const { None }; MOTION],
            next_order: 1,
            stats: InputSchedulerStats {
                queued_critical: 0,
                queued_motion: 0,
                coalesced_motion: 0,
                dropped_motion: 0,
                rejected_critical: 0,
            },
        }
    }

    pub fn enqueue(&mut self, mut input: ScheduledInput) -> Result<(), InputSchedulerError> {
        input.order = self.next_order;
        self.next_order = self.next_order.wrapping_add(1);
        match input.class {
            InputDeliveryClass::Critical => {
                let Some(slot) = self.critical.iter_mut().find(|slot| slot.is_none()) else {
                    self.stats.rejected_critical = self.stats.rejected_critical.saturating_add(1);
                    return Err(InputSchedulerError::CriticalQueueFull);
                };
                *slot = Some(input);
                self.stats.queued_critical = self.stats.queued_critical.saturating_add(1);
                Ok(())
            }
            InputDeliveryClass::Motion => {
                if MOTION == 0 {
                    self.stats.dropped_motion = self.stats.dropped_motion.saturating_add(1);
                    return Err(InputSchedulerError::MotionQueueDisabled);
                }
                if let Some(slot) = self.motion.iter_mut().find(|slot| {
                    slot.as_ref().is_some_and(|current| {
                        current.device_id == input.device_id
                            && current.interface_id == input.interface_id
                    })
                }) {
                    *slot = Some(input);
                    self.stats.coalesced_motion = self.stats.coalesced_motion.saturating_add(1);
                    return Ok(());
                }
                if let Some(slot) = self.motion.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(input);
                    self.stats.queued_motion = self.stats.queued_motion.saturating_add(1);
                    return Ok(());
                }
                let Some(oldest) = self
                    .motion
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, slot)| slot.as_ref().map_or(u32::MAX, |value| value.order))
                    .map(|(index, _)| index)
                else {
                    self.stats.dropped_motion = self.stats.dropped_motion.saturating_add(1);
                    return Err(InputSchedulerError::MotionQueueUnavailable);
                };
                self.motion[oldest] = Some(input);
                self.stats.dropped_motion = self.stats.dropped_motion.saturating_add(1);
                Ok(())
            }
        }
    }

    pub fn pop_next(&mut self) -> Option<ScheduledInput> {
        if let Some(index) = self.critical.iter().position(Option::is_some) {
            return self.critical[index].take();
        }
        let index = self
            .motion
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.as_ref().map(|value| (index, value.order)))
            .min_by_key(|(_, order)| *order)
            .map(|(index, _)| index)?;
        self.motion[index].take()
    }

    pub fn len(&self) -> usize {
        self.critical.iter().flatten().count() + self.motion.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn stats(&self) -> InputSchedulerStats {
        self.stats
    }
}

impl<const CRITICAL: usize, const MOTION: usize> Default for InputScheduler<CRITICAL, MOTION> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(interface_id: u8, value: u8, class: InputDeliveryClass) -> ScheduledInput {
        ScheduledInput::new(
            DeviceId(1),
            InterfaceId(interface_id),
            value as u64,
            value as u32,
            value as u32,
            &[value],
            class,
        )
        .unwrap()
    }

    #[test]
    fn critical_reports_are_delivered_before_motion() {
        let mut scheduler = InputScheduler::<4, 2>::new();
        scheduler
            .enqueue(input(2, 1, InputDeliveryClass::Motion))
            .unwrap();
        scheduler
            .enqueue(input(1, 2, InputDeliveryClass::Critical))
            .unwrap();
        assert_eq!(scheduler.pop_next().unwrap().report.as_slice(), &[2]);
        assert_eq!(scheduler.pop_next().unwrap().report.as_slice(), &[1]);
    }

    #[test]
    fn motion_is_coalesced_per_interface_without_affecting_critical_fifo() {
        let mut scheduler = InputScheduler::<4, 2>::new();
        scheduler
            .enqueue(
                input(2, 1, InputDeliveryClass::Motion).with_motion(MotionCumulative {
                    x: 1,
                    ..MotionCumulative::zero()
                }),
            )
            .unwrap();
        scheduler
            .enqueue(
                input(2, 2, InputDeliveryClass::Motion).with_motion(MotionCumulative {
                    x: 2,
                    ..MotionCumulative::zero()
                }),
            )
            .unwrap();
        scheduler
            .enqueue(input(1, 3, InputDeliveryClass::Critical))
            .unwrap();
        assert_eq!(scheduler.len(), 2);
        assert_eq!(scheduler.pop_next().unwrap().report.as_slice(), &[3]);
        let motion = scheduler.pop_next().unwrap();
        assert_eq!(motion.report.as_slice(), &[2]);
        assert_eq!(motion.motion.x, 2);
        assert_eq!(scheduler.stats().coalesced_motion, 1);
    }

    #[test]
    fn full_critical_queue_rejects_new_critical_report() {
        let mut scheduler = InputScheduler::<1, 1>::new();
        scheduler
            .enqueue(input(1, 1, InputDeliveryClass::Critical))
            .unwrap();
        let rejected = scheduler.enqueue(input(1, 2, InputDeliveryClass::Critical));
        assert!(rejected.is_err());
        assert_eq!(scheduler.stats().rejected_critical, 1);
    }

    #[test]
    fn full_motion_queue_replaces_oldest_motion() {
        let mut scheduler = InputScheduler::<1, 2>::new();
        scheduler
            .enqueue(input(1, 1, InputDeliveryClass::Motion))
            .unwrap();
        scheduler
            .enqueue(input(2, 2, InputDeliveryClass::Motion))
            .unwrap();
        scheduler
            .enqueue(input(3, 3, InputDeliveryClass::Motion))
            .unwrap();
        assert_eq!(scheduler.stats().dropped_motion, 1);
        assert_eq!(scheduler.pop_next().unwrap().report.as_slice(), &[2]);
        assert_eq!(scheduler.pop_next().unwrap().report.as_slice(), &[3]);
    }
}
