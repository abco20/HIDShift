use crate::ids::HostId;
use crate::mouse_accumulator::MouseAccumulatorStats;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeDiagnosticsEvent {
    ResetReason(u8),
    Brownout,
    BleDisconnected { host_id: HostId, reason: u8 },
    BleNotifyFailed,
    BleNotifyTimedOut { critical_release: bool },
    BleManagementNotifyTimedOut,
    UsbLedWriteTimedOut,
    UsbError,
    FlashWrite { success: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCounters {
    pub runtime_input_queue_high_watermark: u16,
    pub ble_control_queue_high_watermark: u16,
    pub ble_notify_queue_high_watermark: u16,
    pub usb_command_queue_high_watermark: u16,
    pub storage_queue_high_watermark: u16,
    pub status_queue_high_watermark: u16,
    pub ble_notify_dropped: u32,
    pub ble_notify_timeouts: u32,
    pub critical_release_failures: u32,
    pub mouse_reports_coalesced: u32,
    pub mouse_movement_saturated: u32,
    pub mirror_non_target_input_dropped: u32,
    pub status_updates_dropped: u32,
    pub usb_led_write_timeouts: u32,
}

impl RuntimeCounters {
    pub const fn new() -> Self {
        Self {
            runtime_input_queue_high_watermark: 0,
            ble_control_queue_high_watermark: 0,
            ble_notify_queue_high_watermark: 0,
            usb_command_queue_high_watermark: 0,
            storage_queue_high_watermark: 0,
            status_queue_high_watermark: 0,
            ble_notify_dropped: 0,
            ble_notify_timeouts: 0,
            critical_release_failures: 0,
            mouse_reports_coalesced: 0,
            mouse_movement_saturated: 0,
            mirror_non_target_input_dropped: 0,
            status_updates_dropped: 0,
            usb_led_write_timeouts: 0,
        }
    }
}

impl Default for RuntimeCounters {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeTransportMetrics {
    pub runtime_input_depth: usize,
    pub ble_control_depth: usize,
    pub ble_notify_depth: usize,
    pub usb_depth: usize,
    pub storage_depth: usize,
    pub status_depth: usize,
    pub mouse: MouseAccumulatorStats,
    pub status_updates_dropped: u32,
}

pub(super) fn saturating_depth(depth: usize) -> u16 {
    depth.min(u16::MAX as usize) as u16
}
