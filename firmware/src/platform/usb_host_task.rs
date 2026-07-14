use core::future::pending;

use embassy_futures::select::{Either, Either3, Either4, select, select3, select4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Timer, with_timeout};
use embassy_usb_driver::host::{PipeError, UsbHostAllocator, UsbPipe, pipe};
use embassy_usb_driver::{Direction, EndpointAddress, EndpointInfo, EndpointType};
use embassy_usb_host::class::hid::{HidError, PROTOCOL_REPORT, ReportDescriptor};
use embassy_usb_host::class::hub::{HubEvent, HubHandler};
use embassy_usb_host::control::{ControlType, Recipient, RequestType, SetupPacket};
use embassy_usb_host::handler::HandlerEvent;
use embassy_usb_host::{BusRoute, BusState};
use embassy_usb_synopsys_otg::PhyType;
use embassy_usb_synopsys_otg::host::{
    HostStateStorage, OtgHost, OtgHostAllocator, OtgHostInstance, on_host_interrupt,
};
use esp_hal::otg_fs::Usb;
use esp_hal::peripherals::Interrupt;
use esp_synopsys_usb_otg::UsbPeripheral;
use hidshift::ids::{DeviceId, InterfaceId};
use hidshift::input::KeyboardLedState;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    RUNTIME_INPUT_QUEUE_CAPACITY, RUNTIME_USB_COMMAND_QUEUE_CAPACITY, RuntimeDiagnosticsEvent,
    UsbTaskCommand,
};
use hidshift::storage::FixedName;
use hidshift::usb_hid::host_interface::{
    HidInterfaceInfo, config_descriptor_has_interface_class, find_hid_interfaces,
};
use hidshift::usb_hid::host_runtime::UsbHidInterfaceRuntimeSession;
use hidshift::usb_hid::topology::{DefaultUsbTopologyManager, UsbDeviceRoute};
use static_cell::ConstStaticCell;

use super::espnow_output::{HostOutputRequest, HostOutputResponse};
use super::usb_output_transport::{UsbKeyboardLedWriter, UsbRawReportWriter};
use super::usb_transport::{UsbHidControl, UsbHidReader};

const HOST_CHANNELS: usize = 8;
const CONFIG_DESCRIPTOR_BUF_LEN: usize = 512;
const REPORT_DESCRIPTOR_BUF_LEN: usize = hidshift::link::MAX_HID_REPORT_DESCRIPTOR_SIZE;
const REPORT_BUF_LEN: usize = hidshift::link::MAX_HID_REPORT_SIZE;
const MAX_REPORT_FIELDS: usize = 48;
const MAX_REPORT_EVENTS: usize = 32;
const MAX_ACTIVE_USB_INTERFACES: usize = 8;
const MAX_HUB_PORTS: usize = 4;
const HUB_CHILD_ENUMERATION_TIMEOUT_MS: u64 = 5_000;
const HUB_ENUMERATION_TOTAL_TIMEOUT_MS: u64 = 8_000;
const HUB_QUIESCED_EVENT_DRAIN_MS: u64 = 750;
const HID_REPORT_DESCRIPTOR_TIMEOUT_MS: u64 = 2_000;
const USB_LED_WRITE_TIMEOUT_MS: u64 = 20;

static HOST_STATE: HostStateStorage<HOST_CHANNELS> = HostStateStorage::new();
static BUS_STATE: BusState = BusState::new();

type FirmwareBusHandle<'d> = embassy_usb_host::BusHandle<'d, OtgHostAllocator<'d>>;

struct ActiveUsbInterfaceSlot<'d> {
    interface_id: InterfaceId,
    reader: UsbHidReader<'d, FirmwareBusHandle<'d>>,
    led_output: bool,
    hid_info: HidInterfaceInfo,
    enum_info: embassy_usb_host::handler::EnumerationInfo,
    session: UsbHidInterfaceRuntimeSession<MAX_REPORT_FIELDS, MAX_REPORT_EVENTS>,
    report_buf: [u8; REPORT_BUF_LEN],
    last_mouse_buttons: hidshift::input::MouseButtons,
    last_led_bytes: Option<hidshift::usb_hid::output::KeyboardLedOutputBytes>,
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

enum UsbSlotReadResult {
    Input {
        message: RuntimeInputMessage,
        movement_only: bool,
    },
    Fatal {
        device_id: DeviceId,
        interface_id: InterfaceId,
        error: HidError,
    },
}

impl<'d> ActiveUsbInterfaceSlot<'d> {
    async fn next_result(&mut self) -> UsbSlotReadResult {
        loop {
            match self.reader.read(&mut self.report_buf).await {
                Ok(n) => match self.session.input_message(&self.report_buf[..n]) {
                    Ok(message) => {
                        let movement_only =
                            movement_only_message(&message, &mut self.last_mouse_buttons);
                        let motion = if movement_only {
                            match &message {
                                RuntimeInputMessage::BridgeEvent(
                                    hidshift::BridgeEvent::InputFrame(
                                        hidshift::input::InputFrame::Standard(frame),
                                    ),
                                ) => frame.mouse.map(|mouse| mouse.movement),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        #[cfg(feature = "espnow")]
                        if super::transport_route::routes_to(hidshift::InputTransport::EspNow) {
                            super::espnow_link_task::forward_input_report(
                                self.session.device_id(),
                                self.interface_id,
                                embassy_time::Instant::now().as_micros(),
                                if movement_only {
                                    hidshift::link::InputDeliveryClass::Motion
                                } else {
                                    hidshift::link::InputDeliveryClass::Critical
                                },
                                motion,
                                &self.report_buf[..n],
                            )
                            .await;
                        }
                        return UsbSlotReadResult::Input {
                            message,
                            movement_only,
                        };
                    }
                    Err(error) => {
                        log::debug!(
                            "firmware: usb input frame decode failed interface={} err={:?}",
                            self.interface_id.0,
                            error
                        );
                    }
                },
                Err(error) => {
                    return UsbSlotReadResult::Fatal {
                        device_id: self.session.device_id(),
                        interface_id: self.interface_id,
                        error,
                    };
                }
            }
        }
    }
}

fn movement_only_message(
    message: &RuntimeInputMessage,
    previous_buttons: &mut hidshift::input::MouseButtons,
) -> bool {
    let RuntimeInputMessage::BridgeEvent(hidshift::BridgeEvent::InputFrame(
        hidshift::input::InputFrame::Standard(frame),
    )) = message
    else {
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
    message: RuntimeInputMessage,
    movement_only: bool,
) {
    if !super::transport_route::routes_to(hidshift::InputTransport::Ble) {
        while movement_queue.take_next().is_some() {}
        return;
    }
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
                    hidshift::BridgeEvent::InputFrame(hidshift::input::InputFrame::Standard(frame)),
                ))
                .await;
        }
        sender.send(message).await;
    }
}

async fn handle_hub_device_detected<'d>(
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
    hub: &mut HubHandler<'d, OtgHostAllocator<'d>, MAX_HUB_PORTS>,
    config_buf: &mut [u8],
    hub_device_id: DeviceId,
    port: u8,
    speed: embassy_usb_driver::Speed,
) {
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
    const ATTACH_ATTEMPTS: usize = 2;
    let mut attach_attempt = 0usize;
    loop {
        attach_attempt += 1;
        let enumerate_result = enumerate_hub_port_with_retries(hub, config_buf, port, speed).await;
        match enumerate_result {
            Ok((child_info, child_config_len)) => {
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
                let child_config_desc = &config_buf[..child_config_len];
                if attach_hid_interfaces_for_device(
                    sender,
                    bus_handle,
                    topology,
                    active_slots,
                    child_device_id,
                    &child_info,
                    child_config_desc,
                )
                .await
                .is_ok()
                {
                    if (port as usize) < MAX_HUB_PORTS {
                        hub_port_devices[port_index] = Some(child_device_id);
                    }
                    Timer::after_millis(500).await;
                    return;
                }

                let _ = remove_device_and_notify(
                    sender,
                    bus_handle,
                    topology,
                    active_slots,
                    child_device_id,
                )
                .await;
                if attach_attempt < ATTACH_ATTEMPTS {
                    log::debug!(
                        "firmware: hub child attach retry port={} attempt={}",
                        port,
                        attach_attempt
                    );
                    Timer::after_millis(250).await;
                    continue;
                }
                return;
            }
            Err(error) => {
                log::warn!(
                    "firmware: hub child enumerate failed port={} err={:?}",
                    port,
                    error
                );
                return;
            }
        }
    }
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

async fn poll_active_slots<'a, 'd>(
    active_slots: &'a mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
) -> UsbSlotReadResult {
    let (slot0_ref, rest) = active_slots.split_at_mut(1);
    let (slot1_ref, rest) = rest.split_at_mut(1);
    let (slot2_ref, rest) = rest.split_at_mut(1);
    let (slot3_ref, rest) = rest.split_at_mut(1);
    let (slot4_ref, rest) = rest.split_at_mut(1);
    let (slot5_ref, rest) = rest.split_at_mut(1);
    let (slot6_ref, slot7_ref) = rest.split_at_mut(1);

    let slot0 = async {
        match slot0_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };
    let slot1 = async {
        match slot1_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };
    let slot2 = async {
        match slot2_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };
    let slot3 = async {
        match slot3_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };
    let slot4 = async {
        match slot4_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };
    let slot5 = async {
        match slot5_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };
    let slot6 = async {
        match slot6_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };
    let slot7 = async {
        match slot7_ref[0].as_mut() {
            Some(slot) => slot.next_result().await,
            None => pending().await,
        }
    };

    let slot_group0 = async {
        match select4(slot0, slot1, slot2, slot3).await {
            Either4::First(result)
            | Either4::Second(result)
            | Either4::Third(result)
            | Either4::Fourth(result) => result,
        }
    };
    let slot_group1 = async {
        match select4(slot4, slot5, slot6, slot7).await {
            Either4::First(result)
            | Either4::Second(result)
            | Either4::Third(result)
            | Either4::Fourth(result) => result,
        }
    };

    match select(slot_group0, slot_group1).await {
        Either::First(result) | Either::Second(result) => result,
    }
}

#[embassy_executor::task]
pub async fn usb_input_task(
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
    log::info!("firmware: usb input task boot");
    log::info!("firmware: waiting for USB HID device on OTG");

    let usb = Usb::new(usb0, usb_dp, usb_dm);
    let host = new_otg_host(usb);
    esp_hal::interrupt::bind_handler(Interrupt::USB, usb_interrupt_handler);

    let (mut bus_controller, bus_handle) = embassy_usb_host::bus(host, &BUS_STATE);
    let config_buf = CONFIG_DESCRIPTOR_STORAGE.take();
    let mut topology = TOPOLOGY_STORAGE.take();
    let mut movement_queue = MOVEMENT_QUEUE_STORAGE.take();
    let mut active_slots = ACTIVE_SLOTS_STORAGE.take();

    loop {
        let speed = bus_controller.wait_for_connection().await;
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
            let Ok(mut hub) =
                HubHandler::<_, MAX_HUB_PORTS>::try_register(&bus_handle, &enum_info).await
            else {
                log::warn!("firmware: root hub registration failed");
                let _ = topology.remove_device(device_id);
                bus_handle.free_address(enum_info.device_address);
                continue;
            };
            log::info!("firmware: root hub registered ports_max={}", MAX_HUB_PORTS);
            let mut hub_port_devices = [None; MAX_HUB_PORTS];
            loop {
                match select4(
                    hub.wait_for_event(),
                    poll_active_slots(&mut active_slots),
                    receiver.receive(),
                    super::espnow_output::output_request_receiver().receive(),
                )
                .await
                {
                    Either4::First(Ok(HandlerEvent::HandlerEvent(HubEvent::DeviceDetected {
                        port,
                        speed,
                    }))) => {
                        log::info!(
                            "firmware: hub device detected port={} speed={:?}",
                            port,
                            speed
                        );
                        quiesce_ble_for_usb_enumeration(ble_quiesce_request, ble_quiesce_ready)
                            .await;
                        if with_timeout(
                            Duration::from_millis(HUB_ENUMERATION_TOTAL_TIMEOUT_MS),
                            handle_hub_device_detected(
                                &sender,
                                &bus_handle,
                                &mut topology,
                                &mut active_slots,
                                &mut hub_port_devices,
                                &mut hub,
                                &mut config_buf[..],
                                device_id,
                                port,
                                speed,
                            ),
                        )
                        .await
                        .is_err()
                        {
                            log::warn!("firmware: hub child attach total timeout port={}", port);
                        }
                        let mut hub_failed = false;
                        loop {
                            match with_timeout(
                                Duration::from_millis(HUB_QUIESCED_EVENT_DRAIN_MS),
                                hub.wait_for_event(),
                            )
                            .await
                            {
                                Ok(Ok(HandlerEvent::HandlerEvent(HubEvent::DeviceDetected {
                                    port,
                                    speed,
                                }))) => {
                                    log::info!(
                                        "firmware: hub device detected port={} speed={:?}",
                                        port,
                                        speed
                                    );
                                    if with_timeout(
                                        Duration::from_millis(HUB_ENUMERATION_TOTAL_TIMEOUT_MS),
                                        handle_hub_device_detected(
                                            &sender,
                                            &bus_handle,
                                            &mut topology,
                                            &mut active_slots,
                                            &mut hub_port_devices,
                                            &mut hub,
                                            &mut config_buf[..],
                                            device_id,
                                            port,
                                            speed,
                                        ),
                                    )
                                    .await
                                    .is_err()
                                    {
                                        log::warn!(
                                            "firmware: hub child attach total timeout port={}",
                                            port
                                        );
                                    }
                                }
                                Ok(Ok(HandlerEvent::HandlerEvent(HubEvent::DeviceRemoved {
                                    port,
                                    ..
                                }))) => {
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
                                Ok(Ok(_)) => {}
                                Ok(Err(error)) => {
                                    log::warn!("firmware: hub event loop failed: {:?}", error);
                                    let _ = remove_device_and_notify(
                                        &sender,
                                        &bus_handle,
                                        &mut topology,
                                        &mut active_slots,
                                        device_id,
                                    )
                                    .await;
                                    hub_failed = true;
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        resume_ble_after_usb_enumeration(ble_quiesce_done).await;
                        if hub_failed {
                            break;
                        }
                    }
                    Either4::First(Ok(HandlerEvent::HandlerEvent(HubEvent::DeviceRemoved {
                        port,
                        ..
                    }))) => {
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
                    Either4::First(Ok(_)) => {}
                    Either4::First(Err(error)) => {
                        log::warn!("firmware: hub event loop failed: {:?}", error);
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
                    Either4::Second(UsbSlotReadResult::Input {
                        message,
                        movement_only,
                    }) => {
                        forward_usb_input(&sender, &mut movement_queue, message, movement_only)
                            .await;
                    }
                    Either4::Second(UsbSlotReadResult::Fatal {
                        device_id: failed_device_id,
                        interface_id,
                        error,
                    }) => {
                        log::warn!(
                            "firmware: usb read failed interface={} err={:?}",
                            interface_id.0,
                            error
                        );
                        sender
                            .send(RuntimeInputMessage::DiagnosticsEvent(
                                RuntimeDiagnosticsEvent::UsbError,
                            ))
                            .await;
                        let _ = remove_device_and_notify(
                            &sender,
                            &bus_handle,
                            &mut topology,
                            &mut active_slots,
                            failed_device_id,
                        )
                        .await;
                    }
                    Either4::Third(command) => {
                        let Some(slot) = active_slots.iter_mut().find_map(|slot| {
                            slot.as_mut().filter(|slot| {
                                command.matches_target(slot.interface_id, slot.session.device_id())
                            })
                        }) else {
                            log::warn!(
                                "firmware: usb command interface missing got={}",
                                command.interface_id.0
                            );
                            continue;
                        };
                        if slot.last_led_bytes == Some(command.bytes) {
                            continue;
                        }
                        if slot.led_output {
                            match UsbKeyboardLedWriter::new_for_interface(
                                &bus_handle,
                                slot.hid_info,
                                &slot.enum_info,
                            ) {
                                Ok(mut led_writer) => {
                                    match with_timeout(
                                        Duration::from_millis(USB_LED_WRITE_TIMEOUT_MS),
                                        led_writer.write_leds(command.bytes),
                                    )
                                    .await
                                    {
                                        Ok(Ok(())) => slot.last_led_bytes = Some(command.bytes),
                                        Ok(Err(error)) => log::warn!(
                                            "firmware: usb led write failed interface={} err={:?}",
                                            slot.interface_id.0,
                                            error
                                        ),
                                        Err(_) => {
                                            log::warn!(
                                                "firmware: usb led write timeout interface={}",
                                                slot.interface_id.0
                                            );
                                            let _ =
                                                sender
                                                    .try_send(RuntimeInputMessage::DiagnosticsEvent(
                                                    RuntimeDiagnosticsEvent::UsbLedWriteTimedOut,
                                                ));
                                        }
                                    }
                                }
                                Err(error) => {
                                    log::warn!(
                                        "firmware: usb led writer unsupported interface={} err={:?}",
                                        slot.interface_id.0,
                                        error
                                    );
                                }
                            }
                        } else {
                            log::debug!(
                                "firmware: usb led write ignored interface={} no_led_output",
                                slot.interface_id.0
                            );
                        }
                    }
                    Either4::Fourth(request) => {
                        handle_raw_output_request(&bus_handle, &mut active_slots, request).await;
                    }
                }
            }
            continue;
        }

        if attach_hid_interfaces_for_device(
            &sender,
            &bus_handle,
            &mut topology,
            &mut active_slots,
            device_id,
            &enum_info,
            config_desc,
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
                poll_active_slots(&mut active_slots),
                receiver.receive(),
                super::espnow_output::output_request_receiver().receive(),
            )
            .await
            {
                Either3::First(UsbSlotReadResult::Input {
                    message,
                    movement_only,
                }) => {
                    forward_usb_input(&sender, &mut movement_queue, message, movement_only).await;
                }
                Either3::First(UsbSlotReadResult::Fatal {
                    device_id: failed_device_id,
                    interface_id,
                    error,
                }) => {
                    log::warn!(
                        "firmware: usb read failed interface={} err={:?}",
                        interface_id.0,
                        error
                    );
                    sender
                        .send(RuntimeInputMessage::DiagnosticsEvent(
                            RuntimeDiagnosticsEvent::UsbError,
                        ))
                        .await;
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
                Either3::Second(command) => {
                    let Some(slot) = active_slots.iter_mut().find_map(|slot| {
                        slot.as_mut().filter(|slot| {
                            command.matches_target(slot.interface_id, slot.session.device_id())
                        })
                    }) else {
                        log::warn!(
                            "firmware: usb command interface missing got={}",
                            command.interface_id.0
                        );
                        continue;
                    };
                    if slot.last_led_bytes == Some(command.bytes) {
                        continue;
                    }
                    if slot.led_output {
                        match UsbKeyboardLedWriter::new_for_interface(
                            &bus_handle,
                            slot.hid_info,
                            &slot.enum_info,
                        ) {
                            Ok(mut led_writer) => {
                                match with_timeout(
                                    Duration::from_millis(USB_LED_WRITE_TIMEOUT_MS),
                                    led_writer.write_leds(command.bytes),
                                )
                                .await
                                {
                                    Ok(Ok(())) => slot.last_led_bytes = Some(command.bytes),
                                    Ok(Err(error)) => log::warn!(
                                        "firmware: usb led write failed interface={} err={:?}",
                                        slot.interface_id.0,
                                        error
                                    ),
                                    Err(_) => {
                                        log::warn!(
                                            "firmware: usb led write timeout interface={}",
                                            slot.interface_id.0
                                        );
                                        let _ =
                                            sender.try_send(RuntimeInputMessage::DiagnosticsEvent(
                                                RuntimeDiagnosticsEvent::UsbLedWriteTimedOut,
                                            ));
                                    }
                                }
                            }
                            Err(error) => {
                                log::warn!(
                                    "firmware: usb led writer unsupported interface={} err={:?}",
                                    slot.interface_id.0,
                                    error
                                );
                            }
                        }
                    } else {
                        log::debug!(
                            "firmware: usb led write ignored interface={} no_led_output",
                            slot.interface_id.0
                        );
                    }
                }
                Either3::Third(request) => {
                    handle_raw_output_request(&bus_handle, &mut active_slots, request).await;
                }
            }
        }
    }
}

async fn quiesce_ble_for_usb_enumeration(
    ble_quiesce_request: Sender<'static, CriticalSectionRawMutex, (), 1>,
    ble_quiesce_ready: Receiver<'static, CriticalSectionRawMutex, (), 1>,
) {
    log::debug!("firmware: usb requesting ble quiesce for hub enumeration");
    ble_quiesce_request.send(()).await;
    ble_quiesce_ready.receive().await;
    log::debug!("firmware: usb ble quiesce ready for hub enumeration");
}

async fn resume_ble_after_usb_enumeration(
    ble_quiesce_done: Sender<'static, CriticalSectionRawMutex, (), 1>,
) {
    ble_quiesce_done.send(()).await;
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
) -> Result<(), ()> {
    let product_name = read_usb_product_name(bus_handle, enum_info)
        .await
        .unwrap_or_else(FixedName::empty);
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
        let Some(slot_index) = active_slots.iter().position(Option::is_none) else {
            log::warn!("firmware: usb active interface capacity exceeded");
            return Err(());
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
                #[cfg(feature = "espnow")]
                super::espnow_link_task::forward_interface_descriptor(
                    device_id,
                    interface_id,
                    hid_info.interface_number,
                    enum_info.device_desc.vendor_id,
                    enum_info.device_desc.product_id,
                    &report_descriptor_buf[..len],
                )
                .await;
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
                match UsbHidInterfaceRuntimeSession::<MAX_REPORT_FIELDS, MAX_REPORT_EVENTS>::from_core_descriptor(
                    interface_id,
                    device_id,
                    descriptor,
                    &report_descriptor_buf[..len],
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
                name: product_name,
                flags: session.device_kind_flags(),
            })
            .await;
        active_slots[slot_index] = Some(ActiveUsbInterfaceSlot {
            interface_id,
            reader,
            led_output,
            hid_info,
            enum_info: *enum_info,
            session,
            report_buf: [0u8; REPORT_BUF_LEN],
            last_mouse_buttons: hidshift::input::MouseButtons::empty(),
            last_led_bytes: None,
        });
    }

    Ok(())
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
    let string_index = enum_info.device_desc.product;
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
    let language_id = match control
        .control_in(&language_request.to_bytes(), &mut language)
        .await
    {
        Ok(length) if length >= 4 && language[1] == 3 => {
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
    let length = control
        .control_in(&request.to_bytes(), &mut descriptor)
        .await
        .ok()?;
    if length < 2 || descriptor[1] != 3 {
        return None;
    }
    let descriptor_len = usize::from(descriptor[0]).min(length).min(descriptor.len());
    let mut ascii = [0u8; hidshift::storage::MAX_HOST_NAME_LEN];
    let mut ascii_len = 0usize;
    for unit in descriptor[2..descriptor_len].chunks_exact(2) {
        if ascii_len == ascii.len() {
            break;
        }
        let code = u16::from_le_bytes([unit[0], unit[1]]);
        if code == 0 {
            break;
        }
        ascii[ascii_len] = if (0x20..=0x7e).contains(&code) {
            code as u8
        } else {
            b'?'
        };
        ascii_len += 1;
    }
    core::str::from_utf8(&ascii[..ascii_len])
        .ok()
        .and_then(FixedName::from_ascii)
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

async fn handle_raw_output_request<'d>(
    bus_handle: &FirmwareBusHandle<'d>,
    active_slots: &mut [Option<ActiveUsbInterfaceSlot<'d>>; MAX_ACTIVE_USB_INTERFACES],
    request: HostOutputRequest,
) {
    let interface_id = match &request {
        HostOutputRequest::SetReport { interface_id, .. }
        | HostOutputRequest::GetReport { interface_id, .. } => *interface_id,
    };
    let Some(slot) = active_slots.iter_mut().find_map(|slot| {
        slot.as_mut()
            .filter(|slot| slot.interface_id == interface_id)
    }) else {
        log::warn!(
            "firmware: raw USB output interface missing interface={}",
            interface_id.0
        );
        return;
    };

    match request {
        HostOutputRequest::SetReport {
            report_type,
            report_id,
            report,
            ..
        } => {
            let prefer_interrupt = report_type == hidshift::link::HidReportType::Output;
            match UsbRawReportWriter::new_for_interface(
                bus_handle,
                slot.hid_info,
                &slot.enum_info,
                prefer_interrupt,
            ) {
                Ok(mut writer) => {
                    match with_timeout(
                        Duration::from_millis(50),
                        writer.write_report(report_type as u8, report_id, report.as_slice()),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => log::warn!(
                            "firmware: raw SET_REPORT failed interface={} err={:?}",
                            interface_id.0,
                            error
                        ),
                        Err(_) => log::warn!(
                            "firmware: raw SET_REPORT timeout interface={}",
                            interface_id.0
                        ),
                    }
                }
                Err(error) => log::warn!(
                    "firmware: raw SET_REPORT pipe failed interface={} err={:?}",
                    interface_id.0,
                    error
                ),
            }
        }
        HostOutputRequest::GetReport {
            report_type,
            report_id,
            requested_len,
            request_id,
            ..
        } => {
            let mut report = [0u8; hidshift::link::MAX_HID_REPORT_SIZE];
            let requested_len = usize::from(requested_len).min(report.len());
            let len = match UsbHidControl::new(bus_handle, slot.hid_info, &slot.enum_info) {
                Ok(mut control) => match with_timeout(
                    Duration::from_millis(50),
                    control.get_report(report_type as u8, report_id, &mut report[..requested_len]),
                )
                .await
                {
                    Ok(Ok(len)) => len,
                    Ok(Err(error)) => {
                        log::warn!(
                            "firmware: raw GET_REPORT failed interface={} err={:?}",
                            interface_id.0,
                            error
                        );
                        0
                    }
                    Err(_) => {
                        log::warn!(
                            "firmware: raw GET_REPORT timeout interface={}",
                            interface_id.0
                        );
                        0
                    }
                },
                Err(error) => {
                    log::warn!(
                        "firmware: raw GET_REPORT pipe failed interface={} err={:?}",
                        interface_id.0,
                        error
                    );
                    0
                }
            };
            if let Ok(report) = heapless::Vec::from_slice(&report[..len]) {
                super::espnow_output::send_output_response(HostOutputResponse {
                    interface_id,
                    report_type,
                    report_id,
                    request_id,
                    report,
                })
                .await;
            }
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
        #[cfg(feature = "espnow")]
        super::espnow_link_task::forward_interface_removed(device_id, interface_id).await;
        sender
            .send(RuntimeInputMessage::UsbHidInterfaceDisconnected { interface_id })
            .await;
    }

    result
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

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::max())]
fn usb_interrupt_handler() {
    let regs = unsafe {
        embassy_usb_synopsys_otg::otg_v1::Otg::from_ptr(
            <Usb<'static> as UsbPeripheral>::REGISTERS.cast_mut(),
        )
    };
    unsafe { on_host_interrupt(regs, &HOST_STATE.as_host_state()) };
}
