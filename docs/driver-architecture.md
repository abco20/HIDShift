# Future PC driver boundary

This document fixes the firmware boundary needed by a future companion driver.
It does not define the driver transport, operating-system API, or release plan.

## Current path

```text
physical USB HID
  -> raw USB source boundary
  -> descriptor parser / normalized input events
  -> active target router
  -> standard BLE HID adapter
  -> PC
```

The production firmware uses this path. Standard keyboard, mouse, and consumer
reports remain usable without installing PC software.

## Future driver path

A later adapter may consume the same source boundary before normalization:

```text
physical USB HID
  -> raw USB source boundary
  -> future driver transport adapter
  -> PC companion service / OS virtual-device driver
```

The PC side is expected to create the virtual device. The ESP32 will not return
to USB Device descriptor emulation. Linux UHID is a likely first backend;
Windows VHF or UdeCx can be evaluated independently after the firmware
transport is measured.

## Preserved information

`usb_hid::source` owns the transport-independent contract. It preserves:

- stable runtime `DeviceId` and `InterfaceId`
- VID, PID, device version, class fields, and available strings
- interface number, alternate setting, subclass, and protocol
- the exact HID report descriptor
- the exact input report bytes, including original report IDs
- GET_REPORT and SET_REPORT targets with device, interface, report type, and report ID

Device and interface identities must not be merged or remapped before an
adapter chooses how to expose them. Fixed capacities fail explicitly instead
of truncating descriptor or report data.

## Reverse direction

Output and feature traffic follows the reverse path:

```text
PC virtual device request
  -> future transport adapter
  -> UsbHidControlRequest
  -> owning USB Host interface
  -> physical device
  -> UsbHidControlResponse
  -> PC
```

The request is owned and allocation-free so it can cross firmware task
boundaries. USB transfer status, transport framing, retries, request IDs, and
timeouts belong to adapters and are deliberately absent from the core type.

## Deferred decisions

The following are intentionally left until the PC prototype can measure them:

- BLE service and framing for raw descriptors and reports
- flow control and report fragmentation
- daemon/driver process split and privilege model
- Windows backend selection
- support policy for vendor applications and firmware updates

Any future protocol should adapt these source types rather than change the
standard BLE HID path or leak OS-specific concepts into the bridge core.
