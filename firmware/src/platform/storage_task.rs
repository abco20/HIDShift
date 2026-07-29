use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::peripherals::FLASH;
use esp_hal::{peripherals::Interrupt, system::Cpu};
use hidshift::ids::HostId;
use hidshift::runtime::bootstrap::storage_with_default_target;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    RUNTIME_INPUT_QUEUE_CAPACITY, RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY, StorageTaskCommand,
};
use hidshift::storage::{
    StorageError, StorageHealth, StoragePersistPriority, StoragePersistence, StorageSlotBackend,
    StorageState, StorageTaskAction, StorageTaskPolicy, restore_latest_storage_state,
};

use super::flash_backend::FirmwareStorageBackend;
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
use super::flash_backend::InMemoryStorageBackend;
#[cfg(not(all(feature = "hardware-e2e", feature = "dual-s3-wired")))]
use super::flash_backend::new_storage_backend;

pub const STORAGE_PERSIST_DEBOUNCE_MS: u64 = 1_000;
pub const STORAGE_PERSIST_LAZY_MS: u64 = 5_000;
pub const STORAGE_ACTIVE_BLE_RETRY_MS: u64 = 1_000;
pub const STORAGE_CRITICAL_FORCE_QUIESCE_MS: u64 = 5_000;
const STORAGE_QUIESCE_HANDSHAKE_TIMEOUT_MS: u64 = 2_000;

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
    flash: FLASH<'static>,
) {
    #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
    let mut backend = {
        let _ = flash;
        log::info!("firmware: dual-S3 E2E uses volatile Host settings storage");
        FirmwareStorageBackend::Volatile(InMemoryStorageBackend::new())
    };
    #[cfg(not(all(feature = "hardware-e2e", feature = "dual-s3-wired")))]
    let mut backend = new_storage_backend(flash);
    let mut persistence =
        StoragePersistence::new(STORAGE_PERSIST_DEBOUNCE_MS, STORAGE_PERSIST_LAZY_MS);
    let storage_policy = StorageTaskPolicy {
        active_ble_retry_ms: STORAGE_ACTIVE_BLE_RETRY_MS,
        critical_force_quiesce_ms: STORAGE_CRITICAL_FORCE_QUIESCE_MS,
    };
    log::info!("firmware: storage command task boot");

    let backend_health = backend.health();
    let mut reported_storage_health = backend_health;
    runtime_input
        .send(RuntimeInputMessage::StorageHealthChanged(backend_health))
        .await;
    let restored_state = restore_latest_storage_state(&backend);
    ble_restore.send(restored_state.clone()).await;
    if let Some(state) = restored_state {
        let had_active_target = state.last_active_host.is_some();
        let state = storage_with_default_target(&state, HostId(1));
        if !had_active_target {
            log::info!("firmware: storage has no active target; restoring default host=1");
        }
        runtime_input
            .send(RuntimeInputMessage::RestoreStorage(state))
            .await;
    } else {
        let state = StorageState::new(0);
        let state = storage_with_default_target(&state, HostId(1));
        #[cfg(not(feature = "dual-s3-wired"))]
        log::info!("firmware: storage empty; restoring default active target host=1");
        #[cfg(feature = "dual-s3-wired")]
        log::info!("firmware: storage empty; restoring default wired target");
        runtime_input
            .send(RuntimeInputMessage::RestoreStorage(state))
            .await;
    }
    loop {
        if backend_health == StorageHealth::Unavailable {
            let command = receiver.receive().await;
            log::error!(
                "firmware: rejected storage command {:?} because flash is unavailable",
                command
            );
            runtime_input
                .send(RuntimeInputMessage::DiagnosticsEvent(
                    hidshift::runtime::RuntimeDiagnosticsEvent::FlashWrite { success: false },
                ))
                .await;
            continue;
        }
        let now_ms = Instant::now().as_millis();
        match storage_policy.evaluate(&persistence, now_ms, active_ble_connections()) {
            StorageTaskAction::AwaitCommand => {
                let command = receiver.receive().await;
                handle_storage_command(
                    command,
                    &mut persistence,
                    &mut backend,
                    runtime_input,
                    ble_quiesce_request,
                    ble_quiesce_ready,
                    ble_quiesce_done,
                )
                .await;
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
                match select(
                    receiver.receive(),
                    Timer::after(Duration::from_millis(delay_ms)),
                )
                .await
                {
                    Either::First(command) => {
                        handle_storage_command(
                            command,
                            &mut persistence,
                            &mut backend,
                            runtime_input,
                            ble_quiesce_request,
                            ble_quiesce_ready,
                            ble_quiesce_done,
                        )
                        .await
                    }
                    Either::Second(()) => {}
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
                let next_health = persistence.effective_health(backend_health);
                if next_health != reported_storage_health {
                    reported_storage_health = next_health;
                    runtime_input
                        .send(RuntimeInputMessage::StorageHealthChanged(next_health))
                        .await;
                }
                if persisted.is_ok_and(|persisted| persisted) {
                    runtime_input
                        .send(RuntimeInputMessage::DiagnosticsEvent(
                            hidshift::runtime::RuntimeDiagnosticsEvent::FlashWrite {
                                success: true,
                            },
                        ))
                        .await;
                    resume_ble_after_flash_write(ble_quiesce_done).await;
                } else {
                    if let Err(error) = persisted {
                        log::error!("firmware: storage_command error {:?}", error);
                    }
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

async fn handle_storage_command(
    command: StorageTaskCommand,
    persistence: &mut StoragePersistence,
    backend: &mut FirmwareStorageBackend,
    runtime_input: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    ble_quiesce_request: Sender<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_ready: Receiver<'static, CriticalSectionRawMutex, Option<StorageState>, 1>,
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
) {
    match command {
        StorageTaskCommand::Persist { state, priority } => {
            stage_storage_snapshot(persistence, state, priority)
        }
        StorageTaskCommand::FactoryReset => {
            log::warn!("firmware: factory reset requested; quiescing BLE");
            let _ = quiesce_ble_for_flash_write(ble_quiesce_request, ble_quiesce_ready).await;
            let usb_interrupt_guard = UsbInterruptQuiesceGuard::new();
            let result = backend.factory_reset();
            drop(usb_interrupt_guard);
            match result {
                Ok(()) => {
                    log::warn!("firmware: factory reset complete; rebooting");
                    Timer::after(Duration::from_millis(100)).await;
                    esp_hal::system::software_reset();
                }
                Err(error) => {
                    log::error!("firmware: factory reset failed {:?}", error);
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
    let result = with_timeout(
        Duration::from_millis(STORAGE_QUIESCE_HANDSHAKE_TIMEOUT_MS),
        async {
            ble_quiesce_request.send(()).await;
            ble_quiesce_ready.receive().await
        },
    )
    .await;
    let snapshot = match result {
        Ok(snapshot) => snapshot,
        Err(_) => {
            log::error!("firmware: storage BLE quiesce handshake timed out; rebooting");
            esp_hal::system::software_reset();
        }
    };
    log::info!("firmware: storage_command ble quiesce ready");
    snapshot
}

async fn resume_ble_after_flash_write(
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
) {
    if with_timeout(
        Duration::from_millis(STORAGE_QUIESCE_HANDSHAKE_TIMEOUT_MS),
        ble_quiesce_done.send(()),
    )
    .await
    .is_err()
    {
        log::error!("firmware: storage BLE resume handshake timed out; rebooting");
        esp_hal::system::software_reset();
    }
}

struct UsbInterruptQuiesceGuard {
    active: bool,
}

impl UsbInterruptQuiesceGuard {
    fn new() -> Self {
        log::info!("firmware: storage_command disabling cache-unsafe interrupts for flash write");
        esp_hal::interrupt::disable(Cpu::ProCpu, Interrupt::USB);
        // The BLE task has already dropped its controller future. Ensure a
        // pending radio interrupt cannot enter esp-radio while flash erase
        // has the instruction cache disabled. Controller initialization
        // re-enables both sources after `ble_quiesce_done`.
        // SAFETY: BLE ownership is quiesced by the request/ready handshake
        // immediately before constructing this guard.
        unsafe {
            let bt = esp_hal::peripherals::BT::steal();
            bt.disable_rwble_interrupt_on_all_cores();
            bt.disable_bb_interrupt_on_all_cores();
        }
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
) -> Result<bool, StorageError> {
    match persistence.persist_due(backend, Instant::now().as_millis()) {
        Ok(Some(result)) => {
            log::info!(
                "firmware: storage_command persisted slot={:?} generation={}",
                result.index,
                result.state.generation
            );
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(error) => Err(error),
    }
}
