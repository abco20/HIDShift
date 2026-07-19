use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Instant, Ticker};
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use hidshift::bridge::{BridgeEvent, NotifyReason};
use hidshift::interchip::message::{
    CAPABILITY_FALLBACK_PROFILE, CAPABILITY_STANDARD_WIRED_HID, CAPABILITY_USB_STATE_REPORTING,
    RECORD_FORCE_FALLBACK, RECORD_HEARTBEAT, RECORD_HELLO, RECORD_HELLO_ACK, RECORD_LINK_RESET,
    RECORD_STANDARD_INPUT_REPORT, RECORD_STANDARD_RELEASE_ALL, RECORD_USB_STATE,
};
use hidshift::interchip::{
    Hello, InterchipRole, ReceiveDisposition, Record, RecordIter, ReliableReceiver, ReliableSender,
    RetransmitAction, SPI_CELL_LEN, SPI_CELL_PAYLOAD_LEN, SPI_PROTOCOL_VERSION, SpiCell,
    StandardInputReport, UsbState, encode_records,
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
const HOST_CAPABILITIES: u32 =
    CAPABILITY_FALLBACK_PROFILE | CAPABILITY_STANDARD_WIRED_HID | CAPABILITY_USB_STATE_REPORTING;
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

        if last_valid_cell_ms
            .is_some_and(|last| now_ms.saturating_sub(last) >= LINK_LOSS_TIMEOUT_MS)
        {
            hello_confirmed = false;
            usb_state = None;
            last_valid_cell_ms = None;
            session_id = nonzero_session(session_id.wrapping_add(1));
            sender.reset_session(session_id);
            receiver = ReliableReceiver::new();
            diagnostics.resets = diagnostics.resets.saturating_add(1);
        }

        sender.set_cumulative_ack(receiver.cumulative_ack());
        let tx_cell = if sender.pending_len() == 0 && !hello_confirmed {
            queue_hello(&mut sender, now_ms)
        } else if sender.pending_len() < hidshift::interchip::SPI_TX_WINDOW && hello_confirmed {
            if let Ok(command) = command_receiver.try_receive() {
                queue_command(&mut sender, command, &mut report_sequence, now_ms)
                    .inspect_err(|_| {
                        diagnostics.command_encode_errors =
                            diagnostics.command_encode_errors.saturating_add(1);
                    })
                    .ok()
            } else if now_ms.saturating_sub(last_heartbeat_ms) >= HEARTBEAT_INTERVAL_MS {
                last_heartbeat_ms = now_ms;
                queue_record(&mut sender, RECORD_HEARTBEAT, &[], now_ms).ok()
            } else {
                retransmit_or_idle(&mut sender, now_ms, &mut diagnostics)
            }
        } else {
            retransmit_or_idle(&mut sender, now_ms, &mut diagnostics)
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
        sender.acknowledge(cell.header.cumulative_ack);
        match receiver.receive(&cell) {
            ReceiveDisposition::Accepted { .. } => {
                if process_records(&cell, &mut hello_confirmed, &mut usb_state).is_err() {
                    diagnostics.crc_or_codec_errors =
                        diagnostics.crc_or_codec_errors.saturating_add(1);
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
    }
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

fn retransmit_or_idle(
    sender: &mut ReliableSender,
    now_ms: u64,
    diagnostics: &mut LinkDiagnostics,
) -> Option<SpiCell> {
    match sender.poll_retransmit(now_ms, RETRANSMIT_TIMEOUT_MS, MAX_RETRANSMIT_ATTEMPTS) {
        RetransmitAction::Send(cell) => {
            diagnostics.retransmissions = diagnostics.retransmissions.saturating_add(1);
            Some(cell)
        }
        RetransmitAction::LinkResetRequired => {
            diagnostics.resets = diagnostics.resets.saturating_add(1);
            let next_session = nonzero_session(sender.session_id().wrapping_add(1));
            sender.reset_session(next_session);
            queue_record(sender, RECORD_LINK_RESET, &[], now_ms).ok()
        }
        RetransmitAction::Idle => None,
    }
}

fn process_records(
    cell: &SpiCell,
    hello_confirmed: &mut bool,
    usb_state: &mut Option<UsbState>,
) -> Result<(), ()> {
    let mut records = RecordIter::new(cell.payload(), cell.header.record_count);
    for record in records.by_ref() {
        let record = record.map_err(|_| ())?;
        match record.record_type {
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
                *usb_state = Some(UsbState::decode(record.data).map_err(|_| ())?);
            }
            RECORD_LINK_RESET => {
                *hello_confirmed = false;
                *usb_state = None;
            }
            _ => {}
        }
    }
    records.finish().map_err(|_| ())
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
