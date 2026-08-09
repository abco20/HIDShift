# Changelog

## Unreleased

- Preserves high-rate mouse movement with 16-bit relative X/Y reports. Existing
  BLE hosts must forget and pair HIDShift again after installing this firmware;
  dual-S3 installations must update both boards.

## 0.1.0 - 2026-07-10

- Bridges USB HID keyboards, mice, and consumer-control devices to BLE HID.
- Supports directly connected devices, composite HID devices, and one level of
  USB hubs.
- Supports keyboard input and LED output, five-button mouse input, vertical and
  horizontal scrolling, and consumer-control input.
- Stores up to four BLE host profiles and restores bonds and the active target
  after reboot.
- Provides button controls for target selection, pairing, and bond removal.
