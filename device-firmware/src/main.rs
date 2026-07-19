#![no_std]
#![no_main]

mod usb_fallback;

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::spi::Mode;
use esp_hal::spi::slave::Spi;
use hidshift::fallback::{
    FALLBACK_USB_DEVICE_RELEASE, FALLBACK_USB_MANUFACTURER, FALLBACK_USB_PRODUCT,
    FALLBACK_USB_PRODUCT_ID, FALLBACK_USB_VENDOR_ID,
};
use hidshift::interchip::{DeviceLink, DeviceLinkEvent, SPI_CELL_LEN, UsbState};
use static_cell::StaticCell;
use usb_device::LangID;
use usb_device::bus::UsbBus;
use usb_device::device::{
    StringDescriptors, UsbDevice, UsbDeviceBuilder, UsbDeviceState, UsbVidPid,
};
use usb_fallback::FallbackUsb;

esp_bootloader_esp_idf::esp_app_desc!();

static USB_ENDPOINT_MEMORY: StaticCell<[u32; 1024]> = StaticCell::new();
const SPI_LINK_LOSS_TIMEOUT_MS: u64 = 1_500;

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let session_id = nonzero_session(esp_hal::rng::Rng::new().random());

    let usb = esp_hal::otg_fs::Usb::new(peripherals.USB0, peripherals.GPIO20, peripherals.GPIO19);
    let endpoint_memory = USB_ENDPOINT_MEMORY.init([0; 1024]);
    let usb_bus = esp_hal::otg_fs::UsbBus::new(usb, endpoint_memory);
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

    run(spi, dma_rx, dma_tx, session_id, usb_device, fallback)
}

fn run<'a, B: UsbBus>(
    spi: esp_hal::spi::slave::dma::SpiDma<'static, esp_hal::Blocking>,
    mut dma_rx: DmaRxBuf,
    mut dma_tx: DmaTxBuf,
    session_id: u32,
    mut usb_device: UsbDevice<'a, B>,
    mut fallback: FallbackUsb<'a, B>,
) -> ! {
    let mut current_usb_state = fallback_usb_state(false);
    let mut link = DeviceLink::new(session_id, current_usb_state);
    let mut ever_linked = false;
    let mut last_valid_spi_ms = now_ms();
    let initial_tx = link.next_transaction(now_ms());
    dma_tx.as_mut_slice().copy_from_slice(&initial_tx);
    dma_rx.as_mut_slice().fill(0);
    let mut transfer = match spi.transfer(SPI_CELL_LEN, dma_rx, SPI_CELL_LEN, dma_tx) {
        Ok(transfer) => transfer,
        Err((error, _, _, _)) => fatal("initial slave DMA queue", error),
    };

    loop {
        while !transfer.is_done() {
            service_usb(
                &mut usb_device,
                &mut fallback,
                &mut link,
                &mut current_usb_state,
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
            fallback.enqueue_link_event(event);
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
            &mut fallback,
            &mut link,
            &mut current_usb_state,
        );
    }
}

fn service_usb<B: UsbBus>(
    usb_device: &mut UsbDevice<'_, B>,
    fallback: &mut FallbackUsb<'_, B>,
    link: &mut DeviceLink,
    current_usb_state: &mut UsbState,
) {
    usb_device.poll(&mut [
        &mut fallback.keyboard,
        &mut fallback.mouse,
        &mut fallback.consumer,
    ]);
    fallback.service();
    let now_ms = now_ms();
    if let Some(output) = fallback.take_keyboard_output()
        && !link.queue_standard_output(output, now_ms)
    {
        fallback.restore_keyboard_output(output);
    }

    let next_usb_state = fallback_usb_state(usb_device.state() == UsbDeviceState::Configured);
    if next_usb_state != *current_usb_state {
        *current_usb_state = next_usb_state;
        link.update_usb_state(*current_usb_state, now_ms);
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
