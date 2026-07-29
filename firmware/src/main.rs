#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use embassy_executor::{SpawnError, SpawnToken, Spawner};
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender, TrySendError};
use esp_backtrace as _;
#[cfg(feature = "hardware-e2e")]
mod e2e_telemetry;
mod platform;
mod wired_management;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
#[cfg(not(feature = "dual-s3-wired"))]
use esp_hal::system::Stack;
use esp_hal::timer::timg::{MwdtStage, TimerGroup, Wdt};
use esp32s3_platform::ble_hid_task::BleRuntimeSnapshot;
use esp32s3_platform::ble_hid_task::active_ble_connections;
use esp32s3_platform::button_task::control_task;
use esp32s3_platform::storage_task::storage_command_task;
use esp32s3_platform::usb_host_task::usb_input_task;
use hidshift::DefaultRuntimeOwner;
use hidshift::mouse_accumulator::MouseReportAccumulator;
use hidshift::runtime::RUNTIME_HOSTS_MAX;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    BleCommandLane, BleTaskCommand, RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY, RUNTIME_INPUT_QUEUE_CAPACITY,
    RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY, RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
    RUNTIME_USB_COMMAND_QUEUE_CAPACITY, RuntimeDiagnosticsEvent, StatusTaskCommand,
    StorageTaskCommand, UsbHostTaskCommand,
};
#[cfg(feature = "dual-s3-wired")]
use hidshift::runtime::{DeviceTaskCommand, RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY};
use hidshift::storage::StorageState;
use platform as esp32s3_platform;
use static_cell::{ConstStaticCell, StaticCell};

esp_bootloader_esp_idf::esp_app_desc!();

static RUNTIME_INPUT_CHANNEL: Channel<
    CriticalSectionRawMutex,
    RuntimeInputMessage,
    RUNTIME_INPUT_QUEUE_CAPACITY,
> = Channel::new();
static RUNTIME_TICK_PENDING: hidshift::runtime::RuntimeTickPending =
    hidshift::runtime::RuntimeTickPending::new();
static RUNTIME_HEARTBEAT: AtomicU32 = AtomicU32::new(0);
static BLE_EXECUTOR_HEARTBEAT: AtomicU32 = AtomicU32::new(0);
static RUNTIME_QUIESCED: AtomicBool = AtomicBool::new(false);
static BLE_CONTROL_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    BleTaskCommand,
    RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
> = Channel::new();
static BLE_NOTIFY_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    BleTaskCommand,
    RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY,
> = Channel::new();
static USB_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    UsbHostTaskCommand,
    RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
> = Channel::new();
#[cfg(feature = "dual-s3-wired")]
static DEVICE_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    DeviceTaskCommand,
    RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY,
> = Channel::new();
static STORAGE_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    StorageTaskCommand,
    RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
> = Channel::new();
static BLE_RESTORE_CHANNEL: Channel<CriticalSectionRawMutex, Option<StorageState>, 1> =
    Channel::new();
static BLE_QUIESCE_REQUEST_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static BLE_QUIESCE_READY_CHANNEL: Channel<CriticalSectionRawMutex, Option<StorageState>, 1> =
    Channel::new();
static BLE_QUIESCE_DONE_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static USB_BLE_QUIESCE_REQUEST_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static USB_BLE_QUIESCE_READY_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static USB_BLE_QUIESCE_DONE_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static BLE_RUNTIME_BARRIER_REQUEST_CHANNEL: Channel<CriticalSectionRawMutex, usize, 1> =
    Channel::new();
static BLE_RUNTIME_BARRIER_DONE_CHANNEL: Channel<CriticalSectionRawMutex, BleRuntimeSnapshot, 1> =
    Channel::new();
static BLE_RUNTIME_BARRIER_RESUME_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static STATUS_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    StatusTaskCommand,
    RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
> = Channel::new();
static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
#[cfg(not(feature = "dual-s3-wired"))]
const BLE_CORE_STACK_SIZE: usize = 48 * 1024;
#[cfg(not(feature = "dual-s3-wired"))]
static BLE_EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
#[cfg(not(feature = "dual-s3-wired"))]
static BLE_CORE_STACK: StaticCell<Stack<BLE_CORE_STACK_SIZE>> = StaticCell::new();
static CHANNEL_TASK_SINK: StaticCell<ChannelTaskSink> = StaticCell::new();
static PENDING_USB_COMMANDS: ConstStaticCell<
    [Option<UsbHostTaskCommand>; RUNTIME_USB_COMMAND_QUEUE_CAPACITY],
> = ConstStaticCell::new([None; RUNTIME_USB_COMMAND_QUEUE_CAPACITY]);
static RUNTIME_OWNER_STORAGE: ConstStaticCell<DefaultRuntimeOwner> =
    ConstStaticCell::new(DefaultRuntimeOwner::new(0));

#[cfg(feature = "dual-s3-wired")]
struct BleTaskResources {
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
}

fn spawn_or_reset<S>(
    spawner: &Spawner,
    task: Result<SpawnToken<S>, SpawnError>,
    task_name: &'static str,
) {
    match task {
        Ok(token) => spawner.spawn(token),
        Err(error) => {
            log::error!(
                "firmware: failed to create task name={} error={:?}; resetting",
                task_name,
                error
            );
            esp_hal::system::software_reset();
        }
    }
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    esp_alloc::heap_allocator!(size: 72 * 1024);

    let reset_reason = esp_hal::system::reset_reason();
    let reset_reason_code = reset_reason.map_or(0, |reason| reason as u8);
    let was_brownout = matches!(
        reset_reason,
        Some(esp_hal::rtc_cntl::SocResetReason::SysBrownOut)
    );
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let boot_session_id = esp_hal::rng::Rng::new().random();
    #[cfg(feature = "hardware-e2e")]
    log::info!(
        "@HIDSHIFT-BRIDGE:BOOT,{},{},{}",
        boot_session_id,
        reset_reason_code,
        u8::from(was_brownout)
    );
    run_firmware(
        peripherals,
        reset_reason_code,
        was_brownout,
        boot_session_id,
    )
}

#[inline(never)]
fn run_firmware(
    peripherals: esp_hal::peripherals::Peripherals,
    reset_reason_code: u8,
    was_brownout: bool,
    boot_session_id: u32,
) -> ! {
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let watchdog = timg0.wdt;
    let sw_ints = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let scheduler_interrupt = sw_ints.software_interrupt0;
    #[cfg(not(feature = "dual-s3-wired"))]
    let ble_core_interrupt = sw_ints.software_interrupt1;
    #[cfg(not(feature = "dual-s3-wired"))]
    let cpu_control = peripherals.CPU_CTRL;
    let (bt, rng, adc1) = (peripherals.BT, peripherals.RNG, peripherals.ADC1);
    esp_rtos::start(timg0.timer0, scheduler_interrupt);
    // The one-board image reserves the second core for the routing owner and
    // BLE delivery. The dual-S3 image keeps USB Host, fixed-rate SPI, routing,
    // and BLE on one explicit executor; both topologies spawn the same tasks
    // through `spawn_runtime_and_ble`.
    #[cfg(not(feature = "dual-s3-wired"))]
    start_ble_core(cpu_control, ble_core_interrupt, bt, rng, adc1);
    #[cfg(feature = "dual-s3-wired")]
    let ble_resources = BleTaskResources { bt, rng, adc1 };

    let gpio0 = peripherals.GPIO0;
    let uart0 = peripherals.UART0;
    let gpio44 = peripherals.GPIO44;
    let usb0 = peripherals.USB0;
    let gpio20 = peripherals.GPIO20;
    let gpio19 = peripherals.GPIO19;
    #[cfg(feature = "dual-s3-wired")]
    let mirror_spi = (
        peripherals.SPI2,
        peripherals.DMA_CH0,
        peripherals.GPIO10,
        peripherals.GPIO11,
        peripherals.GPIO12,
        peripherals.GPIO13,
    );
    let flash = peripherals.FLASH;

    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run(|spawner| {
        spawn_or_reset(
            &spawner,
            startup_task(
                spawner,
                reset_reason_code,
                was_brownout,
                gpio0,
                uart0,
                gpio44,
                usb0,
                gpio20,
                gpio19,
                boot_session_id,
                flash,
                watchdog,
                #[cfg(feature = "dual-s3-wired")]
                ble_resources,
                #[cfg(feature = "dual-s3-wired")]
                mirror_spi,
            ),
            "startup",
        );
    })
}

fn init_channel_task_sink() -> &'static mut ChannelTaskSink {
    let pending_usb = PENDING_USB_COMMANDS.take();
    CHANNEL_TASK_SINK.init_with(|| ChannelTaskSink {
        ble_control: BLE_CONTROL_COMMAND_CHANNEL.sender(),
        ble_notify: BLE_NOTIFY_COMMAND_CHANNEL.sender(),
        usb: USB_COMMAND_CHANNEL.sender(),
        #[cfg(feature = "dual-s3-wired")]
        device: DEVICE_COMMAND_CHANNEL.sender(),
        storage: STORAGE_COMMAND_CHANNEL.sender(),
        status: STATUS_COMMAND_CHANNEL.sender(),
        mouse: MouseReportAccumulator::new(),
        pending_usb,
        pending_status: None,
        status_updates_dropped: 0,
    })
}

fn spawn_runtime_and_ble(
    spawner: &Spawner,
    sink: &'static mut ChannelTaskSink,
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
) {
    spawn_or_reset(
        spawner,
        ble_executor_heartbeat_task(),
        "ble-executor-heartbeat",
    );
    spawn_or_reset(
        spawner,
        runtime_owner_task(
            RUNTIME_INPUT_CHANNEL.receiver(),
            &RUNTIME_TICK_PENDING,
            BLE_RUNTIME_BARRIER_REQUEST_CHANNEL.receiver(),
            BLE_RUNTIME_BARRIER_DONE_CHANNEL.sender(),
            BLE_RUNTIME_BARRIER_RESUME_CHANNEL.receiver(),
            sink,
        ),
        "runtime-owner",
    );
    spawn_or_reset(
        spawner,
        esp32s3_platform::ble_hid_task::ble_host_event_task(
            RUNTIME_INPUT_CHANNEL.sender(),
            BLE_CONTROL_COMMAND_CHANNEL.receiver(),
            BLE_NOTIFY_COMMAND_CHANNEL.receiver(),
            BLE_RESTORE_CHANNEL.receiver(),
            BLE_QUIESCE_REQUEST_CHANNEL.receiver(),
            BLE_QUIESCE_READY_CHANNEL.sender(),
            BLE_QUIESCE_DONE_CHANNEL.receiver(),
            USB_BLE_QUIESCE_REQUEST_CHANNEL.receiver(),
            USB_BLE_QUIESCE_READY_CHANNEL.sender(),
            USB_BLE_QUIESCE_DONE_CHANNEL.receiver(),
            BLE_RUNTIME_BARRIER_REQUEST_CHANNEL.sender(),
            BLE_RUNTIME_BARRIER_DONE_CHANNEL.receiver(),
            BLE_RUNTIME_BARRIER_RESUME_CHANNEL.sender(),
            bt,
            rng,
            adc1,
        ),
        "ble-host-event",
    );
}

#[embassy_executor::task]
async fn ble_executor_heartbeat_task() {
    loop {
        BLE_EXECUTOR_HEARTBEAT.fetch_add(1, Ordering::Relaxed);
        embassy_time::Timer::after_millis(500).await;
    }
}

#[cfg(not(feature = "dual-s3-wired"))]
#[inline(never)]
fn start_ble_core(
    cpu_control: esp_hal::peripherals::CPU_CTRL<'static>,
    ble_core_interrupt: esp_hal::interrupt::software::SoftwareInterrupt<'static, 1>,
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
) {
    let sink = init_channel_task_sink();
    let ble_stack = BLE_CORE_STACK.init_with(Stack::new);
    esp_rtos::start_second_core(cpu_control, ble_core_interrupt, ble_stack, move || {
        let executor = BLE_EXECUTOR.init(esp_rtos::embassy::Executor::new());
        executor.run(|spawner| {
            spawn_runtime_and_ble(&spawner, sink, bt, rng, adc1);
        })
    });
}

#[embassy_executor::task]
async fn startup_task(
    spawner: Spawner,
    reset_reason_code: u8,
    was_brownout: bool,
    gpio0: esp_hal::peripherals::GPIO0<'static>,
    uart0: esp_hal::peripherals::UART0<'static>,
    gpio44: esp_hal::peripherals::GPIO44<'static>,
    usb0: esp_hal::peripherals::USB0<'static>,
    gpio20: esp_hal::peripherals::GPIO20<'static>,
    gpio19: esp_hal::peripherals::GPIO19<'static>,
    boot_session_id: u32,
    flash: esp_hal::peripherals::FLASH<'static>,
    watchdog: Wdt<esp_hal::peripherals::TIMG0<'static>>,
    #[cfg(feature = "dual-s3-wired")] ble_resources: BleTaskResources,
    #[cfg(feature = "dual-s3-wired")] mirror_spi: (
        esp_hal::peripherals::SPI2<'static>,
        esp_hal::peripherals::DMA_CH0<'static>,
        esp_hal::peripherals::GPIO10<'static>,
        esp_hal::peripherals::GPIO11<'static>,
        esp_hal::peripherals::GPIO12<'static>,
        esp_hal::peripherals::GPIO13<'static>,
    ),
) {
    spawn_or_reset(&spawner, watchdog_task(watchdog), "watchdog");
    #[cfg(feature = "dual-s3-wired")]
    {
        let sink = init_channel_task_sink();
        spawn_runtime_and_ble(
            &spawner,
            sink,
            ble_resources.bt,
            ble_resources.rng,
            ble_resources.adc1,
        );
    }
    let storage_sender = RUNTIME_INPUT_CHANNEL.sender();
    let usb_input_sender = RUNTIME_INPUT_CHANNEL.sender();
    let usb_receiver = USB_COMMAND_CHANNEL.receiver();
    #[cfg(feature = "dual-s3-wired")]
    let device_receiver = DEVICE_COMMAND_CHANNEL.receiver();
    let _ = RUNTIME_INPUT_CHANNEL.try_send(RuntimeInputMessage::DiagnosticsEvent(
        RuntimeDiagnosticsEvent::ResetReason(reset_reason_code),
    ));
    let _ = RUNTIME_INPUT_CHANNEL.try_send(RuntimeInputMessage::StorageHealthChanged(
        hidshift::storage::StorageHealth::Initializing,
    ));
    if was_brownout {
        let _ = RUNTIME_INPUT_CHANNEL.try_send(RuntimeInputMessage::DiagnosticsEvent(
            RuntimeDiagnosticsEvent::Brownout,
        ));
    }
    spawn_or_reset(
        &spawner,
        control_task(
            RUNTIME_INPUT_CHANNEL.sender(),
            STORAGE_COMMAND_CHANNEL.sender(),
            &RUNTIME_TICK_PENDING,
            gpio0,
        ),
        "control",
    );
    spawn_or_reset(
        &spawner,
        esp32s3_platform::serial_management_task::serial_management_task(
            RUNTIME_INPUT_CHANNEL.sender(),
            #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
            DEVICE_COMMAND_CHANNEL.sender(),
            uart0,
            gpio44,
            boot_session_id,
        ),
        "serial-management",
    );
    spawn_or_reset(
        &spawner,
        usb_input_bootstrap(
            spawner,
            usb_input_sender,
            usb_receiver,
            usb0,
            gpio20,
            gpio19,
            USB_BLE_QUIESCE_REQUEST_CHANNEL.sender(),
            USB_BLE_QUIESCE_READY_CHANNEL.receiver(),
            USB_BLE_QUIESCE_DONE_CHANNEL.sender(),
        ),
        "usb-input-bootstrap",
    );
    spawn_or_reset(
        &spawner,
        storage_command_task(
            STORAGE_COMMAND_CHANNEL.receiver(),
            storage_sender,
            BLE_RESTORE_CHANNEL.sender(),
            BLE_QUIESCE_REQUEST_CHANNEL.sender(),
            BLE_QUIESCE_READY_CHANNEL.receiver(),
            BLE_QUIESCE_DONE_CHANNEL.sender(),
            active_ble_connections,
            flash,
        ),
        "storage-command",
    );
    spawn_or_reset(
        &spawner,
        status_command_task(
            STATUS_COMMAND_CHANNEL.receiver(),
            BLE_CONTROL_COMMAND_CHANNEL.sender(),
        ),
        "status-command",
    );
    #[cfg(feature = "dual-s3-wired")]
    spawn_or_reset(
        &spawner,
        esp32s3_platform::mirror_spi_task::mirror_spi_master_task(
            device_receiver,
            RUNTIME_INPUT_CHANNEL.sender(),
            boot_session_id,
            mirror_spi.0,
            mirror_spi.1,
            mirror_spi.2,
            mirror_spi.3,
            mirror_spi.4,
            mirror_spi.5,
        ),
        "mirror-spi-master",
    );
    core::future::pending::<()>().await;
}

#[embassy_executor::task]
async fn usb_input_bootstrap(
    spawner: Spawner,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        UsbHostTaskCommand,
        RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    >,
    usb0: esp_hal::peripherals::USB0<'static>,
    usb_dp: esp_hal::peripherals::GPIO20<'static>,
    usb_dm: esp_hal::peripherals::GPIO19<'static>,
    ble_quiesce_request: Sender<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_ready: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
) {
    spawn_or_reset(
        &spawner,
        usb_input_task(
            sender,
            receiver,
            usb0,
            usb_dp,
            usb_dm,
            ble_quiesce_request,
            ble_quiesce_ready,
            ble_quiesce_done,
        ),
        "usb-input",
    );
    core::future::pending::<()>().await;
}

#[embassy_executor::task]
async fn runtime_owner_task(
    receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    tick_pending: &'static hidshift::runtime::RuntimeTickPending,
    barrier_request: Receiver<'static, CriticalSectionRawMutex, usize, 1>,
    barrier_done: Sender<'static, CriticalSectionRawMutex, BleRuntimeSnapshot, 1>,
    barrier_resume: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    mut sink: &'static mut ChannelTaskSink,
) {
    let mut owner = RUNTIME_OWNER_STORAGE.take();

    log::info!("firmware: runtime owner task boot");

    loop {
        owner.observe_transport_metrics(hidshift::runtime::RuntimeTransportMetrics {
            runtime_input_depth: receiver.len(),
            ble_control_depth: sink.ble_control.len(),
            ble_notify_depth: sink.ble_notify.len(),
            usb_depth: sink.usb.len(),
            storage_depth: sink.storage.len(),
            status_depth: sink.status.len(),
            mouse: sink.mouse.stats(),
            status_updates_dropped: sink.status_updates_dropped,
        });
        let message = match select(receiver.receive(), barrier_request.receive()).await {
            Either::First(message) => message,
            Either::Second(active_host_mask) => {
                RUNTIME_QUIESCED.store(true, Ordering::Release);
                if let Err(error) = owner.prepare_for_quiesce() {
                    log::error!("firmware: runtime quiesce preparation failed {:?}", error);
                }
                sink.discard_transient_input();
                for host_index in 0..RUNTIME_HOSTS_MAX {
                    if active_host_mask & (1usize << host_index) != 0 {
                        owner.mark_host_disconnected_for_quiesce(hidshift::HostId(
                            (host_index + 1) as u8,
                        ));
                    }
                }
                let runtime = owner.runtime();
                let storage = match runtime.storage_state() {
                    Ok(storage) => Some(storage),
                    Err(error) => {
                        log::error!("firmware: runtime barrier snapshot failed {:?}", error);
                        None
                    }
                };
                barrier_done
                    .send(BleRuntimeSnapshot {
                        storage,
                        pairable_host: runtime.pairing_mode().map(|state| state.host_id),
                    })
                    .await;
                barrier_resume.receive().await;
                RUNTIME_QUIESCED.store(false, Ordering::Release);
                RUNTIME_HEARTBEAT.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        if matches!(message, RuntimeInputMessage::Tick { .. }) {
            tick_pending.mark_processed();
        }
        process_runtime_message(&mut owner, &mut sink, message).await;
        RUNTIME_HEARTBEAT.fetch_add(1, Ordering::Relaxed);
    }
}

#[embassy_executor::task]
async fn watchdog_task(mut watchdog: Wdt<esp_hal::peripherals::TIMG0<'static>>) {
    const WATCHDOG_TIMEOUT_SECONDS: u64 = 5;
    const HEARTBEAT_SAMPLE_SECONDS: u64 = 1;
    const ALLOWED_MISSED_INTERVALS: u8 = 2;

    watchdog.set_timeout(
        MwdtStage::Stage0,
        esp_hal::time::Duration::from_secs(WATCHDOG_TIMEOUT_SECONDS),
    );
    watchdog.enable();
    watchdog.feed();
    let mut heartbeat_group = hidshift::support::HeartbeatGroup::new(
        [
            RUNTIME_HEARTBEAT.load(Ordering::Relaxed),
            BLE_EXECUTOR_HEARTBEAT.load(Ordering::Relaxed),
        ],
        ALLOWED_MISSED_INTERVALS,
    );
    let mut minimum_heap_free = esp_alloc::HEAP.free();
    let mut heap_log_interval = 0u8;
    let mut quiesced_intervals = 0u8;
    log::info!(
        "firmware: watchdog enabled timeout_s={}",
        WATCHDOG_TIMEOUT_SECONDS
    );

    loop {
        embassy_time::Timer::after_secs(HEARTBEAT_SAMPLE_SECONDS).await;
        let heartbeat = RUNTIME_HEARTBEAT.load(Ordering::Relaxed);
        let ble_executor_heartbeat = BLE_EXECUTOR_HEARTBEAT.load(Ordering::Relaxed);
        let quiesced = RUNTIME_QUIESCED.load(Ordering::Acquire);
        if quiesced {
            quiesced_intervals = quiesced_intervals.saturating_add(1);
        } else {
            quiesced_intervals = 0;
        }
        let task_heartbeats_healthy = heartbeat_group
            .should_feed_watchdog([heartbeat, ble_executor_heartbeat], [!quiesced, true]);
        let healthy = task_heartbeats_healthy && (!quiesced || quiesced_intervals <= 31);
        if healthy {
            watchdog.feed();
            minimum_heap_free = minimum_heap_free.min(esp_alloc::HEAP.free());
            heap_log_interval = heap_log_interval.saturating_add(1);
            if heap_log_interval >= 30 {
                heap_log_interval = 0;
                log::info!(
                    "firmware: heap free={} minimum_free={}",
                    esp_alloc::HEAP.free(),
                    minimum_heap_free
                );
            }
        } else {
            log::error!(
                "firmware: task heartbeat stalled runtime={} ble_executor={}; watchdog reset pending",
                heartbeat,
                ble_executor_heartbeat
            );
            core::future::pending::<()>().await;
        }
    }
}

async fn process_runtime_message(
    owner: &mut DefaultRuntimeOwner,
    sink: &mut ChannelTaskSink,
    message: RuntimeInputMessage,
) {
    #[cfg(feature = "hardware-e2e")]
    if matches!(
        message,
        RuntimeInputMessage::BridgeEvent(hidshift::BridgeEvent::InputFrame(_))
    ) {
        crate::e2e_telemetry::record_runtime(embassy_time::Instant::now().as_micros());
    }
    log::trace!("firmware: runtime_input {:?}", message);

    // Input frames are internally transactional while their outbox is built.
    // After that latest-state input stays committed even if its realtime
    // delivery is dropped: the next broadcast snapshot heals the receiver.
    // Management inputs retain a full rollback checkpoint.
    let checkpoint = owner.checkpoint_for_message(&message);
    if let Err(error) = owner.process_message_in_place(&message) {
        owner.rollback_message(checkpoint);
        log::error!("firmware: runtime owner error {:?}", error);
        return;
    }

    #[cfg(feature = "hardware-e2e")]
    if matches!(
        message,
        RuntimeInputMessage::BridgeEvent(hidshift::BridgeEvent::InputFrame(_))
    ) {
        crate::e2e_telemetry::record_runtime_dispatch(embassy_time::Instant::now().as_micros());
    }

    if let Err(error) = sink.dispatch_runtime_queues(owner.default_queues()).await {
        owner.rollback_message(checkpoint);
        log::error!("firmware: runtime drive error {:?}", error);
        return;
    }
    for effect in owner.default_queues().effects.iter().copied() {
        apply_runtime_effect(effect);
    }
}

#[embassy_executor::task]
async fn status_command_task(
    receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        StatusTaskCommand,
        RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
    >,
    ble_sender: Sender<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    >,
) {
    log::info!("firmware: status command task boot");
    loop {
        let command = receiver.receive().await;
        if let Some(management) = command.management {
            match management.destination {
                hidshift::ManagementDestination::Wired => {
                    print_wired_management_response(management.response);
                }
                hidshift::ManagementDestination::Ble(host_id) => {
                    ble_sender
                        .send(BleTaskCommand::ManagementResponse {
                            host_id,
                            response: management.response,
                        })
                        .await;
                }
            }
        } else {
            log::debug!("firmware: status_command {:?}", command);
        }
    }
}

fn print_wired_management_response(response: hidshift::ManagementResponse) {
    wired_management::print_response(response);
}

struct ChannelTaskSink {
    ble_control: Sender<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    >,
    ble_notify: Sender<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY,
    >,
    usb: Sender<
        'static,
        CriticalSectionRawMutex,
        UsbHostTaskCommand,
        RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    >,
    #[cfg(feature = "dual-s3-wired")]
    device: Sender<
        'static,
        CriticalSectionRawMutex,
        DeviceTaskCommand,
        RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY,
    >,
    storage: Sender<
        'static,
        CriticalSectionRawMutex,
        StorageTaskCommand,
        RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
    >,
    status: Sender<
        'static,
        CriticalSectionRawMutex,
        StatusTaskCommand,
        RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
    >,
    mouse: MouseReportAccumulator<4>,
    pending_usb: &'static mut [Option<UsbHostTaskCommand>; RUNTIME_USB_COMMAND_QUEUE_CAPACITY],
    pending_status: Option<StatusTaskCommand>,
    status_updates_dropped: u32,
}

impl ChannelTaskSink {
    fn discard_transient_input(&mut self) {
        self.mouse.discard_all();
    }

    async fn dispatch_runtime_queues(
        &mut self,
        queues: &hidshift::DefaultRuntimeCommandQueues,
    ) -> Result<(), ChannelTaskSendError> {
        self.flush_mouse_accumulator();
        self.flush_usb_commands();
        self.flush_status_snapshot();
        self.ensure_capacity(queues)?;
        for command in queues.ble.iter().copied() {
            #[cfg(feature = "hardware-e2e")]
            if matches!(command, BleTaskCommand::Notify { .. }) {
                crate::e2e_telemetry::record_ble_queued(embassy_time::Instant::now().as_micros());
            }
            self.send_ble_with_policy(command).await?;
            if command.class() == hidshift::CommandClass::Realtime {
                // Channel wakeups only mark the BLE task runnable. Yield here
                // before lower-priority USB/storage/status dispatch so the
                // executor can begin the GATT notification immediately.
                embassy_futures::yield_now().await;
            }
        }
        #[cfg(feature = "dual-s3-wired")]
        for command in queues.device.iter().copied() {
            self.device.send(command).await;
        }
        for command in queues.usb_host.iter().copied() {
            self.send_usb_with_policy(command).await?;
        }
        for command in queues.storage.iter().cloned() {
            self.send_storage_with_policy(command).await?;
        }
        for command in queues.status.iter().copied() {
            self.send_status_with_policy(command).await?;
        }
        self.flush_mouse_accumulator();
        self.flush_usb_commands();
        self.flush_status_snapshot();
        Ok(())
    }

    fn ensure_capacity<
        const BLE: usize,
        const USB: usize,
        const STORAGE: usize,
        const STATUS: usize,
    >(
        &self,
        queues: &hidshift::RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>,
    ) -> Result<(), ChannelTaskSendError> {
        let control = queues
            .ble
            .iter()
            .filter(|command| command.lane() == BleCommandLane::Control)
            .count();
        let notify = queues.ble.len() - control;
        let coalesced_mouse = queues
            .ble
            .iter()
            .filter(|command| {
                matches!(
                    command,
                    BleTaskCommand::Notify {
                        report: hidshift::reports::BleHidReport::Mouse(_),
                        reason: hidshift::NotifyReason::Input,
                        ..
                    }
                )
            })
            .count();
        let notify = notify.saturating_sub(coalesced_mouse);
        if self.ble_control.free_capacity() < control || self.ble_notify.free_capacity() < notify {
            return Err(ChannelTaskSendError::BleQueueFull);
        }
        if self.storage.free_capacity() < queues.storage.len() {
            return Err(ChannelTaskSendError::StorageQueueFull);
        }
        #[cfg(feature = "dual-s3-wired")]
        if self.device.free_capacity() < queues.device.len() {
            return Err(ChannelTaskSendError::DeviceQueueFull);
        }
        let required_status = queues
            .status
            .iter()
            .filter(|command| command.class() != hidshift::CommandClass::BestEffort)
            .count();
        if self.status.free_capacity() < required_status {
            return Err(ChannelTaskSendError::StatusQueueFull);
        }
        let mut new_pending = heapless::Vec::<
            (hidshift::InterfaceId, hidshift::DeviceId),
            RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
        >::new();
        let critical_usb = queues
            .usb_host
            .iter()
            .filter(|command| command.class() == hidshift::CommandClass::Critical)
            .count();
        if self.usb.free_capacity() < critical_usb {
            return Err(ChannelTaskSendError::UsbQueueFull);
        }
        for command in queues.usb_host.iter().copied() {
            let Some(key) = command.led_target() else {
                continue;
            };
            if self
                .pending_usb
                .iter()
                .flatten()
                .any(|pending| pending.led_target() == Some(key))
                || new_pending.contains(&key)
            {
                continue;
            }
            let _ = new_pending.push(key);
        }
        if self
            .pending_usb
            .iter()
            .filter(|pending| pending.is_none())
            .count()
            < new_pending.len()
        {
            return Err(ChannelTaskSendError::UsbQueueFull);
        }
        Ok(())
    }

    async fn send_ble_with_policy(
        &mut self,
        command: BleTaskCommand,
    ) -> Result<(), ChannelTaskSendError> {
        if let BleTaskCommand::Notify {
            host_id,
            report: hidshift::reports::BleHidReport::Mouse(report),
            reason: hidshift::NotifyReason::Input,
        } = command
        {
            if self.mouse.push(host_id, report) {
                self.flush_mouse_accumulator();
            } else {
                // A button edge cannot be merged into movement accumulated
                // under the old button state. Drain the old state and place
                // the edge on the same ordered lane; ignoring push(false)
                // here used to drop clicks and subsequent 1px movement in
                // release builds.
                self.flush_mouse_accumulator_ordered(host_id).await;
                let _ = self.mouse.set_buttons(host_id, report.as_bytes()[0]);
                self.ble_control.send(command).await;
            }
            return Ok(());
        }
        if let BleTaskCommand::Notify {
            host_id,
            report: hidshift::reports::BleHidReport::Mouse(report),
            reason: _,
        } = command
        {
            // Drain movement under the old button state through the same
            // ordered lane before publishing the edge/release report.
            self.flush_mouse_accumulator_ordered(host_id).await;
            let _ = self.mouse.set_buttons(host_id, report.as_bytes()[0]);
        }
        match command.lane() {
            BleCommandLane::Control => match command.class() {
                hidshift::CommandClass::Critical => {
                    self.ble_control.send(command).await;
                    Ok(())
                }
                hidshift::CommandClass::Realtime => self
                    .ble_control
                    .try_send(command)
                    .map_err(ChannelTaskSendError::from),
                hidshift::CommandClass::BestEffort => {
                    let _ = self.ble_control.try_send(command);
                    Ok(())
                }
            },
            BleCommandLane::Notify => match command.class() {
                hidshift::CommandClass::Critical => {
                    self.ble_notify.send(command).await;
                    Ok(())
                }
                hidshift::CommandClass::Realtime => self
                    .ble_notify
                    .try_send(command)
                    .map_err(ChannelTaskSendError::from),
                hidshift::CommandClass::BestEffort => {
                    let _ = self.ble_notify.try_send(command);
                    Ok(())
                }
            },
        }
    }

    async fn flush_mouse_accumulator_ordered(&mut self, host_id: hidshift::HostId) {
        while let Some(report) = self.mouse.take_next(host_id) {
            self.ble_control
                .send(BleTaskCommand::Notify {
                    host_id,
                    report: hidshift::reports::BleHidReport::Mouse(report),
                    reason: hidshift::NotifyReason::Input,
                })
                .await;
        }
    }

    fn flush_mouse_accumulator(&mut self) {
        for host in 1..=4 {
            if self.ble_notify.free_capacity() == 0 {
                break;
            }
            let host_id = hidshift::HostId(host);
            let Some(report) = self.mouse.take_next(host_id) else {
                continue;
            };
            let command = BleTaskCommand::Notify {
                host_id,
                report: hidshift::reports::BleHidReport::Mouse(report),
                reason: hidshift::NotifyReason::Input,
            };
            if self.ble_notify.try_send(command).is_err() {
                let _ = self.mouse.push(host_id, report);
                break;
            }
        }
    }

    async fn send_usb_with_policy(
        &mut self,
        command: UsbHostTaskCommand,
    ) -> Result<(), ChannelTaskSendError> {
        if command.class() == hidshift::CommandClass::Critical {
            self.usb.send(command).await;
            return Ok(());
        }
        let Some(target) = command.led_target() else {
            return Err(ChannelTaskSendError::UsbQueueFull);
        };
        let slot = self
            .pending_usb
            .iter()
            .position(|pending| pending.is_some_and(|pending| pending.led_target() == Some(target)))
            .or_else(|| self.pending_usb.iter().position(Option::is_none))
            .ok_or(ChannelTaskSendError::UsbQueueFull)?;
        self.pending_usb[slot] = Some(command);
        self.flush_usb_commands();
        Ok(())
    }

    fn flush_usb_commands(&mut self) {
        for pending in self.pending_usb.iter_mut() {
            if self.usb.free_capacity() == 0 {
                break;
            }
            let Some(command) = pending.take() else {
                continue;
            };
            if self.usb.try_send(command).is_err() {
                *pending = Some(command);
                break;
            }
        }
    }

    async fn send_storage_with_policy(
        &mut self,
        command: StorageTaskCommand,
    ) -> Result<(), ChannelTaskSendError> {
        match command.class() {
            hidshift::CommandClass::Critical => {
                self.storage.send(command).await;
                Ok(())
            }
            hidshift::CommandClass::Realtime => self
                .storage
                .try_send(command)
                .map_err(ChannelTaskSendError::from),
            hidshift::CommandClass::BestEffort => {
                let _ = self.storage.try_send(command);
                Ok(())
            }
        }
    }

    async fn send_status_with_policy(
        &mut self,
        command: StatusTaskCommand,
    ) -> Result<(), ChannelTaskSendError> {
        if command.management.is_none() {
            if self.pending_status.is_some() {
                self.status_updates_dropped = self.status_updates_dropped.saturating_add(1);
            }
            self.pending_status = Some(command);
            self.flush_status_snapshot();
            return Ok(());
        }
        match command.class() {
            hidshift::CommandClass::Critical => {
                self.status.send(command).await;
                Ok(())
            }
            hidshift::CommandClass::Realtime => self
                .status
                .try_send(command)
                .map_err(ChannelTaskSendError::from),
            hidshift::CommandClass::BestEffort => {
                let _ = self.status.try_send(command);
                Ok(())
            }
        }
    }

    fn flush_status_snapshot(&mut self) {
        if self.status.free_capacity() == 0 {
            return;
        }
        let Some(command) = self.pending_status.take() else {
            return;
        };
        if self.status.try_send(command).is_err() {
            self.pending_status = Some(command);
        }
    }
}

fn apply_runtime_effect(effect: hidshift::runtime::RuntimeEffect) {
    match effect {
        hidshift::runtime::RuntimeEffect::SetLogLevel(level) => {
            log::set_max_level(match level {
                0 => log::LevelFilter::Error,
                1 => log::LevelFilter::Warn,
                _ => log::LevelFilter::Info,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChannelTaskSendError {
    BleQueueFull,
    #[cfg(feature = "dual-s3-wired")]
    DeviceQueueFull,
    UsbQueueFull,
    StorageQueueFull,
    StatusQueueFull,
}

#[cfg(feature = "dual-s3-wired")]
impl From<TrySendError<DeviceTaskCommand>> for ChannelTaskSendError {
    fn from(_: TrySendError<DeviceTaskCommand>) -> Self {
        Self::DeviceQueueFull
    }
}

impl From<TrySendError<BleTaskCommand>> for ChannelTaskSendError {
    fn from(_: TrySendError<BleTaskCommand>) -> Self {
        Self::BleQueueFull
    }
}

impl From<TrySendError<UsbHostTaskCommand>> for ChannelTaskSendError {
    fn from(_: TrySendError<UsbHostTaskCommand>) -> Self {
        Self::UsbQueueFull
    }
}

impl From<TrySendError<StorageTaskCommand>> for ChannelTaskSendError {
    fn from(_: TrySendError<StorageTaskCommand>) -> Self {
        Self::StorageQueueFull
    }
}

impl From<TrySendError<StatusTaskCommand>> for ChannelTaskSendError {
    fn from(_: TrySendError<StatusTaskCommand>) -> Self {
        Self::StatusQueueFull
    }
}
