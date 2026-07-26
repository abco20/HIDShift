mod ble_bonds;
pub(crate) mod ble_hid_task;
pub mod button_task;
#[cfg_attr(
    all(feature = "hardware-e2e", feature = "dual-s3-wired"),
    allow(dead_code)
)]
pub mod flash_backend;
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
pub mod mirror_e2e_fault;
#[cfg(feature = "dual-s3-wired")]
pub mod mirror_spi_task;
pub mod serial_management_task;
pub mod storage_task;
pub mod usb_host_task;
mod usb_output_transport;
mod usb_transport;
