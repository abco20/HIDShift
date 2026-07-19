#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::spi::Mode;
use esp_hal::spi::slave::Spi;
use hidshift::interchip::{DeviceLink, DeviceLinkEvent, SPI_CELL_LEN, UsbState};

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let session_id = nonzero_session(esp_hal::rng::Rng::new().random());

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

    run(spi, dma_rx, dma_tx, session_id)
}

fn run(
    mut spi: esp_hal::spi::slave::dma::SpiDma<'static, esp_hal::Blocking>,
    mut dma_rx: DmaRxBuf,
    mut dma_tx: DmaTxBuf,
    session_id: u32,
) -> ! {
    let mut link = DeviceLink::new(session_id, initial_usb_state());
    loop {
        let now_ms = esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_millis();
        let tx = link.next_transaction(now_ms);
        dma_tx.as_mut_slice().copy_from_slice(&tx);
        dma_rx.as_mut_slice().fill(0);

        let transfer = match spi.transfer(SPI_CELL_LEN, dma_rx, SPI_CELL_LEN, dma_tx) {
            Ok(transfer) => transfer,
            Err((error, returned_spi, returned_rx, returned_tx)) => {
                log::error!("device-spi: failed to queue slave DMA {:?}", error);
                spi = returned_spi;
                dma_rx = returned_rx;
                dma_tx = returned_tx;
                continue;
            }
        };
        (spi, (dma_rx, dma_tx)) = transfer.wait();
        let mut received = [0u8; SPI_CELL_LEN];
        received.copy_from_slice(dma_rx.as_slice());
        let mut events = heapless::Vec::<DeviceLinkEvent, 4>::new();
        link.handle_transaction(&received, now_ms, &mut events);
        // Commit 5 connects these already decoded events to the native USB
        // endpoint workers. Until then USB remains explicitly not configured.
        for event in events {
            log::trace!("device-spi: queued USB event {:?}", event);
        }
    }
}

fn fatal<T: core::fmt::Debug>(context: &str, error: T) -> ! {
    log::error!("device-spi: {} setup failed {:?}", context, error);
    esp_hal::system::software_reset()
}

const fn nonzero_session(value: u32) -> u32 {
    if value == 0 { 1 } else { value }
}

const fn initial_usb_state() -> UsbState {
    UsbState {
        attached: false,
        configured: false,
        fallback_active: true,
        healthy: true,
        active_profile_hash: 0,
        error_code: 0,
    }
}
