#![no_std]
#![no_main]

use core::cell::RefCell;
use core::sync::atomic::{AtomicU16, Ordering};

use critical_section::Mutex;
use embassy_futures::join::join3;
use embassy_futures::select::{Either3, Either4, select3, select4};
use embassy_time::{Duration, Ticker, Timer, with_timeout};
use embassy_usb::class::hid::{
    Config as HidConfig, HidBootProtocol, HidReader, HidReaderWriter, HidSubclass, HidWriter,
    ReadError, ReportId as UsbReportId, RequestHandler, State as HidState,
};
use embassy_usb::control::OutResponse;
use embassy_usb_host::class::hid::ReportDescriptor;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::otg_fs::{Usb, asynch::Driver as UsbDriver};
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowReceiver, EspNowSender, WifiPhyRate};
use esp_radio::wifi::{ControllerConfig, WifiController};
use hidshift::ids::{DeviceId, InterfaceId};
use hidshift::link::{
    BridgeMessage, BridgeRole, CompositeDescriptor, FragmentEncoder, HidReportType, InputLane,
    InputReportRecord, InputSequenceDecision, InputSequenceWindow, InputSnapshotRecords,
    InterfaceDescriptor, LinkWatchdog, MAX_BRIDGE_MESSAGE_SIZE, MAX_HID_REPORT_SIZE,
    MotionCumulative, PacketKind, Reassembler, ReplayWindow, SessionDecision, SessionHandshake,
    WirePacket,
};
use static_cell::StaticCell;

#[path = "bridge_device/device_management.rs"]
mod device_management;
#[path = "../platform/flash_backend.rs"]
mod flash_backend;
#[path = "../wired_management.rs"]
mod wired_management;

esp_bootloader_esp_idf::esp_app_desc!();

const USB_REPORT_BUFFER_LEN: usize = MAX_HID_REPORT_SIZE + 1;
const MAX_REPORT_FIELDS: usize = 48;
const CONTROL_EVENT_CAPACITY: usize = 8;
const LINK_LOSS_RELEASE_MS: u64 = 20;
const ESPNOW_HEARTBEAT_MS: u64 = 500;
// Reverse-direction telemetry shares the ESP-NOW channel with realtime input.
// Sample each lane independently so keyboard and mouse remain observable
// without making every synthetic input contend with a telemetry frame.
// Keep this odd so alternating keyboard press/release traffic cannot phase
// lock sampling onto only one edge forever.
const E2E_TELEMETRY_STRIDE: u32 = 7;

fn device_hello(session_id: u32, peer_session_id: u32) -> BridgeMessage<'static> {
    BridgeMessage::Hello {
        role: BridgeRole::UsbDevice,
        capabilities: 0x000f,
        session_id,
        peer_session_id,
        next_critical_sequence: 1,
        next_motion_sequence: 1,
    }
}

static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
static DEVICE_STORAGE: StaticCell<flash_backend::FirmwareStorageBackend> = StaticCell::new();
static CONTROL_EVENTS: embassy_sync::channel::Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    PcControlEvent,
    CONTROL_EVENT_CAPACITY,
> = embassy_sync::channel::Channel::new();
const DEVICE_TX_HIGH_CAPACITY: usize = 8;
const DEVICE_TX_NORMAL_CAPACITY: usize = 4;
const DEVICE_TX_TELEMETRY_CAPACITY: usize = 1;
type RawMutex = embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
static DEVICE_TX_HIGH: embassy_sync::channel::Channel<
    RawMutex,
    DeviceTxRequest,
    DEVICE_TX_HIGH_CAPACITY,
> = embassy_sync::channel::Channel::new();
static DEVICE_TX_NORMAL: embassy_sync::channel::Channel<
    RawMutex,
    DeviceTxRequest,
    DEVICE_TX_NORMAL_CAPACITY,
> = embassy_sync::channel::Channel::new();
static DEVICE_TX_TELEMETRY: embassy_sync::channel::Channel<
    RawMutex,
    DeviceTxRequest,
    DEVICE_TX_TELEMETRY_CAPACITY,
> = embassy_sync::channel::Channel::new();
static FEATURE_CACHE: Mutex<RefCell<FeatureCache>> = Mutex::new(RefCell::new(FeatureCache::new()));
static ESPNOW_PAIRING_KEY: Mutex<RefCell<[u8; 16]>> = Mutex::new(RefCell::new([0; 16]));
static NEXT_GET_REPORT_ID: AtomicU16 = AtomicU16::new(1);
static DEVICE_WIRE_SESSION: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static HOST_SESSION_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[derive(Clone, Debug)]
enum PcControlEvent {
    SetReport {
        interface_id: InterfaceId,
        report_type: HidReportType,
        report_id: u8,
        report: heapless::Vec<u8, MAX_HID_REPORT_SIZE>,
    },
    GetReport {
        interface_id: InterfaceId,
        source_report_id: u8,
        composite_report_id: u8,
        requested_len: u16,
        request_id: u16,
    },
}

#[derive(Clone, Debug)]
enum DeviceTxRequest {
    Message {
        encoded: heapless::Vec<u8, MAX_BRIDGE_MESSAGE_SIZE>,
        uses_session_key: bool,
    },
}

#[derive(Clone, Copy)]
struct FeatureCache {
    composite_report_id: u8,
    len: usize,
    bytes: [u8; USB_REPORT_BUFFER_LEN],
    valid: bool,
}

impl FeatureCache {
    const fn new() -> Self {
        Self {
            composite_report_id: 0,
            len: 0,
            bytes: [0; USB_REPORT_BUFFER_LEN],
            valid: false,
        }
    }
}

struct DeviceControlHandler<'a> {
    composite: &'a CompositeDescriptor,
}

impl RequestHandler for DeviceControlHandler<'_> {
    fn get_report(&mut self, id: UsbReportId, buf: &mut [u8]) -> Option<usize> {
        let UsbReportId::Feature(composite_report_id) = id else {
            return None;
        };
        let cached = critical_section::with(|cs| {
            let cache = FEATURE_CACHE.borrow(cs).borrow();
            if !cache.valid || cache.composite_report_id != composite_report_id {
                return None;
            }
            let len = cache.len.min(buf.len());
            buf[..len].copy_from_slice(&cache.bytes[..len]);
            Some(len)
        });
        if cached.is_some() {
            return cached;
        }

        let mut source = [0; MAX_HID_REPORT_SIZE];
        let Ok((interface_id, source_report_id, _)) = self
            .composite
            .decode_host_report(&[composite_report_id], &mut source)
        else {
            return None;
        };
        let request_id = NEXT_GET_REPORT_ID.fetch_add(1, Ordering::Relaxed);
        let _ = CONTROL_EVENTS.try_send(PcControlEvent::GetReport {
            interface_id,
            source_report_id: source_report_id.unwrap_or(0),
            composite_report_id,
            requested_len: buf.len().min(MAX_HID_REPORT_SIZE) as u16,
            request_id,
        });
        None
    }

    fn set_report(&mut self, id: UsbReportId, data: &[u8]) -> OutResponse {
        let (report_type, composite_report_id) = match id {
            UsbReportId::Out(id) => (HidReportType::Output, id),
            UsbReportId::Feature(id) => (HidReportType::Feature, id),
            UsbReportId::In(_) => return OutResponse::Rejected,
        };
        let mut composite_report = [0; USB_REPORT_BUFFER_LEN];
        let composite_len = if data.first().copied() == Some(composite_report_id) {
            if data.len() > composite_report.len() {
                return OutResponse::Rejected;
            }
            composite_report[..data.len()].copy_from_slice(data);
            data.len()
        } else {
            if data.len() + 1 > composite_report.len() {
                return OutResponse::Rejected;
            }
            composite_report[0] = composite_report_id;
            composite_report[1..data.len() + 1].copy_from_slice(data);
            data.len() + 1
        };
        let mut source_report = [0; MAX_HID_REPORT_SIZE];
        let Ok((interface_id, source_id, len)) = self
            .composite
            .decode_host_report(&composite_report[..composite_len], &mut source_report)
        else {
            return OutResponse::Rejected;
        };
        let Ok(report) = heapless::Vec::from_slice(&source_report[..len]) else {
            return OutResponse::Rejected;
        };
        if report_type == HidReportType::Feature {
            critical_section::with(|cs| {
                FEATURE_CACHE.borrow(cs).borrow_mut().valid = false;
            });
        }
        match CONTROL_EVENTS.try_send(PcControlEvent::SetReport {
            interface_id,
            report_type,
            report_id: source_id.unwrap_or(0),
            report,
        }) {
            Ok(()) => OutResponse::Accepted,
            Err(_) => OutResponse::Rejected,
        }
    }
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let boot_session_id = esp_hal::rng::Rng::new().random();
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_ints = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);

    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run(|spawner| {
        match bridge_device_task(
            spawner,
            peripherals.WIFI,
            peripherals.USB0,
            peripherals.GPIO20,
            peripherals.GPIO19,
            peripherals.FLASH,
            peripherals.UART0,
            peripherals.GPIO44,
            boot_session_id,
        ) {
            Ok(token) => spawner.spawn(token),
            Err(_) => esp_hal::system::software_reset(),
        }
    })
}

#[embassy_executor::task]
async fn bridge_device_task(
    spawner: embassy_executor::Spawner,
    wifi: esp_hal::peripherals::WIFI<'static>,
    usb0: esp_hal::peripherals::USB0<'static>,
    usb_dp: esp_hal::peripherals::GPIO20<'static>,
    usb_dm: esp_hal::peripherals::GPIO19<'static>,
    flash: esp_hal::peripherals::FLASH<'static>,
    uart: esp_hal::peripherals::UART0<'static>,
    uart_rx: esp_hal::peripherals::GPIO44<'static>,
    session_id: u32,
) {
    DEVICE_WIRE_SESSION.store(session_id, Ordering::Release);
    let storage = DEVICE_STORAGE.init(flash_backend::new_storage_backend(flash));
    let restored_pairing = storage
        .restored_pairing()
        .filter(|pairing| pairing.local_role == hidshift::espnow_pairing::EspNowRole::UsbDevice);
    let Some(restored_pairing) = restored_pairing else {
        if let Ok(token) = device_management::task(storage, uart, uart_rx) {
            spawner.spawn(token);
        }
        log::warn!("bridge-device: ESP-NOW disabled until pairing is committed");
        core::future::pending::<()>().await;
        return;
    };
    critical_section::with(|cs| *ESPNOW_PAIRING_KEY.borrow(cs).borrow_mut() = restored_pairing.key);
    let (peer_address, channel) = (restored_pairing.peer_address, restored_pairing.channel);
    let controller_config = ControllerConfig::default()
        .with_tx_queue_size(8)
        .with_rx_queue_size(8)
        .with_ampdu_tx_enable(false);
    let controller = match WifiController::new(wifi, controller_config) {
        Ok(controller) => controller,
        Err(error) => {
            log::error!("bridge-device: Wi-Fi init failed: {:?}", error);
            return;
        }
    };
    // ControllerConfig defaults to PowerSaveMode::None. Calling the setter
    // here blocks on the station task in the standalone Device image, while
    // the default already provides the required low-latency behavior.
    let espnow = controller.esp_now();
    log::info!("bridge-device: ESP-NOW interface acquired");
    if espnow.set_channel(channel).is_err() {
        log::error!("bridge-device: ESP-NOW base configuration failed");
        return;
    }
    log::info!("bridge-device: ESP-NOW broadcast transport configured");
    let _ = espnow.set_rate(WifiPhyRate::Rate54m);
    let (_manager, mut radio_tx, mut radio_rx) = espnow.split();
    if let Ok(token) = device_management::task(storage, uart, uart_rx) {
        spawner.spawn(token);
    } else {
        log::error!("bridge-device: failed to start wired management");
        return;
    }
    let mut tx_sequence = 1u32;
    let mut tx_message_id = 1u32;

    let Some((composite, vendor_id, product_id, descriptor_generation)) = collect_descriptors(
        &mut radio_tx,
        &mut radio_rx,
        &mut tx_sequence,
        &mut tx_message_id,
        session_id,
        peer_address,
    )
    .await
    else {
        log::error!("bridge-device: no HID descriptors received");
        return;
    };
    let parsed_descriptor = ReportDescriptor::<MAX_REPORT_FIELDS>::parse(composite.descriptor());
    let motion_descriptor = match to_core_descriptor(&parsed_descriptor) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            log::error!(
                "bridge-device: composite descriptor parse failed: {:?}",
                error
            );
            return;
        }
    };
    log::info!(
        "@HIDSHIFT-BRIDGE:DESCRIPTORS_READY,{},{}",
        composite.descriptor().len(),
        composite.mappings().len()
    );

    let usb = Usb::new(usb0, usb_dp, usb_dm);
    let mut ep_out_buffer = [0u8; 512];
    let driver = UsbDriver::new(usb, &mut ep_out_buffer, Default::default());
    let mut config = embassy_usb::Config::new(vendor_id, product_id);
    config.manufacturer = Some("HIDShift");
    config.product = Some("ESP-NOW HID Bridge");
    config.serial_number = Some("68EE8F6394A0");
    config.max_power = 100;

    let mut config_descriptor = [0; 512];
    let mut bos_descriptor = [0; 256];
    let mut msos_descriptor = [0; 256];
    let mut control_buf = [0; USB_REPORT_BUFFER_LEN];
    // Class state and handlers must outlive the USB builder/device that stores
    // their references (Rust drops locals in reverse declaration order).
    let mut hid_state = HidState::new();
    let mut control_handler = DeviceControlHandler {
        composite: &composite,
    };
    let mut builder = embassy_usb::Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut msos_descriptor,
        &mut control_buf,
    );
    let hid = HidReaderWriter::<_, USB_REPORT_BUFFER_LEN, USB_REPORT_BUFFER_LEN>::new(
        &mut builder,
        &mut hid_state,
        HidConfig {
            report_descriptor: composite.descriptor(),
            request_handler: Some(&mut control_handler),
            poll_ms: 1,
            max_packet_size: 64,
            hid_subclass: HidSubclass::No,
            hid_boot_protocol: HidBootProtocol::None,
        },
    );
    let (reader, writer) = hid.split();
    let mut usb_device = builder.build();
    log::info!("@HIDSHIFT-BRIDGE:DEVICE_READY");

    let bridge = device_bridge_loop(
        reader,
        writer,
        &composite,
        &mut radio_rx,
        descriptor_generation,
        session_id,
        &motion_descriptor,
        peer_address,
    );
    let tx_owner = device_tx_loop(&mut radio_tx, tx_sequence, tx_message_id);
    join3(usb_device.run(), bridge, tx_owner).await;
}

async fn collect_descriptors(
    tx: &mut EspNowSender<'_>,
    rx: &mut EspNowReceiver<'_>,
    tx_sequence: &mut u32,
    tx_message_id: &mut u32,
    session_id: u32,
    peer_address: [u8; 6],
) -> Option<(CompositeDescriptor, u16, u16, u32)> {
    let mut composite = CompositeDescriptor::new();
    let mut reassembler = Reassembler::new();
    let mut identifiers = None;
    let mut handshake = SessionHandshake::new(session_id);
    let mut replay_window = ReplayWindow::new();
    // Avoid delivering a Hello while a simultaneously booting Host is still
    // registering the ESP-NOW receive interrupt and callback state.
    Timer::after_millis(1_000).await;
    loop {
        send_device_message_direct(
            tx,
            device_hello(session_id, 0),
            tx_sequence,
            tx_message_id,
            false,
        )
        .await;
        let received = match with_timeout(Duration::from_millis(500), rx.receive_async()).await {
            Ok(received) => received,
            Err(_) => continue,
        };
        if received.info.src_address != peer_address {
            continue;
        }
        let Ok(mut secure) = hidshift::espnow_security::SecureEspNowFrame::decode(received.data())
        else {
            continue;
        };
        let pairing_key = critical_section::with(|cs| *ESPNOW_PAIRING_KEY.borrow(cs).borrow());
        let host_session = HOST_SESSION_ID.load(Ordering::Acquire);
        let session_key = (host_session != 0).then(|| {
            hidshift::espnow_security::derive_session_key(
                &pairing_key,
                host_session,
                session_id,
                hidshift::espnow_pairing::EspNowRole::UsbHost,
            )
        });
        let selected_key = session_key
            .filter(|key| {
                secure
                    .clone()
                    .open(key, hidshift::espnow_pairing::EspNowRole::UsbHost)
                    .is_ok()
            })
            .unwrap_or(pairing_key);
        let Ok(opened) = secure.open(&selected_key, hidshift::espnow_pairing::EspNowRole::UsbHost)
        else {
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
        if replay_window.observe(packet.sequence) != hidshift::link::InputSequenceDecision::New {
            continue;
        }
        let Ok(Some(bytes)) = reassembler.push(opened.bytes) else {
            continue;
        };
        let Ok(message) = BridgeMessage::decode(bytes) else {
            continue;
        };
        if let BridgeMessage::Hello {
            role: BridgeRole::UsbHost,
            session_id: host_session,
            peer_session_id,
            ..
        } = message
        {
            match handshake.observe(host_session, peer_session_id) {
                SessionDecision::Reply | SessionDecision::Established => {
                    HOST_SESSION_ID.store(host_session, Ordering::Release);
                    send_device_message_direct(
                        tx,
                        device_hello(session_id, host_session),
                        tx_sequence,
                        tx_message_id,
                        false,
                    )
                    .await;
                }
                SessionDecision::Ignore => continue,
            }
            if !handshake.is_established() {
                continue;
            }
        }
        if packet.session != opened.session || packet.sequence != opened.sequence {
            continue;
        }
        if packet.kind == PacketKind::Data && !handshake.accepts_peer(opened.session) {
            continue;
        }
        match message {
            BridgeMessage::InterfaceDescriptor(InterfaceDescriptor {
                interface_id,
                vendor_id,
                product_id,
                descriptor,
                ..
            }) => match composite.add_interface(interface_id, descriptor) {
                Ok(()) => {
                    identifiers.get_or_insert((vendor_id, product_id));
                }
                Err(hidshift::link::CompositeDescriptorError::DuplicateInterface) => {
                    identifiers.get_or_insert((vendor_id, product_id));
                }
                Err(error) => {
                    log::warn!(
                        "bridge-device: descriptor interface={} rejected {:?}",
                        interface_id.0,
                        error
                    );
                }
            },
            BridgeMessage::DescriptorSnapshotEnd {
                interface_count,
                generation,
            } if identifiers.is_some()
                && composite.interface_count() >= interface_count as usize =>
            {
                let (vendor_id, product_id) = identifiers?;
                return Some((composite, vendor_id, product_id, generation));
            }
            _ => {}
        }
    }
}

fn to_core_descriptor<const SRC: usize, const DST: usize>(
    descriptor: &ReportDescriptor<SRC>,
) -> Result<
    hidshift::usb_hid::report::HidReportDescriptor<DST>,
    hidshift::usb_hid::report::HidReportError,
> {
    let mut result = hidshift::usb_hid::report::HidReportDescriptor::new(descriptor.has_report_ids);
    for field in descriptor.fields() {
        result.push(hidshift::usb_hid::report::ReportField {
            report_id: field.report_id,
            usage_page: field.usage_page,
            usage_min: field.usage_min,
            usage_max: field.usage_max,
            bit_offset: field.bit_offset,
            bit_size: field.bit_size,
            count: field.count,
            flags: field.flags,
            logical_min: field.logical_min,
            logical_max: field.logical_max,
        })?;
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn device_bridge_loop<'d, D: embassy_usb::driver::Driver<'d>>(
    mut reader: HidReader<'d, D, USB_REPORT_BUFFER_LEN>,
    mut writer: HidWriter<'d, D, USB_REPORT_BUFFER_LEN>,
    composite: &CompositeDescriptor,
    rx: &mut EspNowReceiver<'_>,
    descriptor_generation: u32,
    device_session_id: u32,
    motion_descriptor: &hidshift::usb_hid::report::HidReportDescriptor<MAX_REPORT_FIELDS>,
    peer_address: [u8; 6],
) {
    let mut reassembler = Reassembler::new();
    let mut watchdog = LinkWatchdog::new(LINK_LOSS_RELEASE_MS);
    let mut output_buf = [0; USB_REPORT_BUFFER_LEN];
    let mut last_reports = heapless::Vec::<LastInputReport, 32>::new();
    let mut critical_sequences = InputSequenceWindow::new();
    let mut motion_sequences = InputSequenceWindow::new();
    let mut handshake = SessionHandshake::new(device_session_id);
    let mut replay_window = ReplayWindow::new();
    let initial_host_session = HOST_SESSION_ID.load(Ordering::Acquire);
    if initial_host_session != 0 {
        let _ = handshake.observe(initial_host_session, device_session_id);
    }
    let mut motion_totals = [const { None }; 256];
    let mut critical_telemetry_count = 0u32;
    let mut motion_telemetry_count = 0u32;
    loop {
        match select3(
            rx.receive_async(),
            reader.read(&mut output_buf),
            Timer::after_millis(5),
        )
        .await
        {
            Either3::First(received) => {
                if received.info.src_address != peer_address {
                    continue;
                }
                let radio_rx_us = embassy_time::Instant::now().as_micros();
                let Ok(mut secure) =
                    hidshift::espnow_security::SecureEspNowFrame::decode(received.data())
                else {
                    continue;
                };
                let pairing_key =
                    critical_section::with(|cs| *ESPNOW_PAIRING_KEY.borrow(cs).borrow());
                let host_session_id = HOST_SESSION_ID.load(Ordering::Acquire);
                let session_key = (host_session_id != 0 && handshake.is_established()).then(|| {
                    hidshift::espnow_security::derive_session_key(
                        &pairing_key,
                        host_session_id,
                        device_session_id,
                        hidshift::espnow_pairing::EspNowRole::UsbHost,
                    )
                });
                let selected_key = session_key
                    .filter(|key| {
                        secure
                            .clone()
                            .open(key, hidshift::espnow_pairing::EspNowRole::UsbHost)
                            .is_ok()
                    })
                    .unwrap_or(pairing_key);
                let Ok(opened) =
                    secure.open(&selected_key, hidshift::espnow_pairing::EspNowRole::UsbHost)
                else {
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
                let Ok(Some(bytes)) = reassembler.push(opened.bytes) else {
                    continue;
                };
                let reassembled_us = embassy_time::Instant::now().as_micros();
                let Ok(message) = BridgeMessage::decode(bytes) else {
                    continue;
                };
                if let BridgeMessage::Hello {
                    role: BridgeRole::UsbHost,
                    session_id,
                    peer_session_id,
                    ..
                } = message
                {
                    let was_established = handshake.is_established();
                    match handshake.observe(session_id, peer_session_id) {
                        SessionDecision::Reply | SessionDecision::Established => {
                            HOST_SESSION_ID.store(session_id, Ordering::Release);
                            queue_device_high(device_hello(device_session_id, session_id));
                        }
                        SessionDecision::Ignore => continue,
                    }
                    if !was_established && handshake.is_established() {
                        critical_sequences.reset();
                        motion_sequences.reset();
                        motion_totals.fill(None);
                        critical_telemetry_count = 0;
                        motion_telemetry_count = 0;
                    }
                }
                if packet.session != opened.session || packet.sequence != opened.sequence {
                    continue;
                }
                if replay_window.observe(packet.sequence)
                    != hidshift::link::InputSequenceDecision::New
                {
                    continue;
                }
                if !handshake.accepts_peer(opened.session) {
                    continue;
                }
                // Only authenticated, correctly session-bound frames keep the
                // link alive. In particular, a spoofed source MAC or bad CMAC
                // cannot suppress the stuck-input release.
                watchdog.observe_packet(embassy_time::Instant::now().as_millis());
                if matches!(message, BridgeMessage::EnterDownloadMode) {
                    log::info!("@HIDSHIFT-BRIDGE:ENTER_DOWNLOAD");
                    Timer::after_millis(20).await;
                    enter_rom_download_mode();
                }
                if matches!(
                    message,
                    BridgeMessage::DescriptorSnapshotEnd { generation, .. }
                        if generation != descriptor_generation
                ) || matches!(message, BridgeMessage::InterfaceRemoved { .. })
                {
                    log::info!("@HIDSHIFT-BRIDGE:REENUMERATE");
                    Timer::after_millis(10).await;
                    esp_hal::system::software_reset();
                }
                let result = handle_host_message(
                    message,
                    composite,
                    &mut writer,
                    &mut last_reports,
                    &mut critical_sequences,
                    &mut motion_sequences,
                    &mut motion_totals,
                    motion_descriptor,
                )
                .await;
                if let Some((lane, e2e_sequence, hid_write_us)) = result.telemetry {
                    #[cfg(feature = "hardware-e2e")]
                    if e2e_sequence != 0
                        && should_sample_telemetry(
                            lane,
                            &mut critical_telemetry_count,
                            &mut motion_telemetry_count,
                        )
                    {
                        queue_device_telemetry(BridgeMessage::E2eBridgeTimestamp {
                            sequence: e2e_sequence,
                            radio_rx_us,
                            reassembled_us,
                            hid_write_us,
                        });
                    }
                }
            }
            Either3::Second(Ok(len)) => {
                forward_interrupt_output(&output_buf[..len], composite);
            }
            Either3::Second(Err(ReadError::Disabled)) => reader.ready().await,
            Either3::Second(Err(_)) => {}
            Either3::Third(()) => {
                while let Ok(control) = CONTROL_EVENTS.try_receive() {
                    match control {
                        PcControlEvent::SetReport {
                            interface_id,
                            report_type,
                            report_id,
                            report,
                        } => {
                            queue_device_message(BridgeMessage::SetReport {
                                device_id: DeviceId(0),
                                interface_id,
                                report_type,
                                report_id,
                                report: report.as_slice(),
                            });
                        }
                        PcControlEvent::GetReport {
                            interface_id,
                            source_report_id,
                            composite_report_id,
                            requested_len,
                            request_id,
                        } => {
                            let _ = composite_report_id;
                            queue_device_message(BridgeMessage::GetReport {
                                device_id: DeviceId(0),
                                interface_id,
                                report_type: HidReportType::Feature,
                                report_id: source_report_id,
                                requested_len,
                                request_id,
                            });
                        }
                    }
                }
                if watchdog.take_release_required(embassy_time::Instant::now().as_millis()) {
                    release_all_reports(&mut writer, &last_reports).await;
                    handshake.reset();
                    replay_window.reset();
                    HOST_SESSION_ID.store(0, Ordering::Release);
                    critical_sequences.reset();
                    motion_sequences.reset();
                    motion_totals.fill(None);
                    log::warn!("@HIDSHIFT-BRIDGE:LINK_LOSS_RELEASE");
                }
            }
        }
    }
}

fn enter_rom_download_mode() -> ! {
    // ESP32-S3 ROM samples this software strap on a SoC reset and enters the
    // USB Serial/JTAG download loader without requiring GPIO0/BOOT.
    esp_hal::peripherals::LPWR::regs()
        .option1()
        .modify(|_, w| w.force_download_boot().set_bit());
    esp_hal::system::software_reset()
}

#[derive(Clone, Copy)]
struct LastInputReport {
    composite_report_id: u8,
    len: usize,
}

struct InputHandlingResult {
    telemetry: Option<(InputLane, u32, u64)>,
}

impl InputHandlingResult {
    const fn new() -> Self {
        Self { telemetry: None }
    }
}

#[cfg(feature = "hardware-e2e")]
fn should_sample_telemetry(
    lane: InputLane,
    critical_count: &mut u32,
    motion_count: &mut u32,
) -> bool {
    let count = match lane {
        InputLane::Critical => critical_count,
        InputLane::Motion => motion_count,
    };
    *count = count.wrapping_add(1);
    *count % E2E_TELEMETRY_STRIDE == 0
}

async fn handle_host_message<'d, D: embassy_usb::driver::Driver<'d>>(
    message: BridgeMessage<'_>,
    composite: &CompositeDescriptor,
    writer: &mut HidWriter<'d, D, USB_REPORT_BUFFER_LEN>,
    last_reports: &mut heapless::Vec<LastInputReport, 32>,
    critical_sequences: &mut InputSequenceWindow,
    motion_sequences: &mut InputSequenceWindow,
    motion_totals: &mut [Option<MotionCumulative>; 256],
    motion_descriptor: &hidshift::usb_hid::report::HidReportDescriptor<MAX_REPORT_FIELDS>,
) -> InputHandlingResult {
    let mut result = InputHandlingResult::new();
    match message {
        BridgeMessage::Hello {
            role: BridgeRole::UsbHost,
            ..
        } => result,
        BridgeMessage::InputSnapshot {
            record_count,
            records,
        } => {
            let Ok(records) = InputSnapshotRecords::new(record_count, records) else {
                return result;
            };
            for record in records {
                match record.lane {
                    InputLane::Critical => {
                        if critical_sequences.observe_forward_only(record.sequence)
                            != InputSequenceDecision::New
                        {
                            continue;
                        }
                        let Some(critical) = InputReportRecord::new(
                            record.device_id,
                            record.interface_id,
                            record.sequence,
                            record.e2e_sequence,
                            record.report,
                        ) else {
                            continue;
                        };
                        if let Some(telemetry) = write_input_report(
                            composite,
                            writer,
                            last_reports,
                            &critical,
                            None,
                            motion_descriptor,
                        )
                        .await
                        {
                            result.telemetry =
                                Some((InputLane::Critical, telemetry.0, telemetry.1));
                        }
                    }
                    InputLane::Motion => {
                        if motion_sequences.observe_forward_only(record.sequence)
                            != InputSequenceDecision::New
                        {
                            continue;
                        }
                        let Some(motion) = InputReportRecord::new_with_motion(
                            record.device_id,
                            record.interface_id,
                            record.sequence,
                            record.e2e_sequence,
                            record.motion,
                            record.report,
                        ) else {
                            continue;
                        };
                        let index = record.interface_id.0 as usize;
                        let previous = motion_totals[index].unwrap_or_else(MotionCumulative::zero);
                        let delta = record.motion.delta_from(Some(previous));
                        motion_totals[index] = Some(record.motion);
                        if let Some(telemetry) = write_input_report(
                            composite,
                            writer,
                            last_reports,
                            &motion,
                            Some(delta),
                            motion_descriptor,
                        )
                        .await
                        {
                            result.telemetry = Some((InputLane::Motion, telemetry.0, telemetry.1));
                        }
                    }
                }
            }
            result
        }
        BridgeMessage::InputReport {
            device_id,
            interface_id,
            lane,
            sequence,
            e2e_sequence,
            motion,
            report,
            ..
        } => {
            if lane == InputLane::Critical {
                if critical_sequences.observe_forward_only(sequence) != InputSequenceDecision::New {
                    return result;
                }
                let Some(critical) =
                    InputReportRecord::new(device_id, interface_id, sequence, e2e_sequence, report)
                else {
                    return result;
                };
                if let Some(telemetry) = write_input_report(
                    composite,
                    writer,
                    last_reports,
                    &critical,
                    None,
                    motion_descriptor,
                )
                .await
                {
                    result.telemetry = Some((InputLane::Critical, telemetry.0, telemetry.1));
                }
            } else {
                if motion_sequences.observe_forward_only(sequence) != InputSequenceDecision::New {
                    return result;
                }
                let Some(motion) = InputReportRecord::new_with_motion(
                    device_id,
                    interface_id,
                    sequence,
                    e2e_sequence,
                    motion,
                    report,
                ) else {
                    return result;
                };
                let index = interface_id.0 as usize;
                let previous = motion_totals[index].unwrap_or_else(MotionCumulative::zero);
                let delta = motion.motion.delta_from(Some(previous));
                motion_totals[index] = Some(motion.motion);
                result.telemetry = write_input_report(
                    composite,
                    writer,
                    last_reports,
                    &motion,
                    Some(delta),
                    motion_descriptor,
                )
                .await
                .map(|telemetry| (InputLane::Motion, telemetry.0, telemetry.1));
            }
            result
        }
        BridgeMessage::GetReportResponse {
            interface_id,
            report_id,
            report,
            ..
        } => {
            let mut source_with_id = [0; USB_REPORT_BUFFER_LEN];
            let source = if report_id != 0
                && report.first().copied() != Some(report_id)
                && report.len() < MAX_HID_REPORT_SIZE
            {
                source_with_id[0] = report_id;
                source_with_id[1..report.len() + 1].copy_from_slice(report);
                &source_with_id[..report.len() + 1]
            } else {
                report
            };
            let mut composite_report = [0; USB_REPORT_BUFFER_LEN];
            if let Ok(len) =
                composite.encode_input_report(interface_id, source, &mut composite_report)
            {
                critical_section::with(|cs| {
                    let mut cache = FEATURE_CACHE.borrow(cs).borrow_mut();
                    cache.composite_report_id = composite_report[0];
                    cache.bytes[..len].copy_from_slice(&composite_report[..len]);
                    cache.len = len;
                    cache.valid = true;
                });
            }
            result
        }
        BridgeMessage::ReleaseAll => {
            release_all_reports(writer, last_reports).await;
            result
        }
        _ => result,
    }
}

async fn write_input_report<'d, D: embassy_usb::driver::Driver<'d>>(
    composite: &CompositeDescriptor,
    writer: &mut HidWriter<'d, D, USB_REPORT_BUFFER_LEN>,
    last_reports: &mut heapless::Vec<LastInputReport, 32>,
    report: &InputReportRecord,
    motion: Option<hidshift::input::MouseMovement>,
    motion_descriptor: &hidshift::usb_hid::report::HidReportDescriptor<MAX_REPORT_FIELDS>,
) -> Option<(u32, u64)> {
    let mut composite_report = [0; USB_REPORT_BUFFER_LEN];
    let Ok(len) =
        composite.encode_input_report(report.interface_id, report.report(), &mut composite_report)
    else {
        return None;
    };
    let mut remaining = motion;
    let mut rewritten_report = [0; USB_REPORT_BUFFER_LEN];
    let mut report_id = composite_report[0];
    loop {
        let chunk = remaining.map(|value| {
            let chunk = hidshift::input::MouseMovement {
                x: value.x.clamp(-127, 127) as i16,
                y: value.y.clamp(-127, 127) as i16,
                wheel: value.wheel.clamp(-127, 127) as i8,
                pan: value.pan.clamp(-127, 127) as i8,
            };
            hidshift::input::MouseMovement {
                x: value.x - chunk.x,
                y: value.y - chunk.y,
                wheel: value.wheel - chunk.wheel,
                pan: value.pan - chunk.pan,
            }
        });
        let report_bytes = if let Some(movement) =
            remaining.map(|value| hidshift::input::MouseMovement {
                x: value.x.clamp(-127, 127) as i16,
                y: value.y.clamp(-127, 127) as i16,
                wheel: value.wheel.clamp(-127, 127) as i8,
                pan: value.pan.clamp(-127, 127) as i8,
            }) {
            let Ok(rewritten_len) = motion_descriptor.rewrite_relative_motion(
                &composite_report[..len],
                movement,
                &mut rewritten_report,
            ) else {
                return None;
            };
            report_id = rewritten_report[0];
            &rewritten_report[..rewritten_len]
        } else {
            &composite_report[..len]
        };
        if let Err(error) = writer.write(report_bytes).await {
            log::debug!("bridge-device: USB IN failed {:?}", error);
            return None;
        }
        remaining = chunk;
        if remaining
            .is_none_or(|value| value.x == 0 && value.y == 0 && value.wheel == 0 && value.pan == 0)
        {
            break;
        }
    }
    if let Some(last) = last_reports
        .iter_mut()
        .find(|last| last.composite_report_id == report_id)
    {
        last.len = len;
    } else {
        let _ = last_reports.push(LastInputReport {
            composite_report_id: report_id,
            len,
        });
    }
    Some((
        report.e2e_sequence,
        embassy_time::Instant::now().as_micros(),
    ))
}

async fn release_all_reports<'d, D: embassy_usb::driver::Driver<'d>>(
    writer: &mut HidWriter<'d, D, USB_REPORT_BUFFER_LEN>,
    last_reports: &[LastInputReport],
) {
    let mut release = [0; USB_REPORT_BUFFER_LEN];
    for report in last_reports {
        release[0] = report.composite_report_id;
        release[1..report.len].fill(0);
        let _ = writer.write(&release[..report.len]).await;
    }
}

fn forward_interrupt_output(composite_report: &[u8], composite: &CompositeDescriptor) {
    let mut source = [0; MAX_HID_REPORT_SIZE];
    let Ok((interface_id, report_id, len)) =
        composite.decode_host_report(composite_report, &mut source)
    else {
        return;
    };
    queue_device_message(BridgeMessage::SetReport {
        device_id: DeviceId(0),
        interface_id,
        report_type: HidReportType::Output,
        report_id: report_id.unwrap_or(0),
        report: &source[..len],
    });
}

async fn send_device_message_direct(
    tx: &mut EspNowSender<'_>,
    message: BridgeMessage<'_>,
    sequence: &mut u32,
    message_id: &mut u32,
    uses_session_key: bool,
) {
    let mut encoded = [0; MAX_BRIDGE_MESSAGE_SIZE];
    let Ok(len) = message.encode(&mut encoded) else {
        return;
    };
    let Ok(fragments) = FragmentEncoder::new(
        &encoded[..len],
        DEVICE_WIRE_SESSION.load(Ordering::Acquire),
        *sequence,
        *message_id,
    ) else {
        return;
    };
    let count = fragments.fragment_count();
    for packet in fragments {
        let Some(secure) = secure_device_packet(&packet, uses_session_key) else {
            continue;
        };
        if tx
            .send_async(&BROADCAST_ADDRESS, secure.as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }
    *sequence = sequence.wrapping_add(count as u32);
    *message_id = message_id.wrapping_add(1);
}

fn encode_device_request(message: BridgeMessage<'_>) -> Option<DeviceTxRequest> {
    let uses_session_key = !matches!(message, BridgeMessage::Hello { .. });
    let mut encoded = [0; MAX_BRIDGE_MESSAGE_SIZE];
    let len = message.encode(&mut encoded).ok()?;
    Some(DeviceTxRequest::Message {
        encoded: heapless::Vec::from_slice(&encoded[..len]).ok()?,
        uses_session_key,
    })
}

fn queue_device_message(message: BridgeMessage<'_>) {
    if let Some(request) = encode_device_request(message) {
        if DEVICE_TX_NORMAL.try_send(request).is_err() {
            log::warn!("bridge-device: normal TX queue full");
        }
    }
}

fn queue_device_high(message: BridgeMessage<'_>) {
    if let Some(request) = encode_device_request(message) {
        if DEVICE_TX_HIGH.try_send(request).is_err() {
            log::warn!("bridge-device: high-priority TX queue full");
        }
    }
}

#[cfg(feature = "hardware-e2e")]
fn queue_device_telemetry(message: BridgeMessage<'_>) {
    let Some(request) = encode_device_request(message) else {
        return;
    };
    let _ = DEVICE_TX_TELEMETRY.try_send(request);
}

async fn device_tx_loop(tx: &mut EspNowSender<'_>, mut sequence: u32, mut message_id: u32) {
    let mut heartbeat_tick = Ticker::every(Duration::from_millis(ESPNOW_HEARTBEAT_MS));
    loop {
        let request = if let Ok(request) = DEVICE_TX_HIGH.try_receive() {
            Some(request)
        } else if let Ok(request) = DEVICE_TX_NORMAL.try_receive() {
            Some(request)
        } else if let Ok(request) = DEVICE_TX_TELEMETRY.try_receive() {
            Some(request)
        } else {
            match select4(
                DEVICE_TX_HIGH.receive(),
                DEVICE_TX_NORMAL.receive(),
                DEVICE_TX_TELEMETRY.receive(),
                heartbeat_tick.next(),
            )
            .await
            {
                Either4::First(request) => Some(request),
                Either4::Second(request) => Some(request),
                Either4::Third(request) => Some(request),
                Either4::Fourth(()) => {
                    let host_session = HOST_SESSION_ID.load(Ordering::Acquire);
                    send_device_message_direct(
                        tx,
                        device_hello(DEVICE_WIRE_SESSION.load(Ordering::Acquire), host_session),
                        &mut sequence,
                        &mut message_id,
                        host_session != 0,
                    )
                    .await;
                    None
                }
            }
        };
        let Some(request) = request else {
            continue;
        };
        match request {
            DeviceTxRequest::Message {
                encoded,
                uses_session_key,
            } => {
                let fragments = FragmentEncoder::new(
                    encoded.as_slice(),
                    DEVICE_WIRE_SESSION.load(Ordering::Acquire),
                    sequence,
                    message_id,
                );
                let Ok(fragments) = fragments else {
                    continue;
                };
                let count = fragments.fragment_count();
                for packet in fragments {
                    let Some(secure) = secure_device_packet(&packet, uses_session_key) else {
                        continue;
                    };
                    if let Err(error) = tx.send_async(&BROADCAST_ADDRESS, secure.as_bytes()).await {
                        log::debug!("bridge-device: broadcast send failed: {:?}", error);
                    }
                }
                sequence = sequence.wrapping_add(count as u32);
                message_id = message_id.wrapping_add(1);
            }
        }
    }
}

fn secure_device_packet(
    packet: &WirePacket,
    uses_session_key: bool,
) -> Option<hidshift::espnow_security::SecureEspNowFrame> {
    let view = WirePacket::decode(packet.as_bytes()).ok()?;
    let pairing_key = critical_section::with(|cs| *ESPNOW_PAIRING_KEY.borrow(cs).borrow());
    let key = if uses_session_key {
        let host_session = HOST_SESSION_ID.load(Ordering::Acquire);
        if host_session == 0 {
            return None;
        }
        hidshift::espnow_security::derive_session_key(
            &pairing_key,
            host_session,
            DEVICE_WIRE_SESSION.load(Ordering::Acquire),
            hidshift::espnow_pairing::EspNowRole::UsbDevice,
        )
    } else {
        pairing_key
    };
    hidshift::espnow_security::SecureEspNowFrame::seal(
        &key,
        hidshift::espnow_pairing::EspNowRole::UsbDevice,
        view.session,
        view.sequence,
        packet.as_bytes(),
    )
    .ok()
}
