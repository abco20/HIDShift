# HIDShift local patch

This is `usb-device` 0.3.2 with a minimal deferred EP0 extension used only by
the optional dual-S3 Device firmware.

The patch adds:

- `ControlIn::defer` and `ControlOut::defer`;
- `UsbDevice::{complete_control_in, complete_control_out,
  reject_deferred_control}`;
- preservation of the EP0 response state while a Mirror control request
  crosses the SPI link.

The normal single-S3 firmware does not depend on this crate. The Device
firmware also enables the upstream `control-buffer-256` feature so HID Feature
reports up to the inter-chip protocol limit can be forwarded without
truncation.
