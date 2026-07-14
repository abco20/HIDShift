use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
#[cfg(not(all(feature = "hardware-e2e", feature = "espnow")))]
use embassy_sync::channel::Receiver;
use hidshift::ids::InterfaceId;
use hidshift::link::{HidReportType, MAX_HID_REPORT_SIZE};

pub static OUTPUT_REQUEST_CHANNEL: Channel<CriticalSectionRawMutex, HostOutputRequest, 8> =
    Channel::new();
pub static OUTPUT_RESPONSE_CHANNEL: Channel<CriticalSectionRawMutex, HostOutputResponse, 8> =
    Channel::new();

#[derive(Clone, Debug)]
pub enum HostOutputRequest {
    SetReport {
        interface_id: InterfaceId,
        report_type: HidReportType,
        report_id: u8,
        report: heapless::Vec<u8, MAX_HID_REPORT_SIZE>,
    },
    GetReport {
        interface_id: InterfaceId,
        report_type: HidReportType,
        report_id: u8,
        requested_len: u16,
        request_id: u16,
    },
}

#[derive(Clone, Debug)]
pub struct HostOutputResponse {
    pub interface_id: InterfaceId,
    pub report_type: HidReportType,
    pub report_id: u8,
    pub request_id: u16,
    pub report: heapless::Vec<u8, MAX_HID_REPORT_SIZE>,
}

#[cfg(not(all(feature = "hardware-e2e", feature = "espnow")))]
pub fn output_request_receiver() -> Receiver<'static, CriticalSectionRawMutex, HostOutputRequest, 8>
{
    OUTPUT_REQUEST_CHANNEL.receiver()
}

pub async fn send_output_response(response: HostOutputResponse) {
    OUTPUT_RESPONSE_CHANNEL.send(response).await;
}
