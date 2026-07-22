#![no_std]
#![no_main]

mod boot_presentation;
mod usb_dynamic;
mod usb_fallback;
mod profile_store;

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::spi::Mode;
use esp_hal::spi::slave::Spi;
use hidshift::fallback::{
    FALLBACK_USB_DEVICE_RELEASE, FALLBACK_USB_MANUFACTURER, FALLBACK_USB_PRODUCT,
    FALLBACK_USB_PRODUCT_ID, FALLBACK_USB_VENDOR_ID,
};
use hidshift::interchip::{
    DeviceLink, DeviceLinkEvent, ProfileResult, ProfileResultStatus, ProfileTransferError,
    ProfileTransferReceiver, SPI_CELL_LEN, StandardOutputReport, UsbState,
};
use hidshift::mirror::{
    HSMI_MAX_SIZE, MirrorRejectReason, ProfileCommitOutcome, UsbDevicePlan, validate_mirror_image,
};
use static_cell::StaticCell;
use usb_device::LangID;
use usb_device::bus::UsbBus;
use usb_device::device::{
    StringDescriptors, UsbDevice, UsbDeviceBuilder, UsbDeviceState, UsbRev, UsbVidPid,
};
use usb_dynamic::{DynamicUsb, RawPacket};
use usb_fallback::FallbackUsb;

esp_bootloader_esp_idf::esp_app_desc!();

static USB_ENDPOINT_MEMORY: StaticCell<[u32; 1024]> = StaticCell::new();
static PROFILE_STAGING: StaticCell<[u8; HSMI_MAX_SIZE]> = StaticCell::new();
static PROFILE_ACTIVE_IMAGE: StaticCell<[u8; HSMI_MAX_SIZE]> = StaticCell::new();
const SPI_LINK_LOSS_TIMEOUT_MS: u64 = 1_500;

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let session_id = nonzero_session(esp_hal::rng::Rng::new().random());
    let mut profile_store = match profile_store::open(peripherals.FLASH) {
        Ok(store) => Some(store),
        Err(error) => {
            log::error!("device-profile: storage unavailable {:?}", error);
            None
        }
    };
    let dynamic_plan = load_boot_profile(&mut profile_store);
    let profile_receiver =
        ProfileTransferReceiver::new(PROFILE_STAGING.init([0; HSMI_MAX_SIZE]));

    let usb = esp_hal::otg_fs::Usb::new(peripherals.USB0, peripherals.GPIO20, peripherals.GPIO19);
    let endpoint_memory = USB_ENDPOINT_MEMORY.init([0; 1024]);
    let usb_bus = esp_hal::otg_fs::UsbBus::new(usb, endpoint_memory);
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) =
        esp_hal::dma_buffers!(SPI_CELL_LEN);
    let dma_rx = match DmaRxBuf::new(rx_descriptors, rx_buffer) {
        Ok(buffer) => buffer,
        Err(error) => fatal("RX DMA", error),
    };
    let dma_tx = match DmaTxBuf::new(tx_descriptors, tx_buffer) {
        Ok(buffer) => buffer,
        Err(error) => fatal("TX DMA", error),
    };
    let spi = Spi::new(peripherals.SPI2, Mode::_0)
        .with_cs(peripherals.GPIO10)
        .with_mosi(peripherals.GPIO11)
        .with_sck(peripherals.GPIO12)
        .with_miso(peripherals.GPIO13)
        .with_dma(peripherals.DMA_CH0);

    if let Some((profile_hash, plan)) = dynamic_plan {
        let dynamic = match DynamicUsb::new(&usb_bus, plan) {
            Ok(dynamic) => dynamic,
            Err(error) => fatal("dynamic endpoints", error),
        };
        let usb_device = build_dynamic_device(&usb_bus, &dynamic);
        run(
            spi,
            dma_rx,
            dma_tx,
            session_id,
            usb_device,
            dynamic,
            profile_store,
            profile_receiver,
            profile_hash,
        )
    } else {
        let fallback = FallbackUsb::new(&usb_bus);
        let usb_builder = match UsbDeviceBuilder::new(
            &usb_bus,
            UsbVidPid(FALLBACK_USB_VENDOR_ID, FALLBACK_USB_PRODUCT_ID),
        )
        .strings(&[StringDescriptors::new(LangID::EN_US)
            .manufacturer(FALLBACK_USB_MANUFACTURER)
            .product(FALLBACK_USB_PRODUCT)])
        {
            Ok(builder) => builder,
            Err(error) => fatal("USB strings", error),
        };
        let usb_device = usb_builder
            .device_release(FALLBACK_USB_DEVICE_RELEASE)
            .build();
        run(
            spi,
            dma_rx,
            dma_tx,
            session_id,
            usb_device,
            fallback,
            profile_store,
            profile_receiver,
            0,
        )
    }
}

fn load_boot_profile(
    store: &mut Option<profile_store::DeviceProfileStore>,
) -> Option<(u32, UsbDevicePlan<'static>)> {
    let Some(profile_hash) = boot_presentation::take_mirror_profile() else {
        log::info!("device-presentation: fallback boot");
        return None;
    };
    log::info!("device-presentation: requested mirror {:08x}", profile_hash);
    let Some(store) = store.as_mut() else {
        log::error!("device-presentation: mirror storage unavailable");
        return None;
    };
    let Some(profile) = store.find(profile_hash).ok().flatten() else {
        log::error!("device-presentation: profile {:08x} not found", profile_hash);
        return None;
    };
    let image = PROFILE_ACTIVE_IMAGE.init([0; HSMI_MAX_SIZE]);
    if let Err(error) = store.read_profile(profile, image) {
        log::error!("device-presentation: profile read failed {:?}", error);
        return None;
    }
    let image: &'static [u8] = &image[..profile.length];
    let plan = match validate_mirror_image(image) {
        Ok(plan) => plan,
        Err(error) => {
            log::error!("device-presentation: stored profile invalid {:?}", error);
            return None;
        }
    };
    log::info!("device-presentation: mirror {:08x} ready", profile_hash);
    Some((profile_hash, plan))
}

fn build_dynamic_device<'a, B: UsbBus>(
    alloc: &'a usb_device::bus::UsbBusAllocator<B>,
    dynamic: &DynamicUsb<'a, B>,
) -> UsbDevice<'a, B> {
    let plan = dynamic.plan();
    let device = plan.device_descriptor;
    let configuration = plan.configuration_descriptor;
    let usb_revision = if u16::from_le_bytes([device[2], device[3]]) >= 0x0210 {
        UsbRev::Usb210
    } else {
        UsbRev::Usb200
    };
    let builder = UsbDeviceBuilder::new(
        alloc,
        UsbVidPid(
            u16::from_le_bytes([device[8], device[9]]),
            u16::from_le_bytes([device[10], device[11]]),
        ),
    )
    .device_class(device[4])
    .device_sub_class(device[5])
    .device_protocol(device[6])
    .device_release(u16::from_le_bytes([device[12], device[13]]))
    .usb_rev(usb_revision)
    .self_powered(configuration[7] & 0x40 != 0);
    let builder = match builder.max_packet_size_0(device[7]) {
        Ok(builder) => builder,
        Err(error) => fatal("dynamic EP0", error),
    };
    let builder = match builder.max_power(usize::from(configuration[8]) * 2) {
        Ok(builder) => builder,
        Err(error) => fatal("dynamic power", error),
    };
    builder.build()
}

trait PresentationRuntime<B: UsbBus> {
    fn poll(&mut self, usb_device: &mut UsbDevice<'_, B>);
    fn enqueue_link_event(&mut self, event: DeviceLinkEvent);
    fn take_standard_output(&mut self) -> Option<StandardOutputReport> {
        None
    }
    fn restore_standard_output(&mut self, _report: StandardOutputReport) {}
    fn take_raw_output(&mut self) -> Option<RawPacket> {
        None
    }
    fn restore_raw_output(&mut self, _packet: RawPacket) {}
    fn usb_state(&self, configured: bool, profile_hash: u32) -> UsbState;
    fn is_fallback(&self) -> bool;
}

impl<B: UsbBus> PresentationRuntime<B> for FallbackUsb<'_, B> {
    fn poll(&mut self, usb_device: &mut UsbDevice<'_, B>) {
        usb_device.poll(&mut [
            &mut self.keyboard,
            &mut self.mouse,
            &mut self.consumer,
        ]);
        self.service();
    }

    fn enqueue_link_event(&mut self, event: DeviceLinkEvent) {
        self.enqueue_link_event(event);
    }

    fn take_standard_output(&mut self) -> Option<StandardOutputReport> {
        self.take_keyboard_output()
    }

    fn restore_standard_output(&mut self, report: StandardOutputReport) {
        self.restore_keyboard_output(report);
    }

    fn usb_state(&self, configured: bool, _profile_hash: u32) -> UsbState {
        fallback_usb_state(configured)
    }

    fn is_fallback(&self) -> bool {
        true
    }
}

impl<B: UsbBus> PresentationRuntime<B> for DynamicUsb<'_, B> {
    fn poll(&mut self, usb_device: &mut UsbDevice<'_, B>) {
        usb_device.poll(&mut [self]);
        self.service();
    }

    fn enqueue_link_event(&mut self, event: DeviceLinkEvent) {
        match event {
            DeviceLinkEvent::RawEndpointIn(report) => {
                if RawPacket::new(report.endpoint_address, report.data())
                    .and_then(|packet| self.enqueue_input(packet))
                    .is_err()
                {
                    self.dropped_packets = self.dropped_packets.saturating_add(1);
                }
            }
            DeviceLinkEvent::StandardInput(_) | DeviceLinkEvent::ReleaseAll => {
                self.drop_standard_report();
            }
            _ => {}
        }
    }

    fn take_raw_output(&mut self) -> Option<RawPacket> {
        self.take_output()
    }

    fn restore_raw_output(&mut self, packet: RawPacket) {
        self.restore_output(packet);
    }

    fn usb_state(&self, _configured: bool, profile_hash: u32) -> UsbState {
        UsbState {
            attached: true,
            configured: self.configured(),
            fallback_active: false,
            healthy: true,
            active_profile_hash: profile_hash,
            error_code: 0,
        }
    }

    fn is_fallback(&self) -> bool {
        false
    }
}

fn run<'a, B: UsbBus, P: PresentationRuntime<B>>(
    spi: esp_hal::spi::slave::dma::SpiDma<'static, esp_hal::Blocking>,
    mut dma_rx: DmaRxBuf,
    mut dma_tx: DmaTxBuf,
    session_id: u32,
    mut usb_device: UsbDevice<'a, B>,
    mut presentation: P,
    mut profile_store: Option<profile_store::DeviceProfileStore>,
    mut profile_receiver: ProfileTransferReceiver<'static>,
    presentation_profile_hash: u32,
) -> ! {
    let mut current_usb_state = presentation.usb_state(false, presentation_profile_hash);
    let mut link = if profile_store.is_some() {
        DeviceLink::new_with_profile_storage(
            session_id,
            current_usb_state,
            presentation_profile_hash,
        )
    } else {
        DeviceLink::new(session_id, current_usb_state)
    };
    let mut ever_linked = false;
    let mut last_valid_spi_ms = now_ms();
    let initial_tx = link.next_transaction(now_ms());
    dma_tx.as_mut_slice().copy_from_slice(&initial_tx);
    dma_rx.as_mut_slice().fill(0);
    let mut transfer = match spi.transfer(SPI_CELL_LEN, dma_rx, SPI_CELL_LEN, dma_tx) {
        Ok(transfer) => transfer,
        Err((error, _, _, _)) => fatal("initial slave DMA queue", error),
    };
    let mut pending_profile_result = None;
    let mut raw_output_sequence = 1u16;

    loop {
        while !transfer.is_done() {
            service_usb(
                &mut usb_device,
                &mut presentation,
                &mut link,
                &mut current_usb_state,
                &mut raw_output_sequence,
            );
            if ever_linked && now_ms().saturating_sub(last_valid_spi_ms) >= SPI_LINK_LOSS_TIMEOUT_MS
            {
                let _ = usb_device.force_reset();
                esp_hal::system::software_reset();
            }
        }
        let (spi, (mut dma_rx, mut dma_tx)) = transfer.wait();
        let now_ms = now_ms();
        let mut received = [0u8; SPI_CELL_LEN];
        received.copy_from_slice(dma_rx.as_slice());
        let mut events = heapless::Vec::<DeviceLinkEvent, 4>::new();
        let valid_cells_before = link.diagnostics().valid_cells;
        link.handle_transaction(&received, now_ms, &mut events);
        if link.diagnostics().valid_cells != valid_cells_before {
            last_valid_spi_ms = now_ms;
        }
        ever_linked |= link.host_compatible();
        for event in events {
            match event {
                DeviceLinkEvent::ProfileBegin(begin) => {
                    if profile_store.is_none() {
                        pending_profile_result = Some(ProfileResult {
                            transfer_id: begin.transfer_id,
                            profile_hash: begin.profile_hash,
                            status: ProfileResultStatus::StorageError,
                            reject_reason: MirrorRejectReason::StorageFailure as u8,
                            detail: 0,
                        });
                    } else if let Err(error) = profile_receiver.begin(begin) {
                        pending_profile_result = Some(profile_transfer_error_result(
                            error,
                            begin.transfer_id,
                            begin.profile_hash,
                        ));
                    }
                }
                DeviceLinkEvent::ProfileChunk(chunk) => {
                    if let Err(error) = profile_receiver.chunk(chunk.as_borrowed()) {
                        pending_profile_result = Some(profile_transfer_error_result(
                            error,
                            chunk.transfer_id(),
                            0,
                        ));
                    }
                }
                DeviceLinkEvent::ProfileCommit { transfer_id } => {
                    let mut result = profile_receiver.commit(transfer_id);
                    if result.status == ProfileResultStatus::Accepted {
                        result = match (profile_store.as_mut(), profile_receiver.committed()) {
                            (Some(store), Some((metadata, image))) => {
                                match store.commit(image, metadata.profile_hash) {
                                    Ok(ProfileCommitOutcome::Stored(_)) => result,
                                    Ok(ProfileCommitOutcome::AlreadyStored(_)) => ProfileResult {
                                        status: ProfileResultStatus::AlreadyStored,
                                        ..result
                                    },
                                    Err(error) => profile_store::storage_error_result(
                                        error,
                                        transfer_id,
                                        metadata.profile_hash,
                                    ),
                                }
                            }
                            _ => ProfileResult {
                                transfer_id,
                                profile_hash: result.profile_hash,
                                status: ProfileResultStatus::StorageError,
                                reject_reason: MirrorRejectReason::StorageFailure as u8,
                                detail: 0,
                            },
                        };
                        profile_receiver.clear_committed();
                    }
                    pending_profile_result = Some(result);
                    log::info!(
                        "device-profile: transfer={} status={} reason={}",
                        result.transfer_id,
                        result.status as u8,
                        result.reject_reason
                    );
                }
                DeviceLinkEvent::ActivateProfile(activate) => {
                    log::info!(
                        "device-presentation: activate op={} hash={:08x}",
                        activate.operation_id,
                        activate.profile_hash
                    );
                    if activate.profile_hash != presentation_profile_hash
                        && profile_store
                            .as_mut()
                            .and_then(|store| store.find(activate.profile_hash).ok().flatten())
                            .is_some()
                    {
                        boot_presentation::request_mirror(activate.profile_hash);
                        let _ = usb_device.force_reset();
                        restart_presentation();
                    }
                }
                DeviceLinkEvent::ForceFallback { .. } if !presentation.is_fallback() => {
                    let _ = usb_device.force_reset();
                    restart_presentation();
                }
                event => presentation.enqueue_link_event(event),
            }
        }
        if let Some(result) = pending_profile_result
            && link.queue_profile_result(result, now_ms)
        {
            pending_profile_result = None;
        }

        // Queue the next transaction before servicing USB. The slave remains
        // ready while the master follows its fixed 500 us polling schedule.
        let tx = link.next_transaction(now_ms);
        dma_tx.as_mut_slice().copy_from_slice(&tx);
        dma_rx.as_mut_slice().fill(0);
        transfer = match spi.transfer(SPI_CELL_LEN, dma_rx, SPI_CELL_LEN, dma_tx) {
            Ok(transfer) => transfer,
            Err((error, _, _, _)) => fatal("slave DMA requeue", error),
        };

        service_usb(
            &mut usb_device,
            &mut presentation,
            &mut link,
            &mut current_usb_state,
            &mut raw_output_sequence,
        );
    }
}

fn restart_presentation() -> ! {
    // The digital-core software reset preserves RTC persistent memory while
    // resetting USB, SPI and DMA. A CPU-only reset is insufficient because it
    // leaves the active SPI slave DMA transaction behind.
    soft_disconnect_usb();
    esp_hal::system::software_reset();
}

fn soft_disconnect_usb() {
    // Synopsys OTG DCTL.SDIS (bit 1) disconnects the pull-up. The current
    // usb-device adapter does not implement UsbBus::force_reset, so perform
    // the hardware operation explicitly before resetting the CPU.
    const DCTL_OFFSET: usize = 0x804;
    const SOFT_DISCONNECT: u32 = 1 << 1;
    let dctl = (esp_hal::peripherals::USB0::PTR as usize + DCTL_OFFSET) as *mut u32;
    // SAFETY: run() exclusively owns USB0, and no USB access occurs after
    // setting SDIS because this function immediately resets the CPU.
    unsafe {
        dctl.write_volatile(dctl.read_volatile() | SOFT_DISCONNECT);
    }
    esp_hal::delay::Delay::new().delay_millis(5);
}

fn profile_transfer_error_result(
    error: ProfileTransferError,
    transfer_id: u32,
    profile_hash: u32,
) -> ProfileResult {
    ProfileResult {
        transfer_id,
        profile_hash,
        status: if error == ProfileTransferError::Busy {
            ProfileResultStatus::Busy
        } else {
            ProfileResultStatus::InvalidImage
        },
        reject_reason: MirrorRejectReason::MalformedImage as u8,
        detail: 0,
    }
}

fn service_usb<B: UsbBus, P: PresentationRuntime<B>>(
    usb_device: &mut UsbDevice<'_, B>,
    presentation: &mut P,
    link: &mut DeviceLink,
    current_usb_state: &mut UsbState,
    raw_output_sequence: &mut u16,
) {
    presentation.poll(usb_device);
    let now_ms = now_ms();
    if let Some(output) = presentation.take_standard_output()
        && !link.queue_standard_output(output, now_ms)
    {
        presentation.restore_standard_output(output);
    }
    if let Some(packet) = presentation.take_raw_output() {
        let sent = hidshift::interchip::RawEndpointReport::new(
            packet.endpoint_address(),
            *raw_output_sequence,
            packet.data(),
        )
        .is_ok_and(|report| link.queue_raw_endpoint_out(report, now_ms));
        if sent {
            *raw_output_sequence = next_nonzero(*raw_output_sequence);
        }
        if !sent {
            presentation.restore_raw_output(packet);
        }
    }

    let next_usb_state = presentation.usb_state(
        usb_device.state() == UsbDeviceState::Configured,
        current_usb_state.active_profile_hash,
    );
    if next_usb_state != *current_usb_state {
        *current_usb_state = next_usb_state;
        link.update_usb_state(*current_usb_state, now_ms);
    }
}

const fn next_nonzero(value: u16) -> u16 {
    if value == u16::MAX || value == 0 {
        1
    } else {
        value + 1
    }
}

fn now_ms() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis()
}

fn fatal<T: core::fmt::Debug>(context: &str, error: T) -> ! {
    log::error!("device-spi: {} setup failed {:?}", context, error);
    esp_hal::system::software_reset()
}

const fn nonzero_session(value: u32) -> u32 {
    if value == 0 { 1 } else { value }
}

const fn fallback_usb_state(configured: bool) -> UsbState {
    UsbState {
        attached: true,
        configured,
        fallback_active: true,
        healthy: true,
        active_profile_hash: 0,
        error_code: 0,
    }
}
