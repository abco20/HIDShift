use core::fmt::Write;
use core::future::pending;
use core::sync::atomic::{AtomicUsize, Ordering};

use bt_hci::cmd::le::{LeAddDeviceToFilterAcceptList, LeClearFilterAcceptList, LeSetPhy};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
use embassy_futures::join::join;
use embassy_futures::select::{Either, Either3, Either4, select, select3, select4};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::rng::{Trng, TrngSource};
use esp_radio::ble::Config as BleControllerConfig;
use esp_radio::ble::controller::BleConnector;
use hidshift::ble_runtime::{
    BleHidAttributeHandles, connected_message, disconnected_message, gatt_write_message,
    security_changed_message,
};
use hidshift::ids::HostId;
use hidshift::management::{
    MANAGEMENT_REQUEST_LEN, MANAGEMENT_RESPONSE_LEN, ManagementDestination, ManagementRequest,
};
use hidshift::reports::{
    HID_INFORMATION, INPUT_REPORT_TYPE, KEYBOARD_REPORT_ID, OUTPUT_REPORT_TYPE,
    V1_CONSUMER_REPORT_MAP, V1_KEYBOARD_REPORT_MAP, V1_MOUSE_REPORT_MAP,
};
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    BleTaskCommand, RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY,
    RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY, RUNTIME_HOSTS_MAX, RUNTIME_INPUT_QUEUE_CAPACITY,
    RuntimeDiagnosticsEvent,
};
#[cfg(not(feature = "hardware-e2e"))]
use hidshift::storage::StoredAddressKind;
use hidshift::storage::{FixedName, StorageState};
use hidshift::{BleConnectionSlots, BleInputGate, BlePeerIdentity, resolve_ble_host_id};
use static_cell::StaticCell;
use trouble_host::prelude::*;

const BLE_DEVICE_NAME: &str = "HIDShift";
const BLE_CONNECTIONS_MAX: usize = 4;
// ESP32-S3 counts advertising in the controller activity limit. Four
// simultaneous peripheral links therefore require one additional activity.
const BLE_CONTROLLER_ACTIVITIES_MAX: u8 = BLE_CONNECTIONS_MAX as u8 + 1;
// One ATT bearer plus one spare control/data lane per connection.
const BLE_L2CAP_CHANNELS_MAX: usize = BLE_CONNECTIONS_MAX * 2;
const BLE_ATTRIBUTE_TABLE_SIZE: usize = 72;
const BLE_NOTIFY_TIMEOUT_MS: u64 = 30;
const MANAGEMENT_NOTIFY_TIMEOUT_MS: u64 = 1_000;
const MANAGEMENT_SERVICE_UUID_LE: [u8; 16] = [
    0x01, 0x00, 0x3a, 0x4f, 0x6d, 0x5b, 0x4b, 0x9f, 0x0d, 0x4f, 0x15, 0x1b, 0x00, 0x00, 0x51, 0x7f,
];

pub fn ble_controller_config() -> BleControllerConfig {
    BleControllerConfig::default()
        .with_max_connections(BLE_CONTROLLER_ACTIVITIES_MAX)
        // HIDShift is a BLE peripheral only. Central scanning and Direct Test
        // Mode reserve sizeable controller heaps but are never exercised.
        .with_scan(false)
        .with_dtm(false)
}

static BLE_ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
static BLE_ACTIVE_HOST_MASK: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BleRuntimeSnapshot {
    pub storage: Option<StorageState>,
    pub pairable_host: Option<HostId>,
}
// The GATT macro generates service fields that are consumed through generated
// attribute metadata, which rustc's dead-code analysis cannot see.
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
    management: ManagementService,
}

static BLE_SERVER: StaticCell<Server> = StaticCell::new();

#[gatt_service(uuid = "7f510000-1b15-4f0d-9f4b-5b6d4f3a0001")]
struct ManagementService {
    #[characteristic(
        uuid = "7f510001-1b15-4f0d-9f4b-5b6d4f3a0001",
        write,
        write_without_response,
        permissions(encrypted),
        value = [0; MANAGEMENT_REQUEST_LEN]
    )]
    request: [u8; MANAGEMENT_REQUEST_LEN],
    #[characteristic(
        uuid = "7f510002-1b15-4f0d-9f4b-5b6d4f3a0001",
        read,
        notify,
        permissions(encrypted),
        value = [0; MANAGEMENT_RESPONSE_LEN]
    )]
    response: [u8; MANAGEMENT_RESPONSE_LEN],
}

#[gatt_service(uuid = "1812")]
struct KeyboardHidService {
    #[characteristic(uuid = "2a4a", read, value = HID_INFORMATION)]
    hid_information: [u8; 4],
    #[characteristic(uuid = "2a4b", read, value = V1_KEYBOARD_REPORT_MAP)]
    report_map: &'static [u8],
    #[characteristic(uuid = "2a4c", write_without_response, value = 0)]
    control_point: u8,
    #[descriptor(uuid = "2908", read, value = [KEYBOARD_REPORT_ID, INPUT_REPORT_TYPE])]
    #[characteristic(uuid = "2a4d", read, notify, value = [0; 8])]
    input_report: [u8; 8],
    #[descriptor(uuid = "2908", read, value = [KEYBOARD_REPORT_ID, OUTPUT_REPORT_TYPE])]
    #[characteristic(uuid = "2a4d", read, write, write_without_response, value = [0])]
    output_report: [u8; 1],
}

#[gatt_service(uuid = "1812")]
struct MouseHidService {
    #[characteristic(uuid = "2a4a", read, value = HID_INFORMATION)]
    hid_information: [u8; 4],
    #[characteristic(uuid = "2a4b", read, value = V1_MOUSE_REPORT_MAP)]
    report_map: &'static [u8],
    #[characteristic(uuid = "2a4c", write_without_response, value = 0)]
    control_point: u8,
    #[descriptor(uuid = "2908", read, value = [0, INPUT_REPORT_TYPE])]
    #[characteristic(uuid = "2a4d", read, notify, value = [0; 5])]
    input_report: [u8; 5],
}

#[gatt_service(uuid = "1812")]
struct ConsumerHidService {
    #[characteristic(uuid = "2a4a", read, value = HID_INFORMATION)]
    hid_information: [u8; 4],
    #[characteristic(uuid = "2a4b", read, value = V1_CONSUMER_REPORT_MAP)]
    report_map: &'static [u8],
    #[characteristic(uuid = "2a4c", write_without_response, value = 0)]
    control_point: u8,
    #[descriptor(uuid = "2908", read, value = [0, INPUT_REPORT_TYPE])]
    #[characteristic(uuid = "2a4d", read, notify, value = [0; 2])]
    input_report: [u8; 2],
}

#[gatt_service(uuid = "180a")]
struct DeviceInformationService {
    #[characteristic(uuid = "2a29", read, value = "HIDShift")]
    manufacturer_name: &'static str,
    #[characteristic(uuid = "2a24", read, value = "firmware")]
    model_number: &'static str,
}

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
    quiesce_request: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    quiesce_ready: Sender<'static, CriticalSectionRawMutex, Option<StorageState>, 1>,
    quiesce_done: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    usb_quiesce_request: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    usb_quiesce_ready: Sender<'static, CriticalSectionRawMutex, (), 1>,
    usb_quiesce_done: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    runtime_barrier_request: Sender<'static, CriticalSectionRawMutex, usize, 1>,
    runtime_barrier_done: Receiver<'static, CriticalSectionRawMutex, BleRuntimeSnapshot, 1>,
    runtime_barrier_resume: Sender<'static, CriticalSectionRawMutex, (), 1>,
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
    let mut current_storage = restored_state;
    let mut pairable_host = None;
    let mut bt = Some(bt);
    let server = match Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: BLE_DEVICE_NAME,
        appearance: &appearance::human_interface_device::KEYBOARD,
    })) {
        Ok(server) => BLE_SERVER.init(server),
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
            ble_controller_config(),
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
                while notify_receiver.try_receive().is_ok() {}
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
                runtime_barrier_resume.send(()).await;
            }
            Either3::Third(()) => {
                while notify_receiver.try_receive().is_ok() {}
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
                runtime_barrier_resume.send(()).await;
            }
        }
    }
}

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
    C: Controller
        + ControllerCmdSync<LeClearFilterAcceptList>
        + ControllerCmdSync<LeAddDeviceToFilterAcceptList>
        + ControllerCmdAsync<LeSetPhy>,
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
        super::ble_bonds::restore(&stack, restored_state);
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
    // Poll application/GATT work before the controller runner. A notification
    // queued by `accept` can then be drained by the runner in the same executor
    // poll. The reverse order leaves it pending until the next wake and can
    // miss an entire 7.5-ms connection event.
    let _ = join(accept, ble_runner_task(runner)).await;
}

fn retain_gatt_service_fields(server: &Server) {
    let _ = &server.device_information;
    let _ = &server.management;
}

#[derive(Clone, Copy, Debug)]
struct BleControlState {
    pairing_allowed: [bool; RUNTIME_HOSTS_MAX],
    restored_bond: [bool; RUNTIME_HOSTS_MAX],
    input_gate: BleInputGate<RUNTIME_HOSTS_MAX>,
}

impl BleControlState {
    fn new(restored_state: Option<&StorageState>, pairable_host: Option<HostId>) -> Self {
        let mut state = Self {
            pairing_allowed: [false; RUNTIME_HOSTS_MAX],
            restored_bond: [false; RUNTIME_HOSTS_MAX],
            input_gate: BleInputGate::new(),
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
            BleTaskCommand::Notify {
                host_id,
                reason:
                    hidshift::NotifyReason::TargetSwitchRelease | hidshift::NotifyReason::SafetyRelease,
                ..
            } => self.input_gate.block(host_id),
            BleTaskCommand::Notify { .. } | BleTaskCommand::ManagementResponse { .. } => {}
            BleTaskCommand::ActivateInput { host_id } => self.input_gate.activate(host_id),
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

    fn should_drop(&self, command: BleTaskCommand) -> bool {
        let BleTaskCommand::Notify {
            host_id,
            reason: hidshift::NotifyReason::Input,
            ..
        } = command
        else {
            return false;
        };
        self.input_gate.should_drop_input(host_id)
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

    fn bonded_peer_count(&self) -> usize {
        self.restored_bond.iter().filter(|bonded| **bonded).count()
    }

    fn restrict_advertising_to_bonds(&self) -> bool {
        if cfg!(feature = "hardware-e2e") {
            true
        } else {
            hidshift::restrict_advertising_to_bonded_peers(
                self.bonded_peer_count(),
                self.pairing_host().is_some(),
            )
        }
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
    #[cfg(feature = "hardware-e2e")]
    if control.pairing_host() == Some(HostId(1))
        && conn.raw().peer_address().into_inner() != hidshift::e2e::E2E_PROBE_BLE_ADDRESS_RAW
    {
        return None;
    }
    let peer_identity = connection_peer_identity(conn);
    let resolved = resolve_ble_host_id(restored_state, peer_identity, control.pairing_host())?;
    let is_pairing = control.pairing_allowed(resolved);
    let reconnect_enabled = restored_state
        .map(|state| state.global_settings.auto_reconnect)
        .unwrap_or(true);
    (is_pairing || reconnect_enabled).then_some(resolved)
}

#[cfg(feature = "hardware-e2e")]
fn e2e_linux_address() -> Option<BdAddr> {
    let value = option_env!("HIDSHIFT_E2E_LINUX_ADDRESS")?;
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

async fn configure_ble_accept_list<C, P>(
    stack: &Stack<'_, C, P>,
    restored_state: Option<&StorageState>,
) where
    C: Controller
        + ControllerCmdSync<LeClearFilterAcceptList>
        + ControllerCmdSync<LeAddDeviceToFilterAcceptList>,
    P: PacketPool,
{
    if let Err(error) = stack.command(LeClearFilterAcceptList::new()).await {
        log::error!("firmware: accept-list clear failed: {:?}", error);
        return;
    }

    #[cfg(not(feature = "hardware-e2e"))]
    if let Some(storage) = restored_state {
        let mut added = 0usize;
        for host in storage.hosts() {
            let Some(bond) = host.bond else {
                continue;
            };
            let address_kind = match bond.peer_address_kind {
                StoredAddressKind::Public => AddrKind::PUBLIC,
                StoredAddressKind::Random => AddrKind::RANDOM,
            };
            match stack
                .command(LeAddDeviceToFilterAcceptList::new(
                    address_kind,
                    BdAddr::new(bond.peer_address),
                ))
                .await
            {
                Ok(()) => added += 1,
                Err(error) => log::error!(
                    "firmware: accept-list add failed host={} err={:?}",
                    host.host_id.0,
                    error
                ),
            }
        }
        log::info!("firmware: programmed {} bonded accept-list peer(s)", added);
    }

    #[cfg(feature = "hardware-e2e")]
    let _ = restored_state;
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
    C: Controller
        + ControllerCmdSync<LeClearFilterAcceptList>
        + ControllerCmdSync<LeAddDeviceToFilterAcceptList>
        + ControllerCmdAsync<LeSetPhy>,
{
    let mut control = BleControlState::new(restored_state, pairable_host);
    configure_ble_accept_list(stack, restored_state).await;
    #[cfg(feature = "hardware-e2e")]
    {
        let address = BdAddr::new(hidshift::e2e::E2E_PROBE_BLE_ADDRESS_RAW);
        if let Err(error) = stack
            .command(LeAddDeviceToFilterAcceptList::new(
                AddrKind::RANDOM,
                address,
            ))
            .await
        {
            log::error!("firmware: E2E accept-list add failed: {:?}", error);
        }
        match e2e_linux_address() {
            Some(address) => {
                if let Err(error) = stack
                    .command(LeAddDeviceToFilterAcceptList::new(
                        AddrKind::PUBLIC,
                        address,
                    ))
                    .await
                {
                    log::error!("firmware: E2E Linux accept-list add failed: {:?}", error);
                }
            }
            None => log::error!("firmware: E2E Linux controller address is missing"),
        }
    }
    loop {
        match select(
            advertise_ble(peripheral, server, control.restrict_advertising_to_bonds()),
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
                    stack,
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
    C: Controller + ControllerCmdAsync<LeSetPhy>,
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

    'connections: loop {
        if slots.connected_count() == 0 {
            break;
        }

        // Keep one advertiser alive across report and GATT processing. The
        // previous loop rebuilt this future after every report; dropping its
        // Advertiser issued HCI Disable Advertising and the next iteration
        // immediately configured/enabled it again. Those control commands and
        // the first advertising event could preempt the following 7.5-ms BLE
        // connection event. Only connection/pairing-policy changes need to
        // rebuild the advertiser.
        let advertising = advertise_if_slot_available(
            peripheral,
            server,
            slots.should_advertise_additional_connection(control.pairing_host().is_some()),
            control.restrict_advertising_to_bonds(),
        );
        let mut advertising = core::pin::pin!(advertising);

        loop {
            match select3(
                receive_ble_command(control_receiver, notify_receiver),
                process_slot_events(&mut connection_slots, &slots, server, sender),
                advertising.as_mut(),
            )
            .await
            {
                // HID reports are realtime traffic. embassy-futures select3 is
                // deliberately biased by argument order, so command reception
                // must be first rather than behind GATT and advertising work.
                Either3::Third(Ok(conn)) => {
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
                        continue 'connections;
                    };
                    let peer_identity = connection_peer_identity(&conn);
                    match slots.connect_first_free(host_id, peer_identity) {
                        Ok(assigned_slot) => {
                            configure_ble_connection(
                                assigned_slot.index(),
                                host_id,
                                stack,
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
                    continue 'connections;
                }
                Either3::Third(Err(err)) => {
                    log::warn!("firmware: ble advertising failed: {:?}", err);
                    continue 'connections;
                }
                Either3::Second(slot_event) => {
                    let (slot, progress) = slot_event;
                    if let ConnectionProgress::Disconnected = progress {
                        if let Err(err) = slots.set_disconnected(slot) {
                            log::error!("firmware: ble slot state error {:?}", err);
                        }
                        clear_connection_slot(&mut connection_slots, slot);
                        continue 'connections;
                    }
                }
                Either3::First(command) => {
                    #[cfg(feature = "hardware-e2e")]
                    if matches!(command, BleTaskCommand::Notify { .. }) {
                        crate::e2e_telemetry::record_ble_receive(Instant::now().as_micros());
                    }
                    let restart_advertising = command.changes_advertising_policy();
                    control.apply_command(command);
                    if control.should_drop(command) {
                        log::debug!("firmware: dropping stale notify after target release");
                        continue;
                    }
                    dispatch_ble_command_to_connected_slot(
                        stack,
                        server,
                        &mut slots,
                        &mut connection_slots,
                        command,
                        sender,
                    )
                    .await;
                    apply_ble_stack_command(stack, command);
                    if restart_advertising {
                        continue 'connections;
                    }
                }
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
    restrict_to_accept_list: bool,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>>
where
    C: Controller,
{
    if should_advertise {
        advertise_ble(peripheral, server, restrict_to_accept_list).await
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
        let Some(bond_information) = super::ble_bonds::to_trouble(bond) else {
            return;
        };
        if let Err(err) = stack.remove_bond_information(bond_information.identity) {
            log::error!("firmware: clear bond failed: {:?}", err);
        }
    }
}

async fn dispatch_ble_command_to_connected_slot<C>(
    stack: &Stack<'_, C, DefaultPacketPool>,
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
) where
    C: Controller,
{
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
        BleTaskCommand::ActivateInput { .. } => {}
        BleTaskCommand::Notify { .. } => {
            let critical_release = !matches!(
                command,
                BleTaskCommand::Notify {
                    reason: hidshift::NotifyReason::Input,
                    ..
                }
            );
            match with_timeout(
                Duration::from_millis(BLE_NOTIFY_TIMEOUT_MS),
                dispatch_ble_command_to_slot(stack, server, conn, command),
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    let _ = sender.try_send(RuntimeInputMessage::DiagnosticsEvent(
                        RuntimeDiagnosticsEvent::BleNotifyFailed,
                    ));
                }
                Err(_) => {
                    log::warn!("firmware: ble notify timeout host={}", host_id.0);
                    let _ = sender.try_send(RuntimeInputMessage::DiagnosticsEvent(
                        RuntimeDiagnosticsEvent::BleNotifyTimedOut { critical_release },
                    ));
                }
            }
        }
        BleTaskCommand::ManagementResponse { .. } => {
            if with_timeout(
                Duration::from_millis(MANAGEMENT_NOTIFY_TIMEOUT_MS),
                dispatch_ble_command_to_slot(stack, server, conn, command),
            )
            .await
            .is_err()
            {
                log::warn!(
                    "firmware: management response notify timeout host={}",
                    host_id.0
                );
                let _ = sender.try_send(RuntimeInputMessage::DiagnosticsEvent(
                    RuntimeDiagnosticsEvent::BleManagementNotifyTimedOut,
                ));
            }
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
        | BleTaskCommand::ClearBond { host_id, .. }
        | BleTaskCommand::ActivateInput { host_id }
        | BleTaskCommand::ManagementResponse { host_id, .. } => host_id,
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
        BleTaskCommand::ActivateInput { host_id } => {
            log::debug!("firmware: input activated for host={}", host_id.0);
        }
        BleTaskCommand::ManagementResponse { host_id, .. } => {
            log::debug!(
                "firmware: dropping management response without connection host={}",
                host_id.0
            );
        }
    }
}

async fn configure_ble_connection<C, P>(
    slot: usize,
    host_id: HostId,
    stack: &Stack<'_, C, P>,
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
    C: Controller + ControllerCmdAsync<LeSetPhy>,
{
    #[cfg(feature = "hardware-e2e")]
    {
        let params = conn.raw().params();
        crate::e2e_telemetry::record_ble_connected(
            params.conn_interval.as_micros() as u32,
            params.peripheral_latency,
            params.supervision_timeout.as_millis() as u32,
        );
    }
    let timing = hidshift::low_latency_ble_connection_timing();
    let requested = RequestedConnParams {
        min_connection_interval: Duration::from_micros(u64::from(timing.interval_min_us)),
        max_connection_interval: Duration::from_micros(u64::from(timing.interval_max_us)),
        max_latency: timing.peripheral_latency,
        min_event_length: Duration::from_micros(0),
        max_event_length: Duration::from_micros(0),
        supervision_timeout: Duration::from_millis(u64::from(timing.supervision_timeout_ms)),
    };
    match conn
        .raw()
        .update_connection_params_l2cap(stack, &requested)
        .await
    {
        Ok(()) => log::info!(
            "firmware: ble slot {} requested interval={}..{}us latency={} timeout={}ms",
            slot,
            timing.interval_min_us,
            timing.interval_max_us,
            timing.peripheral_latency,
            timing.supervision_timeout_ms
        ),
        Err(err) => log::warn!(
            "firmware: ble slot {} connection parameter request failed: {:?}",
            slot,
            err
        ),
    }
    let preferred_phy = match timing.preferred_phy {
        hidshift::BlePhyPreference::Le1M => PhyKind::Le1M,
        hidshift::BlePhyPreference::Le2M => PhyKind::Le2M,
    };
    match conn.raw().set_phy(stack, preferred_phy).await {
        Ok(()) => log::info!(
            "firmware: ble slot {} requested phy={:?}",
            slot,
            preferred_phy
        ),
        Err(err) => log::warn!("firmware: ble slot {} phy request failed: {:?}", slot, err),
    }
    sender.send(connected_message(host_id)).await;
    if let Some(name) = fallback_ble_peer_name(conn) {
        sender
            .send(RuntimeInputMessage::HostNameDiscovered { host_id, name })
            .await;
    }
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

fn fallback_ble_peer_name<P>(conn: &GattConnection<'_, '_, P>) -> Option<FixedName>
where
    P: PacketPool,
{
    let address = conn.raw().peer_identity().bd_addr.into_inner();
    let mut name = heapless::String::<16>::new();
    write!(
        name,
        "BLE-{:02X}{:02X}{:02X}",
        address[3], address[4], address[5]
    )
    .ok()?;
    FixedName::from_ascii(name.as_str())
}

fn observe_ble_hci_tx(stage: TxObserverStage, _pdu: &[u8]) {
    #[cfg(feature = "hardware-e2e")]
    {
        let now_us = Instant::now().as_micros();
        match stage {
            TxObserverStage::Prepared => crate::e2e_telemetry::record_notify_done(now_us),
            TxObserverStage::Dequeued => crate::e2e_telemetry::record_hci_dequeue(now_us),
            TxObserverStage::CreditGranted => crate::e2e_telemetry::record_hci_credit(now_us),
            TxObserverStage::Submitted => {
                // During an isolated E2E input the notification is the only
                // outbound payload before telemetry is queried.
                crate::e2e_telemetry::record_hci_submit(now_us)
            }
        }
    }
    #[cfg(not(feature = "hardware-e2e"))]
    let _ = stage;
}

async fn ble_runner_task<C, P>(mut runner: Runner<'_, C, P>)
where
    C: Controller,
    P: PacketPool,
{
    loop {
        #[cfg(feature = "hardware-e2e")]
        let result = runner.run_with_tx_observer(observe_ble_hci_tx).await;
        #[cfg(not(feature = "hardware-e2e"))]
        let result = runner.run().await;
        if let Err(err) = result {
            log::error!("firmware: ble runner failed: {:?}", err);
        }
    }
}

async fn advertise_ble<'values, 'server, C>(
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'server>,
    restrict_to_accept_list: bool,
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
    let mut scan_data = [0; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::ServiceUuids128(&[MANAGEMENT_SERVICE_UUID_LE])],
        &mut scan_data,
    )?;

    let parameters = AdvertisementParameters {
        filter_policy: if restrict_to_accept_list {
            AdvFilterPolicy::FilterConn
        } else {
            AdvFilterPolicy::Unfiltered
        },
        ..Default::default()
    };
    let advertiser = peripheral
        .advertise(
            &parameters,
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..len],
                scan_data: &scan_data[..scan_len],
            },
        )
        .await?;

    log::debug!("firmware: waiting for BLE connection");
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
    match event {
        GattConnectionEvent::Disconnected { reason } => {
            #[cfg(feature = "hardware-e2e")]
            crate::e2e_telemetry::record_ble_disconnected();
            let active = decrement_active_ble_connections();
            log::info!(
                "firmware: ble slot {} disconnected: {:?} active_ble={}",
                slot,
                reason,
                active
            );
            sender.send(disconnected_message(host_id)).await;
            sender
                .send(RuntimeInputMessage::DiagnosticsEvent(
                    RuntimeDiagnosticsEvent::BleDisconnected {
                        host_id,
                        reason: u8::from(reason),
                    },
                ))
                .await;
            mark_host_disconnected(host_id);
            ConnectionProgress::Disconnected
        }
        GattConnectionEvent::Gatt { event } => {
            // ATT has a short response deadline. Never wait for the shared runtime
            // queue before acknowledging a GATT request: USB input can legitimately
            // fill that queue and used to stall descriptor reads and CCCD writes.
            let runtime_message = match event {
                GattEvent::Read(event) => {
                    if let Ok(reply) = event.accept() {
                        reply.send().await;
                    }
                    None
                }
                GattEvent::Write(event) => {
                    let message = if event.handle() == server.management.request.handle {
                        match ManagementRequest::decode(event.data()) {
                            Ok(request) => Some(RuntimeInputMessage::ManagementRequest {
                                destination: ManagementDestination::Ble(host_id),
                                request,
                                now_ms: Instant::now().as_millis(),
                            }),
                            Err(err) => {
                                log::warn!(
                                    "firmware: invalid management request slot={} err={:?}",
                                    slot,
                                    err
                                );
                                None
                            }
                        }
                    } else {
                        let handles = ble_hid_attribute_handles(server);
                        match gatt_write_message(host_id, handles, event.handle(), event.data()) {
                            Ok(message) => Some(message),
                            Err(err) => {
                                log::warn!(
                                    "firmware: ble gatt write adapter failed slot={} handle={} err={:?}",
                                    slot,
                                    event.handle(),
                                    err
                                );
                                None
                            }
                        }
                    };
                    if let Ok(reply) = event.accept() {
                        reply.send().await;
                    }
                    message
                }
                _ => {
                    if let Ok(reply) = event.accept() {
                        reply.send().await;
                    }
                    None
                }
            };

            // Once the peer has its ATT response, backpressure here is safe.
            if let Some(message) = runtime_message {
                sender.send(message).await;
            }
            report_encryption_if_ready(slot, conn, sender, host_id, encryption_reported).await;
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
                    bond.map(|bond| {
                        super::ble_bonds::from_trouble(bond, conn.raw().peer_addr_kind())
                    }),
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
        GattConnectionEvent::ConnectionParamsUpdated {
            conn_interval,
            peripheral_latency,
            supervision_timeout,
        } => {
            #[cfg(feature = "hardware-e2e")]
            crate::e2e_telemetry::record_ble_connection_parameters(
                conn_interval.as_micros() as u32,
                peripheral_latency,
                supervision_timeout.as_millis() as u32,
            );
            log::info!(
                "firmware: ble slot {} connection parameters interval={}us latency={} timeout={}ms",
                slot,
                conn_interval.as_micros(),
                peripheral_latency,
                supervision_timeout.as_millis()
            );
            ConnectionProgress::Stay
        }
        GattConnectionEvent::PhyUpdated { tx_phy, rx_phy } => {
            #[cfg(feature = "hardware-e2e")]
            crate::e2e_telemetry::record_ble_phy(
                phy_telemetry_value(tx_phy),
                phy_telemetry_value(rx_phy),
            );
            log::info!(
                "firmware: ble slot {} phy updated tx={:?} rx={:?}",
                slot,
                tx_phy,
                rx_phy
            );
            ConnectionProgress::Stay
        }
        _ => ConnectionProgress::Stay,
    }
}

#[cfg(feature = "hardware-e2e")]
const fn phy_telemetry_value(phy: PhyKind) -> u8 {
    match phy {
        PhyKind::Le1M => 1,
        PhyKind::Le2M => 2,
        PhyKind::LeCoded => 3,
        PhyKind::LeCodedS2 => 4,
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

async fn dispatch_ble_command_to_slot<C, P>(
    stack: &Stack<'_, C, P>,
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    command: BleTaskCommand,
) -> bool
where
    C: Controller,
    P: PacketPool,
{
    match command {
        BleTaskCommand::Notify {
            host_id,
            report,
            reason,
        } => {
            if !send_ble_hid_report(stack, server, conn, report).await {
                log::warn!(
                    "firmware: ble notify failed host={} reason={:?}",
                    host_id.0,
                    reason
                );
                return false;
            }
            true
        }
        BleTaskCommand::AllowPairing { host_id } => {
            log::info!("firmware: pairing enabled for host={}", host_id.0);
            true
        }
        BleTaskCommand::RejectPairing { host_id } => {
            log::info!("firmware: pairing disabled for host={}", host_id.0);
            true
        }
        BleTaskCommand::ClearBond { host_id, .. } => {
            log::info!("firmware: clear bond requested for host={}", host_id.0);
            true
        }
        BleTaskCommand::ActivateInput { host_id } => {
            log::debug!("firmware: input activated for host={}", host_id.0);
            true
        }
        BleTaskCommand::ManagementResponse { host_id, response } => {
            let success = server
                .management
                .response
                .notify(conn, &response.encode())
                .await
                .is_ok();
            if !success {
                log::warn!(
                    "firmware: management response notify failed host={}",
                    host_id.0
                );
            }
            success
        }
    }
}

async fn send_ble_hid_report<C, P>(
    stack: &Stack<'_, C, P>,
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    report: hidshift::reports::BleHidReport,
) -> bool
where
    C: Controller,
    P: PacketPool,
{
    #[cfg(feature = "hardware-e2e")]
    crate::e2e_telemetry::record_notify_start(Instant::now().as_micros());
    // The runtime already carries a typed report with the exact GATT payload
    // width. Avoid rebuilding a generic notification vector and validating it
    // a second time on this latency-sensitive path.
    let result = match report {
        hidshift::reports::BleHidReport::Keyboard(report) => {
            server
                .keyboard_hid
                .input_report
                .notify_immediate(stack, conn, report.as_bytes(), observe_ble_hci_tx)
                .await
        }
        hidshift::reports::BleHidReport::Mouse(report) => {
            server
                .mouse_hid
                .input_report
                .notify_immediate(stack, conn, report.as_bytes(), observe_ble_hci_tx)
                .await
        }
        hidshift::reports::BleHidReport::Consumer(report) => {
            server
                .consumer_hid
                .input_report
                .notify_immediate(stack, conn, report.as_bytes(), observe_ble_hci_tx)
                .await
        }
    };
    if result.is_err() {
        return false;
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
