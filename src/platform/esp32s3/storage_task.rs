use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Instant, Timer};
#[cfg(feature = "storage")]
use esp_hal::peripherals::FLASH;
#[cfg(all(feature = "storage", feature = "usb-host"))]
use esp_hal::{peripherals::Interrupt, system::Cpu};
#[cfg(not(feature = "storage"))]
use hidshift::BridgeEvent;
use hidshift::ids::HostId;
use hidshift::runtime::bootstrap::{initial_pairing_host, storage_with_default_target};
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    RUNTIME_INPUT_QUEUE_CAPACITY, RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY, StorageTaskCommand,
};
use hidshift::storage::{
    StoragePersistPriority, StoragePersistence, StorageSlotBackend, StorageState,
    StorageTaskAction, StorageTaskPolicy, restore_latest_storage_state,
};
use hidshift::target_control::ButtonIntent;

use super::flash_backend::new_storage_backend;

pub const STORAGE_PERSIST_DEBOUNCE_MS: u64 = 1_000;
pub const STORAGE_PERSIST_LAZY_MS: u64 = 5_000;
pub const STORAGE_ACTIVE_BLE_RETRY_MS: u64 = 1_000;
pub const STORAGE_CRITICAL_FORCE_QUIESCE_MS: u64 = 5_000;

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
    #[cfg(feature = "ble-hid")] ble_restore: Sender<
        'static,
        CriticalSectionRawMutex,
        Option<StorageState>,
        1,
    >,
    #[cfg(all(feature = "ble-hid", feature = "storage"))] ble_quiesce_request: Sender<
        'static,
        CriticalSectionRawMutex,
        (),
        1,
    >,
    #[cfg(all(feature = "ble-hid", feature = "storage"))] ble_quiesce_ready: Receiver<
        'static,
        CriticalSectionRawMutex,
        Option<StorageState>,
        1,
    >,
    #[cfg(all(feature = "ble-hid", feature = "storage"))] ble_quiesce_done: Sender<
        'static,
        CriticalSectionRawMutex,
        (),
        1,
    >,
    active_ble_connections: fn() -> usize,
    #[cfg(feature = "storage")] flash: FLASH<'static>,
) {
    #[cfg(feature = "storage")]
    let mut backend = new_storage_backend(flash);
    #[cfg(not(feature = "storage"))]
    let mut backend = new_storage_backend();
    let mut persistence =
        StoragePersistence::new(STORAGE_PERSIST_DEBOUNCE_MS, STORAGE_PERSIST_LAZY_MS);
    let storage_policy = StorageTaskPolicy {
        active_ble_retry_ms: STORAGE_ACTIVE_BLE_RETRY_MS,
        critical_force_quiesce_ms: STORAGE_CRITICAL_FORCE_QUIESCE_MS,
    };
    log::info!("firmware: storage command task boot");

    let restored_state = restore_latest_storage_state(&backend);
    #[cfg(feature = "ble-hid")]
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
    #[cfg(not(feature = "storage"))]
    {
        log::info!("firmware: storage disabled; default active target host=1");
        runtime_input
            .send(RuntimeInputMessage::BridgeEvent(
                BridgeEvent::SwitchTarget { target: HostId(1) },
            ))
            .await;
    }

    loop {
        let now_ms = Instant::now().as_millis();
        match storage_policy.evaluate(&persistence, now_ms, active_ble_connections()) {
            StorageTaskAction::AwaitCommand => {
                let command = receiver.receive().await;
                stage_storage_snapshot(&mut persistence, command.state, command.priority);
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
                        stage_storage_snapshot(&mut persistence, command.state, command.priority)
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
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                let quiesce_snapshot = quiesce_ble_for_flash_write(
                    #[cfg(all(feature = "ble-hid", feature = "storage"))]
                    ble_quiesce_request,
                    #[cfg(all(feature = "ble-hid", feature = "storage"))]
                    ble_quiesce_ready,
                )
                .await;
                #[cfg(not(all(feature = "ble-hid", feature = "storage")))]
                quiesce_ble_for_flash_write().await;
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                let Some(state) = quiesce_snapshot else {
                    log::error!(
                        "firmware: storage_command aborting persist without runtime snapshot"
                    );
                    resume_ble_after_flash_write(ble_quiesce_done).await;
                    continue;
                };
                #[cfg(all(feature = "ble-hid", feature = "storage"))]
                persistence.stage_quiesce_snapshot(state, Instant::now().as_millis());
                let usb_interrupt_guard = UsbInterruptQuiesceGuard::new();
                let persisted = persist_due_storage_snapshot(&mut persistence, &mut backend);
                drop(usb_interrupt_guard);
                if persisted {
                    reset_after_flash_write_if_required();
                } else {
                    runtime_input
                        .send(RuntimeInputMessage::DiagnosticsEvent(
                            hidshift::runtime::RuntimeDiagnosticsEvent::FlashWrite {
                                success: false,
                            },
                        ))
                        .await;
                    resume_ble_after_flash_write(
                        #[cfg(all(feature = "ble-hid", feature = "storage"))]
                        ble_quiesce_done,
                    )
                    .await;
                }
            }
        }
    }
}

async fn quiesce_ble_for_flash_write(
    #[cfg(all(feature = "ble-hid", feature = "storage"))] ble_quiesce_request: Sender<
        'static,
        CriticalSectionRawMutex,
        (),
        1,
    >,
    #[cfg(all(feature = "ble-hid", feature = "storage"))] ble_quiesce_ready: Receiver<
        'static,
        CriticalSectionRawMutex,
        Option<StorageState>,
        1,
    >,
) -> Option<StorageState> {
    #[cfg(all(feature = "ble-hid", feature = "storage"))]
    {
        log::info!("firmware: storage_command requesting ble quiesce");
        ble_quiesce_request.send(()).await;
        let snapshot = ble_quiesce_ready.receive().await;
        log::info!("firmware: storage_command ble quiesce ready");
        return snapshot;
    }
    #[cfg(not(all(feature = "ble-hid", feature = "storage")))]
    None
}

async fn resume_ble_after_flash_write(
    #[cfg(all(feature = "ble-hid", feature = "storage"))] ble_quiesce_done: Sender<
        'static,
        CriticalSectionRawMutex,
        (),
        1,
    >,
) {
    #[cfg(all(feature = "ble-hid", feature = "storage"))]
    {
        ble_quiesce_done.send(()).await;
    }
}

fn reset_after_flash_write_if_required() {
    #[cfg(all(feature = "ble-hid", feature = "storage"))]
    {
        log::info!("firmware: storage_command reset after flash write");
        esp_hal::system::software_reset();
    }
}

struct UsbInterruptQuiesceGuard {
    #[cfg(all(feature = "storage", feature = "usb-host"))]
    active: bool,
}

impl UsbInterruptQuiesceGuard {
    fn new() -> Self {
        #[cfg(all(feature = "storage", feature = "usb-host"))]
        {
            log::info!("firmware: storage_command disabling USB interrupt for flash write");
            esp_hal::interrupt::disable(Cpu::ProCpu, Interrupt::USB);
            return Self { active: true };
        }
        #[cfg(not(all(feature = "storage", feature = "usb-host")))]
        Self {}
    }
}

impl Drop for UsbInterruptQuiesceGuard {
    fn drop(&mut self) {
        #[cfg(all(feature = "storage", feature = "usb-host"))]
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
