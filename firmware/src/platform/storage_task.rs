use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::FLASH;
use esp_hal::{peripherals::Interrupt, system::Cpu};
use hidshift::espnow_pairing::{EspNowPairing, EspNowRole};
use hidshift::espnow_pairing_management::{EspNowPairingAction, EspNowPairingService};
use hidshift::ids::HostId;
use hidshift::management::{
    ManagementDestination, ManagementRequest, ManagementResponse, ManagementResult,
};
use hidshift::runtime::bootstrap::{initial_pairing_host, storage_with_default_target};
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    BleTaskCommand, RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY, RUNTIME_INPUT_QUEUE_CAPACITY,
    RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY, StorageTaskCommand,
};
use hidshift::storage::{
    StoragePersistPriority, StoragePersistence, StorageSlotBackend, StorageState,
    StorageTaskAction, StorageTaskPolicy, restore_latest_storage_state,
};
use hidshift::target_control::ButtonIntent;

use super::flash_backend::{FirmwareStorageBackend, new_storage_backend};

pub const STORAGE_PERSIST_DEBOUNCE_MS: u64 = 1_000;
pub const STORAGE_PERSIST_LAZY_MS: u64 = 5_000;
pub const STORAGE_ACTIVE_BLE_RETRY_MS: u64 = 1_000;
pub const STORAGE_CRITICAL_FORCE_QUIESCE_MS: u64 = 5_000;

#[derive(Clone, Copy, Debug)]
pub struct EspNowManagementRequest {
    pub destination: ManagementDestination,
    pub request: ManagementRequest,
}

static ESPNOW_MANAGEMENT_CHANNEL: Channel<CriticalSectionRawMutex, EspNowManagementRequest, 4> =
    Channel::new();
static ESPNOW_RESTORE_CHANNEL: Channel<CriticalSectionRawMutex, Option<EspNowPairing>, 1> =
    Channel::new();

pub fn espnow_management_sender()
-> Sender<'static, CriticalSectionRawMutex, EspNowManagementRequest, 4> {
    ESPNOW_MANAGEMENT_CHANNEL.sender()
}

pub fn espnow_restore_receiver()
-> Receiver<'static, CriticalSectionRawMutex, Option<EspNowPairing>, 1> {
    ESPNOW_RESTORE_CHANNEL.receiver()
}

#[embassy_executor::task]
pub async fn storage_command_task(
    receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        StorageTaskCommand,
        RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY,
    >,
    runtime_input: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    ble_restore: Sender<'static, CriticalSectionRawMutex, Option<StorageState>, 1>,
    ble_quiesce_request: Sender<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_ready: Receiver<'static, CriticalSectionRawMutex, Option<StorageState>, 1>,
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
    active_ble_connections: fn() -> usize,
    ble_control: Sender<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    >,
    flash: FLASH<'static>,
    #[cfg(feature = "espnow")] radio_ready: Receiver<'static, CriticalSectionRawMutex, bool, 1>,
) {
    #[cfg(feature = "espnow")]
    if !radio_ready.receive().await {
        log::warn!("firmware: ESP-NOW unavailable before storage init");
    }
    let mut backend = new_storage_backend(flash);
    let espnow_pairing = backend.restored_pairing();
    let mut espnow_service = EspNowPairingService::new(EspNowRole::UsbHost, espnow_pairing);
    #[cfg(feature = "espnow")]
    ESPNOW_RESTORE_CHANNEL.send(espnow_pairing).await;
    let mut persistence =
        StoragePersistence::new(STORAGE_PERSIST_DEBOUNCE_MS, STORAGE_PERSIST_LAZY_MS);
    let storage_policy = StorageTaskPolicy {
        active_ble_retry_ms: STORAGE_ACTIVE_BLE_RETRY_MS,
        critical_force_quiesce_ms: STORAGE_CRITICAL_FORCE_QUIESCE_MS,
    };
    log::info!("firmware: storage command task boot");

    let restored_state = restore_latest_storage_state(&backend);
    ble_restore.send(restored_state.clone()).await;
    if let Some(state) = restored_state {
        let initial_pairing_host = initial_pairing_host(&state, HostId(1));
        let had_active_target = state.last_active_host.is_some();
        let state = storage_with_default_target(&state, HostId(1));
        if !had_active_target {
            log::info!("firmware: storage has no active target; restoring default host=1");
        }
        runtime_input
            .send(RuntimeInputMessage::RestoreStorage(state))
            .await;
        if initial_pairing_host.is_some() {
            log::info!("firmware: storage empty; opening initial pairing host=1");
            runtime_input
                .send(RuntimeInputMessage::ButtonIntent {
                    intent: ButtonIntent::EnterPairingMode,
                    now_ms: Instant::now().as_millis(),
                })
                .await;
        }
    } else {
        log::info!("firmware: storage empty; restoring default active target host=1");
        let mut state = StorageState::new(0);
        state.last_active_host = Some(HostId(1));
        runtime_input
            .send(RuntimeInputMessage::RestoreStorage(state))
            .await;
        log::info!("firmware: storage empty; opening initial pairing host=1");
        runtime_input
            .send(RuntimeInputMessage::ButtonIntent {
                intent: ButtonIntent::EnterPairingMode,
                now_ms: Instant::now().as_millis(),
            })
            .await;
    }
    loop {
        let now_ms = Instant::now().as_millis();
        match storage_policy.evaluate(&persistence, now_ms, active_ble_connections()) {
            StorageTaskAction::AwaitCommand => {
                match select(receiver.receive(), ESPNOW_MANAGEMENT_CHANNEL.receive()).await {
                    Either::First(command) => {
                        stage_storage_snapshot(&mut persistence, command.state, command.priority)
                    }
                    Either::Second(request) => {
                        handle_espnow_management(
                            request,
                            &mut espnow_service,
                            &mut backend,
                            ble_control,
                        )
                        .await;
                    }
                }
            }
            StorageTaskAction::WaitForDeadline { delay_ms }
            | StorageTaskAction::DeferForActiveBle { delay_ms } => {
                if matches!(
                    storage_policy.evaluate(&persistence, now_ms, active_ble_connections()),
                    StorageTaskAction::DeferForActiveBle { .. }
                ) {
                    log::debug!(
                        "firmware: storage_command defer flash write active_ble={} priority={:?}",
                        active_ble_connections(),
                        persistence.pending_priority()
                    );
                }
                match select3(
                    receiver.receive(),
                    ESPNOW_MANAGEMENT_CHANNEL.receive(),
                    Timer::after(Duration::from_millis(delay_ms)),
                )
                .await
                {
                    Either3::First(command) => {
                        stage_storage_snapshot(&mut persistence, command.state, command.priority)
                    }
                    Either3::Second(request) => {
                        handle_espnow_management(
                            request,
                            &mut espnow_service,
                            &mut backend,
                            ble_control,
                        )
                        .await;
                    }
                    Either3::Third(()) => {}
                }
            }
            StorageTaskAction::QuiesceAndPersist { forced } => {
                if forced {
                    log::info!(
                        "firmware: storage_command forcing ble quiesce for overdue critical persist"
                    );
                }
                let quiesce_snapshot =
                    quiesce_ble_for_flash_write(ble_quiesce_request, ble_quiesce_ready).await;
                let Some(state) = quiesce_snapshot else {
                    log::error!(
                        "firmware: storage_command aborting persist without runtime snapshot"
                    );
                    resume_ble_after_flash_write(ble_quiesce_done).await;
                    continue;
                };
                persistence.stage_quiesce_snapshot(state, Instant::now().as_millis());
                let usb_interrupt_guard = UsbInterruptQuiesceGuard::new();
                let persisted = persist_due_storage_snapshot(&mut persistence, &mut backend);
                drop(usb_interrupt_guard);
                if persisted {
                    runtime_input
                        .send(RuntimeInputMessage::DiagnosticsEvent(
                            hidshift::runtime::RuntimeDiagnosticsEvent::FlashWrite {
                                success: true,
                            },
                        ))
                        .await;
                    resume_ble_after_flash_write(ble_quiesce_done).await;
                } else {
                    runtime_input
                        .send(RuntimeInputMessage::DiagnosticsEvent(
                            hidshift::runtime::RuntimeDiagnosticsEvent::FlashWrite {
                                success: false,
                            },
                        ))
                        .await;
                    resume_ble_after_flash_write(ble_quiesce_done).await;
                }
            }
        }
    }
}

async fn quiesce_ble_for_flash_write(
    ble_quiesce_request: Sender<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_ready: Receiver<'static, CriticalSectionRawMutex, Option<StorageState>, 1>,
) -> Option<StorageState> {
    log::info!("firmware: storage_command requesting ble quiesce");
    ble_quiesce_request.send(()).await;
    let snapshot = ble_quiesce_ready.receive().await;
    log::info!("firmware: storage_command ble quiesce ready");
    snapshot
}

async fn resume_ble_after_flash_write(
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
) {
    ble_quiesce_done.send(()).await;
}

async fn handle_espnow_management(
    request: EspNowManagementRequest,
    service: &mut EspNowPairingService,
    backend: &mut FirmwareStorageBackend,
    ble_control: Sender<
        'static,
        CriticalSectionRawMutex,
        BleTaskCommand,
        RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    >,
) {
    let local = esp_hal::efuse::interface_mac_address(esp_hal::efuse::InterfaceMacAddress::Station);
    let local_address = local.as_bytes().try_into().unwrap_or([0; 6]);
    let mut outcome = service.handle(request.request.command, local_address);
    let restart = match outcome.action {
        EspNowPairingAction::None => false,
        EspNowPairingAction::Persist(pairing) => match backend.write_pairing(pairing) {
            Ok(()) => {
                service.persisted(pairing);
                true
            }
            Err(error) => {
                log::error!("firmware: ESP-NOW pairing persist failed: {:?}", error);
                outcome.result = ManagementResult::InternalError;
                false
            }
        },
        EspNowPairingAction::Clear => match backend.clear_pairing() {
            Ok(()) => {
                service.cleared();
                true
            }
            Err(error) => {
                log::error!("firmware: ESP-NOW pairing clear failed: {:?}", error);
                outcome.result = ManagementResult::InternalError;
                false
            }
        },
    };
    let response = ManagementResponse {
        request_id: request.request.request_id,
        result: outcome.result,
        payload: outcome.payload,
    };
    match request.destination {
        ManagementDestination::Wired => print_wired_response(response),
        ManagementDestination::Ble(host_id) => {
            ble_control
                .send(BleTaskCommand::ManagementResponse { host_id, response })
                .await;
        }
    }
    if restart && outcome.result == ManagementResult::Ok {
        Timer::after_millis(100).await;
        esp_hal::system::software_reset();
    }
}

fn print_wired_response(response: ManagementResponse) {
    crate::wired_management::print_response(response);
}

struct UsbInterruptQuiesceGuard {
    active: bool,
}

impl UsbInterruptQuiesceGuard {
    fn new() -> Self {
        log::info!("firmware: storage_command disabling USB interrupt for flash write");
        esp_hal::interrupt::disable(Cpu::ProCpu, Interrupt::USB);
        Self { active: true }
    }
}

impl Drop for UsbInterruptQuiesceGuard {
    fn drop(&mut self) {
        if self.active {
            esp_hal::interrupt::enable(Interrupt::USB, esp_hal::interrupt::Priority::max());
            self.active = false;
            log::info!("firmware: storage_command restored USB interrupt");
        }
    }
}

fn stage_storage_snapshot(
    persistence: &mut StoragePersistence,
    state: StorageState,
    priority: StoragePersistPriority,
) {
    log::info!(
        "firmware: storage_command staged generation={} priority={:?}",
        state.generation,
        priority
    );
    persistence.stage(state, priority, Instant::now().as_millis());
}

fn persist_due_storage_snapshot<B: StorageSlotBackend>(
    persistence: &mut StoragePersistence,
    backend: &mut B,
) -> bool {
    match persistence.persist_due(backend, Instant::now().as_millis()) {
        Ok(Some(result)) => {
            log::info!(
                "firmware: storage_command persisted slot={:?} generation={}",
                result.index,
                result.state.generation
            );
            true
        }
        Ok(None) => false,
        Err(error) => {
            log::error!("firmware: storage_command error {:?}", error);
            false
        }
    }
}
