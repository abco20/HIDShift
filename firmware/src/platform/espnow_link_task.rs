use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};
use critical_section::Mutex;
use embassy_futures::select::{Either4, select4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver};
use embassy_time::{Duration, Ticker};
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowManager, EspNowReceiver, WifiPhyRate};
use esp_radio::wifi::{ControllerConfig, PowerSaveMode, WifiController};
use hidshift::ids::{DeviceId, InterfaceId};
use hidshift::link::{
    BridgeMessage, BridgeRole, CriticalStateRefresh, FragmentEncoder, HidReportType,
    InputDeliveryClass, InputReportRecord, InputScheduler, InputSnapshotHistory,
    InterfaceDescriptor, LinkWatchdog, MAX_BRIDGE_MESSAGE_SIZE, MAX_HID_INTERFACES,
    MAX_HID_REPORT_DESCRIPTOR_SIZE, MAX_HID_REPORT_SIZE, MotionCumulative, PacketKind,
    REALTIME_CRITICAL_JOURNAL_CAPACITY, Reassembler, ReplayWindow, SessionDecision,
    SessionHandshake, WIRE_PAYLOAD_MAX, WirePacket,
};
use static_cell::StaticCell;

use super::espnow_output::{
    HostOutputRequest, HostOutputResponse, OUTPUT_REQUEST_CHANNEL, OUTPUT_RESPONSE_CHANNEL,
    send_output_response,
};

const CONTROL_QUEUE_CAPACITY: usize = 8;
const REPORT_QUEUE_CAPACITY: usize = 32;
const CRITICAL_INPUT_QUEUE_CAPACITY: usize = 16;
const MOTION_INPUT_QUEUE_CAPACITY: usize = 4;
const INPUT_RECOVERY_TICK_MS: u64 = 1;
const INPUT_SNAPSHOT_RECORD_BUDGET: usize = WIRE_PAYLOAD_MAX - 2;
const ESPNOW_HEARTBEAT_MS: u64 = 500;
const ESPNOW_LINK_TIMEOUT_MS: u64 = 1_500;

#[cfg(feature = "hardware-e2e")]
const E2E_DEVICE_ID: DeviceId = DeviceId(0xfd);
#[cfg(feature = "hardware-e2e")]
const E2E_INTERFACE_ID: InterfaceId = InterfaceId(0xfd);
#[cfg(feature = "hardware-e2e")]
const E2E_REPORT_DESCRIPTOR: &[u8] = &[
    // Keyboard, report ID 1.
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x75, 0x08, 0x95, 0x01, 0x81, 0x01, 0x19, 0x00,
    0x29, 0xff, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00, 0xc0,
    // Mouse, report ID 2, 16-bit X/Y plus wheel and pan.
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x05, 0x81, 0x02, 0x75, 0x03, 0x95, 0x01,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x00, 0x80, 0x26, 0xff, 0x7f, 0x75, 0x10,
    0x95, 0x02, 0x81, 0x06, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06,
    0x05, 0x0c, 0x0a, 0x38, 0x02, 0x81, 0x06, 0xc0, 0xc0,
    // Consumer control, report ID 3.
    0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x03, 0x15, 0x00, 0x26, 0xff, 0x03, 0x19, 0x00, 0x2a,
    0xff, 0x03, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xc0,
    // Bidirectional vendor page, report ID 4.
    0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x04, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08,
    0x95, 0x3f, 0x09, 0x01, 0x81, 0x02, 0x09, 0x01, 0x91, 0x02, 0x09, 0x01, 0xb1, 0x02, 0xc0,
];

static CONTROL_CHANNEL: Channel<CriticalSectionRawMutex, HostLinkControl, CONTROL_QUEUE_CAPACITY> =
    Channel::new();
static REPORT_CHANNEL: Channel<CriticalSectionRawMutex, HostInputReport, REPORT_QUEUE_CAPACITY> =
    Channel::new();
static RADIO_READY_CHANNEL: Channel<CriticalSectionRawMutex, bool, 1> = Channel::new();
static STORAGE_RADIO_READY_CHANNEL: Channel<CriticalSectionRawMutex, bool, 1> = Channel::new();
static WIFI_CONTROLLER_STORAGE: StaticCell<WifiController<'static>> = StaticCell::new();
static HOST_TX_NORMAL_CHANNEL: Channel<CriticalSectionRawMutex, HostTxRequest, 4> = Channel::new();
static INPUT_SCHEDULER_STORAGE: StaticCell<
    InputScheduler<CRITICAL_INPUT_QUEUE_CAPACITY, MOTION_INPUT_QUEUE_CAPACITY>,
> = StaticCell::new();
static DESCRIPTORS_STORAGE: StaticCell<[Option<HostLinkControl>; MAX_HID_INTERFACES]> =
    StaticCell::new();
static REASSEMBLER_STORAGE: StaticCell<Reassembler> = StaticCell::new();
static SEND_BUFFER_STORAGE: StaticCell<[u8; MAX_BRIDGE_MESSAGE_SIZE]> = StaticCell::new();
static NEXT_CRITICAL_SEQUENCE: AtomicU32 = AtomicU32::new(1);
static NEXT_MOTION_SEQUENCE: AtomicU32 = AtomicU32::new(1);
static DEVICE_SESSION_ID: AtomicU32 = AtomicU32::new(0);
static HOST_WIRE_SESSION: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "hardware-e2e")]
static DROP_NEXT_CRITICAL: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "hardware-e2e")]
static DROP_NEXT_MOTION: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "hardware-e2e")]
static SUPPRESS_NEXT_CRITICAL_RECOVERY: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "hardware-e2e")]
static E2E_LAST_MOUSE_BUTTONS: AtomicU8 = AtomicU8::new(0);
static MOTION_TOTAL_X: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];
static MOTION_TOTAL_Y: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];
static MOTION_TOTAL_WHEEL: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];
static MOTION_TOTAL_PAN: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];
static ESPNOW_PAIRING_KEY: Mutex<RefCell<[u8; 16]>> = Mutex::new(RefCell::new([0; 16]));

type HostTxPayload = heapless::Vec<u8, MAX_BRIDGE_MESSAGE_SIZE>;

#[derive(Clone, Debug)]
struct HostTxRequest {
    payload: HostTxPayload,
}

#[derive(Clone, Debug)]
struct HostCriticalStateRefresh {
    payload: HostTxPayload,
    refresh: CriticalStateRefresh,
}

fn host_hello_for_peer(session_id: u32, peer_session_id: u32) -> BridgeMessage<'static> {
    BridgeMessage::Hello {
        role: BridgeRole::UsbHost,
        capabilities: 0x000f,
        session_id,
        peer_session_id,
        next_critical_sequence: NEXT_CRITICAL_SEQUENCE.load(Ordering::Relaxed),
        next_motion_sequence: NEXT_MOTION_SEQUENCE.load(Ordering::Relaxed),
    }
}

pub fn radio_ready_receiver() -> Receiver<'static, CriticalSectionRawMutex, bool, 1> {
    RADIO_READY_CHANNEL.receiver()
}

pub fn storage_radio_ready_receiver() -> Receiver<'static, CriticalSectionRawMutex, bool, 1> {
    STORAGE_RADIO_READY_CHANNEL.receiver()
}

pub fn device_session_id() -> u32 {
    DEVICE_SESSION_ID.load(Ordering::Acquire)
}

async fn signal_radio_ready(ready: bool) {
    RADIO_READY_CHANNEL.send(ready).await;
    STORAGE_RADIO_READY_CHANNEL.send(ready).await;
}

#[derive(Clone, Debug)]
pub enum HostLinkControl {
    InterfaceDescriptor {
        device_id: DeviceId,
        interface_id: InterfaceId,
        interface_index: u8,
        vendor_id: u16,
        product_id: u16,
        descriptor: heapless::Vec<u8, MAX_HID_REPORT_DESCRIPTOR_SIZE>,
    },
    InterfaceRemoved {
        device_id: DeviceId,
        interface_id: InterfaceId,
    },
    EnterDownloadMode,
}

#[derive(Clone, Debug)]
pub struct HostInputReport {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    pub ingress_us: u64,
    pub sequence: u32,
    pub e2e_sequence: u32,
    pub motion: MotionCumulative,
    pub class: InputDeliveryClass,
    pub report: heapless::Vec<u8, MAX_HID_REPORT_SIZE>,
}

#[cfg(not(feature = "hardware-e2e"))]
pub async fn forward_interface_descriptor(
    device_id: DeviceId,
    interface_id: InterfaceId,
    interface_index: u8,
    vendor_id: u16,
    product_id: u16,
    descriptor: &[u8],
) {
    let Ok(descriptor) = heapless::Vec::from_slice(descriptor) else {
        log::warn!(
            "firmware: ESP-NOW descriptor rejected interface={} len={}",
            interface_id.0,
            descriptor.len()
        );
        return;
    };
    CONTROL_CHANNEL
        .send(HostLinkControl::InterfaceDescriptor {
            device_id,
            interface_id,
            interface_index,
            vendor_id,
            product_id,
            descriptor,
        })
        .await;
}

#[cfg(not(feature = "hardware-e2e"))]
pub async fn forward_input_report(
    device_id: DeviceId,
    interface_id: InterfaceId,
    ingress_us: u64,
    class: InputDeliveryClass,
    motion: Option<hidshift::input::MouseMovement>,
    report: &[u8],
) {
    let Ok(report) = heapless::Vec::from_slice(report) else {
        log::warn!(
            "firmware: ESP-NOW report rejected interface={} len={}",
            interface_id.0,
            report.len()
        );
        return;
    };
    let sequence = match class {
        InputDeliveryClass::Critical => NEXT_CRITICAL_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        InputDeliveryClass::Motion => NEXT_MOTION_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    };
    let motion = motion
        .filter(|_| class == InputDeliveryClass::Motion)
        .map(|movement| accumulate_motion(interface_id, movement))
        .unwrap_or_else(MotionCumulative::zero);
    REPORT_CHANNEL
        .send(HostInputReport {
            device_id,
            interface_id,
            ingress_us,
            sequence,
            e2e_sequence: 0,
            motion,
            class,
            report,
        })
        .await;
}

fn accumulate_motion(
    interface_id: InterfaceId,
    movement: hidshift::input::MouseMovement,
) -> MotionCumulative {
    let index = interface_id.0 as usize;
    MotionCumulative {
        x: MOTION_TOTAL_X[index]
            .fetch_add(i32::from(movement.x), Ordering::Relaxed)
            .wrapping_add(i32::from(movement.x)),
        y: MOTION_TOTAL_Y[index]
            .fetch_add(i32::from(movement.y), Ordering::Relaxed)
            .wrapping_add(i32::from(movement.y)),
        wheel: MOTION_TOTAL_WHEEL[index]
            .fetch_add(i32::from(movement.wheel), Ordering::Relaxed)
            .wrapping_add(i32::from(movement.wheel)),
        pan: MOTION_TOTAL_PAN[index]
            .fetch_add(i32::from(movement.pan), Ordering::Relaxed)
            .wrapping_add(i32::from(movement.pan)),
    }
}

#[cfg(not(feature = "hardware-e2e"))]
pub async fn forward_interface_removed(device_id: DeviceId, interface_id: InterfaceId) {
    CONTROL_CHANNEL
        .send(HostLinkControl::InterfaceRemoved {
            device_id,
            interface_id,
        })
        .await;
}

#[cfg(feature = "hardware-e2e")]
pub async fn forward_e2e_packet(packet: hidshift::e2e::E2ePacket, ingress_us: u64) {
    use hidshift::e2e::{E2eCommand, E2eInputLane};
    if let E2eCommand::MouseBurst { count, x, y } = packet.command {
        E2E_LAST_MOUSE_BUTTONS.store(0, Ordering::Relaxed);
        for _ in 0..count {
            let mut report = heapless::Vec::new();
            let _ = report.extend_from_slice(&[2, 0]);
            let _ = report.extend_from_slice(&x.to_le_bytes());
            let _ = report.extend_from_slice(&y.to_le_bytes());
            let _ = report.extend_from_slice(&[0, 0]);
            let movement = hidshift::input::MouseMovement {
                x,
                y,
                wheel: 0,
                pan: 0,
            };
            REPORT_CHANNEL
                .send(HostInputReport {
                    device_id: E2E_DEVICE_ID,
                    interface_id: E2E_INTERFACE_ID,
                    ingress_us,
                    sequence: NEXT_MOTION_SEQUENCE.fetch_add(1, Ordering::Relaxed),
                    e2e_sequence: packet.sequence,
                    motion: accumulate_motion(E2E_INTERFACE_ID, movement),
                    class: InputDeliveryClass::Motion,
                    report,
                })
                .await;
        }
        return;
    }
    let mut reports = heapless::Vec::<heapless::Vec<u8, MAX_HID_REPORT_SIZE>, 3>::new();
    match packet.command {
        E2eCommand::Keyboard { modifiers, keys } => {
            let mut report = heapless::Vec::new();
            let _ = report.extend_from_slice(&[1, modifiers, 0]);
            let _ = report.extend_from_slice(&keys);
            let _ = reports.push(report);
        }
        E2eCommand::Mouse {
            buttons,
            x,
            y,
            wheel,
            pan,
        } => {
            let mut report = heapless::Vec::new();
            let _ = report.push(2);
            let _ = report.push(buttons);
            let _ = report.extend_from_slice(&x.to_le_bytes());
            let _ = report.extend_from_slice(&y.to_le_bytes());
            let _ = report.push(wheel as u8);
            let _ = report.push(pan as u8);
            let _ = reports.push(report);
        }
        E2eCommand::Consumer { usage } => {
            let mut report = heapless::Vec::new();
            let _ = report.push(3);
            let _ = report.extend_from_slice(&usage.to_le_bytes());
            let _ = reports.push(report);
        }
        E2eCommand::VendorInput { len, seed } => {
            let len = usize::from(len).min(63);
            let mut report = heapless::Vec::new();
            let _ = report.push(4);
            for index in 0..len {
                let _ = report.push(seed.wrapping_add(index as u8));
            }
            let _ = reports.push(report);
        }
        E2eCommand::ReleaseAll => {
            E2E_LAST_MOUSE_BUTTONS.store(0, Ordering::Relaxed);
            for report in [
                &[1, 0, 0, 0, 0, 0, 0, 0, 0][..],
                &[2, 0, 0, 0, 0, 0, 0, 0][..],
                &[3, 0, 0][..],
            ] {
                if let Ok(report) = heapless::Vec::from_slice(report) {
                    let _ = reports.push(report);
                }
            }
        }
        E2eCommand::EnterDeviceDownload => {
            CONTROL_CHANNEL
                .send(HostLinkControl::EnterDownloadMode)
                .await;
        }
        E2eCommand::DropNextInput { lane } => {
            match lane {
                E2eInputLane::Motion => DROP_NEXT_MOTION.store(true, Ordering::Relaxed),
                E2eInputLane::Critical => DROP_NEXT_CRITICAL.store(true, Ordering::Relaxed),
            }
            log::info!("@HIDSHIFT-BRIDGE:FAULT_ARMED,{:?}", lane);
        }
        E2eCommand::DropNextInputBurst { lane } => {
            match lane {
                E2eInputLane::Motion => DROP_NEXT_MOTION.store(true, Ordering::Relaxed),
                E2eInputLane::Critical => {
                    DROP_NEXT_CRITICAL.store(true, Ordering::Relaxed);
                    SUPPRESS_NEXT_CRITICAL_RECOVERY.store(true, Ordering::Relaxed);
                }
            }
            log::info!("@HIDSHIFT-BRIDGE:FAULT_BURST_ARMED,{:?}", lane);
        }
        E2eCommand::Hello
        | E2eCommand::ReadTimestamp { .. }
        | E2eCommand::SelectTransport { .. }
        | E2eCommand::MouseBurst { .. } => {}
    }
    let motion = match packet.command {
        E2eCommand::Mouse {
            x, y, wheel, pan, ..
        } => Some(hidshift::input::MouseMovement { x, y, wheel, pan }),
        _ => None,
    };
    for report in reports {
        let class = match packet.command {
            E2eCommand::Mouse { buttons, .. }
                if E2E_LAST_MOUSE_BUTTONS.swap(buttons, Ordering::Relaxed) == buttons =>
            {
                InputDeliveryClass::Motion
            }
            _ => InputDeliveryClass::Critical,
        };
        #[cfg(feature = "hardware-e2e")]
        crate::e2e_telemetry::record_espnow_enqueue(
            packet.sequence,
            ingress_us,
            embassy_time::Instant::now().as_micros(),
        );
        REPORT_CHANNEL
            .send(HostInputReport {
                device_id: E2E_DEVICE_ID,
                interface_id: E2E_INTERFACE_ID,
                ingress_us,
                sequence: match class {
                    InputDeliveryClass::Critical => {
                        NEXT_CRITICAL_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                    }
                    InputDeliveryClass::Motion => {
                        NEXT_MOTION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                    }
                },
                e2e_sequence: packet.sequence,
                motion: motion
                    .map(|movement| accumulate_motion(E2E_INTERFACE_ID, movement))
                    .unwrap_or_else(MotionCumulative::zero),
                class,
                report,
            })
            .await;
    }
}

#[embassy_executor::task]
pub async fn espnow_host_task(
    spawner: embassy_executor::Spawner,
    wifi: esp_hal::peripherals::WIFI<'static>,
    session_id: u32,
) {
    HOST_WIRE_SESSION.store(session_id, Ordering::Release);
    let local_mac =
        esp_hal::efuse::interface_mac_address(esp_hal::efuse::InterfaceMacAddress::Station);
    log::info!("firmware: ESP-NOW host boot local={}", local_mac);
    let controller_config = ControllerConfig::default()
        .with_tx_queue_size(2)
        .with_rx_queue_size(8)
        .with_ampdu_tx_enable(false);
    let mut controller = match WifiController::new(wifi, controller_config) {
        Ok(controller) => controller,
        Err(error) => {
            log::error!("firmware: Wi-Fi controller init failed: {:?}", error);
            signal_radio_ready(false).await;
            return;
        }
    };
    if let Err(error) = controller.set_power_saving(PowerSaveMode::None) {
        log::warn!("firmware: Wi-Fi power save disable failed: {:?}", error);
    }
    let controller = WIFI_CONTROLLER_STORAGE.init(controller);
    log::info!("firmware: ESP-NOW Wi-Fi controller ready");
    let espnow = controller.esp_now();
    log::info!("firmware: ESP-NOW interface acquired");
    signal_radio_ready(true).await;
    let restored = super::storage_task::espnow_restore_receiver()
        .receive()
        .await;
    let Some(configured) = restored
        .filter(|pairing| pairing.local_role == hidshift::espnow_pairing::EspNowRole::UsbHost)
    else {
        log::warn!("firmware: ESP-NOW disabled until pairing is committed");
        core::future::pending::<()>().await;
        return;
    };
    critical_section::with(|cs| *ESPNOW_PAIRING_KEY.borrow(cs).borrow_mut() = configured.key);
    let (peer_address, channel) = (configured.peer_address, configured.channel);
    log::info!(
        "firmware: ESP-NOW pairing paired={} peer={:02x?} channel={}",
        true,
        peer_address,
        channel
    );
    if let Err(error) = espnow.set_channel(channel) {
        log::error!("firmware: ESP-NOW channel failed: {:?}", error);
        signal_radio_ready(false).await;
        return;
    }
    log::info!("firmware: ESP-NOW channel configured");
    // ESP-NOW installs a station broadcast peer when it starts. Realtime
    // input deliberately avoids unicast MAC ACK/retry latency; source MAC,
    // session and sequence validation remain at the bridge boundary.
    log::info!("firmware: ESP-NOW broadcast transport configured");
    // The two boards are used at short range, so favor airtime and callback
    // latency over long-range link margin for interactive HID traffic.
    if let Err(error) = espnow.set_rate(WifiPhyRate::Rate54m) {
        log::warn!("firmware: ESP-NOW rate configuration failed: {:?}", error);
    }
    log::info!("firmware: ESP-NOW radio configuration complete");
    log::info!("firmware: ESP-NOW radio ready signalled");
    let (manager, mut radio_tx, radio_rx) = espnow.split();
    log::info!("firmware: ESP-NOW split complete");
    let mut sequence = 1u32;
    let mut message_id = 1u32;
    let descriptors = DESCRIPTORS_STORAGE.init_with(|| [const { None }; MAX_HID_INTERFACES]);
    let send_buffer = SEND_BUFFER_STORAGE.init_with(|| [0; MAX_BRIDGE_MESSAGE_SIZE]);
    let mut descriptor_generation = 0u32;
    log::info!("firmware: ESP-NOW state initialized");
    #[cfg(feature = "hardware-e2e")]
    {
        if let Ok(descriptor) = heapless::Vec::from_slice(E2E_REPORT_DESCRIPTOR) {
            descriptors[0] = Some(HostLinkControl::InterfaceDescriptor {
                device_id: E2E_DEVICE_ID,
                interface_id: E2E_INTERFACE_ID,
                interface_index: 0xfd,
                vendor_id: 0x303a,
                product_id: 0x4001,
                descriptor,
            });
            descriptor_generation = 1;
        }
    }

    log::info!("@HIDSHIFT-BRIDGE:HOST_READY");
    send_message(
        &mut radio_tx,
        send_buffer,
        host_hello_for_peer(session_id, 0),
        &mut sequence,
        &mut message_id,
    )
    .await;
    log::info!("firmware: ESP-NOW startup hello sent");
    match host_tx_task(radio_tx, sequence, message_id, session_id) {
        Ok(token) => spawner.spawn(token),
        Err(error) => {
            log::error!("firmware: failed to create ESP-NOW TX owner: {:?}", error);
            esp_hal::system::software_reset();
        }
    }
    #[cfg(all(feature = "hardware-e2e", feature = "espnow"))]
    match synthetic_output_responder_task() {
        Ok(token) => spawner.spawn(token),
        Err(error) => {
            log::error!(
                "firmware: failed to create synthetic HID responder: {:?}",
                error
            );
            esp_hal::system::software_reset();
        }
    }
    match host_coordinator_task(
        manager,
        radio_rx,
        descriptors,
        session_id,
        descriptor_generation,
        peer_address,
    ) {
        Ok(token) => spawner.spawn(token),
        Err(error) => {
            log::error!(
                "firmware: failed to create ESP-NOW coordinator: {:?}",
                error
            );
            esp_hal::system::software_reset();
        }
    }
}

/// Emulates the source USB HID device for synthetic ESP-NOW E2E. Production
/// image routes these requests to `usb_host_task`; this responder lets hidraw
/// output and feature control transfers exercise the same ESP-NOW protocol.
#[cfg(all(feature = "hardware-e2e", feature = "espnow"))]
#[embassy_executor::task]
async fn synthetic_output_responder_task() {
    let mut feature_report = heapless::Vec::<u8, MAX_HID_REPORT_SIZE>::new();
    loop {
        match OUTPUT_REQUEST_CHANNEL.receive().await {
            HostOutputRequest::SetReport {
                interface_id,
                report_type,
                report_id,
                report,
            } => {
                if report_type == HidReportType::Feature {
                    feature_report = report.clone();
                }
                log::info!(
                    "@HIDSHIFT-BRIDGE:SET_REPORT,{},{:?},{},{}",
                    interface_id.0,
                    report_type,
                    report_id,
                    report.len()
                );
            }
            HostOutputRequest::GetReport {
                interface_id,
                report_type,
                report_id,
                requested_len,
                request_id,
            } => {
                let requested_len = usize::from(requested_len).min(MAX_HID_REPORT_SIZE);
                let mut report = heapless::Vec::new();
                if feature_report.is_empty() {
                    for index in 0..requested_len {
                        let _ = report.push((index as u8).wrapping_add(0x40));
                    }
                } else {
                    let len = feature_report.len().min(requested_len);
                    let _ = report.extend_from_slice(&feature_report[..len]);
                }
                send_output_response(HostOutputResponse {
                    interface_id,
                    report_type,
                    report_id,
                    request_id,
                    report,
                })
                .await;
            }
        }
    }
}

#[embassy_executor::task]
async fn host_coordinator_task(
    _manager: EspNowManager<'static>,
    mut radio_rx: EspNowReceiver<'static>,
    descriptors: &'static mut [Option<HostLinkControl>; MAX_HID_INTERFACES],
    session_id: u32,
    mut descriptor_generation: u32,
    peer_address: [u8; 6],
) {
    let reassembler = REASSEMBLER_STORAGE.init_with(Reassembler::new);
    let mut handshake = SessionHandshake::new(session_id);
    let mut replay_window = ReplayWindow::new();
    let mut watchdog = LinkWatchdog::new(ESPNOW_LINK_TIMEOUT_MS);
    let mut heartbeat_tick = Ticker::every(Duration::from_millis(ESPNOW_HEARTBEAT_MS));
    log::info!("firmware: ESP-NOW coordinator starting");
    loop {
        match select4(
            CONTROL_CHANNEL.receive(),
            radio_rx.receive_async(),
            OUTPUT_RESPONSE_CHANNEL.receive(),
            heartbeat_tick.next(),
        )
        .await
        {
            Either4::First(control) => {
                update_descriptor_cache(descriptors, &control);
                descriptor_generation = descriptor_generation.wrapping_add(1);
                let message = match &control {
                    HostLinkControl::InterfaceDescriptor {
                        device_id,
                        interface_id,
                        interface_index,
                        vendor_id,
                        product_id,
                        descriptor,
                    } => BridgeMessage::InterfaceDescriptor(InterfaceDescriptor {
                        device_id: *device_id,
                        interface_id: *interface_id,
                        interface_index: *interface_index,
                        vendor_id: *vendor_id,
                        product_id: *product_id,
                        descriptor: descriptor.as_slice(),
                    }),
                    HostLinkControl::InterfaceRemoved {
                        device_id,
                        interface_id,
                    } => BridgeMessage::InterfaceRemoved {
                        device_id: *device_id,
                        interface_id: *interface_id,
                    },
                    HostLinkControl::EnterDownloadMode => BridgeMessage::EnterDownloadMode,
                };
                queue_host_message(message);
            }
            Either4::Second(received) => {
                if received.info.src_address != peer_address {
                    log::warn!(
                        "firmware: rejecting ESP-NOW frame from unregistered peer {:02x?}",
                        received.info.src_address
                    );
                    continue;
                }
                let Ok(mut secure) =
                    hidshift::espnow_security::SecureEspNowFrame::decode(received.data())
                else {
                    continue;
                };
                let pairing_key =
                    critical_section::with(|cs| *ESPNOW_PAIRING_KEY.borrow(cs).borrow());
                let session_key = (DEVICE_SESSION_ID.load(Ordering::Acquire) != 0).then(|| {
                    let device_session = DEVICE_SESSION_ID.load(Ordering::Acquire);
                    hidshift::espnow_security::derive_session_key(
                        &pairing_key,
                        session_id,
                        device_session,
                        hidshift::espnow_pairing::EspNowRole::UsbDevice,
                    )
                });
                let selected_key = session_key
                    .filter(|key| {
                        secure
                            .clone()
                            .open(key, hidshift::espnow_pairing::EspNowRole::UsbDevice)
                            .is_ok()
                    })
                    .unwrap_or(pairing_key);
                let Ok(opened) = secure.open(
                    &selected_key,
                    hidshift::espnow_pairing::EspNowRole::UsbDevice,
                ) else {
                    continue;
                };
                let Ok(packet) = WirePacket::decode(opened.bytes) else {
                    continue;
                };
                if packet.session != opened.session || packet.sequence != opened.sequence {
                    continue;
                }
                if packet.kind != PacketKind::Data {
                    continue;
                }
                if replay_window.observe(packet.sequence)
                    != hidshift::link::InputSequenceDecision::New
                {
                    continue;
                }
                if let Ok(Some(bytes)) = reassembler.push(opened.bytes) {
                    if let Ok(message) = BridgeMessage::decode(bytes) {
                        if let BridgeMessage::Hello {
                            role: BridgeRole::UsbDevice,
                            session_id,
                            peer_session_id,
                            ..
                        } = message
                        {
                            let was_established = handshake.is_established();
                            let decision = handshake.observe(session_id, peer_session_id);
                            if matches!(decision, SessionDecision::Reply) {
                                queue_host_message(host_hello_for_peer(
                                    handshake.local_session(),
                                    session_id,
                                ));
                            }
                            if matches!(decision, SessionDecision::Established) && !was_established
                            {
                                replay_window.reset();
                                DEVICE_SESSION_ID.store(session_id, Ordering::Release);
                                super::transport_route::set_available(
                                    hidshift::InputTransport::EspNow,
                                    true,
                                );
                                queue_descriptor_snapshot(descriptors, descriptor_generation);
                                queue_host_message(host_hello_for_peer(
                                    handshake.local_session(),
                                    session_id,
                                ));
                            }
                        }
                        if packet.session != opened.session || packet.sequence != opened.sequence {
                            continue;
                        }
                        if packet.kind == PacketKind::Data
                            && !handshake.accepts_peer(opened.session)
                        {
                            continue;
                        }
                        watchdog.observe_packet(embassy_time::Instant::now().as_millis());
                        handle_device_message(message).await;
                    }
                }
            }
            Either4::Third(response) => {
                queue_host_message(BridgeMessage::GetReportResponse {
                    device_id: DeviceId(0),
                    interface_id: response.interface_id,
                    report_type: response.report_type,
                    report_id: response.report_id,
                    request_id: response.request_id,
                    report: response.report.as_slice(),
                });
            }
            Either4::Fourth(()) => {
                if watchdog.take_release_required(embassy_time::Instant::now().as_millis()) {
                    DEVICE_SESSION_ID.store(0, Ordering::Release);
                    replay_window.reset();
                    handshake.reset();
                    super::transport_route::set_available(hidshift::InputTransport::EspNow, false);
                    log::warn!("firmware: ESP-NOW peer heartbeat timed out");
                }
            }
        }
    }
}

fn enqueue_scheduled_input(
    scheduler: &mut InputScheduler<CRITICAL_INPUT_QUEUE_CAPACITY, MOTION_INPUT_QUEUE_CAPACITY>,
    report: HostInputReport,
) {
    let Some(input) = hidshift::link::ScheduledInput::new(
        report.device_id,
        report.interface_id,
        report.ingress_us,
        report.sequence,
        report.e2e_sequence,
        report.report.as_slice(),
        report.class,
    )
    .map(|input| input.with_motion(report.motion)) else {
        log::warn!("firmware: input scheduler rejected oversized report");
        return;
    };
    if scheduler.enqueue(input).is_err() {
        log::warn!("firmware: critical input queue full");
    }
}

fn prepare_scheduled_input(
    input: hidshift::link::ScheduledInput,
    input_history: &mut InputSnapshotHistory<REALTIME_CRITICAL_JOURNAL_CAPACITY>,
    critical_refresh: &mut Option<HostCriticalStateRefresh>,
) -> Option<HostTxPayload> {
    let payload = if input.class() == InputDeliveryClass::Critical {
        let Some(report) = InputReportRecord::new_with_motion(
            input.device_id,
            input.interface_id,
            input.sequence,
            input.e2e_sequence,
            input.motion,
            input.report.as_slice(),
        ) else {
            return None;
        };
        input_history.push(report);
        let mut records = [0u8; MAX_BRIDGE_MESSAGE_SIZE];
        let Ok(snapshot) = input_history.encode_recent(&mut records, INPUT_SNAPSHOT_RECORD_BUDGET)
        else {
            log::warn!("firmware: input snapshot encoding failed");
            return None;
        };
        let payload = encode_host_message(BridgeMessage::InputSnapshot {
            record_count: snapshot.record_count,
            records: &records[..snapshot.records_len],
        })?;
        // A newer state supersedes the previous bounded refresh schedule.
        *critical_refresh = Some(HostCriticalStateRefresh {
            payload: payload.clone(),
            refresh: CriticalStateRefresh::after_primary(embassy_time::Instant::now().as_micros()),
        });
        #[cfg(feature = "hardware-e2e")]
        if SUPPRESS_NEXT_CRITICAL_RECOVERY.swap(false, Ordering::Relaxed) {
            *critical_refresh = None;
        }
        payload
    } else {
        let motion = InputReportRecord::new_with_motion(
            input.device_id,
            input.interface_id,
            input.sequence,
            input.e2e_sequence,
            input.motion,
            input.report.as_slice(),
        )?;
        let mut records = [0u8; MAX_BRIDGE_MESSAGE_SIZE];
        let snapshot = input_history
            .encode_state(&mut records, INPUT_SNAPSHOT_RECORD_BUDGET, Some(&motion))
            .ok()?;
        // This motion frame also refreshed the critical state, so a separate
        // idle refresh would only contend with subsequent realtime input.
        if snapshot.record_count > 1
            && let Some(refresh) = critical_refresh.as_mut()
        {
            refresh
                .refresh
                .defer_after_piggyback(embassy_time::Instant::now().as_micros());
        }
        let payload = encode_host_message(BridgeMessage::InputSnapshot {
            record_count: snapshot.record_count,
            records: &records[..snapshot.records_len],
        })?;
        payload
    };
    #[cfg(feature = "hardware-e2e")]
    {
        let drop = match input.class() {
            InputDeliveryClass::Critical => DROP_NEXT_CRITICAL.swap(false, Ordering::Relaxed),
            InputDeliveryClass::Motion => DROP_NEXT_MOTION.swap(false, Ordering::Relaxed),
        };
        if drop {
            log::warn!(
                "@HIDSHIFT-BRIDGE:FAULT_DROPPED,{:?},{}",
                input.lane(),
                input.sequence
            );
            return None;
        }
    }
    Some(payload)
}

fn encode_host_message(message: BridgeMessage<'_>) -> Option<HostTxPayload> {
    let mut encoded = [0; MAX_BRIDGE_MESSAGE_SIZE];
    let len = message.encode(&mut encoded).ok()?;
    heapless::Vec::from_slice(&encoded[..len]).ok()
}

fn queue_host_message(message: BridgeMessage<'_>) {
    let Some(payload) = encode_host_message(message) else {
        return;
    };
    if HOST_TX_NORMAL_CHANNEL
        .try_send(HostTxRequest { payload })
        .is_err()
    {
        log::warn!("firmware: Host control TX queue full");
    }
}

fn queue_descriptor_snapshot(
    descriptors: &[Option<HostLinkControl>; MAX_HID_INTERFACES],
    generation: u32,
) {
    for descriptor in descriptors.iter().flatten() {
        let HostLinkControl::InterfaceDescriptor {
            device_id,
            interface_id,
            interface_index,
            vendor_id,
            product_id,
            descriptor,
        } = descriptor
        else {
            continue;
        };
        queue_host_message(BridgeMessage::InterfaceDescriptor(InterfaceDescriptor {
            device_id: *device_id,
            interface_id: *interface_id,
            interface_index: *interface_index,
            vendor_id: *vendor_id,
            product_id: *product_id,
            descriptor: descriptor.as_slice(),
        }));
    }
    queue_host_message(BridgeMessage::DescriptorSnapshotEnd {
        interface_count: descriptors.iter().flatten().count() as u8,
        generation,
    });
}

fn update_descriptor_cache(
    cache: &mut [Option<HostLinkControl>; MAX_HID_INTERFACES],
    control: &HostLinkControl,
) {
    match control {
        HostLinkControl::InterfaceDescriptor { interface_id, .. } => {
            if let Some(slot) = cache.iter_mut().find(|entry| {
                matches!(
                    entry,
                    Some(HostLinkControl::InterfaceDescriptor {
                        interface_id: cached,
                        ..
                    }) if cached == interface_id
                )
            }) {
                *slot = Some(control.clone());
            } else if let Some(slot) = cache.iter_mut().find(|entry| entry.is_none()) {
                *slot = Some(control.clone());
            }
        }
        HostLinkControl::InterfaceRemoved { interface_id, .. } => {
            if let Some(slot) = cache.iter_mut().find(|entry| {
                matches!(
                    entry,
                    Some(HostLinkControl::InterfaceDescriptor {
                        interface_id: cached,
                        ..
                    }) if cached == interface_id
                )
            }) {
                *slot = None;
            }
        }
        HostLinkControl::EnterDownloadMode => {}
    }
}

async fn send_message(
    radio: &mut esp_radio::esp_now::EspNowSender<'_>,
    send_buffer: &mut [u8; MAX_BRIDGE_MESSAGE_SIZE],
    message: BridgeMessage<'_>,
    sequence: &mut u32,
    message_id: &mut u32,
) {
    let uses_session_key = !matches!(message, BridgeMessage::Hello { .. });
    let Ok(len) = message.encode(send_buffer) else {
        log::warn!("firmware: bridge message encode failed");
        return;
    };
    let _ = send_encoded_payload(
        radio,
        &send_buffer[..len],
        sequence,
        message_id,
        uses_session_key,
    )
    .await;
}

async fn send_encoded_payload(
    radio: &mut esp_radio::esp_now::EspNowSender<'_>,
    encoded: &[u8],
    sequence: &mut u32,
    message_id: &mut u32,
    uses_session_key: bool,
) -> Option<(u64, u64)> {
    let fragments = FragmentEncoder::new(
        encoded,
        HOST_WIRE_SESSION.load(Ordering::Acquire),
        *sequence,
        *message_id,
    );
    let Ok(fragments) = fragments else {
        return None;
    };
    let mut first_send_start_us = None;
    let mut last_send_done_us = None;
    let count = fragments.fragment_count();
    for packet in fragments {
        // Keep one owner and await the callback before submitting another
        // frame. esp-radio exposes one global callback slot, so dropping the
        // waiter would let a later frame overwrite the previous completion.
        first_send_start_us.get_or_insert_with(|| embassy_time::Instant::now().as_micros());
        let Ok(view) = WirePacket::decode(packet.as_bytes()) else {
            continue;
        };
        let pairing_key = critical_section::with(|cs| *ESPNOW_PAIRING_KEY.borrow(cs).borrow());
        let key = if uses_session_key {
            let device_session = DEVICE_SESSION_ID.load(Ordering::Acquire);
            if device_session == 0 {
                return None;
            }
            hidshift::espnow_security::derive_session_key(
                &pairing_key,
                HOST_WIRE_SESSION.load(Ordering::Acquire),
                device_session,
                hidshift::espnow_pairing::EspNowRole::UsbHost,
            )
        } else {
            pairing_key
        };
        let Ok(secure) = hidshift::espnow_security::SecureEspNowFrame::seal(
            &key,
            hidshift::espnow_pairing::EspNowRole::UsbHost,
            view.session,
            view.sequence,
            packet.as_bytes(),
        ) else {
            continue;
        };
        let result = radio
            .send_async(&BROADCAST_ADDRESS, secure.as_bytes())
            .await;
        last_send_done_us = Some(embassy_time::Instant::now().as_micros());
        // Broadcast has no destination MAC ACK. The controller may report a
        // failed delivery bit even after the action frame reached the peer.
        if let Err(error) = result {
            log::trace!("firmware: broadcast callback status: {:?}", error);
        }
    }
    *sequence = sequence.wrapping_add(count as u32);
    *message_id = message_id.wrapping_add(1);
    first_send_start_us.zip(last_send_done_us)
}

#[embassy_executor::task]
async fn host_tx_task(
    mut radio: esp_radio::esp_now::EspNowSender<'static>,
    mut sequence: u32,
    mut message_id: u32,
    session_id: u32,
) {
    let input_scheduler = INPUT_SCHEDULER_STORAGE.init_with(
        InputScheduler::<CRITICAL_INPUT_QUEUE_CAPACITY, MOTION_INPUT_QUEUE_CAPACITY>::new,
    );
    let mut input_history = InputSnapshotHistory::<REALTIME_CRITICAL_JOURNAL_CAPACITY>::new();
    let mut critical_refresh: Option<HostCriticalStateRefresh> = None;
    let mut recovery_tick = Ticker::every(Duration::from_millis(INPUT_RECOVERY_TICK_MS));
    let mut heartbeat_tick = Ticker::every(Duration::from_millis(ESPNOW_HEARTBEAT_MS));
    let mut heartbeat_buffer = [0; MAX_BRIDGE_MESSAGE_SIZE];
    loop {
        while let Ok(report) = REPORT_CHANNEL.try_receive() {
            enqueue_scheduled_input(input_scheduler, report);
        }
        if let Some(input) = input_scheduler.pop_next() {
            let e2e_sequence = input.e2e_sequence;
            let ingress_us = input.ingress_us;
            #[cfg(feature = "hardware-e2e")]
            if e2e_sequence != 0 {
                crate::e2e_telemetry::record_espnow_dequeue(
                    e2e_sequence,
                    embassy_time::Instant::now().as_micros(),
                );
            }
            let Some(payload) =
                prepare_scheduled_input(input, &mut input_history, &mut critical_refresh)
            else {
                continue;
            };
            let timing = send_encoded_payload(
                &mut radio,
                payload.as_slice(),
                &mut sequence,
                &mut message_id,
                true,
            )
            .await;
            #[cfg(feature = "hardware-e2e")]
            if e2e_sequence != 0 {
                let (send_start_us, tx_done_us) = timing.unwrap_or_else(|| {
                    let now_us = embassy_time::Instant::now().as_micros();
                    (now_us, now_us)
                });
                crate::e2e_telemetry::record_espnow_tx(
                    e2e_sequence,
                    ingress_us,
                    send_start_us,
                    tx_done_us,
                );
            }
            continue;
        }
        if let Ok(request) = HOST_TX_NORMAL_CHANNEL.try_receive() {
            let _ = send_encoded_payload(
                &mut radio,
                request.payload.as_slice(),
                &mut sequence,
                &mut message_id,
                true,
            )
            .await;
            continue;
        }
        match select4(
            REPORT_CHANNEL.receive(),
            HOST_TX_NORMAL_CHANNEL.receive(),
            recovery_tick.next(),
            heartbeat_tick.next(),
        )
        .await
        {
            Either4::First(report) => enqueue_scheduled_input(input_scheduler, report),
            Either4::Second(request) => {
                let _ = send_encoded_payload(
                    &mut radio,
                    request.payload.as_slice(),
                    &mut sequence,
                    &mut message_id,
                    true,
                )
                .await;
            }
            Either4::Third(()) => {
                // Re-check realtime input after the timer wake-up. This makes
                // an idle state refresh yield to a report that arrived in the
                // same scheduler turn.
                while let Ok(report) = REPORT_CHANNEL.try_receive() {
                    enqueue_scheduled_input(input_scheduler, report);
                }
                if input_scheduler.len() != 0 {
                    continue;
                }
                let now_us = embassy_time::Instant::now().as_micros();
                let payload = critical_refresh.as_mut().and_then(|refresh| {
                    refresh
                        .refresh
                        .take_due(now_us)
                        .then(|| refresh.payload.clone())
                });
                if critical_refresh
                    .as_ref()
                    .is_some_and(|refresh| refresh.refresh.is_complete())
                {
                    critical_refresh = None;
                }
                if let Some(payload) = payload {
                    let _ = send_encoded_payload(
                        &mut radio,
                        payload.as_slice(),
                        &mut sequence,
                        &mut message_id,
                        true,
                    )
                    .await;
                }
            }
            Either4::Fourth(()) => {
                send_message(
                    &mut radio,
                    &mut heartbeat_buffer,
                    host_hello_for_peer(session_id, 0),
                    &mut sequence,
                    &mut message_id,
                )
                .await;
            }
        }
    }
}

async fn handle_device_message(message: BridgeMessage<'_>) {
    match message {
        BridgeMessage::E2eBridgeTimestamp {
            sequence,
            radio_rx_us,
            reassembled_us,
            hid_write_us,
        } => {
            #[cfg(feature = "hardware-e2e")]
            crate::e2e_telemetry::record_device_bridge_timing(
                sequence,
                radio_rx_us,
                reassembled_us,
                hid_write_us,
            );
        }
        BridgeMessage::SetReport {
            device_id,
            interface_id,
            report_type,
            report_id,
            report,
        } => {
            let _ = device_id;
            let Ok(report) = heapless::Vec::from_slice(report) else {
                return;
            };
            OUTPUT_REQUEST_CHANNEL
                .send(HostOutputRequest::SetReport {
                    interface_id,
                    report_type,
                    report_id,
                    report,
                })
                .await;
        }
        BridgeMessage::GetReport {
            interface_id,
            report_type,
            report_id,
            requested_len,
            request_id,
            ..
        } => {
            OUTPUT_REQUEST_CHANNEL
                .send(HostOutputRequest::GetReport {
                    interface_id,
                    report_type,
                    report_id,
                    requested_len,
                    request_id,
                })
                .await;
        }
        _ => {}
    }
}
