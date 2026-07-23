# Optional dual-S3 Wired USB and HID Mirror

The `dual-s3-wired` feature keeps Host S3 responsible for USB Host, BLE, input
aggregation, routing, and management. Device S3 is a dedicated native USB HID
Device. The default firmware does not include this path and runs on one
ESP32-S3.

## Wiring

Connect a common ground and only these four SPI signals:

| Signal | Host S3 | Device S3 |
| --- | --- | --- |
| CS | GPIO10 output | GPIO10 input |
| MOSI | GPIO11 output | GPIO11 input |
| SCLK | GPIO12 output | GPIO12 input |
| MISO | GPIO13 input | GPIO13 output |

The link uses SPI2, mode 0, MSB first, 10 MHz, DMA, and fixed 128-byte
transactions. Host S3 is master and polls every 500 µs. No READY, IRQ, or reset
wire is used. Device S3 native USB uses GPIO19 D- and GPIO20 D+.

## Output and presentation

Exactly one output is active: Wired USB or BLE host 1–4. A disconnected or
unready selection becomes inactive and never fails over. Target switching
releases the old target, suppresses inputs already held, sends a neutral state
to the new target, and only then activates it.

Mirror selection is stored separately:

- Wired without an available Mirror uses `HIDShift Wired`, a driverless
  Keyboard + Mouse + Consumer composite HID.
- Wired with an available Mirror presents that device's validated descriptors
  and forwards raw interrupt and HID control reports without Report ID
  rewriting.
- BLE always uses normalized aggregated input. Device S3 remains neutral
  Fallback USB and the saved Mirror selection is retained.

Only one Full-Speed, HID-only, single-configuration device can be mirrored.
Up to four HID interfaces, four IN endpoints, four OUT endpoints, 64-byte
interrupt packets, and a 16 KiB MirrorImage are accepted. Bulk, isochronous,
non-HID interfaces, alternate settings, and multiple configurations are
rejected before activation. A source device's Remote Wakeup capability is
preserved in the Configuration Descriptor, including the standard
SET/CLEAR_FEATURE and GET_STATUS state. Device S3 does not currently originate
a resume signal while the Wired PC is suspended.

## Build and flash

```sh
mise run esp:install
mise run firmware:build-dual
mise run device-firmware:build

HIDSHIFT_HOST_PORT=/dev/serial/by-id/<host> mise run firmware:flash-dual
HIDSHIFT_DEVICE_PORT=/dev/serial/by-id/<device> mise run device-firmware:flash
```

Host S3 connects to the USB Hub. Device S3 connects its native USB port to the
Wired PC. Device USB contains HID only; use Host S3 BLE management during
Mirror mode.

## Management

```sh
hidshiftctl --ble target status
hidshiftctl --ble target usb
hidshiftctl --ble target ble 1
hidshiftctl --ble mirror list
hidshiftctl --ble mirror select 0
hidshiftctl --ble mirror clear
```

The Web UI shows routing and Mirror candidates only when firmware reports the
dual-S3 capability. It stays connected over BLE during USB
detach/re-enumeration.

## Recovery

Device S3 validates and stores profiles transactionally in two flash slots.
Invalid images preserve the current presentation. A missing or incompatible
SPI link falls back to `HIDShift Wired`; it never sends input to BLE
automatically. Profile activation holds USB detached for 100 ms so the PC
cannot retain stale descriptors across profile changes.
