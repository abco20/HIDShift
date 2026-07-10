use alloc::boxed::Box;
use core::future::pending;
use core::sync::atomic::{AtomicUsize, Ordering};

use embassy_futures::join::join;
use embassy_futures::select::{Either, Either3, Either4, select, select3, select4};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::Timer;
use esp_hal::rng::{Trng, TrngSource};
use esp_radio::ble::controller::BleConnector;
use hidshift::ble_runtime::{
    BleHidAttributeHandles, connected_message, disconnected_message, gatt_write_message,
    security_changed_message,
};
use hidshift::ids::HostId;
use hidshift::reports::{
    HID_INFORMATION, INPUT_REPORT_TYPE, KEYBOARD_REPORT_ID, OUTPUT_REPORT_TYPE,
    V1_CONSUMER_REPORT_MAP, V1_KEYBOARD_REPORT_MAP, V1_MOUSE_REPORT_MAP,
};
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    BleTaskCommand, RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY, RUNTIME_HOSTS_MAX, RUNTIME_INPUT_QUEUE_CAPACITY,
};
use hidshift::storage::{StorageState, StoredBond, StoredSecurityLevel};
use hidshift::{
    BLE_HID_NOTIFICATIONS_PER_REPORT_MAX, BleConnectionSlots, BlePeerIdentity,
    notifications_for_input_report, resolve_ble_host_id, typed_notification,
};
use trouble_host::prelude::*;

const BLE_DEVICE_NAME: &str = "HIDShift";
const BLE_CONNECTIONS_MAX: usize = 4;
// One ATT bearer plus one spare control/data lane per connection.
const BLE_L2CAP_CHANNELS_MAX: usize = BLE_CONNECTIONS_MAX * 2;
const BLE_ATTRIBUTE_TABLE_SIZE: usize = 64;

static BLE_ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
static BLE_ACTIVE_HOST_MASK: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(feature = "storage")]
pub struct BleRuntimeSnapshot {
    pub storage: Option<StorageState>,
    pub pairable_host: Option<HostId>,
}
#[allow(dead_code)]
#[gatt_server(
    connections_max = BLE_CONNECTIONS_MAX,
    mutex_type = NoopRawMutex,
    attribute_table_size = BLE_ATTRIBUTE_TABLE_SIZE
)]
struct Server {
    keyboard_hid: KeyboardHidService,
    mouse_hid: MouseHidService,
    consumer_hid: ConsumerHidService,
    device_information: DeviceInformationService,
}

#[gatt_service(uuid = "00001812-0000-1000-8000-00805f9b34fb")]
struct KeyboardHidService {
    #[characteristic(uuid = "00002a4a-0000-1000-8000-00805f9b34fb", read, value = HID_INFORMATION)]
    hid_information: [u8; 4],
    #[characteristic(uuid = "00002a4b-0000-1000-8000-00805f9b34fb", read, value = V1_KEYBOARD_REPORT_MAP)]
    report_map: &'static [u8],
    #[characteristic(
        uuid = "00002a4c-0000-1000-8000-00805f9b34fb",
        write_without_response,
        value = 0
    )]
    control_point: u8,
    #[descriptor(uuid = "00002908-0000-1000-8000-00805f9b34fb", read, value = [KEYBOARD_REPORT_ID, INPUT_REPORT_TYPE])]
    #[characteristic(uuid = "00002a4d-0000-1000-8000-00805f9b34fb", read, notify, value = [0; 8])]
    input_report: [u8; 8],
    #[descriptor(uuid = "00002908-0000-1000-8000-00805f9b34fb", read, value = [KEYBOARD_REPORT_ID, OUTPUT_REPORT_TYPE])]
    #[characteristic(uuid = "00002a4d-0000-1000-8000-00805f9b34fb", read, write, write_without_response, value = [0])]
    output_report: [u8; 1],
}

#[gatt_service(uuid = "00001812-0000-1000-8000-00805f9b34fb")]
struct MouseHidService {
    #[characteristic(uuid = "00002a4a-0000-1000-8000-00805f9b34fb", read, value = HID_INFORMATION)]
    hid_information: [u8; 4],
    #[characteristic(uuid = "00002a4b-0000-1000-8000-00805f9b34fb", read, value = V1_MOUSE_REPORT_MAP)]
    report_map: &'static [u8],
    #[characteristic(
        uuid = "00002a4c-0000-1000-8000-00805f9b34fb",
        write_without_response,
        value = 0
    )]
    control_point: u8,
    #[descriptor(uuid = "00002908-0000-1000-8000-00805f9b34fb", read, value = [0, INPUT_REPORT_TYPE])]
    #[characteristic(uuid = "00002a4d-0000-1000-8000-00805f9b34fb", read, notify, value = [0; 5])]
    input_report: [u8; 5],
}

#[gatt_service(uuid = "00001812-0000-1000-8000-00805f9b34fb")]
struct ConsumerHidService {
    #[characteristic(uuid = "00002a4a-0000-1000-8000-00805f9b34fb", read, value = HID_INFORMATION)]
    hid_information: [u8; 4],
    #[characteristic(uuid = "00002a4b-0000-1000-8000-00805f9b34fb", read, value = V1_CONSUMER_REPORT_MAP)]
    report_map: &'static [u8],
    #[characteristic(
        uuid = "00002a4c-0000-1000-8000-00805f9b34fb",
        write_without_response,
        value = 0
    )]
    control_point: u8,
    #[descriptor(uuid = "00002908-0000-1000-8000-00805f9b34fb", read, value = [0, INPUT_REPORT_TYPE])]
    #[characteristic(uuid = "00002a4d-0000-1000-8000-00805f9b34fb", read, notify, value = [0; 2])]
    input_report: [u8; 2],
}

#[gatt_service(uuid = "0000180a-0000-1000-8000-00805f9b34fb")]
struct DeviceInformationService {
    #[characteristic(
        uuid = "00002a29-0000-1000-8000-00805f9b34fb",
        read,
        value = "HIDShift"
    )]
    manufacturer_name: &'static str,
    #[characteristic(
        uuid = "00002a24-0000-1000-8000-00805f9b34fb",
        read,
        value = "firmware"
    )]
    model_number: &'static str,
}

#[cfg(feature = "storage")]
pub fn active_ble_connections() -> usize {
    BLE_ACTIVE_CONNECTIONS.load(Ordering::Relaxed)
}

#[embassy_executor::task]
pub async fn ble_host_event_task(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
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
    restore_receiver: Receiver<'static, CriticalSectionRawMutex, Option<StorageState>, 1>,
    #[cfg(feature = "storage")] quiesce_request: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    #[cfg(feature = "storage")] quiesce_ready: Sender<
        'static,
        CriticalSectionRawMutex,
        Option<StorageState>,
        1,
    >,
    #[cfg(feature = "storage")] quiesce_done: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    #[cfg(feature = "storage")] usb_quiesce_request: Receiver<
        'static,
        CriticalSectionRawMutex,
        (),
        1,
    >,
    #[cfg(feature = "storage")] usb_quiesce_ready: Sender<'static, CriticalSectionRawMutex, (), 1>,
    #[cfg(feature = "storage")] usb_quiesce_done: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    #[cfg(feature = "storage")] runtime_barrier_request: Sender<
        'static,
        CriticalSectionRawMutex,
        usize,
        1,
    >,
    #[cfg(feature = "storage")] runtime_barrier_done: Receiver<
        'static,
        CriticalSectionRawMutex,
        BleRuntimeSnapshot,
        1,
    >,
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
) {
    log::info!("firmware: ble host event task boot");

    let _trng_source = TrngSource::new(rng, adc1);
    let mut trng = match Trng::try_new() {
        Ok(trng) => trng,
        Err(error) => {
            log::error!(
                "firmware: TRNG initialization failed: {:?}; resetting",
                error
            );
            Timer::after_secs(1).await;
            esp_hal::system::software_reset();
        }
    };
    let restored_state = restore_receiver.receive().await;
    #[cfg(feature = "storage")]
    let mut current_storage = restored_state;
    #[cfg(not(feature = "storage"))]
    let current_storage = restored_state;
    #[cfg(feature = "storage")]
    let mut pairable_host = None;
    #[cfg(not(feature = "storage"))]
    let pairable_host = None;
    let mut bt = Some(bt);
    let server = match Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: BLE_DEVICE_NAME,
        appearance: &appearance::human_interface_device::KEYBOARD,
    })) {
        Ok(server) => Box::leak(Box::new(server)),
        Err(error) => {
            log::error!(
                "firmware: GATT server initialization failed: {:?}; resetting",
                error
            );
            Timer::after_secs(1).await;
            esp_hal::system::software_reset();
        }
    };
    retain_gatt_service_fields(server);

    loop {
        let connector = match BleConnector::new(
            match bt.take() {
                Some(bt) => bt,
                None => unsafe { esp_hal::peripherals::BT::steal() },
            },
            Default::default(),
        ) {
            Ok(connector) => connector,
            Err(error) => {
                log::error!(
                    "firmware: BLE controller initialization failed: {:?}; retrying",
                    error
                );
                Timer::after_secs(1).await;
                continue;
            }
        };
        let controller: ExternalController<_, 20> = ExternalController::new(connector);

        #[cfg(feature = "storage")]
        match select3(
            run_ble_host_events(
                controller,
                &mut trng,
                sender,
                control_receiver,
                notify_receiver,
                server,
                current_storage.as_ref(),
                pairable_host,
            ),
            quiesce_request.receive(),
            usb_quiesce_request.receive(),
        )
        .await
        {
            Either3::First(()) => {}
            Either3::Second(()) => {
                let snapshot = disconnect_runtime_hosts_before_quiesce(
                    runtime_barrier_request,
                    runtime_barrier_done,
                )
                .await;
                if let Some(storage) = snapshot.storage.clone() {
                    current_storage = Some(storage);
                }
                pairable_host = snapshot.pairable_host;
                log::info!("firmware: ble quiesced for flash write");
                quiesce_ready.send(snapshot.storage).await;
                quiesce_done.receive().await;
            }
            Either3::Third(()) => {
                let snapshot = disconnect_runtime_hosts_before_quiesce(
                    runtime_barrier_request,
                    runtime_barrier_done,
                )
                .await;
                if let Some(storage) = snapshot.storage {
                    current_storage = Some(storage);
                }
                pairable_host = snapshot.pairable_host;
                log::info!("firmware: ble quiesced for usb enumeration");
                usb_quiesce_ready.send(()).await;
                usb_quiesce_done.receive().await;
            }
        }

        #[cfg(not(feature = "storage"))]
        run_ble_host_events(
            controller,
            &mut trng,
            sender,
            control_receiver,
            notify_receiver,
            server,
            current_storage.as_ref(),
            pairable_host,
        )
        .await;
    }
}

#[cfg(feature = "storage")]
async fn disconnect_runtime_hosts_before_quiesce(
    barrier_request: Sender<'static, CriticalSectionRawMutex, usize, 1>,
    barrier_done: Receiver<'static, CriticalSectionRawMutex, BleRuntimeSnapshot, 1>,
) -> BleRuntimeSnapshot {
    let host_mask = BLE_ACTIVE_HOST_MASK.swap(0, Ordering::AcqRel);
    BLE_ACTIVE_CONNECTIONS.store(0, Ordering::Release);
    barrier_request.send(host_mask).await;
    barrier_done.receive().await
}

async fn run_ble_host_events<'server, C>(
    controller: C,
    rng: &mut Trng,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
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
    server: &'server Server<'server>,
    restored_state: Option<&StorageState>,
    pairable_host: Option<HostId>,
) where
    C: Controller,
{
    let mut resources: HostResources<
        DefaultPacketPool,
        BLE_CONNECTIONS_MAX,
        BLE_L2CAP_CHANNELS_MAX,
    > = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_generator_seed(rng);
    stack.set_io_capabilities(IoCapabilities::NoInputNoOutput);
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();
    if let Some(restored_state) = restored_state {
        restore_ble_bonds(&stack, restored_state);
    }

    log::info!("firmware: ble advertising as {}", BLE_DEVICE_NAME);
    let accept = accept_ble_connections(
        &stack,
        &mut peripheral,
        server,
        sender,
        control_receiver,
        notify_receiver,
        restored_state,
        pairable_host,
    );
    let _ = join(ble_runner_task(runner), accept).await;
}

fn restore_ble_bonds<C, P>(stack: &Stack<'_, C, P>, storage: &StorageState)
where
    C: Controller,
    P: PacketPool,
{
    let mut restored = 0usize;
    for host in storage.hosts().iter() {
        let Some(bond) = host.bond else {
            continue;
        };
        match trouble_bond_from_stored(bond) {
            Some(bond_information) => {
                log::debug!(
                    "firmware: restoring bond host={} identity={:?} bonded={} level={:?}",
                    host.host_id.0,
                    bond_information.identity,
                    bond_information.is_bonded,
                    bond_information.security_level
                );
                if let Err(err) = stack.add_bond_information(bond_information) {
                    log::error!(
                        "firmware: failed to restore bond for host={} err={:?}",
                        host.host_id.0,
                        err
                    );
                } else {
                    restored += 1;
                }
            }
            None => {
                log::warn!("firmware: invalid stored bond for host={}", host.host_id.0);
            }
        }
    }
    let stack_bonds = stack.get_bond_information();
    log::info!(
        "firmware: restored {} bond(s); stack now has {} bond(s)",
        restored,
        stack_bonds.len()
    );
}

fn stored_bond_from_trouble(bond: BondInformation) -> StoredBond {
    StoredBond {
        peer_address: bond.identity.bd_addr.into_inner(),
        peer_irk: bond.identity.irk.map(|irk| irk.to_le_bytes()),
        ltk: bond.ltk.to_le_bytes(),
        is_bonded: bond.is_bonded,
        security_level: match bond.security_level {
            SecurityLevel::NoEncryption => StoredSecurityLevel::NoEncryption,
            SecurityLevel::Encrypted => StoredSecurityLevel::Encrypted,
            SecurityLevel::EncryptedAuthenticated => StoredSecurityLevel::EncryptedAuthenticated,
        },
    }
}

fn trouble_bond_from_stored(bond: StoredBond) -> Option<BondInformation> {
    let identity = Identity {
        bd_addr: BdAddr::new(bond.peer_address),
        irk: bond.peer_irk.map(IdentityResolvingKey::from_le_bytes),
    };
    Some(BondInformation::new(
        identity,
        LongTermKey::from_le_bytes(bond.ltk),
        match bond.security_level {
            StoredSecurityLevel::NoEncryption => SecurityLevel::NoEncryption,
            StoredSecurityLevel::Encrypted => SecurityLevel::Encrypted,
            StoredSecurityLevel::EncryptedAuthenticated => SecurityLevel::EncryptedAuthenticated,
        },
        bond.is_bonded,
    ))
}

fn retain_gatt_service_fields(server: &Server) {
    let _ = &server.device_information;
}

#[derive(Clone, Copy, Debug)]
struct BleControlState {
    pairing_allowed: [bool; RUNTIME_HOSTS_MAX],
    restored_bond: [bool; RUNTIME_HOSTS_MAX],
}

impl BleControlState {
    fn new(restored_state: Option<&StorageState>, pairable_host: Option<HostId>) -> Self {
        let mut state = Self {
            pairing_allowed: [false; RUNTIME_HOSTS_MAX],
            restored_bond: [false; RUNTIME_HOSTS_MAX],
        };
        if let Some(storage) = restored_state {
            for host in storage.hosts() {
                if host.bond.is_some() {
                    state.set_restored_bond(host.host_id, true);
                }
            }
        }
        if let Some(host_id) = pairable_host {
            state.set_pairing_allowed(host_id, true);
        }
        state
    }

    fn apply_command(&mut self, command: BleTaskCommand) {
        match command {
            BleTaskCommand::Notify { .. } => {}
            BleTaskCommand::AllowPairing { host_id } => self.set_pairing_allowed(host_id, true),
            BleTaskCommand::RejectPairing { host_id } => self.set_pairing_allowed(host_id, false),
            BleTaskCommand::ClearBond { host_id, .. } => {
                self.set_pairing_allowed(host_id, false);
                self.set_restored_bond(host_id, false);
            }
        }
    }

    fn pairing_allowed(&self, host_id: HostId) -> bool {
        host_id_index(host_id)
            .and_then(|index| self.pairing_allowed.get(index))
            .copied()
            .unwrap_or(false)
    }

    fn restored_bond(&self, host_id: HostId) -> bool {
        host_id_index(host_id)
            .and_then(|index| self.restored_bond.get(index))
            .copied()
            .unwrap_or(false)
    }

    fn pairing_host(&self) -> Option<HostId> {
        self.pairing_allowed
            .iter()
            .position(|allowed| *allowed)
            .map(|index| HostId((index + 1) as u8))
    }

    fn set_pairing_allowed(&mut self, host_id: HostId, allowed: bool) {
        if let Some(index) = host_id_index(host_id) {
            if index < self.pairing_allowed.len() {
                self.pairing_allowed[index] = allowed;
            }
        }
    }

    fn set_restored_bond(&mut self, host_id: HostId, restored: bool) {
        if let Some(index) = host_id_index(host_id) {
            if index < self.restored_bond.len() {
                self.restored_bond[index] = restored;
            }
        }
    }
}

fn host_id_index(host_id: HostId) -> Option<usize> {
    host_id.0.checked_sub(1).map(|index| index as usize)
}

fn connection_peer_identity<P>(conn: &GattConnection<'_, '_, P>) -> BlePeerIdentity
where
    P: PacketPool,
{
    let identity = conn.raw().peer_identity();
    BlePeerIdentity {
        peer_address: identity.bd_addr.into_inner(),
        peer_irk: identity.irk.map(|irk| irk.to_le_bytes()),
    }
}

fn resolve_connection_host_id<P>(
    conn: &GattConnection<'_, '_, P>,
    restored_state: Option<&StorageState>,
    control: &BleControlState,
) -> Option<HostId>
where
    P: PacketPool,
{
    let peer_identity = connection_peer_identity(conn);
    resolve_ble_host_id(restored_state, peer_identity, control.pairing_host())
}

async fn accept_ble_connections<'values, 'server, C>(
    stack: &Stack<'values, C, DefaultPacketPool>,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'server>,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
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
    restored_state: Option<&StorageState>,
    pairable_host: Option<HostId>,
) where
    C: Controller,
{
    let mut control = BleControlState::new(restored_state, pairable_host);
    loop {
        match select(
            advertise_ble(peripheral, server),
            receive_ble_command(control_receiver, notify_receiver),
        )
        .await
        {
            Either::First(Ok(conn)) => {
                let Some(host_id) = resolve_connection_host_id(&conn, restored_state, &control)
                else {
                    log::warn!(
                        "firmware: rejecting unknown peer outside pairing identity={:?}",
                        conn.raw().peer_identity()
                    );
                    if let Err(err) = conn.raw().set_bondable(false) {
                        log::warn!(
                            "firmware: ble set_bondable failed while rejecting unknown peer: {:?}",
                            err
                        );
                    }
                    conn.raw().disconnect();
                    continue;
                };
                configure_ble_connection(
                    0,
                    host_id,
                    &conn,
                    sender,
                    control.pairing_allowed(host_id),
                    control.restored_bond(host_id),
                )
                .await;
                manage_ble_connections(
                    stack,
                    peripheral,
                    server,
                    sender,
                    control_receiver,
                    notify_receiver,
                    Some(conn),
                    None,
                    None,
                    None,
                    restored_state,
                    &mut control,
                )
                .await;
            }
            Either::First(Err(err)) => {
                log::warn!("firmware: ble advertising failed: {:?}", err);
            }
            Either::Second(command) => {
                apply_ble_stack_command(stack, command);
                control.apply_command(command);
                log_ble_command_without_connection(command);
            }
        }
    }
}

async fn manage_ble_connections<'values, 'server, C>(
    stack: &Stack<'values, C, DefaultPacketPool>,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'server>,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
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
    slot0: Option<GattConnection<'values, 'server, DefaultPacketPool>>,
    slot1: Option<GattConnection<'values, 'server, DefaultPacketPool>>,
    slot2: Option<GattConnection<'values, 'server, DefaultPacketPool>>,
    slot3: Option<GattConnection<'values, 'server, DefaultPacketPool>>,
    restored_state: Option<&StorageState>,
    control: &mut BleControlState,
) where
    C: Controller,
{
    let mut slots = BleConnectionSlots::<BLE_CONNECTIONS_MAX>::new();
    let mut connection_slots = [
        BleConnectionTaskSlot::new(slot0),
        BleConnectionTaskSlot::new(slot1),
        BleConnectionTaskSlot::new(slot2),
        BleConnectionTaskSlot::new(slot3),
    ];
    for (slot, state) in connection_slots.iter().enumerate() {
        initialize_ble_slot(
            &mut slots,
            slot,
            state.conn.as_ref(),
            restored_state,
            control,
        );
    }

    loop {
        if slots.connected_count() == 0 {
            break;
        }

        match select3(
            advertise_if_slot_available(peripheral, server, slots.should_advertise()),
            process_slot_events(&mut connection_slots, &slots, server, sender),
            receive_ble_command(control_receiver, notify_receiver),
        )
        .await
        {
            Either3::First(Ok(conn)) => {
                let Some(host_id) = resolve_connection_host_id(&conn, restored_state, control)
                else {
                    log::warn!(
                        "firmware: rejecting unknown peer outside pairing identity={:?}",
                        conn.raw().peer_identity()
                    );
                    if let Err(err) = conn.raw().set_bondable(false) {
                        log::warn!(
                            "firmware: ble set_bondable failed while rejecting unknown peer: {:?}",
                            err
                        );
                    }
                    conn.raw().disconnect();
                    continue;
                };
                let peer_identity = connection_peer_identity(&conn);
                match slots.connect_first_free(host_id, peer_identity) {
                    Ok(assigned_slot) => {
                        configure_ble_connection(
                            assigned_slot.index(),
                            host_id,
                            &conn,
                            sender,
                            control.pairing_allowed(host_id),
                            control.restored_bond(host_id),
                        )
                        .await;
                        assign_connection_to_slot(
                            &mut connection_slots,
                            assigned_slot.index(),
                            conn,
                        );
                    }
                    Err(err) => {
                        log::error!("firmware: ble slot state error {:?}", err);
                        conn.raw().disconnect();
                    }
                }
            }
            Either3::First(Err(err)) => {
                log::warn!("firmware: ble advertising failed: {:?}", err);
            }
            Either3::Second(slot_event) => {
                let (slot, progress) = slot_event;
                if let ConnectionProgress::Disconnected = progress {
                    if let Err(err) = slots.set_disconnected(slot) {
                        log::error!("firmware: ble slot state error {:?}", err);
                    }
                    clear_connection_slot(&mut connection_slots, slot);
                }
            }
            Either3::Third(command) => {
                control.apply_command(command);
                dispatch_ble_command_to_connected_slot(
                    server,
                    &mut slots,
                    &mut connection_slots,
                    command,
                    sender,
                )
                .await;
                apply_ble_stack_command(stack, command);
            }
        }
    }
}

struct BleConnectionTaskSlot<'values, 'server> {
    conn: Option<GattConnection<'values, 'server, DefaultPacketPool>>,
    encryption_reported: bool,
}

impl<'values, 'server> BleConnectionTaskSlot<'values, 'server> {
    const fn new(conn: Option<GattConnection<'values, 'server, DefaultPacketPool>>) -> Self {
        Self {
            conn,
            encryption_reported: false,
        }
    }
}

fn initialize_ble_slot<P>(
    slots: &mut BleConnectionSlots<BLE_CONNECTIONS_MAX>,
    slot: usize,
    conn: Option<&GattConnection<'_, '_, P>>,
    restored_state: Option<&StorageState>,
    control: &BleControlState,
) where
    P: PacketPool,
{
    let Some(conn) = conn else {
        return;
    };
    let Some(host_id) = resolve_connection_host_id(conn, restored_state, control) else {
        log::info!(
            "firmware: skipping slot init for unknown peer identity={:?}",
            conn.raw().peer_identity()
        );
        return;
    };
    let peer_identity = connection_peer_identity(conn);
    if let Err(err) = slots.set_connected(slot, host_id, peer_identity) {
        log::error!("firmware: ble slot state error {:?}", err);
    }
}

async fn advertise_if_slot_available<'values, 'server, C>(
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'server>,
    should_advertise: bool,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>>
where
    C: Controller,
{
    if should_advertise {
        advertise_ble(peripheral, server).await
    } else {
        pending().await
    }
}

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
    match select(control_receiver.receive(), notify_receiver.receive()).await {
        Either::First(command) | Either::Second(command) => command,
    }
}

async fn process_slot_event<P>(
    slot: usize,
    host_id: Option<HostId>,
    conn: Option<&GattConnection<'_, '_, P>>,
    server: &Server<'_>,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    encryption_reported: &mut bool,
) -> (usize, ConnectionProgress)
where
    P: PacketPool,
{
    match (host_id, conn) {
        (Some(host_id), Some(conn)) => (
            slot,
            process_gatt_event(slot, host_id, conn, server, sender, encryption_reported).await,
        ),
        _ => pending().await,
    }
}

async fn process_slot_events<'values, 'server>(
    connection_slots: &mut [BleConnectionTaskSlot<'values, 'server>; BLE_CONNECTIONS_MAX],
    slots: &BleConnectionSlots<BLE_CONNECTIONS_MAX>,
    server: &Server<'_>,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
) -> (usize, ConnectionProgress) {
    let (slot0_ref, rest) = connection_slots.split_at_mut(1);
    let (slot1_ref, rest) = rest.split_at_mut(1);
    let (slot2_ref, slot3_ref) = rest.split_at_mut(1);

    let slot0 = process_slot_event(
        0,
        slots.host_for_slot(0),
        slot0_ref[0].conn.as_ref(),
        server,
        sender,
        &mut slot0_ref[0].encryption_reported,
    );
    let slot1 = process_slot_event(
        1,
        slots.host_for_slot(1),
        slot1_ref[0].conn.as_ref(),
        server,
        sender,
        &mut slot1_ref[0].encryption_reported,
    );
    let slot2 = process_slot_event(
        2,
        slots.host_for_slot(2),
        slot2_ref[0].conn.as_ref(),
        server,
        sender,
        &mut slot2_ref[0].encryption_reported,
    );
    let slot3 = process_slot_event(
        3,
        slots.host_for_slot(3),
        slot3_ref[0].conn.as_ref(),
        server,
        sender,
        &mut slot3_ref[0].encryption_reported,
    );

    match select4(slot0, slot1, slot2, slot3).await {
        Either4::First(event)
        | Either4::Second(event)
        | Either4::Third(event)
        | Either4::Fourth(event) => event,
    }
}

fn assign_connection_to_slot<'values, 'server>(
    connection_slots: &mut [BleConnectionTaskSlot<'values, 'server>; BLE_CONNECTIONS_MAX],
    slot: usize,
    conn: GattConnection<'values, 'server, DefaultPacketPool>,
) {
    if let Some(entry) = connection_slots.get_mut(slot) {
        entry.conn = Some(conn);
        entry.encryption_reported = false;
    }
}

fn clear_connection_slot<'values, 'server>(
    connection_slots: &mut [BleConnectionTaskSlot<'values, 'server>; BLE_CONNECTIONS_MAX],
    slot: usize,
) {
    if let Some(entry) = connection_slots.get_mut(slot) {
        entry.conn = None;
        entry.encryption_reported = false;
    }
}

fn apply_ble_stack_command<C, P>(stack: &Stack<'_, C, P>, command: BleTaskCommand)
where
    C: Controller,
    P: PacketPool,
{
    if let BleTaskCommand::ClearBond {
        bond: Some(bond), ..
    } = command
    {
        let Some(bond_information) = trouble_bond_from_stored(bond) else {
            return;
        };
        if let Err(err) = stack.remove_bond_information(bond_information.identity) {
            log::error!("firmware: clear bond failed: {:?}", err);
        }
    }
}

async fn dispatch_ble_command_to_connected_slot(
    server: &Server<'_>,
    slots: &mut BleConnectionSlots<BLE_CONNECTIONS_MAX>,
    connection_slots: &mut [BleConnectionTaskSlot<'_, '_>; BLE_CONNECTIONS_MAX],
    command: BleTaskCommand,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
) {
    let host_id = ble_command_host_id(command);
    let Some(slot_index) = slots
        .dispatch_slot_for_host(host_id)
        .map(|slot| slot.index())
    else {
        log_missing_ble_slot(host_id);
        return;
    };
    let Some(conn) = connection_slots
        .get(slot_index)
        .and_then(|slot| slot.conn.as_ref())
    else {
        log_missing_ble_slot(host_id);
        return;
    };
    match command {
        BleTaskCommand::RejectPairing { .. } => {
            if let Err(err) = conn.raw().set_bondable(false) {
                log::warn!("firmware: ble set_bondable(false) failed: {:?}", err);
            }
        }
        BleTaskCommand::ClearBond { .. } => {
            let _ = conn.raw().set_bondable(false);
            conn.raw().disconnect();
            sender.send(disconnected_message(host_id)).await;
            mark_host_disconnected(host_id);
            decrement_active_ble_connections();
            let _ = slots.set_disconnected(slot_index);
            clear_connection_slot(connection_slots, slot_index);
        }
        BleTaskCommand::AllowPairing { .. } => {
            if let Err(err) = conn.raw().set_bondable(true) {
                log::warn!("firmware: ble set_bondable(true) failed: {:?}", err);
            }
        }
        BleTaskCommand::Notify { .. } => {
            dispatch_ble_command_to_slot(server, conn, command).await;
        }
    }
}

fn log_missing_ble_slot(host_id: HostId) {
    log::debug!("firmware: no ble slot for host_id={}", host_id.0);
}

fn ble_command_host_id(command: BleTaskCommand) -> HostId {
    match command {
        BleTaskCommand::Notify { host_id, .. }
        | BleTaskCommand::AllowPairing { host_id }
        | BleTaskCommand::RejectPairing { host_id }
        | BleTaskCommand::ClearBond { host_id, .. } => host_id,
    }
}

fn log_ble_command_without_connection(command: BleTaskCommand) {
    match command {
        BleTaskCommand::Notify {
            host_id, reason, ..
        } => {
            log::debug!(
                "firmware: dropping ble notify without connection host={} reason={:?}",
                host_id.0,
                reason
            );
        }
        BleTaskCommand::AllowPairing { host_id } => {
            log::info!(
                "firmware: pairing enabled for host={} without active connection",
                host_id.0
            );
        }
        BleTaskCommand::RejectPairing { host_id } => {
            log::info!(
                "firmware: pairing disabled for host={} without active connection",
                host_id.0
            );
        }
        BleTaskCommand::ClearBond { host_id, .. } => {
            log::info!("firmware: clear bond requested for host={}", host_id.0);
        }
    }
}

async fn configure_ble_connection<P>(
    slot: usize,
    host_id: HostId,
    conn: &GattConnection<'_, '_, P>,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    pairing_allowed: bool,
    restored_bond: bool,
) where
    P: PacketPool,
{
    sender.send(connected_message(host_id)).await;
    mark_host_connected(host_id);
    let active = BLE_ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed) + 1;
    log::info!(
        "firmware: ble slot {} connected host={} active_ble={} pairing_allowed={} restored_bond={}",
        slot,
        host_id.0,
        active,
        pairing_allowed,
        restored_bond
    );
    log::debug!(
        "firmware: ble slot {} peer={:?} identity={:?}",
        slot,
        conn.raw().peer_address(),
        conn.raw().peer_identity()
    );
    if let Err(err) = conn.raw().set_bondable(pairing_allowed) {
        log::warn!("firmware: ble set_bondable failed: {:?}", err);
    }
    match conn.raw().security_level() {
        Ok(level) => log::debug!(
            "firmware: ble slot {} security before request={:?}",
            slot,
            level
        ),
        Err(err) => log::debug!(
            "firmware: ble slot {} security level read failed before request: {:?}",
            slot,
            err
        ),
    }
    if restored_bond && !pairing_allowed {
        log::info!(
            "firmware: ble slot {} waiting for central encryption from restored bond",
            slot
        );
    } else if let Err(err) = conn.raw().request_security() {
        log::warn!("firmware: ble request_security failed: {:?}", err);
    } else {
        log::debug!("firmware: ble slot {} security requested", slot);
    }
}

async fn ble_runner_task<C, P>(mut runner: Runner<'_, C, P>)
where
    C: Controller,
    P: PacketPool,
{
    loop {
        if let Err(err) = runner.run().await {
            log::error!("firmware: ble runner failed: {:?}", err);
        }
    }
}

async fn advertise_ble<'values, 'server, C>(
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'server>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>>
where
    C: Controller,
{
    let mut adv_data = [0; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids16(&[[0x12, 0x18]]),
            AdStructure::CompleteLocalName(BLE_DEVICE_NAME.as_bytes()),
        ],
        &mut adv_data,
    )?;

    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..len],
                scan_data: &[],
            },
        )
        .await?;

    log::info!("firmware: waiting for BLE connection");
    Ok(advertiser.accept().await?.with_attribute_server(server)?)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionProgress {
    Stay,
    Disconnected,
}

async fn process_gatt_event<P>(
    slot: usize,
    host_id: HostId,
    conn: &GattConnection<'_, '_, P>,
    server: &Server<'_>,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    encryption_reported: &mut bool,
) -> ConnectionProgress
where
    P: PacketPool,
{
    let event = conn.next().await;
    report_encryption_if_ready(slot, conn, sender, host_id, encryption_reported).await;
    match event {
        GattConnectionEvent::Disconnected { reason } => {
            let active = decrement_active_ble_connections();
            log::info!(
                "firmware: ble slot {} disconnected: {:?} active_ble={}",
                slot,
                reason,
                active
            );
            sender.send(disconnected_message(host_id)).await;
            mark_host_disconnected(host_id);
            ConnectionProgress::Disconnected
        }
        GattConnectionEvent::Gatt { event } => {
            let reply = match event {
                GattEvent::Read(event) => event.accept(),
                GattEvent::Write(event) => {
                    let handles = ble_hid_attribute_handles(server);
                    match gatt_write_message(host_id, handles, event.handle(), event.data()) {
                        Ok(message) => sender.send(message).await,
                        Err(err) => {
                            log::warn!(
                                "firmware: ble gatt write adapter failed slot={} handle={} err={:?}",
                                slot,
                                event.handle(),
                                err
                            );
                        }
                    }
                    event.accept()
                }
                _ => event.accept(),
            };

            if let Ok(reply) = reply {
                reply.send().await;
            }
            ConnectionProgress::Stay
        }
        GattConnectionEvent::PairingComplete {
            security_level,
            bond,
        } => {
            let bonded = bond.is_some();
            if security_level.encrypted() {
                *encryption_reported = true;
            }
            if let Some(bond) = bond.as_ref() {
                log::debug!(
                    "firmware: ble slot {} pairing bond identity={:?} bonded={} level={:?}",
                    slot,
                    bond.identity,
                    bond.is_bonded,
                    bond.security_level
                );
            }
            sender
                .send(security_changed_message(
                    host_id,
                    security_level.encrypted(),
                    bonded,
                    bond.map(stored_bond_from_trouble),
                ))
                .await;
            log::info!(
                "firmware: ble slot {} pairing complete level={:?} bonded={}",
                slot,
                security_level,
                bonded
            );
            ConnectionProgress::Stay
        }
        GattConnectionEvent::PairingFailed(err) => {
            log::warn!("firmware: ble slot {} pairing failed: {:?}", slot, err);
            ConnectionProgress::Stay
        }
        _ => ConnectionProgress::Stay,
    }
}

async fn report_encryption_if_ready<P>(
    slot: usize,
    conn: &GattConnection<'_, '_, P>,
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    host_id: HostId,
    encryption_reported: &mut bool,
) where
    P: PacketPool,
{
    if *encryption_reported {
        return;
    }
    match conn.raw().security_level() {
        Ok(level) if level.encrypted() => {
            *encryption_reported = true;
            log::info!(
                "firmware: ble slot {} encryption observed level={:?}",
                slot,
                level
            );
            sender
                .send(security_changed_message(host_id, true, false, None))
                .await;
        }
        Ok(_) => {}
        Err(err) => {
            log::debug!(
                "firmware: ble slot {} security level read failed during event: {:?}",
                slot,
                err
            );
        }
    }
}

fn ble_hid_attribute_handles(server: &Server<'_>) -> BleHidAttributeHandles {
    BleHidAttributeHandles {
        keyboard_input_cccd: server.keyboard_hid.input_report.cccd_handle,
        mouse_input_cccd: server.mouse_hid.input_report.cccd_handle,
        consumer_input_cccd: server.consumer_hid.input_report.cccd_handle,
        keyboard_output_cccd: server.keyboard_hid.output_report.cccd_handle,
        keyboard_output_report: server.keyboard_hid.output_report.handle,
        boot_keyboard_output_report: None,
    }
}

async fn dispatch_ble_command_to_slot<P>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    command: BleTaskCommand,
) where
    P: PacketPool,
{
    match command {
        BleTaskCommand::Notify {
            host_id,
            report,
            reason,
        } => {
            if !send_ble_hid_report(server, conn, report).await {
                log::warn!(
                    "firmware: ble notify failed host={} reason={:?}",
                    host_id.0,
                    reason
                );
            }
        }
        BleTaskCommand::AllowPairing { host_id } => {
            log::info!("firmware: pairing enabled for host={}", host_id.0);
        }
        BleTaskCommand::RejectPairing { host_id } => {
            log::info!("firmware: pairing disabled for host={}", host_id.0);
        }
        BleTaskCommand::ClearBond { host_id, .. } => {
            log::info!("firmware: clear bond requested for host={}", host_id.0);
        }
    }
}

async fn send_ble_hid_report<P>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    report: hidshift::reports::BleHidReport,
) -> bool
where
    P: PacketPool,
{
    let mut notifications = heapless::Vec::<_, BLE_HID_NOTIFICATIONS_PER_REPORT_MAX>::new();
    if notifications_for_input_report(report, &mut notifications).is_err() {
        return false;
    }

    for notification in notifications.iter() {
        #[cfg(feature = "diagnostic-input")]
        log::trace!(
            "firmware: ble notify tx characteristic={:?} bytes={:02x?}",
            notification.characteristic,
            notification.as_slice()
        );
        let typed = match typed_notification(notification) {
            Ok(typed) => typed,
            Err(_) => return false,
        };

        let result = match typed {
            hidshift::BleTypedNotification::KeyboardInputReport(value) => {
                server.keyboard_hid.input_report.notify(conn, &value).await
            }
            hidshift::BleTypedNotification::MouseInputReport(value) => {
                server.mouse_hid.input_report.notify(conn, &value).await
            }
            hidshift::BleTypedNotification::ConsumerInputReport(value) => {
                server.consumer_hid.input_report.notify(conn, &value).await
            }
        };

        if result.is_err() {
            return false;
        }
        #[cfg(feature = "diagnostic-input")]
        log::trace!(
            "firmware: ble notify ok characteristic={:?}",
            notification.characteristic
        );
    }

    true
}

fn decrement_active_ble_connections() -> usize {
    let mut current = BLE_ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
    loop {
        if current == 0 {
            return 0;
        }
        match BLE_ACTIVE_CONNECTIONS.compare_exchange(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return current - 1,
            Err(next) => current = next,
        }
    }
}

fn host_mask(host_id: HostId) -> usize {
    host_id
        .0
        .checked_sub(1)
        .filter(|index| (*index as usize) < usize::BITS as usize)
        .map(|index| 1usize << index)
        .unwrap_or(0)
}

fn mark_host_connected(host_id: HostId) {
    BLE_ACTIVE_HOST_MASK.fetch_or(host_mask(host_id), Ordering::AcqRel);
}

fn mark_host_disconnected(host_id: HostId) {
    BLE_ACTIVE_HOST_MASK.fetch_and(!host_mask(host_id), Ordering::AcqRel);
}
