use core::marker::PhantomData;

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, Either4, select3, select4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Timer, with_timeout};
use embassy_usb_driver::host::{DeviceEvent, PipeError, UsbHostAllocator, UsbPipe, pipe};
use embassy_usb_driver::{Direction, EndpointAddress, EndpointInfo, EndpointType, Speed};
use embassy_usb_host::class::hid::{HidError, PROTOCOL_REPORT, ReportDescriptor};
use embassy_usb_host::class::hub::{HubEvent, HubHandler};
use embassy_usb_host::control::{ControlType, Recipient, RequestType, SetupPacket};
use embassy_usb_host::handler::HandlerEvent;
use embassy_usb_host::{BusRoute, BusState};
use embassy_usb_synopsys_otg::PhyType;
use embassy_usb_synopsys_otg::host::{
    HostStateStorage, OtgHost, OtgHostAllocator, OtgHostInstance, OtgHostPortControl,
    on_host_interrupt,
};
use esp_hal::otg_fs::Usb;
use esp_hal::peripherals::Interrupt;
use esp_synopsys_usb_otg::UsbPeripheral;
use hidshift::ids::{DeviceId, InterfaceId};
use hidshift::input::KeyboardLedState;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    RUNTIME_INPUT_QUEUE_CAPACITY, RUNTIME_USB_COMMAND_QUEUE_CAPACITY, RuntimeDiagnosticsEvent,
    UsbHostTaskCommand,
};
use hidshift::storage::FixedName;
use hidshift::usb_hid::host_interface::{
    HidInterfaceInfo, config_descriptor_has_interface_class, find_hid_interfaces,
};
use hidshift::usb_hid::host_runtime::UsbHidInterfaceRuntimeSession;
use hidshift::usb_hid::topology::{DefaultUsbTopologyManager, UsbDeviceRoute};
use static_cell::ConstStaticCell;

#[cfg(feature = "dual-s3-wired")]
use hidshift::interchip::{
    CONTROL_DATA_MAX_LEN, ControlStatus, MirrorControlResponse, ProfileTransferEncoder,
};
#[cfg(feature = "dual-s3-wired")]
use hidshift::mirror::{HSMI_MAX_SIZE, HidReportRecord, MirrorCaptureSource, StringRecord};
#[cfg(feature = "dual-s3-wired")]
use hidshift::{MirrorCandidateId, MirrorCaptureError, capture_mirror_profile};

use super::usb_output_transport::UsbKeyboardLedWriter;
#[cfg(feature = "dual-s3-wired")]
use super::usb_transport::UsbRawOutWriter;
use super::usb_transport::{UsbHidControl, UsbHidReader};

const HOST_CHANNELS: usize = 8;
const CONFIG_DESCRIPTOR_BUF_LEN: usize = 512;
const REPORT_DESCRIPTOR_BUF_LEN: usize = hidshift::USB_HID_REPORT_DESCRIPTOR_MAX_LEN;
const REPORT_BUF_LEN: usize = hidshift::USB_HID_REPORT_MAX_LEN;
const MAX_REPORT_FIELDS: usize = 48;
const MAX_REPORT_EVENTS: usize = 32;
// ESP32-S3 exposes eight host channels. HubHandler permanently owns its
// interrupt and control pipes, so at most five HID readers are kept active and
// the final channel remains available for LED, control, and optional OUT work.
const MAX_ACTIVE_USB_INTERFACES: usize = 5;
const USB_READER_QUEUE_CAPACITY: usize = 32;
const MAX_HUB_PORTS: usize = 4;
const HUB_CHILD_ENUMERATION_TIMEOUT_MS: u64 = 5_000;
const HID_REPORT_DESCRIPTOR_TIMEOUT_MS: u64 = 2_000;
const USB_CONTROL_REQUEST_TIMEOUT_MS: u64 = 500;
const USB_LED_WRITE_TIMEOUT_MS: u64 = 20;
const BLE_QUIESCE_HANDSHAKE_TIMEOUT_MS: u64 = 2_000;

static HOST_STATE: HostStateStorage<HOST_CHANNELS> = HostStateStorage::new();
static BUS_STATE: BusState = BusState::new();

type FirmwareBusHandle<'d> = embassy_usb_host::BusHandle<'d, OtgHostAllocator<'d>>;

struct ActiveUsbInterfaceSlot<'d> {
    interface_id: InterfaceId,
    led_output: bool,
    hid_info: HidInterfaceInfo,
    enum_info: embassy_usb_host::handler::EnumerationInfo,
    session: UsbHidInterfaceRuntimeSession<MAX_REPORT_FIELDS, MAX_REPORT_EVENTS>,
    last_mouse_buttons: hidshift::input::MouseButtons,
    last_led_bytes: Option<hidshift::usb_hid::output::KeyboardLedOutputBytes>,
    _lifetime: PhantomData<&'d ()>,
    #[cfg(feature = "dual-s3-wired")]
    raw_in_sequence: u16,
}

// The USB task polls several nested futures while retaining up to eight HID
// sessions. Keep its long-lived state out of the executor's thread stack; the
// ESP32-S3 USB host path otherwise needs more stack than the shared executor
// task has available during its first poll.
type StaticActiveUsbInterfaceSlot = ActiveUsbInterfaceSlot<'static>;

static CONFIG_DESCRIPTOR_STORAGE: ConstStaticCell<[u8; CONFIG_DESCRIPTOR_BUF_LEN]> =
    ConstStaticCell::new([0; CONFIG_DESCRIPTOR_BUF_LEN]);
static TOPOLOGY_STORAGE: ConstStaticCell<DefaultUsbTopologyManager> =
    ConstStaticCell::new(DefaultUsbTopologyManager::new());
static MOVEMENT_QUEUE_STORAGE: ConstStaticCell<
    hidshift::input::UsbMovementCoalescer<MAX_ACTIVE_USB_INTERFACES>,
> = ConstStaticCell::new(hidshift::input::UsbMovementCoalescer::<
    MAX_ACTIVE_USB_INTERFACES,
>::new());
static ACTIVE_SLOTS_STORAGE: ConstStaticCell<
    [Option<StaticActiveUsbInterfaceSlot>; MAX_ACTIVE_USB_INTERFACES],
> = ConstStaticCell::new([const { None }; MAX_ACTIVE_USB_INTERFACES]);

#[cfg(feature = "dual-s3-wired")]
const MIRROR_BOS_MAX_LEN: usize = 256;
#[cfg(feature = "dual-s3-wired")]
const MIRROR_STRING_RECORDS_MAX: usize = 16;
#[cfg(feature = "dual-s3-wired")]
const MIRROR_STRING_DESCRIPTOR_MAX_LEN: usize = 256;

#[cfg(feature = "dual-s3-wired")]
struct MirrorCaptureScratch {
    image: [u8; HSMI_MAX_SIZE],
    bos: [u8; MIRROR_BOS_MAX_LEN],
    strings: [[u8; MIRROR_STRING_DESCRIPTOR_MAX_LEN]; MIRROR_STRING_RECORDS_MAX],
    string_indices: [u8; MIRROR_STRING_RECORDS_MAX],
    string_lang_ids: [u16; MIRROR_STRING_RECORDS_MAX],
    string_lengths: [usize; MIRROR_STRING_RECORDS_MAX],
}

#[cfg(feature = "dual-s3-wired")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MirrorProfileCaptureError {
    UnsupportedUsbSpeed,
    ControlPipeUnavailable,
    TooManyStrings,
    TooManyHidInterfaces,
    Capture(MirrorCaptureError),
    ProfileTransferRejected,
}

#[cfg(feature = "dual-s3-wired")]
impl MirrorCaptureScratch {
    const fn new() -> Self {
        Self {
            image: [0; HSMI_MAX_SIZE],
            bos: [0; MIRROR_BOS_MAX_LEN],
            strings: [[0; MIRROR_STRING_DESCRIPTOR_MAX_LEN]; MIRROR_STRING_RECORDS_MAX],
            string_indices: [0; MIRROR_STRING_RECORDS_MAX],
            string_lang_ids: [0; MIRROR_STRING_RECORDS_MAX],
            string_lengths: [0; MIRROR_STRING_RECORDS_MAX],
        }
    }
}

#[cfg(feature = "dual-s3-wired")]
static MIRROR_CAPTURE_STORAGE: ConstStaticCell<MirrorCaptureScratch> =
    ConstStaticCell::new(MirrorCaptureScratch::new());

struct UsbDecodedInput {
    standard: Option<hidshift::input::InputFrame>,
    movement_only: bool,
    #[cfg(feature = "dual-s3-wired")]
    raw: hidshift::interchip::RawEndpointReport,
    #[cfg(feature = "dual-s3-wired")]
    device_id: DeviceId,
}

#[derive(Clone, Copy)]
struct UsbReaderReport {
    device_id: DeviceId,
    interface_id: InterfaceId,
    len: usize,
    data: [u8; REPORT_BUF_LEN],
}

enum UsbReaderEvent {
    Report(UsbReaderReport),
    Fatal {
        device_id: DeviceId,
        interface_id: InterfaceId,
        error: HidError,
    },
}

static USB_READER_QUEUE: Channel<
    CriticalSectionRawMutex,
    UsbReaderEvent,
    USB_READER_QUEUE_CAPACITY,
> = Channel::new();

enum UsbHubTaskEvent {
    DeviceEnumerated {
        port: u8,
        enum_info: embassy_usb_host::handler::EnumerationInfo,
        config_len: usize,
        config: [u8; CONFIG_DESCRIPTOR_BUF_LEN],
    },
    DeviceRemoved {
        port: u8,
    },
    Failed,
}

static USB_HUB_EVENT_QUEUE: Channel<CriticalSectionRawMutex, UsbHubTaskEvent, 2> = Channel::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsbRootTaskEvent {
    Connected(Speed),
    Disconnected,
}

static USB_ROOT_EVENT_QUEUE: Channel<CriticalSectionRawMutex, UsbRootTaskEvent, 4> = Channel::new();

#[embassy_executor::task]
async fn usb_root_event_task(
    mut controller: embassy_usb_host::BusController<'static, OtgHost<'static>>,
    events: Sender<'static, CriticalSectionRawMutex, UsbRootTaskEvent, 4>,
) {
    loop {
        match controller.wait_for_device_event().await {
            DeviceEvent::Connected(speed) => {
                events.send(UsbRootTaskEvent::Connected(speed)).await;
            }
            DeviceEvent::Disconnected => {
                events.send(UsbRootTaskEvent::Disconnected).await;
            }
            DeviceEvent::Overcurrent => {
                log::warn!("firmware: USB root port overcurrent");
                events.send(UsbRootTaskEvent::Disconnected).await;
            }
            _ => {}
        }
    }
}

#[embassy_executor::task]
async fn usb_hub_task(
    mut hub: HubHandler<'static, OtgHostAllocator<'static>, MAX_HUB_PORTS>,
    events: Sender<'static, CriticalSectionRawMutex, UsbHubTaskEvent, 2>,
    ble_quiesce_request: Sender<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_ready: Receiver<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
) {
    let mut config = [0; CONFIG_DESCRIPTOR_BUF_LEN];
    loop {
        match hub.wait_for_event().await {
            Ok(HandlerEvent::HandlerEvent(HubEvent::DeviceDetected { port, speed })) => {
                log::info!(
                    "firmware: hub device detected port={} speed={:?}",
                    port,
                    speed
                );
                quiesce_ble_for_usb_enumeration(ble_quiesce_request, ble_quiesce_ready).await;
                let result =
                    enumerate_hub_port_with_retries(&mut hub, &mut config, port, speed).await;
                resume_ble_after_usb_enumeration(ble_quiesce_done).await;
                match result {
                    Ok((enum_info, config_len)) => {
                        events
                            .send(UsbHubTaskEvent::DeviceEnumerated {
                                port,
                                enum_info,
                                config_len,
                                config,
                            })
                            .await;
                    }
                    Err(error) => log::warn!(
                        "firmware: hub child enumerate failed port={} err={:?}",
                        port,
                        error
                    ),
                }
            }
            Ok(HandlerEvent::HandlerEvent(HubEvent::DeviceRemoved { port, .. })) => {
                events.send(UsbHubTaskEvent::DeviceRemoved { port }).await;
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!("firmware: hub event task failed: {:?}", error);
                events.send(UsbHubTaskEvent::Failed).await;
                return;
            }
        }
    }
}

#[embassy_executor::task(pool_size = 5)]
async fn usb_interface_reader_task(
    mut reader: UsbHidReader<'static, FirmwareBusHandle<'static>>,
    device_id: DeviceId,
    interface_id: InterfaceId,
    sender: Sender<'static, CriticalSectionRawMutex, UsbReaderEvent, USB_READER_QUEUE_CAPACITY>,
) {
    let mut report_buf = [0u8; REPORT_BUF_LEN];
    loop {
        match reader.read(&mut report_buf).await {
            Ok(len) => {
                let mut data = [0u8; REPORT_BUF_LEN];
                data[..len].copy_from_slice(&report_buf[..len]);
                sender
                    .send(UsbReaderEvent::Report(UsbReaderReport {
                        device_id,
                        interface_id,
                        len,
                        data,
                    }))
                    .await;
            }
            Err(error) => {
                sender
                    .send(UsbReaderEvent::Fatal {
                        device_id,
                        interface_id,
                        error,
                    })
                    .await;
                return;
            }
        }
    }
}

impl<'d> ActiveUsbInterfaceSlot<'d> {
    fn handle_report(&mut self, report: &[u8]) -> Option<UsbDecodedInput> {
        #[cfg(feature = "dual-s3-wired")]
        let raw = {
            self.raw_in_sequence = self.raw_in_sequence.wrapping_add(1);
            if self.raw_in_sequence == 0 {
                self.raw_in_sequence = 1;
            }
            match hidshift::interchip::RawEndpointReport::new(
                self.hid_info.interrupt_in_ep,
                self.raw_in_sequence,
                report,
            ) {
                Ok(report) => report,
                Err(error) => {
                    log::warn!(
                        "firmware: raw mirror IN rejected interface={} err={:?}",
                        self.interface_id.0,
                        error
                    );
                    return None;
                }
            }
        };
        let decoded = self
            .session
            .capture_input_report(report)
            .map_err(hidshift::usb_hid::host_runtime::UsbHidInterfaceRuntimeInputError::from)
            .and_then(|report| self.session.input_message(report));
        let (standard, movement_only) = match decoded {
            Ok(RuntimeInputMessage::BridgeEvent(hidshift::BridgeEvent::InputFrame(frame))) => {
                let movement_only = movement_only_frame(&frame, &mut self.last_mouse_buttons);
                (Some(frame), movement_only)
            }
            Ok(_) => (None, false),
            Err(error) => {
                log::debug!(
                    "firmware: usb input frame decode failed interface={} err={:?}",
                    self.interface_id.0,
                    error
                );
                (None, false)
            }
        };
        #[cfg(feature = "dual-s3-wired")]
        return Some(UsbDecodedInput {
            standard,
            movement_only,
            raw,
            device_id: self.session.device_id(),
        });
        #[cfg(not(feature = "dual-s3-wired"))]
        standard.map(|standard| UsbDecodedInput {
            standard: Some(standard),
            movement_only,
        })
    }
}

fn movement_only_frame(
    frame: &hidshift::input::InputFrame,
    previous_buttons: &mut hidshift::input::MouseButtons,
) -> bool {
    let hidshift::input::InputFrame::Standard(frame) = frame else {
        return false;
    };
    let Some(mouse) = frame.mouse else {
        return false;
    };
    let buttons_unchanged = mouse.buttons == *previous_buttons;
    *previous_buttons = mouse.buttons;
    frame.keyboard.is_none()
        && frame.consumer.is_none()
        && buttons_unchanged
        && mouse.movement != hidshift::input::MouseMovement::neutral()
}

async fn forward_usb_input(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    movement_queue: &mut hidshift::input::UsbMovementCoalescer<MAX_ACTIVE_USB_INTERFACES>,
    standard: Option<hidshift::input::InputFrame>,
    movement_only: bool,
    #[cfg(feature = "dual-s3-wired")] raw: hidshift::interchip::RawEndpointReport,
    #[cfg(feature = "dual-s3-wired")] device_id: DeviceId,
) {
    #[cfg(feature = "dual-s3-wired")]
    {
        let _ = (movement_queue, movement_only);
        sender
            .send(RuntimeInputMessage::UsbEndpointIn {
                device_id,
                report: raw,
                standard,
            })
            .await;
    }
    #[cfg(not(feature = "dual-s3-wired"))]
    {
        let Some(standard) = standard else {
            return;
        };
        let message = RuntimeInputMessage::BridgeEvent(hidshift::BridgeEvent::InputFrame(standard));
        while sender.free_capacity() > 0 {
            let Some(frame) = movement_queue.take_next() else {
                break;
            };
            if sender
                .try_send(RuntimeInputMessage::BridgeEvent(
                    hidshift::BridgeEvent::InputFrame(hidshift::input::InputFrame::Standard(
                        frame.clone(),
                    )),
                ))
                .is_err()
            {
                let _ = movement_queue.push(&frame);
                break;
            }
        }
        if movement_only {
            if sender.try_send(message.clone()).is_err() {
                let RuntimeInputMessage::BridgeEvent(hidshift::BridgeEvent::InputFrame(
                    hidshift::input::InputFrame::Standard(frame),
                )) = &message
                else {
                    return;
                };
                if let Err(error) = movement_queue.push(frame) {
                    log::warn!(
                        "firmware: mouse movement coalescer rejected input: {:?}",
                        error
                    );
                }
            }
        } else {
            while let Some(frame) = movement_queue.take_next() {
                sender
                    .send(RuntimeInputMessage::BridgeEvent(
                        hidshift::BridgeEvent::InputFrame(hidshift::input::InputFrame::Standard(
                            frame,
                        )),
                    ))
                    .await;
            }
            sender.send(message).await;
        }
    }
}

async fn handle_usb_reader_event<'d>(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    movement_queue: &mut hidshift::input::UsbMovementCoalescer<MAX_ACTIVE_USB_INTERFACES>,
    topology: &mut DefaultUsbTopologyManager,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    event: UsbReaderEvent,
) -> Option<DeviceId> {
    match event {
        UsbReaderEvent::Report(report) => {
            let Some(slot) = active_slots.iter_mut().find_map(|slot| {
                slot.as_mut().filter(|slot| {
                    slot.interface_id == report.interface_id
                        && slot.session.device_id() == report.device_id
                })
            }) else {
                return None;
            };
            let Some(UsbDecodedInput {
                standard,
                movement_only,
                #[cfg(feature = "dual-s3-wired")]
                raw,
                #[cfg(feature = "dual-s3-wired")]
                device_id,
            }) = slot.handle_report(&report.data[..report.len])
            else {
                return None;
            };
            forward_usb_input(
                sender,
                movement_queue,
                standard,
                movement_only,
                #[cfg(feature = "dual-s3-wired")]
                raw,
                #[cfg(feature = "dual-s3-wired")]
                device_id,
            )
            .await;
            None
        }
        UsbReaderEvent::Fatal {
            device_id,
            interface_id,
            error,
        } => {
            if !active_slots.iter().flatten().any(|slot| {
                slot.interface_id == interface_id && slot.session.device_id() == device_id
            }) {
                return None;
            }
            log::warn!(
                "firmware: usb read failed device={} interface={} err={:?}",
                device_id.0,
                interface_id.0,
                error
            );
            sender
                .send(RuntimeInputMessage::DiagnosticsEvent(
                    RuntimeDiagnosticsEvent::UsbError,
                ))
                .await;
            remove_interface_and_notify(sender, topology, active_slots, interface_id).await;
            (!active_slots
                .iter()
                .flatten()
                .any(|slot| slot.session.device_id() == device_id))
            .then_some(device_id)
        }
    }
}

async fn handle_hub_device_enumerated<'d>(
    spawner: Spawner,
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    bus_handle: &FirmwareBusHandle<'d>,
    topology: &mut DefaultUsbTopologyManager,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    hub_port_devices: &mut [Option<DeviceId>; MAX_HUB_PORTS],
    hub_device_id: DeviceId,
    port: u8,
    child_info: embassy_usb_host::handler::EnumerationInfo,
    child_config_desc: &[u8],
    #[cfg(feature = "dual-s3-wired")] mirror_capture: &mut MirrorCaptureScratch,
) where
    'd: 'static,
{
    let Some(port_index) =
        hidshift::usb_hid::topology::tracked_hub_port_index::<MAX_HUB_PORTS>(port)
    else {
        log::warn!(
            "firmware: rejecting hub child outside tracked port range port={} max={}",
            port,
            MAX_HUB_PORTS
        );
        return;
    };
    let child_device_id = match topology.connect_device(
        child_info.device_address,
        UsbDeviceRoute::Downstream {
            hub_device_id,
            port,
        },
    ) {
        Ok(device_id) => device_id,
        Err(error) => {
            log::warn!(
                "firmware: usb topology downstream device register failed: {:?}",
                error
            );
            bus_handle.free_address(child_info.device_address);
            return;
        }
    };
    if attach_hid_interfaces_for_device(
        spawner,
        sender,
        bus_handle,
        topology,
        active_slots,
        child_device_id,
        &child_info,
        child_config_desc,
        &[port + 1],
        #[cfg(feature = "dual-s3-wired")]
        MirrorCandidateId(port_index as u8),
        #[cfg(feature = "dual-s3-wired")]
        mirror_capture,
    )
    .await
    .is_ok()
    {
        hub_port_devices[port_index] = Some(child_device_id);
        return;
    }

    let _ =
        remove_device_and_notify(sender, bus_handle, topology, active_slots, child_device_id).await;
}

async fn handle_hub_device_removed<'d>(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    bus_handle: &FirmwareBusHandle<'d>,
    topology: &mut DefaultUsbTopologyManager,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    hub_port_devices: &mut [Option<DeviceId>; MAX_HUB_PORTS],
    port: u8,
) {
    log::info!("firmware: hub device removed port={}", port);
    if let Some(port_index) =
        hidshift::usb_hid::topology::tracked_hub_port_index::<MAX_HUB_PORTS>(port)
    {
        if let Some(child_device_id) = hub_port_devices[port_index].take() {
            let _ = remove_device_and_notify(
                sender,
                bus_handle,
                topology,
                active_slots,
                child_device_id,
            )
            .await;
        }
    }
}

#[embassy_executor::task]
pub async fn usb_input_task(
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
    log::info!("firmware: usb input task boot");
    log::info!("firmware: waiting for USB HID device on OTG");

    let usb = Usb::new(usb0, usb_dp, usb_dm);
    let host = new_otg_host(usb);
    let port_control = host.port_control();

    let (bus_controller, bus_handle) = embassy_usb_host::bus(host, &BUS_STATE);
    let config_buf = CONFIG_DESCRIPTOR_STORAGE.take();
    let mut topology = TOPOLOGY_STORAGE.take();
    let mut movement_queue = MOVEMENT_QUEUE_STORAGE.take();
    let mut active_slots = ACTIVE_SLOTS_STORAGE.take();
    let reader_receiver = USB_READER_QUEUE.receiver();
    #[cfg(feature = "dual-s3-wired")]
    let mirror_capture = MIRROR_CAPTURE_STORAGE.take();
    let root_event_receiver = USB_ROOT_EVENT_QUEUE.receiver();
    while root_event_receiver.try_receive().is_ok() {}
    let root_task = match usb_root_event_task(bus_controller, USB_ROOT_EVENT_QUEUE.sender()) {
        Ok(task) => task,
        Err(_) => {
            log::error!("firmware: USB root event task capacity exceeded");
            esp_hal::system::software_reset();
        }
    };
    spawner.spawn(root_task);

    loop {
        let speed = loop {
            match root_event_receiver.receive().await {
                UsbRootTaskEvent::Connected(speed) => break speed,
                UsbRootTaskEvent::Disconnected => {}
            }
        };
        log::info!("firmware: usb connected speed={:?}", speed);

        // All slots should have been detached on the previous disconnect, but
        // clear them defensively before accepting a new USB session.
        for slot in active_slots.iter_mut() {
            *slot = None;
        }

        let (enum_info, config_len) = match bus_handle
            .enumerate(BusRoute::Direct(speed), &mut config_buf[..])
            .await
        {
            Ok(result) => {
                log::info!(
                    "firmware: usb enumerated addr={} config_len={} vid={:04x} pid={:04x}",
                    result.0.device_address,
                    result.1,
                    result.0.device_desc.vendor_id,
                    result.0.device_desc.product_id
                );
                result
            }
            Err(error) => {
                log::warn!("firmware: usb enumerate failed: {:?}", error);
                sender
                    .send(RuntimeInputMessage::DiagnosticsEvent(
                        RuntimeDiagnosticsEvent::UsbError,
                    ))
                    .await;
                continue;
            }
        };

        let device_id =
            match topology.connect_device(enum_info.device_address, UsbDeviceRoute::Direct) {
                Ok(device_id) => device_id,
                Err(error) => {
                    log::warn!("firmware: usb topology device register failed: {:?}", error);
                    bus_handle.free_address(enum_info.device_address);
                    continue;
                }
            };
        let config_desc = &config_buf[..config_len];
        if config_descriptor_has_interface_class(config_desc, 0x09) {
            let Ok(hub) =
                HubHandler::<_, MAX_HUB_PORTS>::try_register(&bus_handle, &enum_info).await
            else {
                log::warn!("firmware: root hub registration failed");
                let _ = topology.remove_device(device_id);
                bus_handle.free_address(enum_info.device_address);
                continue;
            };
            log::info!("firmware: root hub registered ports_max={}", MAX_HUB_PORTS);
            let mut hub_port_devices = [None; MAX_HUB_PORTS];
            let hub_event_receiver = USB_HUB_EVENT_QUEUE.receiver();
            while hub_event_receiver.try_receive().is_ok() {}
            let hub_task = match usb_hub_task(
                hub,
                USB_HUB_EVENT_QUEUE.sender(),
                ble_quiesce_request,
                ble_quiesce_ready,
                ble_quiesce_done,
            ) {
                Ok(task) => task,
                Err(_) => {
                    log::warn!("firmware: usb hub task capacity exceeded");
                    let _ = topology.remove_device(device_id);
                    bus_handle.free_address(enum_info.device_address);
                    continue;
                }
            };
            spawner.spawn(hub_task);
            loop {
                match select4(
                    hub_event_receiver.receive(),
                    reader_receiver.receive(),
                    receiver.receive(),
                    root_event_receiver.receive(),
                )
                .await
                {
                    Either4::First(UsbHubTaskEvent::DeviceEnumerated {
                        port,
                        enum_info: child_info,
                        config_len: child_config_len,
                        config: child_config,
                    }) => {
                        handle_hub_device_enumerated(
                            spawner,
                            &sender,
                            &bus_handle,
                            &mut topology,
                            &mut active_slots,
                            &mut hub_port_devices,
                            device_id,
                            port,
                            child_info,
                            &child_config[..child_config_len],
                            #[cfg(feature = "dual-s3-wired")]
                            mirror_capture,
                        )
                        .await;
                    }
                    Either4::First(UsbHubTaskEvent::DeviceRemoved { port }) => {
                        handle_hub_device_removed(
                            &sender,
                            &bus_handle,
                            &mut topology,
                            &mut active_slots,
                            &mut hub_port_devices,
                            port,
                        )
                        .await;
                    }
                    Either4::First(UsbHubTaskEvent::Failed) => {
                        let _ = remove_device_and_notify(
                            &sender,
                            &bus_handle,
                            &mut topology,
                            &mut active_slots,
                            device_id,
                        )
                        .await;
                        break;
                    }
                    Either4::Second(event) => {
                        if let Some(failed_device_id) = handle_usb_reader_event(
                            &sender,
                            &mut movement_queue,
                            &mut topology,
                            &mut active_slots,
                            event,
                        )
                        .await
                        {
                            for child_device_id in hub_port_devices.iter_mut() {
                                if *child_device_id == Some(failed_device_id) {
                                    *child_device_id = None;
                                }
                            }
                            let _ = remove_device_and_notify(
                                &sender,
                                &bus_handle,
                                &mut topology,
                                &mut active_slots,
                                failed_device_id,
                            )
                            .await;
                        }
                    }
                    Either4::Third(command) => {
                        handle_usb_command(
                            &sender,
                            &bus_handle,
                            &port_control,
                            &mut active_slots,
                            command,
                        )
                        .await;
                    }
                    Either4::Fourth(root_event) => {
                        log::info!("firmware: USB root session ended: {:?}", root_event);
                        let _ = remove_device_and_notify(
                            &sender,
                            &bus_handle,
                            &mut topology,
                            &mut active_slots,
                            device_id,
                        )
                        .await;
                        break;
                    }
                }
            }
            continue;
        }

        if attach_hid_interfaces_for_device(
            spawner,
            &sender,
            &bus_handle,
            &mut topology,
            &mut active_slots,
            device_id,
            &enum_info,
            config_desc,
            &[0],
            #[cfg(feature = "dual-s3-wired")]
            MirrorCandidateId(0),
            #[cfg(feature = "dual-s3-wired")]
            mirror_capture,
        )
        .await
        .is_err()
        {
            let _ = remove_device_and_notify(
                &sender,
                &bus_handle,
                &mut topology,
                &mut active_slots,
                device_id,
            )
            .await;
            continue;
        }

        loop {
            match select3(
                reader_receiver.receive(),
                receiver.receive(),
                root_event_receiver.receive(),
            )
            .await
            {
                Either3::First(event) => {
                    if let Some(failed_device_id) = handle_usb_reader_event(
                        &sender,
                        &mut movement_queue,
                        &mut topology,
                        &mut active_slots,
                        event,
                    )
                    .await
                    {
                        let _ = remove_device_and_notify(
                            &sender,
                            &bus_handle,
                            &mut topology,
                            &mut active_slots,
                            failed_device_id,
                        )
                        .await;
                        break;
                    }
                }
                Either3::Second(command) => {
                    handle_usb_command(
                        &sender,
                        &bus_handle,
                        &port_control,
                        &mut active_slots,
                        command,
                    )
                    .await;
                }
                Either3::Third(root_event) => {
                    log::info!("firmware: USB root session ended: {:?}", root_event);
                    let _ = remove_device_and_notify(
                        &sender,
                        &bus_handle,
                        &mut topology,
                        &mut active_slots,
                        device_id,
                    )
                    .await;
                    break;
                }
            }
        }
    }
}

async fn handle_usb_command<'d>(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    bus_handle: &FirmwareBusHandle<'d>,
    port_control: &OtgHostPortControl<'d>,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    command: UsbHostTaskCommand,
) {
    match command {
        UsbHostTaskCommand::SetBusState(state) => match state {
            hidshift::UsbHostBusState::Running => {
                log::info!("firmware: resuming USB input bus");
                port_control.resume().await;
            }
            hidshift::UsbHostBusState::Suspended => {
                log::info!("firmware: suspending USB input bus");
                port_control.suspend().await;
            }
        },
        UsbHostTaskCommand::KeyboardLedWrite {
            interface_id,
            device_id,
            bytes,
        } => {
            let Some(slot) = active_slots.iter_mut().find_map(|slot| {
                slot.as_mut().filter(|slot| {
                    slot.interface_id == interface_id && slot.session.device_id() == device_id
                })
            }) else {
                log::warn!(
                    "firmware: usb LED target missing device={} interface={}",
                    device_id.0,
                    interface_id.0
                );
                return;
            };
            if slot.last_led_bytes == Some(bytes) {
                return;
            }
            if !slot.led_output {
                log::debug!(
                    "firmware: usb LED ignored interface={} no_led_output",
                    slot.interface_id.0
                );
                return;
            }
            match UsbKeyboardLedWriter::new_for_interface(
                bus_handle,
                slot.hid_info,
                &slot.enum_info,
            ) {
                Ok(mut led_writer) => match with_timeout(
                    Duration::from_millis(USB_LED_WRITE_TIMEOUT_MS),
                    led_writer.write_leds(bytes),
                )
                .await
                {
                    Ok(Ok(())) => slot.last_led_bytes = Some(bytes),
                    Ok(Err(error)) => log::warn!(
                        "firmware: usb LED write failed interface={} err={:?}",
                        slot.interface_id.0,
                        error
                    ),
                    Err(_) => {
                        log::warn!(
                            "firmware: usb LED write timeout interface={}",
                            slot.interface_id.0
                        );
                        let _ = sender.try_send(RuntimeInputMessage::DiagnosticsEvent(
                            RuntimeDiagnosticsEvent::UsbLedWriteTimedOut,
                        ));
                    }
                },
                Err(error) => log::warn!(
                    "firmware: usb LED writer unsupported interface={} err={:?}",
                    slot.interface_id.0,
                    error
                ),
            }
        }
        #[cfg(feature = "dual-s3-wired")]
        UsbHostTaskCommand::MirrorEndpointOut { device_id, report } => {
            let Some(slot) = active_slots.iter().find_map(|slot| {
                slot.as_ref().filter(|slot| {
                    slot.session.device_id() == device_id
                        && slot.hid_info.interrupt_out_ep == report.endpoint_address
                })
            }) else {
                log::warn!(
                    "firmware: mirror OUT target missing device={} endpoint=0x{:02x}",
                    device_id.0,
                    report.endpoint_address
                );
                return;
            };
            let mut writer = match UsbRawOutWriter::new(bus_handle, slot.hid_info, &slot.enum_info)
            {
                Ok(Some(writer)) => writer,
                Ok(None) | Err(_) => {
                    log::warn!(
                        "firmware: mirror OUT pipe unavailable endpoint=0x{:02x}",
                        report.endpoint_address
                    );
                    return;
                }
            };
            match with_timeout(Duration::from_millis(250), writer.write(report.data())).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::warn!(
                    "firmware: mirror OUT failed endpoint=0x{:02x} err={:?}",
                    report.endpoint_address,
                    error
                ),
                Err(_) => log::warn!(
                    "firmware: mirror OUT timeout endpoint=0x{:02x}",
                    report.endpoint_address
                ),
            }
        }
        #[cfg(feature = "dual-s3-wired")]
        UsbHostTaskCommand::MirrorControlRequest { device_id, request } => {
            let Some(slot) = active_slots
                .iter()
                .flatten()
                .find(|slot| slot.session.device_id() == device_id)
            else {
                send_mirror_control_response(
                    sender,
                    MirrorControlResponse::new(
                        request.request_id,
                        ControlStatus::Disconnected,
                        &[],
                    ),
                )
                .await;
                return;
            };
            let mut response_data = [0u8; CONTROL_DATA_MAX_LEN];
            let mut control = match UsbHidControl::new(bus_handle, slot.hid_info, &slot.enum_info) {
                Ok(control) => control,
                Err(_) => {
                    send_mirror_control_response(
                        sender,
                        MirrorControlResponse::new(
                            request.request_id,
                            ControlStatus::Disconnected,
                            &[],
                        ),
                    )
                    .await;
                    return;
                }
            };
            let result = with_timeout(
                Duration::from_millis(250),
                control.forward(request.setup_packet, request.data(), &mut response_data),
            )
            .await;
            let response = match result {
                Ok(Ok(length)) => MirrorControlResponse::new(
                    request.request_id,
                    ControlStatus::Success,
                    &response_data[..length],
                ),
                Ok(Err(HidError::Transfer(PipeError::Stall))) => {
                    MirrorControlResponse::new(request.request_id, ControlStatus::Stall, &[])
                }
                Ok(Err(HidError::Transfer(PipeError::Disconnected))) => {
                    MirrorControlResponse::new(request.request_id, ControlStatus::Disconnected, &[])
                }
                Ok(Err(_)) | Err(_) => {
                    MirrorControlResponse::new(request.request_id, ControlStatus::Timeout, &[])
                }
            };
            send_mirror_control_response(sender, response).await;
        }
    }
}

#[cfg(feature = "dual-s3-wired")]
async fn send_mirror_control_response(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    response: Result<MirrorControlResponse, hidshift::interchip::MessageError>,
) {
    if let Ok(response) = response {
        sender
            .send(RuntimeInputMessage::MirrorControlCompleted(response))
            .await;
    }
}

async fn quiesce_ble_for_usb_enumeration(
    ble_quiesce_request: Sender<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_ready: Receiver<'static, CriticalSectionRawMutex, (), 1>,
) {
    log::debug!("firmware: usb requesting ble quiesce for hub enumeration");
    if with_timeout(
        Duration::from_millis(BLE_QUIESCE_HANDSHAKE_TIMEOUT_MS),
        async {
            ble_quiesce_request.send(()).await;
            ble_quiesce_ready.receive().await;
        },
    )
    .await
    .is_err()
    {
        log::error!("firmware: USB BLE quiesce handshake timed out; rebooting");
        esp_hal::system::software_reset();
    }
    log::debug!("firmware: usb ble quiesce ready for hub enumeration");
}

async fn resume_ble_after_usb_enumeration(
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
) {
    if with_timeout(
        Duration::from_millis(BLE_QUIESCE_HANDSHAKE_TIMEOUT_MS),
        ble_quiesce_done.send(()),
    )
    .await
    .is_err()
    {
        log::error!("firmware: USB BLE resume handshake timed out; rebooting");
        esp_hal::system::software_reset();
    }
}

async fn enumerate_hub_port_with_retries<'d, A: embassy_usb_driver::host::UsbHostAllocator<'d>>(
    hub: &mut HubHandler<'d, A, MAX_HUB_PORTS>,
    config_buf: &mut [u8],
    port: u8,
    speed: embassy_usb_driver::Speed,
) -> Result<(embassy_usb_host::handler::EnumerationInfo, usize), embassy_usb_host::EnumerationError>
{
    const ATTEMPTS: usize = 4;
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = with_timeout(
            Duration::from_millis(HUB_CHILD_ENUMERATION_TIMEOUT_MS),
            hub.enumerate_port(config_buf, port, speed),
        )
        .await;
        match result {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(error)) if attempt < ATTEMPTS => {
                log::debug!(
                    "firmware: hub child enumerate retry port={} attempt={} err={:?}",
                    port,
                    attempt,
                    error
                );
                Timer::after_millis(100).await;
            }
            Ok(Err(error)) => return Err(error),
            Err(_) if attempt < ATTEMPTS => {
                log::debug!(
                    "firmware: hub child enumerate retry port={} attempt={} err=Timeout",
                    port,
                    attempt
                );
                Timer::after_millis(100).await;
            }
            Err(_) => return Err(embassy_usb_host::EnumerationError::RequestFailed),
        }
    }
}

async fn attach_hid_interfaces_for_device<'d>(
    spawner: Spawner,
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    bus_handle: &FirmwareBusHandle<'d>,
    topology: &mut DefaultUsbTopologyManager,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    device_id: DeviceId,
    enum_info: &embassy_usb_host::handler::EnumerationInfo,
    config_desc: &[u8],
    physical_port_path: &[u8],
    #[cfg(feature = "dual-s3-wired")] mirror_candidate: MirrorCandidateId,
    #[cfg(feature = "dual-s3-wired")] mirror_capture: &mut MirrorCaptureScratch,
) -> Result<(), ()>
where
    'd: 'static,
{
    let mut pending_readers = heapless::Vec::<
        (
            usize,
            UsbHidReader<'d, FirmwareBusHandle<'d>>,
            DeviceId,
            InterfaceId,
        ),
        MAX_ACTIVE_USB_INTERFACES,
    >::new();
    let product_name = read_usb_product_name(bus_handle, enum_info)
        .await
        .unwrap_or_else(FixedName::empty);
    let identity =
        match read_usb_string_units(bus_handle, enum_info, enum_info.device_desc.serial_number)
            .await
        {
            Some(serial) if !serial.is_empty() => hidshift::InputIdentity::from_serial_utf16(
                enum_info.device_desc.vendor_id,
                enum_info.device_desc.product_id,
                &serial,
            )
            .unwrap_or_else(|| {
                hidshift::InputIdentity::from_port_path(
                    enum_info.device_desc.vendor_id,
                    enum_info.device_desc.product_id,
                    physical_port_path,
                )
            }),
            _ => hidshift::InputIdentity::from_port_path(
                enum_info.device_desc.vendor_id,
                enum_info.device_desc.product_id,
                physical_port_path,
            ),
        };
    let hid_interfaces = match find_hid_interfaces::<8>(config_desc) {
        Ok(interfaces) if !interfaces.is_empty() => interfaces,
        Ok(_) => {
            log::warn!("firmware: usb hid interface missing");
            return Err(());
        }
        Err(error) => {
            log::warn!("firmware: usb hid interface scan failed: {:?}", error);
            return Err(());
        }
    };
    if hid_interfaces.len() > 1 {
        log::info!(
            "firmware: usb composite hid detected interfaces={}",
            hid_interfaces.len()
        );
    }

    for hid_info in hid_interfaces.iter().copied() {
        let Some(slot_index) = active_slots.iter().position(Option::is_none) else {
            log::warn!(
                "firmware: skipping USB HID interface={} to preserve host control channel",
                hid_info.interface_number
            );
            continue;
        };
        let interface_id = match topology.register_interface(device_id, hid_info.interface_number) {
            Ok(interface_id) => interface_id,
            Err(error) => {
                log::warn!(
                    "firmware: usb topology interface register failed: {:?}",
                    error
                );
                return Err(());
            }
        };
        let mut control = match UsbHidControl::new(bus_handle, hid_info, enum_info) {
            Ok(control) => control,
            Err(error) => {
                log::warn!("firmware: usb hid control unsupported: {:?}", error);
                return Err(());
            }
        };
        match with_timeout(Duration::from_millis(500), control.set_idle(0, 0)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log::debug!(
                "firmware: usb set_idle failed interface={} err={:?}",
                interface_id.0,
                error
            ),
            Err(_) => log::debug!(
                "firmware: usb set_idle timed out interface={}",
                interface_id.0
            ),
        }
        if hid_info.supports_set_protocol() {
            match with_timeout(
                Duration::from_millis(500),
                control.ensure_protocol(PROTOCOL_REPORT),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::debug!(
                    "firmware: usb set_protocol(report) failed interface={} err={:?}",
                    interface_id.0,
                    error
                ),
                Err(_) => log::debug!(
                    "firmware: usb set_protocol(report) timed out interface={}",
                    interface_id.0
                ),
            }
        }

        let mut report_descriptor_buf = [0u8; REPORT_DESCRIPTOR_BUF_LEN];
        let session = match fetch_report_descriptor_with_retries(
            &mut control,
            &mut report_descriptor_buf,
            interface_id,
        )
        .await
        {
            Ok(len) => {
                let report_descriptor =
                    ReportDescriptor::<MAX_REPORT_FIELDS>::parse(&report_descriptor_buf[..len]);
                let descriptor = match to_core_descriptor(&report_descriptor) {
                    Ok(descriptor) => descriptor,
                    Err(error) => {
                        log::warn!(
                            "firmware: usb descriptor capacity exceeded interface={} err={:?}",
                            interface_id.0,
                            error
                        );
                        return Err(());
                    }
                };
                let source = match hidshift::UsbHidInterfaceSnapshot::new(
                    device_id,
                    interface_id,
                    hid_info.interface_number,
                    0,
                    hid_info.interface_subclass,
                    hid_info.interface_protocol,
                    &report_descriptor_buf[..len],
                ) {
                    Ok(source) => source,
                    Err(error) => {
                        log::warn!(
                            "firmware: usb source snapshot rejected interface={} err={:?}",
                            interface_id.0,
                            error
                        );
                        return Err(());
                    }
                };
                match UsbHidInterfaceRuntimeSession::<MAX_REPORT_FIELDS, MAX_REPORT_EVENTS>::from_source_snapshot(
                    source,
                    descriptor,
                    hid_info.boot_keyboard_led_fallback_allowed(),
                ) {
                    Ok(session) => session,
                    Err(error) => {
                        log::warn!(
                            "firmware: usb descriptor unsupported interface={} err={:?}",
                            interface_id.0,
                            error
                        );
                        return Err(());
                    }
                }
            }
            Err(error) => {
                log::warn!(
                    "firmware: usb report descriptor read failed interface={} err={:?}",
                    interface_id.0,
                    error
                );
                return Err(());
            }
        };
        drop(control);

        let led_output = if let Some(report) = session.led_output() {
            match report.build(KeyboardLedState::empty()) {
                Ok(bytes) => {
                    match UsbKeyboardLedWriter::new_for_interface(bus_handle, hid_info, enum_info) {
                        Ok(mut writer) => match writer.write_leds(bytes).await {
                            Ok(()) => true,
                            Err(error) => {
                                log::debug!(
                                    "firmware: usb led output unavailable interface={} err={:?}",
                                    interface_id.0,
                                    error
                                );
                                false
                            }
                        },
                        Err(error) => {
                            log::debug!(
                                "firmware: usb led output unavailable interface={} err={:?}",
                                interface_id.0,
                                error
                            );
                            false
                        }
                    }
                }
                Err(error) => {
                    log::debug!(
                        "firmware: usb led output invalid interface={} err={:?}",
                        interface_id.0,
                        error
                    );
                    false
                }
            }
        } else {
            false
        };

        let reader = match UsbHidReader::new(bus_handle, hid_info, enum_info) {
            Ok(reader) => reader,
            Err(HidError::NoPipe) => {
                log::warn!(
                    "firmware: skipping USB HID interface={} because no host channel is available",
                    hid_info.interface_number
                );
                let _ = topology.remove_interface(interface_id);
                continue;
            }
            Err(error) => {
                log::warn!("firmware: usb hid reader unsupported: {:?}", error);
                return Err(());
            }
        };
        log::info!(
            "firmware: usb runtime session ready device={} interface={} fields={} report_ids={} interval_ms={} out_ep=0x{:02x}",
            session.device_id().0,
            session.interface_id().0,
            session.descriptor().len(),
            session.descriptor().has_report_ids,
            hid_info.interrupt_in_interval_ms,
            hid_info.interrupt_out_ep
        );
        sender
            .send(RuntimeInputMessage::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                led_output: led_output.then(|| session.led_output()).flatten(),
            })
            .await;
        sender
            .send(RuntimeInputMessage::UsbDeviceMetadataUpdated {
                device_id,
                vendor_id: enum_info.device_desc.vendor_id,
                product_id: enum_info.device_desc.product_id,
                instance_hash: identity.instance_hash,
                name: product_name,
                flags: session.device_kind_flags(),
            })
            .await;
        active_slots[slot_index] = Some(ActiveUsbInterfaceSlot {
            interface_id,
            led_output,
            hid_info,
            enum_info: *enum_info,
            session,
            last_mouse_buttons: hidshift::input::MouseButtons::empty(),
            last_led_bytes: None,
            _lifetime: PhantomData,
            #[cfg(feature = "dual-s3-wired")]
            raw_in_sequence: 0,
        });
        pending_readers
            .push((slot_index, reader, device_id, interface_id))
            .map_err(|_| ())?;
    }

    if pending_readers.is_empty() {
        log::warn!("firmware: usb device has no serviceable HID interfaces");
        return Err(());
    }

    #[cfg(feature = "dual-s3-wired")]
    match capture_and_forward_mirror_profile(
        sender,
        bus_handle,
        active_slots,
        device_id,
        enum_info,
        config_desc,
        mirror_candidate,
        physical_port_path,
        mirror_capture,
    )
    .await
    {
        Ok(()) => {
            sender
                .send(RuntimeInputMessage::UsbDeviceMetadataUpdated {
                    device_id,
                    vendor_id: enum_info.device_desc.vendor_id,
                    product_id: enum_info.device_desc.product_id,
                    instance_hash: identity.instance_hash,
                    name: product_name,
                    flags: 0x10,
                })
                .await;
        }
        Err(error) => {
            log::warn!(
                "firmware: usb device is not mirrorable device={} err={:?}",
                device_id.0,
                error
            );
        }
    }

    // Start interrupt polling only after every control request and mirror
    // profile transfer has completed. Otherwise a 1 kHz mouse fills the reader
    // queue while this task is still attaching the remaining interfaces.
    for (_slot_index, reader, device_id, interface_id) in pending_readers {
        let reader_task = match usb_interface_reader_task(
            reader,
            device_id,
            interface_id,
            USB_READER_QUEUE.sender(),
        ) {
            Ok(token) => token,
            Err(_) => {
                log::warn!(
                    "firmware: usb reader task capacity exceeded device={} interface={}",
                    device_id.0,
                    interface_id.0
                );
                return Err(());
            }
        };
        spawner.spawn(reader_task);
    }

    Ok(())
}

#[cfg(feature = "dual-s3-wired")]
#[allow(clippy::too_many_arguments)]
async fn capture_and_forward_mirror_profile<'d>(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    bus_handle: &FirmwareBusHandle<'d>,
    active_slots: &[Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    device_id: DeviceId,
    enum_info: &embassy_usb_host::handler::EnumerationInfo,
    config_desc: &[u8],
    candidate: MirrorCandidateId,
    port_path: &[u8],
    scratch: &mut MirrorCaptureScratch,
) -> Result<(), MirrorProfileCaptureError> {
    if enum_info.speed() != embassy_usb_driver::Speed::Full {
        return Err(MirrorProfileCaptureError::UnsupportedUsbSpeed);
    }

    let endpoint = EndpointInfo {
        addr: EndpointAddress::from_parts(0, Direction::In),
        ep_type: EndpointType::Control,
        max_packet_size: enum_info.device_desc.max_packet_size0 as u16,
        interval_ms: 0,
    };
    let mut control = bus_handle
        .alloc_pipe::<pipe::Control, pipe::InOut>(
            enum_info.device_address,
            &endpoint,
            enum_info.split(),
        )
        .map_err(|_| MirrorProfileCaptureError::ControlPipeUnavailable)?;

    let mut bos_len = 0usize;
    if enum_info.device_desc.bcd_usb >= 0x0201 {
        let setup = get_descriptor_setup(15, 0, 0, 5);
        if let Ok(Ok(length)) = with_timeout(
            Duration::from_millis(USB_CONTROL_REQUEST_TIMEOUT_MS),
            control.control_in(&setup.to_bytes(), &mut scratch.bos[..5]),
        )
        .await
            && length >= 5
            && scratch.bos[1] == 15
        {
            let total = usize::from(u16::from_le_bytes([scratch.bos[2], scratch.bos[3]]))
                .min(scratch.bos.len());
            let setup = get_descriptor_setup(15, 0, 0, total as u16);
            if let Ok(Ok(length)) = with_timeout(
                Duration::from_millis(USB_CONTROL_REQUEST_TIMEOUT_MS),
                control.control_in(&setup.to_bytes(), &mut scratch.bos[..total]),
            )
            .await
            {
                bos_len = length.min(total);
            }
        }
    }

    scratch.string_lengths.fill(0);
    let language_setup = get_descriptor_setup(3, 0, 0, MIRROR_STRING_DESCRIPTOR_MAX_LEN as u16);
    let mut string_count = 0usize;
    let language_id = match with_timeout(
        Duration::from_millis(USB_CONTROL_REQUEST_TIMEOUT_MS),
        control.control_in(&language_setup.to_bytes(), &mut scratch.strings[0]),
    )
    .await
    {
        Ok(Ok(length)) if length >= 4 && scratch.strings[0][1] == 3 => {
            let length = usize::from(scratch.strings[0][0])
                .min(length)
                .min(MIRROR_STRING_DESCRIPTOR_MAX_LEN);
            scratch.string_indices[0] = 0;
            scratch.string_lang_ids[0] = 0;
            scratch.string_lengths[0] = length;
            string_count = 1;
            u16::from_le_bytes([scratch.strings[0][2], scratch.strings[0][3]])
        }
        _ => 0x0409,
    };

    let mut indices = [0u8; MIRROR_STRING_RECORDS_MAX - 1];
    let index_count = collect_string_indices(
        enum_info.device_desc.manufacturer,
        enum_info.device_desc.product,
        enum_info.device_desc.serial_number,
        config_desc,
        &mut indices,
    );
    for index in indices[..index_count].iter().copied() {
        if string_count == MIRROR_STRING_RECORDS_MAX {
            break;
        }
        let setup = get_descriptor_setup(
            3,
            index,
            language_id,
            MIRROR_STRING_DESCRIPTOR_MAX_LEN as u16,
        );
        if let Ok(Ok(length)) = with_timeout(
            Duration::from_millis(USB_CONTROL_REQUEST_TIMEOUT_MS),
            control.control_in(&setup.to_bytes(), &mut scratch.strings[string_count][..]),
        )
        .await
            && length >= 2
            && scratch.strings[string_count][1] == 3
        {
            let length = usize::from(scratch.strings[string_count][0])
                .min(length)
                .min(MIRROR_STRING_DESCRIPTOR_MAX_LEN);
            scratch.string_indices[string_count] = index;
            scratch.string_lang_ids[string_count] = language_id;
            scratch.string_lengths[string_count] = length;
            string_count += 1;
        }
    }
    drop(control);

    let mut strings = heapless::Vec::<StringRecord<'_>, MIRROR_STRING_RECORDS_MAX>::new();
    for index in 0..string_count {
        strings
            .push(StringRecord {
                index: scratch.string_indices[index],
                lang_id: scratch.string_lang_ids[index],
                descriptor: &scratch.strings[index][..scratch.string_lengths[index]],
            })
            .map_err(|_| MirrorProfileCaptureError::TooManyStrings)?;
    }
    let mut reports = heapless::Vec::<HidReportRecord<'_>, 4>::new();
    for slot in active_slots
        .iter()
        .flatten()
        .filter(|slot| slot.session.device_id() == device_id)
    {
        reports
            .push(HidReportRecord {
                interface_number: slot.hid_info.interface_number,
                descriptor: slot.session.source_snapshot().report_descriptor(),
            })
            .map_err(|_| MirrorProfileCaptureError::TooManyHidInterfaces)?;
    }

    let device_descriptor = raw_device_descriptor(enum_info.device_desc);
    let captured = capture_mirror_profile(
        MirrorCaptureSource {
            flags: 0,
            device_descriptor: &device_descriptor,
            configuration_descriptor: config_desc,
            bos_descriptor: &scratch.bos[..bos_len],
            strings: strings.as_slice(),
            hid_reports: reports.as_slice(),
            port_path,
        },
        &mut scratch.image,
    )
    .map_err(MirrorProfileCaptureError::Capture)?;

    let transfer = ProfileTransferEncoder::new(
        captured.profile_hash,
        captured.profile_hash,
        &scratch.image[..captured.image_length],
    )
    .map_err(|_| MirrorProfileCaptureError::ProfileTransferRejected)?;
    for command in transfer {
        sender
            .send(RuntimeInputMessage::DeviceCommandRequested(command.into()))
            .await;
    }
    sender
        .send(RuntimeInputMessage::MirrorCandidateRegistered {
            candidate,
            stable_id: captured.stable_id,
            profile_hash: Some(captured.profile_hash),
            synthetic: false,
            source_device: Some(device_id),
        })
        .await;
    Ok(())
}

#[cfg(feature = "dual-s3-wired")]
fn get_descriptor_setup(
    descriptor_type: u8,
    index: u8,
    language_id: u16,
    length: u16,
) -> SetupPacket {
    SetupPacket {
        request_type: RequestType {
            direction: Direction::In,
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
        },
        request: 6,
        value: u16::from(descriptor_type) << 8 | u16::from(index),
        index: language_id,
        length,
    }
}

#[cfg(feature = "dual-s3-wired")]
fn raw_device_descriptor(descriptor: embassy_usb_host::descriptor::DeviceDescriptor) -> [u8; 18] {
    let mut raw = [0u8; 18];
    raw[0] = descriptor.len;
    raw[1] = descriptor.descriptor_type;
    raw[2..4].copy_from_slice(&descriptor.bcd_usb.to_le_bytes());
    raw[4] = descriptor.device_class;
    raw[5] = descriptor.device_subclass;
    raw[6] = descriptor.device_protocol;
    raw[7] = descriptor.max_packet_size0;
    raw[8..10].copy_from_slice(&descriptor.vendor_id.to_le_bytes());
    raw[10..12].copy_from_slice(&descriptor.product_id.to_le_bytes());
    raw[12..14].copy_from_slice(&descriptor.bcd_device.to_le_bytes());
    raw[14] = descriptor.manufacturer;
    raw[15] = descriptor.product;
    raw[16] = descriptor.serial_number;
    raw[17] = descriptor.num_configurations;
    raw
}

#[cfg(feature = "dual-s3-wired")]
fn collect_string_indices(
    manufacturer: u8,
    product: u8,
    serial: u8,
    config_desc: &[u8],
    output: &mut [u8],
) -> usize {
    let mut count = 0usize;
    for index in [manufacturer, product, serial] {
        push_unique_string_index(index, output, &mut count);
    }
    let mut offset = 0usize;
    while offset + 2 <= config_desc.len() {
        let length = usize::from(config_desc[offset]);
        if length < 2 || offset + length > config_desc.len() {
            break;
        }
        let descriptor = &config_desc[offset..offset + length];
        match descriptor[1] {
            2 if length >= 7 => push_unique_string_index(descriptor[6], output, &mut count),
            4 if length >= 9 => push_unique_string_index(descriptor[8], output, &mut count),
            _ => {}
        }
        offset += length;
    }
    count
}

#[cfg(feature = "dual-s3-wired")]
fn push_unique_string_index(index: u8, output: &mut [u8], count: &mut usize) {
    if index == 0 || output[..*count].contains(&index) || *count == output.len() {
        return;
    }
    output[*count] = index;
    *count += 1;
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

async fn read_usb_product_name<'d>(
    bus_handle: &FirmwareBusHandle<'d>,
    enum_info: &embassy_usb_host::handler::EnumerationInfo,
) -> Option<FixedName> {
    let units = read_usb_string_units(bus_handle, enum_info, enum_info.device_desc.product).await?;
    let mut ascii = heapless::Vec::<u8, 32>::new();
    for code in units {
        ascii
            .push(if (0x20..=0x7e).contains(&code) {
                code as u8
            } else {
                b'?'
            })
            .ok()?;
    }
    core::str::from_utf8(&ascii)
        .ok()
        .and_then(FixedName::from_ascii)
}

async fn read_usb_string_units<'d>(
    bus_handle: &FirmwareBusHandle<'d>,
    enum_info: &embassy_usb_host::handler::EnumerationInfo,
    string_index: u8,
) -> Option<heapless::Vec<u16, 32>> {
    if string_index == 0 {
        return None;
    }
    let endpoint = EndpointInfo {
        addr: EndpointAddress::from_parts(0, embassy_usb_driver::Direction::In),
        ep_type: EndpointType::Control,
        max_packet_size: enum_info.device_desc.max_packet_size0 as u16,
        interval_ms: 0,
    };
    let mut control = bus_handle
        .alloc_pipe::<pipe::Control, pipe::InOut>(
            enum_info.device_address,
            &endpoint,
            enum_info.split(),
        )
        .ok()?;
    let language_request = SetupPacket {
        request_type: RequestType {
            direction: Direction::In,
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
        },
        request: 6,
        value: 0x0300,
        index: 0,
        length: 4,
    };
    let mut language = [0u8; 4];
    let language_id = match with_timeout(
        Duration::from_millis(USB_CONTROL_REQUEST_TIMEOUT_MS),
        control.control_in(&language_request.to_bytes(), &mut language),
    )
    .await
    {
        Ok(Ok(length)) if length >= 4 && language[1] == 3 => {
            u16::from_le_bytes([language[2], language[3]])
        }
        _ => 0x0409,
    };
    let request = SetupPacket {
        request_type: RequestType {
            direction: Direction::In,
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
        },
        request: 6,
        value: 0x0300 | string_index as u16,
        index: language_id,
        length: 66,
    };
    let mut descriptor = [0u8; 66];
    let length = with_timeout(
        Duration::from_millis(USB_CONTROL_REQUEST_TIMEOUT_MS),
        control.control_in(&request.to_bytes(), &mut descriptor),
    )
    .await
    .ok()?
    .ok()?;
    if length < 2 || descriptor[1] != 3 {
        return None;
    }
    let descriptor_len = usize::from(descriptor[0]).min(length).min(descriptor.len());
    let mut units = heapless::Vec::<u16, 32>::new();
    for unit in descriptor[2..descriptor_len].chunks_exact(2) {
        let code = u16::from_le_bytes([unit[0], unit[1]]);
        if code == 0 {
            break;
        }
        units.push(code).ok()?;
    }
    Some(units)
}

async fn fetch_report_descriptor_with_retries<'d>(
    control: &mut UsbHidControl<'d, FirmwareBusHandle<'d>>,
    report_descriptor_buf: &mut [u8; REPORT_DESCRIPTOR_BUF_LEN],
    interface_id: InterfaceId,
) -> Result<usize, HidError> {
    const ATTEMPTS: usize = 3;
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = with_timeout(
            Duration::from_millis(HID_REPORT_DESCRIPTOR_TIMEOUT_MS),
            control.fetch_report_descriptor(report_descriptor_buf),
        )
        .await;
        match result {
            Ok(Ok(bytes)) => return Ok(bytes.len()),
            Ok(Err(error)) if attempt < ATTEMPTS => {
                log::debug!(
                    "firmware: usb report descriptor read retry interface={} attempt={} err={:?}",
                    interface_id.0,
                    attempt,
                    error
                );
                Timer::after_millis(100).await;
            }
            Ok(Err(error)) => return Err(error),
            Err(_) if attempt < ATTEMPTS => {
                log::debug!(
                    "firmware: usb report descriptor read retry interface={} attempt={} err=Timeout",
                    interface_id.0,
                    attempt
                );
                Timer::after_millis(100).await;
            }
            Err(_) => return Err(HidError::Transfer(PipeError::Timeout)),
        }
    }
}

async fn remove_device_and_notify<'d>(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    bus_handle: &FirmwareBusHandle<'d>,
    topology: &mut DefaultUsbTopologyManager,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    device_id: DeviceId,
) -> Result<(), ()> {
    let mut disconnected = heapless::Vec::<InterfaceId, MAX_ACTIVE_USB_INTERFACES>::new();
    let mut result = Ok(());

    #[cfg(feature = "dual-s3-wired")]
    sender
        .send(RuntimeInputMessage::MirrorSourceDisconnected { device_id })
        .await;

    match topology.remove_device(device_id) {
        Ok(removal) => {
            for interface in removal.interfaces() {
                detach_active_slot_by_interface(
                    active_slots,
                    interface.interface_id,
                    &mut disconnected,
                );
            }
            for device in removal.devices() {
                bus_handle.free_address(device.usb_address);
            }
        }
        Err(error) => {
            log::warn!(
                "firmware: usb topology remove failed device={} err={:?}",
                device_id.0,
                error
            );
            result = Err(());
        }
    }

    detach_active_slots_for_device(active_slots, device_id, &mut disconnected);
    for interface_id in disconnected {
        sender
            .send(RuntimeInputMessage::UsbHidInterfaceDisconnected { interface_id })
            .await;
    }

    result
}

async fn remove_interface_and_notify<'d>(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    topology: &mut DefaultUsbTopologyManager,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    interface_id: InterfaceId,
) {
    if let Err(error) = topology.remove_interface(interface_id) {
        log::warn!(
            "firmware: usb topology interface remove failed interface={} err={:?}",
            interface_id.0,
            error
        );
    }

    let mut disconnected = heapless::Vec::<InterfaceId, MAX_ACTIVE_USB_INTERFACES>::new();
    detach_active_slot_by_interface(active_slots, interface_id, &mut disconnected);
    for interface_id in disconnected {
        sender
            .send(RuntimeInputMessage::UsbHidInterfaceDisconnected { interface_id })
            .await;
    }
}

fn detach_active_slot_by_interface<'d>(
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    interface_id: InterfaceId,
    disconnected: &mut heapless::Vec<InterfaceId, MAX_ACTIVE_USB_INTERFACES>,
) {
    for slot in active_slots.iter_mut() {
        let should_detach = matches!(slot, Some(slot) if slot.interface_id == interface_id);
        if should_detach {
            if let Some(slot) = slot.take() {
                push_unique_interface_id(disconnected, slot.interface_id);
            }
        }
    }
}

fn detach_active_slots_for_device<'d>(
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    device_id: DeviceId,
    disconnected: &mut heapless::Vec<InterfaceId, MAX_ACTIVE_USB_INTERFACES>,
) {
    for slot in active_slots.iter_mut() {
        let should_detach = matches!(slot, Some(slot) if slot.session.device_id() == device_id);
        if should_detach {
            if let Some(slot) = slot.take() {
                push_unique_interface_id(disconnected, slot.interface_id);
            }
        }
    }
}

fn push_unique_interface_id<const N: usize>(
    interfaces: &mut heapless::Vec<InterfaceId, N>,
    interface_id: InterfaceId,
) {
    if !interfaces.iter().any(|existing| *existing == interface_id) {
        let _ = interfaces.push(interface_id);
    }
}

fn new_otg_host(usb: Usb<'static>) -> OtgHost<'static> {
    <Usb<'static> as UsbPeripheral>::enable();

    let regs = unsafe {
        embassy_usb_synopsys_otg::otg_v1::Otg::from_ptr(
            <Usb<'static> as UsbPeripheral>::REGISTERS.cast_mut(),
        )
    };

    let instance = OtgHostInstance {
        regs,
        state: HOST_STATE.as_host_state(),
        fifo_depth_words: <Usb<'static> as UsbPeripheral>::FIFO_DEPTH_WORDS as u16,
        phy_type: PhyType::InternalFullSpeed,
    };

    core::mem::forget(usb);
    OtgHost::new(instance)
}

pub fn bind_interrupt_handler() {
    esp_hal::interrupt::bind_handler(Interrupt::USB, usb_interrupt_handler);
}

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::max())]
fn usb_interrupt_handler() {
    let regs = unsafe {
        embassy_usb_synopsys_otg::otg_v1::Otg::from_ptr(
            <Usb<'static> as UsbPeripheral>::REGISTERS.cast_mut(),
        )
    };
    unsafe { on_host_interrupt(regs, &HOST_STATE.as_host_state()) };
}
