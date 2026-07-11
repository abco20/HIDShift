#![no_std]
#![no_main]

#[cfg(feature = "ble-hid")]
extern crate alloc;

use embassy_executor::{SpawnError, SpawnToken, Spawner};
#[cfg(all(feature = "ble-hid", feature = "storage"))]
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender, TrySendError};
use esp_backtrace as _;
#[path = "../platform/esp32s3/mod.rs"]
mod esp32s3_platform;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::timer::timg::TimerGroup;
#[cfg(all(feature = "ble-hid", feature = "storage"))]
use esp32s3_platform::ble_hid_task::BleRuntimeSnapshot;
#[cfg(all(feature = "ble-hid", feature = "storage"))]
use esp32s3_platform::ble_hid_task::active_ble_connections;
use esp32s3_platform::button_task::control_task;
use esp32s3_platform::storage_task::storage_command_task;
use esp32s3_platform::usb_host_task::usb_input_task;
use hidshift::DefaultRuntimeOwner;
#[cfg(not(feature = "ble-hid"))]
use hidshift::ble_notify::BleNotificationSink;
#[cfg(not(feature = "ble-hid"))]
use hidshift::ble_notify::dispatch_ble_task_command;
use hidshift::mouse_accumulator::MouseReportAccumulator;
#[cfg(all(feature = "ble-hid", feature = "storage"))]
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
#[cfg(feature = "ble-hid")]
use hidshift::storage::StorageState;
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

static RUNTIME_INPUT_CHANNEL: Channel<
    CriticalSectionRawMutex,
    RuntimeInputMessage,
    RUNTIME_INPUT_QUEUE_CAPACITY,
> = Channel::new();
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
#[cfg(feature = "ble-hid")]
static BLE_RESTORE_CHANNEL: Channel<CriticalSectionRawMutex, Option<StorageState>, 1> =
    Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static BLE_QUIESCE_REQUEST_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static BLE_QUIESCE_READY_CHANNEL: Channel<CriticalSectionRawMutex, Option<StorageState>, 1> =
    Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static BLE_QUIESCE_DONE_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static USB_BLE_QUIESCE_REQUEST_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static USB_BLE_QUIESCE_READY_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static USB_BLE_QUIESCE_DONE_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static BLE_RUNTIME_BARRIER_REQUEST_CHANNEL: Channel<CriticalSectionRawMutex, usize, 1> =
    Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static BLE_RUNTIME_BARRIER_DONE_CHANNEL: Channel<CriticalSectionRawMutex, BleRuntimeSnapshot, 1> =
    Channel::new();
#[cfg(all(feature = "ble-hid", feature = "storage"))]
static BLE_RUNTIME_BARRIER_RESUME_CHANNEL: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
static STATUS_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    StatusTaskCommand,
    RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
> = Channel::new();
static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();

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

    #[cfg(feature = "ble-hid")]
    {
        esp_alloc::heap_allocator!(size: 72 * 1024);
    }

    let reset_reason = esp_hal::system::reset_reason();
    let reset_reason_code = reset_reason.map_or(0, |reason| reason as u8);
    let was_brownout = matches!(
        reset_reason,
        Some(esp_hal::rtc_cntl::SocResetReason::SysBrownOut)
    );
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_ints = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);

    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run(|spawner| {
        let runtime_owner_receiver = RUNTIME_INPUT_CHANNEL.receiver();
        let storage_sender = RUNTIME_INPUT_CHANNEL.sender();
        let usb_input_sender = RUNTIME_INPUT_CHANNEL.sender();
        let usb_receiver = USB_COMMAND_CHANNEL.receiver();
        #[cfg(feature = "ble-hid")]
        let ble_control_receiver = BLE_CONTROL_COMMAND_CHANNEL.receiver();
        #[cfg(feature = "ble-hid")]
        let ble_notify_receiver = BLE_NOTIFY_COMMAND_CHANNEL.receiver();
        #[cfg(feature = "ble-hid")]
        let ble_input_sender = RUNTIME_INPUT_CHANNEL.sender();
        #[cfg(feature = "ble-hid")]
        let ble_restore_receiver = BLE_RESTORE_CHANNEL.receiver();
        let sink = ChannelTaskSink {
            ble_control: BLE_CONTROL_COMMAND_CHANNEL.sender(),
            ble_notify: BLE_NOTIFY_COMMAND_CHANNEL.sender(),
            usb: USB_COMMAND_CHANNEL.sender(),
            storage: STORAGE_COMMAND_CHANNEL.sender(),
            status: STATUS_COMMAND_CHANNEL.sender(),
            mouse: MouseReportAccumulator::new(),
            pending_usb: [None; RUNTIME_USB_COMMAND_QUEUE_CAPACITY],
            pending_status: None,
            status_updates_dropped: 0,
        };

        spawn_or_reset(
            &spawner,
            runtime_owner_task(
                runtime_owner_receiver,
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                BLE_RUNTIME_BARRIER_REQUEST_CHANNEL.receiver(),
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                BLE_RUNTIME_BARRIER_DONE_CHANNEL.sender(),
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
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
            control_task(RUNTIME_INPUT_CHANNEL.sender(), peripherals.GPIO0),
            "control",
        );
        spawn_or_reset(
            &spawner,
            esp32s3_platform::serial_management_task::serial_management_task(
                RUNTIME_INPUT_CHANNEL.sender(),
                peripherals.UART0,
                peripherals.GPIO44,
            ),
            "serial-management",
        );
        spawn_or_reset(
            &spawner,
            usb_input_task(
                usb_input_sender,
                usb_receiver,
                peripherals.USB0,
                peripherals.GPIO20,
                peripherals.GPIO19,
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                USB_BLE_QUIESCE_REQUEST_CHANNEL.sender(),
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                USB_BLE_QUIESCE_READY_CHANNEL.receiver(),
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                USB_BLE_QUIESCE_DONE_CHANNEL.sender(),
            ),
            "usb-input",
        );
        #[cfg(feature = "ble-hid")]
        spawn_or_reset(
            &spawner,
            esp32s3_platform::ble_hid_task::ble_host_event_task(
                ble_input_sender,
                ble_control_receiver,
                ble_notify_receiver,
                ble_restore_receiver,
                #[cfg(feature = "storage")]
                BLE_QUIESCE_REQUEST_CHANNEL.receiver(),
                #[cfg(feature = "storage")]
                BLE_QUIESCE_READY_CHANNEL.sender(),
                #[cfg(feature = "storage")]
                BLE_QUIESCE_DONE_CHANNEL.receiver(),
                #[cfg(feature = "storage")]
                USB_BLE_QUIESCE_REQUEST_CHANNEL.receiver(),
                #[cfg(feature = "storage")]
                USB_BLE_QUIESCE_READY_CHANNEL.sender(),
                #[cfg(feature = "storage")]
                USB_BLE_QUIESCE_DONE_CHANNEL.receiver(),
                #[cfg(feature = "storage")]
                BLE_RUNTIME_BARRIER_REQUEST_CHANNEL.sender(),
                #[cfg(feature = "storage")]
                BLE_RUNTIME_BARRIER_DONE_CHANNEL.receiver(),
                #[cfg(feature = "storage")]
                BLE_RUNTIME_BARRIER_RESUME_CHANNEL.sender(),
                peripherals.BT,
                peripherals.RNG,
                peripherals.ADC1,
            ),
            "ble-host-event",
        );
        #[cfg(not(feature = "ble-hid"))]
        spawn_or_reset(
            &spawner,
            ble_command_task(
                BLE_CONTROL_COMMAND_CHANNEL.receiver(),
                BLE_NOTIFY_COMMAND_CHANNEL.receiver(),
            ),
            "ble-command",
        );
        #[cfg(not(feature = "usb-host"))]
        spawn_or_reset(
            &spawner,
            usb_command_task(USB_COMMAND_CHANNEL.receiver()),
            "usb-command",
        );
        #[cfg(feature = "storage")]
        spawn_or_reset(
            &spawner,
            storage_command_task(
                STORAGE_COMMAND_CHANNEL.receiver(),
                storage_sender,
                #[cfg(feature = "ble-hid")]
                BLE_RESTORE_CHANNEL.sender(),
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                BLE_QUIESCE_REQUEST_CHANNEL.sender(),
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                BLE_QUIESCE_READY_CHANNEL.receiver(),
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                BLE_QUIESCE_DONE_CHANNEL.sender(),
                active_ble_connections,
                peripherals.FLASH,
            ),
            "storage-command",
        );
        #[cfg(not(feature = "storage"))]
        spawn_or_reset(
            &spawner,
            storage_command_task(
                STORAGE_COMMAND_CHANNEL.receiver(),
                storage_sender,
                #[cfg(feature = "ble-hid")]
                BLE_RESTORE_CHANNEL.sender(),
                active_ble_connections,
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
    })
}

#[embassy_executor::task]
async fn runtime_owner_task(
    receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    #[cfg(all(feature = "ble-hid", feature = "storage"))] barrier_request: Receiver<
        'static,
        CriticalSectionRawMutex,
        usize,
        1,
    >,
    #[cfg(all(feature = "ble-hid", feature = "storage"))] barrier_done: Sender<
        'static,
        CriticalSectionRawMutex,
        BleRuntimeSnapshot,
        1,
    >,
    #[cfg(all(feature = "ble-hid", feature = "storage"))] barrier_resume: Receiver<
        'static,
        CriticalSectionRawMutex,
        (),
        1,
    >,
    mut sink: ChannelTaskSink,
) {
    let mut owner = DefaultRuntimeOwner::new(0);

    log::info!("firmware: runtime owner task boot");

    loop {
        owner.observe_transport_metrics(
            receiver.len(),
            sink.mouse.stats(),
            sink.status_updates_dropped,
        );
        #[cfg(all(feature = "ble-hid", feature = "storage"))]
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
        #[cfg(not(all(feature = "ble-hid", feature = "storage")))]
        let message = receiver.receive().await;
        process_runtime_message(&mut owner, &mut sink, message).await;
    }
}

async fn process_runtime_message(
    owner: &mut DefaultRuntimeOwner,
    sink: &mut ChannelTaskSink,
    message: RuntimeInputMessage,
) {
    log::trace!("firmware: runtime_input {:?}", message);

    let next_owner = match owner.staged_message(&message) {
        Ok(owner) => owner,
        Err(error) => {
            log::error!("firmware: runtime owner error {:?}", error);
            return;
        }
    };

    if let Err(error) = sink
        .dispatch_runtime_queues(next_owner.default_queues())
        .await
    {
        log::error!("firmware: runtime drive error {:?}", error);
        return;
    }
    *owner = next_owner;
}

#[embassy_executor::task]
#[cfg(not(feature = "ble-hid"))]
async fn ble_command_task(
    control_receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    >,
    notify_receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY,
    >,
) {
    log::info!("firmware: ble command task boot");
    let mut sink = LoggingBleNotificationSink;
    loop {
        let command = receive_ble_command(control_receiver, notify_receiver).await;
        log::trace!("firmware: ble_command {:?}", command);
        if let Err(error) = dispatch_ble_task_command(command, &mut sink) {
            log::error!("firmware: ble_notify_dispatch error {:?}", error);
        }
    }
}

#[embassy_executor::task]
async fn usb_command_task(
    receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        UsbTaskCommand,
        RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    >,
) {
    log::info!("firmware: usb command task boot");
    loop {
        let command = receiver.receive().await;
        log::debug!("firmware: usb_command {:?}", command);
    }
}

#[cfg(not(all(feature = "ble-hid", feature = "storage")))]
fn active_ble_connections() -> usize {
    0
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
    let bytes = response.encode();
    esp_println::println!(
        "@HIDSHIFT:{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
        bytes[16],
        bytes[17],
        bytes[18],
        bytes[19]
    );
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
    #[cfg(all(feature = "ble-hid", feature = "storage"))]
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
            self.send_ble_with_policy(command).await?;
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

    fn ensure_capacity(
        &self,
        queues: &hidshift::DefaultRuntimeCommandQueues,
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
            let _ = self.mouse.push(host_id, report);
            self.flush_mouse_accumulator();
            return Ok(());
        }
        if let BleTaskCommand::Notify {
            host_id,
            reason:
                hidshift::NotifyReason::InputRelease
                | hidshift::NotifyReason::TargetSwitchRelease
                | hidshift::NotifyReason::UsbDeviceRemovedRelease
                | hidshift::NotifyReason::SafetyRelease,
            ..
        } = command
        {
            self.mouse.discard(host_id);
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
                pending.is_some_and(|pending| pending.interface_id == command.interface_id)
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

#[cfg(not(feature = "ble-hid"))]
struct LoggingBleNotificationSink;

#[cfg(not(feature = "ble-hid"))]
impl BleNotificationSink for LoggingBleNotificationSink {
    type Error = core::convert::Infallible;

    fn send_notification(
        &mut self,
        characteristic: hidshift::reports::BleHidCharacteristic,
        value: &[u8],
    ) -> Result<(), Self::Error> {
        log::trace!(
            "firmware: ble_notification characteristic={:?} bytes={:?}",
            characteristic,
            value
        );
        Ok(())
    }
}

impl RuntimeTaskSink for ChannelTaskSink {
    type Error = ChannelTaskSendError;

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

#[cfg(not(feature = "ble-hid"))]
async fn receive_ble_command(
    control_receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    >,
    notify_receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY,
    >,
) -> BleTaskCommand {
    if let Ok(command) = control_receiver.try_receive() {
        return command;
    }
    if let Ok(command) = notify_receiver.try_receive() {
        return command;
    }
    match embassy_futures::select::select(control_receiver.receive(), notify_receiver.receive())
        .await
    {
        embassy_futures::select::Either::First(command)
        | embassy_futures::select::Either::Second(command) => command,
    }
}
