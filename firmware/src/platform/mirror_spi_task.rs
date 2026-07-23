use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Instant, Ticker};
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use hidshift::bridge::{BridgeEvent, NotifyReason};
use hidshift::input::KeyboardLedState;
use hidshift::interchip::message::{
    CAPABILITY_CONTROL_FORWARDING, CAPABILITY_DYNAMIC_PROFILE, CAPABILITY_FALLBACK_PROFILE,
    CAPABILITY_STANDARD_WIRED_HID, CAPABILITY_USB_STATE_REPORTING, RECORD_ACTIVATE_PROFILE,
    RECORD_CONTROL_REQUEST, RECORD_CONTROL_RESPONSE, RECORD_FORCE_FALLBACK, RECORD_HEARTBEAT,
    RECORD_HELLO, RECORD_HELLO_ACK, RECORD_LINK_RESET, RECORD_PROFILE_BEGIN, RECORD_PROFILE_CHUNK,
    RECORD_PROFILE_COMMIT, RECORD_PROFILE_RESULT, RECORD_RAW_ENDPOINT_IN, RECORD_RAW_ENDPOINT_OUT,
    RECORD_STANDARD_INPUT_REPORT, RECORD_STANDARD_OUTPUT_REPORT, RECORD_STANDARD_RELEASE_ALL,
    RECORD_USB_STATE,
};
use hidshift::interchip::{
    CONTROL_FRAGMENT_LAST, ControlRequestAssembler, ControlRequestFragment,
    ControlResponseFragment, Hello, InterchipRole, MirrorControlRequest, MirrorControlResponse,
    ProfileResult, RawEndpointReport, ReceiveDisposition, Record, RecordIter,
    ReliableDeliveryQueue, ReliableReceiver, ReliableSender, RetransmitAction, SPI_CELL_LEN,
    SPI_CELL_PAYLOAD_LEN, SPI_PROTOCOL_VERSION, SPI_TX_WINDOW, SpiCell, StandardInputReport,
    StandardOutputReport, UsbState, encode_records,
};
use hidshift::output_target::OutputTargetAvailability;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::runtime::{
    DeviceTaskCommand, RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY, RUNTIME_INPUT_QUEUE_CAPACITY,
};

const SPI_POLL_INTERVAL: Duration = Duration::from_micros(500);
const RETRANSMIT_TIMEOUT_MS: u64 = 5;
const MAX_RETRANSMIT_ATTEMPTS: u8 = 8;
const HEARTBEAT_INTERVAL_MS: u64 = 500;
const LINK_LOSS_TIMEOUT_MS: u64 = 1_500;
const HOST_CAPABILITIES: u32 = CAPABILITY_DYNAMIC_PROFILE
    | CAPABILITY_FALLBACK_PROFILE
    | CAPABILITY_STANDARD_WIRED_HID
    | CAPABILITY_USB_STATE_REPORTING
    | CAPABILITY_CONTROL_FORWARDING;
const REQUIRED_DEVICE_CAPABILITIES: u32 =
    CAPABILITY_FALLBACK_PROFILE | CAPABILITY_STANDARD_WIRED_HID | CAPABILITY_USB_STATE_REPORTING;

#[derive(Debug, Default)]
struct LinkDiagnostics {
    transactions: u32,
    valid_cells: u32,
    crc_or_codec_errors: u32,
    duplicates: u32,
    sequence_gaps: u32,
    retransmissions: u32,
    resets: u32,
    command_encode_errors: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WireCommand {
    Command(DeviceTaskCommand),
    ControlResponseFragment {
        response: MirrorControlResponse,
        offset: usize,
    },
    Heartbeat,
}

#[embassy_executor::task]
pub async fn mirror_spi_master_task(
    command_receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        DeviceTaskCommand,
        RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY,
    >,
    runtime_sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    session_id: u32,
    spi2: esp_hal::peripherals::SPI2<'static>,
    dma_channel: esp_hal::peripherals::DMA_CH0<'static>,
    cs: esp_hal::peripherals::GPIO10<'static>,
    mosi: esp_hal::peripherals::GPIO11<'static>,
    sclk: esp_hal::peripherals::GPIO12<'static>,
    miso: esp_hal::peripherals::GPIO13<'static>,
) {
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) =
        esp_hal::dma_buffers!(SPI_CELL_LEN);
    let dma_rx = match DmaRxBuf::new(rx_descriptors, rx_buffer) {
        Ok(buffer) => buffer,
        Err(error) => {
            log::error!("mirror-spi: RX DMA setup failed {:?}", error);
            esp_hal::system::software_reset();
        }
    };
    let dma_tx = match DmaTxBuf::new(tx_descriptors, tx_buffer) {
        Ok(buffer) => buffer,
        Err(error) => {
            log::error!("mirror-spi: TX DMA setup failed {:?}", error);
            esp_hal::system::software_reset();
        }
    };
    let spi = match Spi::new(
        spi2,
        Config::default()
            .with_frequency(Rate::from_mhz(10))
            .with_mode(Mode::_0),
    ) {
        Ok(spi) => spi,
        Err(error) => {
            log::error!("mirror-spi: SPI2 setup failed {:?}", error);
            esp_hal::system::software_reset();
        }
    }
    .with_cs(cs)
    .with_mosi(mosi)
    .with_sck(sclk)
    .with_miso(miso)
    .with_dma(dma_channel)
    .with_buffers(dma_rx, dma_tx)
    .into_async();

    run_link(spi, command_receiver, runtime_sender, session_id).await;
}

async fn run_link(
    mut spi: esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
    command_receiver: Receiver<
        'static,
        CriticalSectionRawMutex,
        DeviceTaskCommand,
        RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY,
    >,
    runtime_sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    initial_session_id: u32,
) {
    let mut session_id = nonzero_session(initial_session_id);
    let mut sender = ReliableSender::new(session_id);
    let mut receiver = ReliableReceiver::new();
    let mut report_sequence = 1u16;
    let mut hello_confirmed = false;
    let mut usb_state = None;
    let mut last_valid_cell_ms = None;
    let mut last_heartbeat_ms = 0;
    let mut reported_availability = None;
    let mut pending_wired_leds = None;
    let mut pending_profile_result = None;
    let mut pending_raw_endpoint_out = None;
    let mut pending_control_request = None;
    let mut pending_device_usb_state = None;
    let mut control_request_assembler = ControlRequestAssembler::new();
    let mut delivery = ReliableDeliveryQueue::<WireCommand>::new();
    let mut expanding_control_response: Option<(MirrorControlResponse, usize)> = None;
    let mut diagnostics = LinkDiagnostics::default();
    let mut ticker = Ticker::every(SPI_POLL_INTERVAL);

    loop {
        ticker.next().await;
        let now_ms = Instant::now().as_millis();
        let desired_availability =
            availability(hello_confirmed, usb_state, last_valid_cell_ms, now_ms);
        report_availability(
            &runtime_sender,
            desired_availability,
            &mut reported_availability,
        );
        report_wired_leds(&runtime_sender, &mut pending_wired_leds);
        report_profile_result(&runtime_sender, &mut pending_profile_result);
        report_raw_endpoint_out(&runtime_sender, &mut pending_raw_endpoint_out);
        report_control_request(&runtime_sender, &mut pending_control_request);
        report_device_usb_state(&runtime_sender, &mut pending_device_usb_state);

        if last_valid_cell_ms
            .is_some_and(|last| now_ms.saturating_sub(last) >= LINK_LOSS_TIMEOUT_MS)
        {
            hello_confirmed = false;
            usb_state = None;
            last_valid_cell_ms = None;
            session_id = nonzero_session(session_id.wrapping_add(1));
            delivery.retry_after_session_reset();
            sender.reset_session(session_id);
            receiver = ReliableReceiver::new();
            control_request_assembler.reset();
            diagnostics.resets = diagnostics.resets.saturating_add(1);
        }

        sender.set_cumulative_ack(receiver.cumulative_ack());
        let retransmit =
            sender.poll_retransmit(now_ms, RETRANSMIT_TIMEOUT_MS, MAX_RETRANSMIT_ATTEMPTS);
        let tx_cell = match retransmit {
            RetransmitAction::Send(cell) => {
                diagnostics.retransmissions = diagnostics.retransmissions.saturating_add(1);
                Some(cell)
            }
            RetransmitAction::LinkResetRequired => {
                diagnostics.resets = diagnostics.resets.saturating_add(1);
                delivery.retry_after_session_reset();
                session_id = nonzero_session(session_id.wrapping_add(1));
                sender.reset_session(session_id);
                hello_confirmed = false;
                usb_state = None;
                control_request_assembler.reset();
                queue_record(&mut sender, RECORD_LINK_RESET, &[], now_ms).ok()
            }
            RetransmitAction::Idle if !hello_confirmed && sender.pending_len() == 0 => {
                queue_hello(&mut sender, now_ms)
            }
            RetransmitAction::Idle if hello_confirmed && sender.pending_len() < SPI_TX_WINDOW => {
                let mut from_retry = false;
                let wire_command = if let Some(command) = delivery.next_retry() {
                    from_retry = true;
                    Some(command)
                } else if let Some((response, offset)) = expanding_control_response {
                    Some(WireCommand::ControlResponseFragment { response, offset })
                } else {
                    match command_receiver.try_receive() {
                        Ok(DeviceTaskCommand::ControlResponse(response)) => {
                            expanding_control_response = Some((response, 0));
                            Some(WireCommand::ControlResponseFragment {
                                response,
                                offset: 0,
                            })
                        }
                        Ok(command) => Some(WireCommand::Command(command)),
                        Err(_) => None,
                    }
                };

                if let Some(command) = wire_command {
                    match queue_wire_command(&mut sender, command, &mut report_sequence, now_ms) {
                        Ok((cell, control_progress)) => {
                            if !from_retry
                                && let Some((response, next_offset, last)) = control_progress
                            {
                                expanding_control_response =
                                    (!last).then_some((response, next_offset));
                            }
                            if delivery.record_queued(command).is_err() {
                                diagnostics.command_encode_errors =
                                    diagnostics.command_encode_errors.saturating_add(1);
                            }
                            Some(cell)
                        }
                        Err(()) => {
                            diagnostics.command_encode_errors =
                                diagnostics.command_encode_errors.saturating_add(1);
                            None
                        }
                    }
                } else if now_ms.saturating_sub(last_heartbeat_ms) >= HEARTBEAT_INTERVAL_MS {
                    last_heartbeat_ms = now_ms;
                    let command = WireCommand::Heartbeat;
                    match queue_wire_command(&mut sender, command, &mut report_sequence, now_ms) {
                        Ok((cell, _)) => {
                            if delivery.record_queued(command).is_err() {
                                diagnostics.command_encode_errors =
                                    diagnostics.command_encode_errors.saturating_add(1);
                            }
                            Some(cell)
                        }
                        Err(()) => {
                            diagnostics.command_encode_errors =
                                diagnostics.command_encode_errors.saturating_add(1);
                            None
                        }
                    }
                } else {
                    None
                }
            }
            RetransmitAction::Idle => None,
        };

        let tx_cell = match tx_cell {
            Some(cell) => cell,
            None => {
                let mut cell = SpiCell::empty(session_id);
                cell.header.cumulative_ack = receiver.cumulative_ack();
                cell
            }
        };
        let tx = match tx_cell.encode() {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.command_encode_errors =
                    diagnostics.command_encode_errors.saturating_add(1);
                log::warn!("mirror-spi: failed to encode cell {:?}", error);
                continue;
            }
        };
        let mut rx = [0u8; SPI_CELL_LEN];
        if let Err(error) = spi.transfer_async(&mut rx, &tx).await {
            log::warn!("mirror-spi: DMA transaction failed {:?}", error);
            continue;
        }
        diagnostics.transactions = diagnostics.transactions.saturating_add(1);

        let cell = match SpiCell::decode(&rx) {
            Ok(cell) => cell,
            Err(_) => {
                diagnostics.crc_or_codec_errors = diagnostics.crc_or_codec_errors.saturating_add(1);
                continue;
            }
        };
        diagnostics.valid_cells = diagnostics.valid_cells.saturating_add(1);
        last_valid_cell_ms = Some(now_ms);
        let disposition = receiver.receive(&cell);
        let remote_session_changed = matches!(
            disposition,
            ReceiveDisposition::Accepted {
                session_changed: true,
                ..
            } | ReceiveDisposition::SessionChanged
        );
        if remote_session_changed {
            hello_confirmed = false;
            usb_state = None;
            session_id = nonzero_session(session_id.wrapping_add(1));
            delivery.retry_after_session_reset();
            sender.reset_session(session_id);
            control_request_assembler.reset();
            diagnostics.resets = diagnostics.resets.saturating_add(1);
        } else {
            let acknowledged = sender.acknowledge(cell.header.cumulative_ack);
            delivery.acknowledge(acknowledged);
        }
        match disposition {
            ReceiveDisposition::Accepted { .. } => {
                match process_records(
                    &cell,
                    &mut hello_confirmed,
                    &mut usb_state,
                    &mut control_request_assembler,
                ) {
                    Ok(processed) => {
                        if let Some(leds) = processed.wired_leds {
                            pending_wired_leds = Some(leds);
                        }
                        if let Some(result) = processed.profile_result {
                            pending_profile_result = Some(result);
                        }
                        if let Some(report) = processed.raw_endpoint_out {
                            pending_raw_endpoint_out = Some(report);
                        }
                        if let Some(request) = processed.control_request {
                            pending_control_request = Some(request);
                        }
                        if let Some(state) = processed.usb_state {
                            pending_device_usb_state = Some(state);
                        }
                    }
                    Err(()) => {
                        diagnostics.crc_or_codec_errors =
                            diagnostics.crc_or_codec_errors.saturating_add(1);
                    }
                }
            }
            ReceiveDisposition::Duplicate { .. } => {
                diagnostics.duplicates = diagnostics.duplicates.saturating_add(1);
            }
            ReceiveDisposition::Gap { .. } => {
                diagnostics.sequence_gaps = diagnostics.sequence_gaps.saturating_add(1);
            }
            ReceiveDisposition::SessionChanged | ReceiveDisposition::Empty => {}
        }
        if !remote_session_changed && !hello_confirmed && delivery.has_inflight() {
            session_id = nonzero_session(session_id.wrapping_add(1));
            delivery.retry_after_session_reset();
            sender.reset_session(session_id);
            control_request_assembler.reset();
            diagnostics.resets = diagnostics.resets.saturating_add(1);
        }
    }
}

fn queue_hello(sender: &mut ReliableSender, now_ms: u64) -> Option<SpiCell> {
    let hello = Hello {
        role: InterchipRole::Host,
        protocol_version: SPI_PROTOCOL_VERSION,
        firmware_major: 0,
        firmware_minor: 2,
        capabilities: HOST_CAPABILITIES,
        active_profile_hash: 0,
    }
    .encode();
    queue_record(sender, RECORD_HELLO, &hello, now_ms).ok()
}

fn queue_wire_command(
    sender: &mut ReliableSender,
    command: WireCommand,
    report_sequence: &mut u16,
    now_ms: u64,
) -> Result<(SpiCell, Option<(MirrorControlResponse, usize, bool)>), ()> {
    match command {
        WireCommand::Command(command) => {
            queue_command(sender, command, report_sequence, now_ms).map(|cell| (cell, None))
        }
        WireCommand::ControlResponseFragment { response, offset } => {
            let (cell, next_offset, last) =
                queue_control_response_fragment(sender, response, offset, now_ms)?;
            Ok((cell, Some((response, next_offset, last))))
        }
        WireCommand::Heartbeat => {
            queue_record(sender, RECORD_HEARTBEAT, &[], now_ms).map(|cell| (cell, None))
        }
    }
}

fn queue_command(
    sender: &mut ReliableSender,
    command: DeviceTaskCommand,
    report_sequence: &mut u16,
    now_ms: u64,
) -> Result<SpiCell, ()> {
    match command {
        DeviceTaskCommand::StandardReport { report, reason } => {
            let message = StandardInputReport {
                flags: notify_reason_flags(reason),
                sequence: *report_sequence,
                report,
            };
            *report_sequence = next_nonzero(*report_sequence);
            let (data, length) = message.encode();
            queue_record(
                sender,
                RECORD_STANDARD_INPUT_REPORT,
                &data[..length as usize],
                now_ms,
            )
            .map_err(|_| ())
        }
        DeviceTaskCommand::ReleaseAll => {
            queue_record(sender, RECORD_STANDARD_RELEASE_ALL, &[], now_ms).map_err(|_| ())
        }
        DeviceTaskCommand::ActivateFallback { operation_id } => queue_record(
            sender,
            RECORD_FORCE_FALLBACK,
            &operation_id.to_le_bytes(),
            now_ms,
        )
        .map_err(|_| ()),
        DeviceTaskCommand::ActivateMirror(activate) => {
            let cell = queue_record(sender, RECORD_ACTIVATE_PROFILE, &activate.encode(), now_ms)
                .map_err(|_| ())?;
            log::info!(
                "mirror-spi: activate op={} hash={:08x}",
                activate.operation_id,
                activate.profile_hash
            );
            Ok(cell)
        }
        DeviceTaskCommand::ProfileBegin(begin) => {
            queue_record(sender, RECORD_PROFILE_BEGIN, &begin.encode(), now_ms).map_err(|_| ())
        }
        DeviceTaskCommand::ProfileChunk(chunk) => {
            let mut data = [0; 104];
            let length = chunk.as_borrowed().encode(&mut data).map_err(|_| ())?;
            queue_record(sender, RECORD_PROFILE_CHUNK, &data[..length], now_ms).map_err(|_| ())
        }
        DeviceTaskCommand::ProfileCommit { transfer_id } => queue_record(
            sender,
            RECORD_PROFILE_COMMIT,
            &transfer_id.to_le_bytes(),
            now_ms,
        )
        .map_err(|_| ()),
        DeviceTaskCommand::RawEndpointIn(report) => {
            let mut data = [0; hidshift::interchip::message::RAW_ENDPOINT_MAX_WIRE_LEN];
            let length = report.encode(&mut data).map_err(|_| ())?;
            queue_record(sender, RECORD_RAW_ENDPOINT_IN, &data[..length], now_ms).map_err(|_| ())
        }
        DeviceTaskCommand::ControlResponse(_) => Err(()),
    }
}

fn queue_control_response_fragment(
    sender: &mut ReliableSender,
    response: hidshift::interchip::MirrorControlResponse,
    offset: usize,
    now_ms: u64,
) -> Result<(SpiCell, usize, bool), ()> {
    let fragment = ControlResponseFragment::from_response(response, offset).map_err(|_| ())?;
    let mut data = [0; hidshift::interchip::CONTROL_RESPONSE_FRAGMENT_MAX_WIRE_LEN];
    let length = fragment.encode(&mut data).map_err(|_| ())?;
    let cell =
        queue_record(sender, RECORD_CONTROL_RESPONSE, &data[..length], now_ms).map_err(|_| ())?;
    Ok((
        cell,
        offset + fragment.data().len(),
        fragment.flags & CONTROL_FRAGMENT_LAST != 0,
    ))
}

fn queue_record(
    sender: &mut ReliableSender,
    record_type: u8,
    data: &[u8],
    now_ms: u64,
) -> Result<SpiCell, ()> {
    let mut payload = [0u8; SPI_CELL_PAYLOAD_LEN];
    let (length, count) = encode_records(
        &[Record {
            record_type,
            flags: 0,
            data,
        }],
        &mut payload,
    )
    .map_err(|_| ())?;
    sender
        .queue(&payload[..length as usize], count, now_ms)
        .map_err(|_| ())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ProcessedRecords {
    wired_leds: Option<KeyboardLedState>,
    profile_result: Option<ProfileResult>,
    raw_endpoint_out: Option<RawEndpointReport>,
    control_request: Option<MirrorControlRequest>,
    usb_state: Option<UsbState>,
}

fn process_records(
    cell: &SpiCell,
    hello_confirmed: &mut bool,
    usb_state: &mut Option<UsbState>,
    control_request_assembler: &mut ControlRequestAssembler,
) -> Result<ProcessedRecords, ()> {
    let mut processed = ProcessedRecords::default();
    let mut records = RecordIter::new(cell.payload(), cell.header.record_count);
    for record in records.by_ref() {
        let record = record.map_err(|_| ())?;
        match record.record_type {
            RECORD_HELLO => {
                let hello = Hello::decode(record.data).map_err(|_| ())?;
                if hello.role != InterchipRole::Device
                    || hello.protocol_version != SPI_PROTOCOL_VERSION
                {
                    return Err(());
                }
                *hello_confirmed = false;
                *usb_state = None;
            }
            RECORD_HELLO_ACK => {
                let hello = Hello::decode(record.data).map_err(|_| ())?;
                *hello_confirmed = hello.role == InterchipRole::Device
                    && hello.protocol_version == SPI_PROTOCOL_VERSION
                    && hello.capabilities & REQUIRED_DEVICE_CAPABILITIES
                        == REQUIRED_DEVICE_CAPABILITIES;
                if !*hello_confirmed {
                    *usb_state = None;
                }
            }
            RECORD_USB_STATE => {
                let state = UsbState::decode(record.data).map_err(|_| ())?;
                *usb_state = Some(state);
                processed.usb_state = Some(state);
            }
            RECORD_LINK_RESET => {
                *hello_confirmed = false;
                *usb_state = None;
                control_request_assembler.reset();
            }
            RECORD_STANDARD_OUTPUT_REPORT => {
                let report = StandardOutputReport::decode(record.data).map_err(|_| ())?;
                if report.kind != 1 {
                    return Err(());
                }
                let [bits] = report.data() else {
                    return Err(());
                };
                processed.wired_leds = Some(KeyboardLedState::from_bits_truncate(*bits));
            }
            RECORD_PROFILE_RESULT => {
                let result = ProfileResult::decode(record.data).map_err(|_| ())?;
                log::info!(
                    "mirror-spi: profile result transfer={} hash={:08x} status={} reason={} detail={}",
                    result.transfer_id,
                    result.profile_hash,
                    result.status as u8,
                    result.reject_reason,
                    result.detail
                );
                processed.profile_result = Some(result);
            }
            RECORD_RAW_ENDPOINT_OUT => {
                processed.raw_endpoint_out =
                    Some(RawEndpointReport::decode(record.data).map_err(|_| ())?);
            }
            RECORD_CONTROL_REQUEST => {
                processed.control_request = control_request_assembler
                    .push(ControlRequestFragment::decode(record.data).map_err(|_| ())?)
                    .map_err(|_| ())?;
            }
            _ => {}
        }
    }
    records.finish().map_err(|_| ())?;
    Ok(processed)
}

fn availability(
    hello_confirmed: bool,
    usb_state: Option<UsbState>,
    last_valid_cell_ms: Option<u64>,
    now_ms: u64,
) -> OutputTargetAvailability {
    if last_valid_cell_ms.is_none_or(|last| now_ms.saturating_sub(last) >= LINK_LOSS_TIMEOUT_MS)
        || !hello_confirmed
    {
        return OutputTargetAvailability::Unavailable;
    }
    match usb_state {
        Some(state) if state.healthy && state.attached && state.configured => {
            OutputTargetAvailability::Ready
        }
        Some(state) if !state.healthy => OutputTargetAvailability::Unavailable,
        _ => OutputTargetAvailability::ConnectedNotReady,
    }
}

fn report_availability(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    desired: OutputTargetAvailability,
    reported: &mut Option<OutputTargetAvailability>,
) {
    if *reported == Some(desired) {
        return;
    }
    if sender
        .try_send(RuntimeInputMessage::BridgeEvent(
            BridgeEvent::WiredAvailabilityChanged {
                availability: desired,
            },
        ))
        .is_ok()
    {
        *reported = Some(desired);
    }
}

fn report_wired_leds(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    pending: &mut Option<KeyboardLedState>,
) {
    let Some(leds) = *pending else {
        return;
    };
    if sender
        .try_send(RuntimeInputMessage::BridgeEvent(
            BridgeEvent::WiredKeyboardLedChanged { leds },
        ))
        .is_ok()
    {
        #[cfg(feature = "hardware-e2e")]
        log::info!("@HIDSHIFT-MIRROR:WIRED_LEDS,{:02X}", leds.bits());
        *pending = None;
    }
}

fn report_profile_result(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    pending: &mut Option<ProfileResult>,
) {
    let Some(result) = *pending else {
        return;
    };
    if sender
        .try_send(RuntimeInputMessage::DeviceProfileResult(result))
        .is_ok()
    {
        *pending = None;
    }
}

fn report_raw_endpoint_out(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    pending: &mut Option<RawEndpointReport>,
) {
    let Some(report) = *pending else {
        return;
    };
    if sender
        .try_send(RuntimeInputMessage::MirrorEndpointOut(report))
        .is_ok()
    {
        #[cfg(feature = "hardware-e2e")]
        log::info!(
            "@HIDSHIFT-MIRROR:RAW_OUT,{:02X},{},{:02X?}",
            report.endpoint_address,
            report.data().len(),
            report.data()
        );
        #[cfg(feature = "hardware-e2e")]
        log::info!(
            "@HIDSHIFT-MIRROR:RAW_OUT_CRC,{:02X},{},{:04X}",
            report.endpoint_address,
            report.data().len(),
            hidshift::checksum::crc16_ccitt_false(report.data())
        );
        *pending = None;
    }
}

fn report_control_request(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    pending: &mut Option<MirrorControlRequest>,
) {
    let Some(request) = *pending else {
        return;
    };
    if sender
        .try_send(RuntimeInputMessage::MirrorControlRequest(request))
        .is_ok()
    {
        *pending = None;
    }
}

fn report_device_usb_state(
    sender: &Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    pending: &mut Option<UsbState>,
) {
    let Some(state) = *pending else {
        return;
    };
    if sender
        .try_send(RuntimeInputMessage::DeviceUsbState(state))
        .is_ok()
    {
        *pending = None;
    }
}

const fn notify_reason_flags(reason: NotifyReason) -> u8 {
    match reason {
        NotifyReason::Input => 0,
        NotifyReason::InputEdge => 1 << 0,
        NotifyReason::InputRelease => 1 << 1,
        NotifyReason::TargetSwitchRelease => 1 << 2,
        NotifyReason::UsbDeviceRemovedRelease => 1 << 3,
        NotifyReason::SafetyRelease => 1 << 4,
    }
}

const fn next_nonzero(value: u16) -> u16 {
    if value == u16::MAX || value == 0 {
        1
    } else {
        value + 1
    }
}

const fn nonzero_session(value: u32) -> u32 {
    if value == 0 { 1 } else { value }
}
