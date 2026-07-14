#![no_std]
#![no_main]

extern crate alloc;

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
#[cfg(feature = "espnow")]
use esp_hal::interrupt::software::SoftwareInterrupt;
use esp_hal::interrupt::software::SoftwareInterruptControl;
#[cfg(feature = "espnow")]
use esp_hal::ram;
#[cfg(feature = "espnow")]
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
#[cfg(feature = "espnow")]
use esp_radio::ble::controller::BleConnector;
use esp32s3_platform::ble_hid_task::BleRuntimeSnapshot;
use esp32s3_platform::ble_hid_task::active_ble_connections;
use esp32s3_platform::button_task::control_task;
#[cfg(feature = "espnow")]
use esp32s3_platform::espnow_link_task::espnow_host_task;
use esp32s3_platform::storage_task::storage_command_task;
#[cfg(not(all(feature = "hardware-e2e", feature = "espnow")))]
use esp32s3_platform::usb_host_task::usb_input_task;
use hidshift::DefaultRuntimeOwner;
use hidshift::mouse_accumulator::MouseReportAccumulator;
use hidshift::runtime::RUNTIME_HOSTS_MAX;
use hidshift::runtime::driver::RuntimeTaskSink;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    BleCommandLane, BleTaskCommand, RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY, RUNTIME_INPUT_QUEUE_CAPACITY,
    RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY, RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
    RUNTIME_USB_COMMAND_QUEUE_CAPACITY, RuntimeDiagnosticsEvent, StatusTaskCommand,
    StorageTaskCommand, UsbTaskCommand,
};
use hidshift::storage::StorageState;
use platform as esp32s3_platform;
use static_cell::{ConstStaticCell, StaticCell};

esp_bootloader_esp_idf::esp_app_desc!();

// esp-radio's S3 PHY maps WIFI_BB (interrupt source 3) to the raw level-6
// vector during coexistence calibration. esp-hal 1.1 leaves that vector as a
// weak default. This is the complete vendor `bb_intr_handl` operation: a
// volatile read acknowledges the calibration interrupt. Keep it in RAM
// because a radio ISR must not depend on the flash cache.
#[cfg(feature = "espnow")]
#[unsafe(no_mangle)]
#[ram]
extern "C" fn level6_interrupt(_save_frame: *mut ()) {
    const WIFI_BB_INTERRUPT_ACK: *const u32 = 0x6001_d04c as *const u32;
    unsafe {
        core::ptr::read_volatile(WIFI_BB_INTERRUPT_ACK);
    }
}

static RUNTIME_INPUT_CHANNEL: Channel<
    CriticalSectionRawMutex,
    RuntimeInputMessage,
    RUNTIME_INPUT_QUEUE_CAPACITY,
> = Channel::new();
static RUNTIME_TICK_PENDING: hidshift::runtime::RuntimeTickPending =
    hidshift::runtime::RuntimeTickPending::new();
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
    UsbTaskCommand,
    RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
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
#[cfg(feature = "espnow")]
static BLE_CORE_EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
#[cfg(feature = "espnow")]
static BLE_CORE_STACK: ConstStaticCell<Stack<{ 24 * 1024 }>> = ConstStaticCell::new(Stack::new());
// Priority zero is shared with idle/application work and can postpone a
// cross-core BLE command until the next scheduler slice. Priority one remains
// below esp-radio controller work while waking the BLE host executor promptly.
#[cfg(feature = "espnow")]
const BLE_CORE_EXECUTOR_THREAD_PRIORITY: usize = 1;
#[cfg(feature = "espnow")]
struct CoexistCoreResources {
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
    cpu_ctrl: esp_hal::peripherals::CPU_CTRL<'static>,
    interrupt: SoftwareInterrupt<'static, 1>,
}
static CHANNEL_TASK_SINK: StaticCell<ChannelTaskSink> = StaticCell::new();
static RUNTIME_OWNER_STORAGE: ConstStaticCell<DefaultRuntimeOwner> =
    ConstStaticCell::new(DefaultRuntimeOwner::new(0));

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

    #[cfg(not(feature = "espnow"))]
    esp_alloc::heap_allocator!(size: 72 * 1024);
    #[cfg(feature = "espnow")]
    {
        esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 64 * 1024);
        // BLE runs on core1 with a dedicated 42-KiB stack below. Keep most
        // allocations in reclaimed RAM and one regular-DRAM region large
        // enough for the BLE controller's 8-KiB RTOS stack plus allocator
        // metadata. Keeping this region contiguous also avoids Wi-Fi heap
        // fragmentation starving the controller during coexistence startup.
        esp_alloc::heap_allocator!(size: 24 * 1024);
    }

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
    let sw_ints = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let scheduler_interrupt = sw_ints.software_interrupt0;
    #[cfg(feature = "espnow")]
    let ble_core_interrupt = sw_ints.software_interrupt1;
    let gpio0 = peripherals.GPIO0;
    let uart0 = peripherals.UART0;
    let gpio44 = peripherals.GPIO44;
    let usb0 = peripherals.USB0;
    let gpio20 = peripherals.GPIO20;
    let gpio19 = peripherals.GPIO19;
    let wifi = peripherals.WIFI;
    let flash = peripherals.FLASH;
    #[cfg(feature = "espnow")]
    let cpu_ctrl = peripherals.CPU_CTRL;
    #[cfg(any(not(feature = "espnow"), feature = "espnow"))]
    let (bt, rng, adc1) = (peripherals.BT, peripherals.RNG, peripherals.ADC1);
    esp_rtos::start(timg0.timer0, scheduler_interrupt);

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
                wifi,
                boot_session_id,
                flash,
                #[cfg(feature = "espnow")]
                CoexistCoreResources {
                    bt,
                    rng,
                    adc1,
                    cpu_ctrl,
                    interrupt: ble_core_interrupt,
                },
                #[cfg(not(feature = "espnow"))]
                bt,
                #[cfg(not(feature = "espnow"))]
                rng,
                #[cfg(not(feature = "espnow"))]
                adc1,
            ),
            "startup",
        );
    })
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
    wifi: esp_hal::peripherals::WIFI<'static>,
    boot_session_id: u32,
    flash: esp_hal::peripherals::FLASH<'static>,
    #[cfg(feature = "espnow")] coexist: CoexistCoreResources,
    #[cfg(not(feature = "espnow"))] bt: esp_hal::peripherals::BT<'static>,
    #[cfg(not(feature = "espnow"))] rng: esp_hal::peripherals::RNG<'static>,
    #[cfg(not(feature = "espnow"))] adc1: esp_hal::peripherals::ADC1<'static>,
) {
    let runtime_owner_receiver = RUNTIME_INPUT_CHANNEL.receiver();
    let storage_sender = RUNTIME_INPUT_CHANNEL.sender();
    #[cfg(not(all(feature = "hardware-e2e", feature = "espnow")))]
    let usb_input_sender = RUNTIME_INPUT_CHANNEL.sender();
    #[cfg(not(all(feature = "hardware-e2e", feature = "espnow")))]
    let usb_receiver = USB_COMMAND_CHANNEL.receiver();
    #[cfg(not(feature = "espnow"))]
    let ble_control_receiver = BLE_CONTROL_COMMAND_CHANNEL.receiver();
    #[cfg(not(feature = "espnow"))]
    let ble_notify_receiver = BLE_NOTIFY_COMMAND_CHANNEL.receiver();
    #[cfg(not(feature = "espnow"))]
    let ble_input_sender = RUNTIME_INPUT_CHANNEL.sender();
    #[cfg(not(feature = "espnow"))]
    let ble_restore_receiver = BLE_RESTORE_CHANNEL.receiver();
    #[cfg(all(feature = "hardware-e2e", feature = "espnow"))]
    let _ = (usb0, gpio20, gpio19);
    #[cfg(feature = "espnow")]
    let storage_radio_ready_receiver =
        esp32s3_platform::espnow_link_task::storage_radio_ready_receiver();
    let sink = CHANNEL_TASK_SINK.init_with(|| ChannelTaskSink {
        ble_control: BLE_CONTROL_COMMAND_CHANNEL.sender(),
        ble_notify: BLE_NOTIFY_COMMAND_CHANNEL.sender(),
        usb: USB_COMMAND_CHANNEL.sender(),
        storage: STORAGE_COMMAND_CHANNEL.sender(),
        status: STATUS_COMMAND_CHANNEL.sender(),
        mouse: MouseReportAccumulator::new(),
        pending_usb: [None; RUNTIME_USB_COMMAND_QUEUE_CAPACITY],
        pending_status: None,
        status_updates_dropped: 0,
    });

    spawn_or_reset(
        &spawner,
        runtime_owner_task(
            runtime_owner_receiver,
            &RUNTIME_TICK_PENDING,
            BLE_RUNTIME_BARRIER_REQUEST_CHANNEL.receiver(),
            BLE_RUNTIME_BARRIER_DONE_CHANNEL.sender(),
            BLE_RUNTIME_BARRIER_RESUME_CHANNEL.receiver(),
            sink,
        ),
        "runtime-owner",
    );
    let _ = RUNTIME_INPUT_CHANNEL.try_send(RuntimeInputMessage::DiagnosticsEvent(
        RuntimeDiagnosticsEvent::ResetReason(reset_reason_code),
    ));
    if was_brownout {
        let _ = RUNTIME_INPUT_CHANNEL.try_send(RuntimeInputMessage::DiagnosticsEvent(
            RuntimeDiagnosticsEvent::Brownout,
        ));
    }
    spawn_or_reset(
        &spawner,
        control_task(RUNTIME_INPUT_CHANNEL.sender(), &RUNTIME_TICK_PENDING, gpio0),
        "control",
    );
    spawn_or_reset(
        &spawner,
        esp32s3_platform::serial_management_task::serial_management_task(
            RUNTIME_INPUT_CHANNEL.sender(),
            uart0,
            gpio44,
            boot_session_id,
        ),
        "serial-management",
    );
    // The bridge E2E image injects reports at the normalized UART boundary.
    // Keeping the USB Host future out of that image avoids exceeding the
    // ESP32-S3 shared executor's startup stack; the normal firmware image
    // still starts the USB Host task below.
    #[cfg(not(all(feature = "hardware-e2e", feature = "espnow")))]
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
    #[cfg(feature = "espnow")]
    spawn_or_reset(
        &spawner,
        espnow_host_task(spawner, wifi, boot_session_id),
        "espnow-host",
    );
    #[cfg(feature = "espnow")]
    spawn_or_reset(
        &spawner,
        ble_coexist_core_bootstrap_task(
            coexist.bt,
            coexist.rng,
            coexist.adc1,
            coexist.cpu_ctrl,
            coexist.interrupt,
        ),
        "ble-coexist-core-bootstrap",
    );
    #[cfg(not(feature = "espnow"))]
    spawn_or_reset(
        &spawner,
        esp32s3_platform::ble_hid_task::ble_host_event_task(
            ble_input_sender,
            ble_control_receiver,
            ble_notify_receiver,
            ble_restore_receiver,
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
            BLE_CONTROL_COMMAND_CHANNEL.sender(),
            flash,
            #[cfg(feature = "espnow")]
            storage_radio_ready_receiver,
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
    core::future::pending::<()>().await;
}

#[cfg(feature = "espnow")]
#[embassy_executor::task]
async fn ble_coexist_core_bootstrap_task(
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
    cpu_ctrl: esp_hal::peripherals::CPU_CTRL<'static>,
    ble_core_interrupt: SoftwareInterrupt<'static, 1>,
) {
    let radio_ready = esp32s3_platform::espnow_link_task::radio_ready_receiver();
    let _ = radio_ready.receive().await;
    log::info!(
        "firmware: BLE coexist radio barrier passed heap_free={}",
        esp_alloc::HEAP.free()
    );
    esp_rtos::start_second_core(
        cpu_ctrl,
        ble_core_interrupt,
        BLE_CORE_STACK.take(),
        move || {
            esp_rtos::CurrentThreadHandle::get().set_priority(BLE_CORE_EXECUTOR_THREAD_PRIORITY);
            // Keep the BLE host and Espressif controller task on core1. The
            // default controller placement is core0, where Wi-Fi/ESP-NOW and
            // the input runtime already run; that adds a second cross-core
            // handoff to every HCI ACL submission and increases both radios'
            // scheduling tails during coexistence.
            log::info!(
                "firmware: BLE controller initialization starting on core1 heap_free={}",
                esp_alloc::HEAP.free()
            );
            let connector = match BleConnector::new(
                bt,
                esp32s3_platform::ble_hid_task::ble_controller_config(),
            ) {
                Ok(connector) => connector,
                Err(error) => {
                    log::error!(
                        "firmware: BLE controller initialization failed on core1: {:?}; resetting",
                        error
                    );
                    esp_hal::system::software_reset();
                }
            };
            log::info!(
                "firmware: BLE controller initialized on core1 heap_free={}",
                esp_alloc::HEAP.free()
            );
            let executor = BLE_CORE_EXECUTOR.init(esp_rtos::embassy::Executor::new());
            executor.run(|spawner| {
                spawn_or_reset(
                    &spawner,
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
                        connector,
                        rng,
                        adc1,
                    ),
                    "ble-host-event-core1",
                );
            })
        },
    );

    core::future::pending::<()>().await;
}

#[cfg(not(all(feature = "hardware-e2e", feature = "espnow")))]
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
        UsbTaskCommand,
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
        owner.observe_transport_metrics(
            receiver.len(),
            sink.mouse.stats(),
            sink.status_updates_dropped,
        );
        let message = match select(receiver.receive(), barrier_request.receive()).await {
            Either::First(message) => message,
            Either::Second(active_host_mask) => {
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
                continue;
            }
        };
        if matches!(message, RuntimeInputMessage::Tick { .. }) {
            tick_pending.mark_processed();
        }
        process_runtime_message(&mut owner, &mut sink, message).await;
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
        UsbTaskCommand,
        RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
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
    pending_usb: [Option<UsbTaskCommand>; RUNTIME_USB_COMMAND_QUEUE_CAPACITY],
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
        for command in queues.usb.iter().copied() {
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
        for command in queues.usb.iter().copied() {
            let key = (command.interface_id, command.device_id);
            if self
                .pending_usb
                .iter()
                .flatten()
                .any(|pending| (pending.interface_id, pending.device_id) == key)
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
        command: UsbTaskCommand,
    ) -> Result<(), ChannelTaskSendError> {
        let slot = self
            .pending_usb
            .iter()
            .position(|pending| {
                pending.is_some_and(|pending| {
                    pending.interface_id == command.interface_id
                        && pending.device_id == command.device_id
                })
            })
            .or_else(|| self.pending_usb.iter().position(Option::is_none))
            .ok_or(ChannelTaskSendError::UsbQueueFull)?;
        self.pending_usb[slot] = Some(command);
        self.flush_usb_commands();
        Ok(())
    }

    fn flush_usb_commands(&mut self) {
        for pending in &mut self.pending_usb {
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

impl RuntimeTaskSink for ChannelTaskSink {
    type Error = ChannelTaskSendError;

    fn reserve_batch<
        const BLE: usize,
        const USB: usize,
        const STORAGE: usize,
        const STATUS: usize,
    >(
        &mut self,
        queues: &hidshift::RuntimeCommandQueues<BLE, USB, STORAGE, STATUS>,
    ) -> Result<(), (hidshift::runtime::driver::RuntimeTaskKind, Self::Error)> {
        self.ensure_capacity(queues).map_err(|error| {
            let task = match error {
                ChannelTaskSendError::BleQueueFull => {
                    hidshift::runtime::driver::RuntimeTaskKind::Ble
                }
                ChannelTaskSendError::UsbQueueFull => {
                    hidshift::runtime::driver::RuntimeTaskKind::Usb
                }
                ChannelTaskSendError::StorageQueueFull => {
                    hidshift::runtime::driver::RuntimeTaskKind::Storage
                }
                ChannelTaskSendError::StatusQueueFull => {
                    hidshift::runtime::driver::RuntimeTaskKind::Status
                }
            };
            (task, error)
        })
    }

    fn send_ble(&mut self, command: BleTaskCommand) -> Result<(), Self::Error> {
        match command.lane() {
            BleCommandLane::Control => self
                .ble_control
                .try_send(command)
                .map_err(ChannelTaskSendError::from),
            BleCommandLane::Notify => self
                .ble_notify
                .try_send(command)
                .map_err(ChannelTaskSendError::from),
        }
    }

    fn send_usb(&mut self, command: UsbTaskCommand) -> Result<(), Self::Error> {
        self.usb
            .try_send(command)
            .map_err(ChannelTaskSendError::from)
    }

    fn send_storage(&mut self, command: StorageTaskCommand) -> Result<(), Self::Error> {
        self.storage
            .try_send(command)
            .map_err(ChannelTaskSendError::from)
    }

    fn send_status(&mut self, command: StatusTaskCommand) -> Result<(), Self::Error> {
        self.status
            .try_send(command)
            .map_err(ChannelTaskSendError::from)
    }

    fn apply_effect(&mut self, effect: hidshift::runtime::RuntimeEffect) {
        apply_runtime_effect(effect);
    }
}

fn apply_runtime_effect(effect: hidshift::runtime::RuntimeEffect) {
    match effect {
        hidshift::runtime::RuntimeEffect::SetLogLevel(level) => {
            log::set_max_level(match level {
                0 => log::LevelFilter::Error,
                1 => log::LevelFilter::Warn,
                2 => log::LevelFilter::Info,
                3 => log::LevelFilter::Debug,
                _ => log::LevelFilter::Trace,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChannelTaskSendError {
    BleQueueFull,
    UsbQueueFull,
    StorageQueueFull,
    StatusQueueFull,
}

impl From<TrySendError<BleTaskCommand>> for ChannelTaskSendError {
    fn from(_: TrySendError<BleTaskCommand>) -> Self {
        Self::BleQueueFull
    }
}

impl From<TrySendError<UsbTaskCommand>> for ChannelTaskSendError {
    fn from(_: TrySendError<UsbTaskCommand>) -> Self {
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
