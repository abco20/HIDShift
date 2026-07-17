#![no_std]
#![no_main]

#[cfg(any(
    all(feature = "esp32", feature = "esp32s3"),
    not(any(feature = "esp32", feature = "esp32s3"))
))]
compile_error!("select exactly one probe chip feature: esp32 or esp32s3");

extern crate alloc;

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_futures::join::join3;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, UartRx};
use esp_radio::ble::controller::BleConnector;
use static_cell::StaticCell;
use trouble_host::prelude::*;

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2;
const HID_SERVICE_UUID: Uuid = Uuid::new_short(0x1812);
const REPORT_UUID: Uuid = Uuid::new_short(0x2a4d);

static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
static NOTIFICATION_SEQUENCE: AtomicU32 = AtomicU32::new(0);

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    #[cfg(feature = "esp32")]
    let telemetry_pin = peripherals.GPIO3;
    #[cfg(feature = "esp32s3")]
    let telemetry_pin = peripherals.GPIO44;
    let telemetry_rx = match UartRx::new(peripherals.UART0, UartConfig::default()) {
        Ok(rx) => rx.with_rx(telemetry_pin).into_async(),
        Err(_) => esp_hal::system::software_reset(),
    };
    let timer = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timer.timer0, software_interrupts.software_interrupt0);

    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run(move |spawner| {
        match telemetry_task(telemetry_rx) {
            Ok(token) => spawner.spawn(token),
            Err(error) => {
                log::error!("@HIDSHIFT-PROBE:ERROR,telemetry-spawn,{:?}", error);
                esp_hal::system::software_reset();
            }
        }
        match probe_task(peripherals.BT, peripherals.RNG, peripherals.ADC1) {
            Ok(token) => spawner.spawn(token),
            Err(error) => {
                log::error!("@HIDSHIFT-PROBE:ERROR,spawn,{:?}", error);
                esp_hal::system::software_reset();
            }
        }
    })
}

#[embassy_executor::task]
async fn telemetry_task(mut rx: UartRx<'static, Async>) {
    let mut line = [0u8; 16];
    let mut len = 0usize;
    let mut byte = [0u8; 1];
    loop {
        match rx.read_async(&mut byte).await {
            Ok(1) if matches!(byte[0], b'\r' | b'\n') => {
                if let Some(sequence) = decode_clock_request(&line[..len]) {
                    let now_us = Instant::now().as_micros();
                    esp_println::println!("@T:{:08x}:{:016x}", sequence, now_us);
                }
                len = 0;
            }
            Ok(1) if len < line.len() => {
                line[len] = byte[0];
                len += 1;
            }
            Ok(1) => len = 0,
            Ok(_) => {}
            Err(_) => len = 0,
        }
    }
}

fn decode_clock_request(line: &[u8]) -> Option<u32> {
    if line.len() != 11 || !line.starts_with(b"@T:") {
        return None;
    }
    let mut value = 0u32;
    for byte in &line[3..] {
        value = (value << 4) | u32::from(hex_nibble(*byte)?);
    }
    Some(value)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[embassy_executor::task]
async fn probe_task(
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
) {
    let _trng_source = TrngSource::new(rng, adc1);
    let mut trng = match Trng::try_new() {
        Ok(trng) => trng,
        Err(error) => {
            log::error!("@HIDSHIFT-PROBE:ERROR,trng,{:?}", error);
            return;
        }
    };

    let connector = match BleConnector::new(bt, Default::default()) {
        Ok(connector) => connector,
        Err(error) => {
            log::error!("@HIDSHIFT-PROBE:ERROR,controller,{:?}", error);
            return;
        }
    };
    let controller: ExternalController<_, 20> = ExternalController::new(connector);
    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(Address::random(hidshift::e2e::E2E_PROBE_BLE_ADDRESS_RAW))
        .set_random_generator_seed(&mut trng);
    stack.set_io_capabilities(IoCapabilities::NoInputNoOutput);
    let Host {
        mut central,
        mut runner,
        ..
    } = stack.build();

    log::info!("@HIDSHIFT-PROBE:READY,1");
    let workflow = async {
        let dut_address = match dut_address() {
            Some(address) => address,
            None => {
                log::error!("@HIDSHIFT-PROBE:ERROR,dut-address");
                return;
            }
        };
        let filter = [(AddrKind::PUBLIC, &dut_address)];
        log::info!("@HIDSHIFT-PROBE:TARGET,{:?}", dut_address);
        loop {
            log::info!("@HIDSHIFT-PROBE:SCANNING");
            let config = ConnectConfig {
                connect_params: RequestedConnParams {
                    min_connection_interval: Duration::from_micros(7_500),
                    max_connection_interval: Duration::from_micros(7_500),
                    supervision_timeout: Duration::from_secs(2),
                    ..Default::default()
                },
                scan_config: ScanConfig {
                    active: true,
                    filter_accept_list: &filter,
                    interval: Duration::from_millis(60),
                    window: Duration::from_millis(60),
                    ..Default::default()
                },
            };
            let connection = match central.connect(&config).await {
                Ok(connection) => connection,
                Err(error) => {
                    log::warn!("@HIDSHIFT-PROBE:CONNECT-ERROR,{:?}", error);
                    Timer::after_millis(500).await;
                    continue;
                }
            };
            log::info!(
                "@HIDSHIFT-PROBE:CONNECTED,{:?},{}",
                connection.peer_address(),
                Instant::now().as_micros()
            );
            log::info!("@HIDSHIFT-PROBE:PARAMS,{:?}", connection.params());
            let _ = connection.set_bondable(true);
            let _ = connection.request_security();

            let secured =
                match with_timeout(Duration::from_secs(10), wait_for_security(&connection)).await {
                    Ok(Some(bond)) => {
                        if let Some(bond) = bond
                            && let Err(error) = stack.add_bond_information(bond)
                        {
                            log::warn!("@HIDSHIFT-PROBE:BOND-ERROR,{:?}", error);
                        }
                        log::info!("@HIDSHIFT-PROBE:ENCRYPTED");
                        true
                    }
                    Ok(None) => {
                        log::warn!("@HIDSHIFT-PROBE:PAIRING-FAILED");
                        false
                    }
                    Err(_) => {
                        log::warn!("@HIDSHIFT-PROBE:PAIRING-TIMEOUT");
                        false
                    }
                };
            if !secured {
                connection.disconnect();
                Timer::after_millis(250).await;
                continue;
            }

            // The DUT intentionally reconnects once after creating a fresh
            // bond to release trouble-host's global pairing state. Let the
            // disconnect event settle before starting GATT discovery; doing
            // so on an already closed link can otherwise wait indefinitely.
            Timer::after_millis(250).await;
            if !connection.is_connected() {
                log::info!("@HIDSHIFT-PROBE:RECONNECT-AFTER-BOND");
                // Give the DUT time to persist the critical bond update and
                // rebuild its BLE stack before reconnecting with that bond.
                Timer::after_secs(5).await;
                continue;
            }

            match run_gatt_probe(&stack, &connection).await {
                Ok(()) => log::warn!("@HIDSHIFT-PROBE:DISCONNECTED"),
                Err(error) => log::warn!("@HIDSHIFT-PROBE:GATT-ERROR,{:?}", error),
            }
            connection.disconnect();
            Timer::after_millis(250).await;
        }
    };

    match select(runner.run(), workflow).await {
        Either::First(error) => log::error!("@HIDSHIFT-PROBE:ERROR,runner,{:?}", error),
        Either::Second(()) => log::error!("@HIDSHIFT-PROBE:ERROR,workflow-stopped"),
    }
}

fn dut_address() -> Option<BdAddr> {
    let value = option_env!("HIDSHIFT_DUT_ADDRESS")?;
    let mut visible = [0u8; 6];
    let mut parts = value.split(':');
    for byte in &mut visible {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    visible.reverse();
    Some(BdAddr::new(visible))
}

async fn wait_for_security<P: PacketPool>(
    connection: &Connection<'_, P>,
) -> Option<Option<BondInformation>> {
    loop {
        match connection.next().await {
            ConnectionEvent::PairingComplete {
                security_level,
                bond,
            } => {
                return security_level.encrypted().then_some(bond);
            }
            ConnectionEvent::PairingFailed(_) | ConnectionEvent::Disconnected { .. } => {
                return None;
            }
            _ => {}
        }
    }
}

async fn run_gatt_probe<'a, C, P>(
    stack: &'a Stack<'a, C, P>,
    connection: &Connection<'a, P>,
) -> Result<(), BleHostError<C::Error>>
where
    C: Controller,
    P: PacketPool,
{
    let client = GattClient::<C, P, 8>::new(stack, connection).await?;
    match select3(
        client.task(),
        collect_notifications(&client),
        wait_for_disconnect(connection),
    )
    .await
    {
        Either3::First(result) => result,
        Either3::Second(result) => result,
        Either3::Third(()) => Ok(()),
    }
}

async fn wait_for_disconnect<P: PacketPool>(connection: &Connection<'_, P>) {
    loop {
        if let ConnectionEvent::Disconnected { .. } = connection.next().await {
            return;
        }
    }
}

async fn collect_notifications<C, P>(
    client: &GattClient<'_, C, P, 8>,
) -> Result<(), BleHostError<C::Error>>
where
    C: Controller,
    P: PacketPool,
{
    let services = client.services_by_uuid(&HID_SERVICE_UUID).await?;
    let (keyboard_listener, mouse_listener, consumer_listener) = match services.as_slice() {
        [combined] => {
            let characteristics = client.characteristics::<8>(combined).await?;
            let mut inputs = characteristics
                .iter()
                .filter(|characteristic| characteristic.props.has_cccd());
            let keyboard = inputs.next().ok_or(trouble_host::Error::NotFound)?;
            let mouse = inputs.next().ok_or(trouble_host::Error::NotFound)?;
            let consumer = inputs.next().ok_or(trouble_host::Error::NotFound)?;
            if inputs.next().is_some() {
                return Err(trouble_host::Error::InsufficientSpace.into());
            }
            (
                client.subscribe(keyboard, false).await?,
                client.subscribe(mouse, false).await?,
                client.subscribe(consumer, false).await?,
            )
        }
        [keyboard_service, mouse_service, consumer_service] => {
            let keyboard = client
                .characteristic_by_uuid::<[u8; 8]>(keyboard_service, &REPORT_UUID)
                .await?;
            let mouse = client
                .characteristic_by_uuid::<[u8; 5]>(mouse_service, &REPORT_UUID)
                .await?;
            let consumer = client
                .characteristic_by_uuid::<[u8; 2]>(consumer_service, &REPORT_UUID)
                .await?;
            (
                client.subscribe(&keyboard, false).await?,
                client.subscribe(&mouse, false).await?,
                client.subscribe(&consumer, false).await?,
            )
        }
        _ => {
            log::warn!("@HIDSHIFT-PROBE:SERVICE-COUNT,{}", services.len());
            return Err(trouble_host::Error::NotFound.into());
        }
    };
    log::info!("@HIDSHIFT-PROBE:SUBSCRIBED,keyboard,mouse,consumer");

    let _ = join3(
        relay_notifications("keyboard", keyboard_listener),
        relay_notifications("mouse", mouse_listener),
        relay_notifications("consumer", consumer_listener),
    )
    .await;
    Ok(())
}

async fn relay_notifications(kind: &'static str, mut listener: NotificationListener<'_, 512>) {
    loop {
        let notification = listener.next().await;
        let bytes = notification.as_ref();
        let now_us = Instant::now().as_micros();
        let sequence = NOTIFICATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        match (kind, bytes) {
            ("keyboard", [a, b, c, d, e, f, g, h]) => esp_println::println!(
                "@N:{:08x}:k:{:016x}:{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                sequence,
                now_us,
                a,
                b,
                c,
                d,
                e,
                f,
                g,
                h
            ),
            ("mouse", [a, b, c, d, e]) => esp_println::println!(
                "@N:{:08x}:m:{:016x}:{:02x}{:02x}{:02x}{:02x}{:02x}",
                sequence,
                now_us,
                a,
                b,
                c,
                d,
                e
            ),
            ("consumer", [a, b]) => {
                esp_println::println!("@N:{:08x}:c:{:016x}:{:02x}{:02x}", sequence, now_us, a, b)
            }
            _ => log::warn!("@HIDSHIFT-PROBE:BAD-NOTIFY,{},{}", kind, bytes.len()),
        }
    }
}
