# Local patches

Base crate: `trouble-host` 0.6.0.

Upstream source: <https://github.com/embassy-rs/trouble>

License: `MIT OR Apache-2.0`. This copy is redistributed under the MIT option;
see [LICENSE-MIT](LICENSE-MIT).

This vendor copy intentionally differs from the published crate in five source
files only:

- `connection.rs` exposes the existing LE L2CAP connection-parameter request
  path. HIDShift uses it because some Linux centrals reject the optional Link
  Layer procedure asynchronously.
- `connection_manager.rs`, `host.rs`, and `lib.rs` add ordered immediate PDU
  submission and transmit-stage observation. This removes an executor wake
  from latency-sensitive HID notifications while preserving queued ATT order.
- `attribute.rs` exposes `notify_immediate` on a characteristic.

The repository selects this copy through `[patch.crates-io]` in the root
`Cargo.toml`. Remove the patch once upstream provides equivalent explicit
L2CAP parameter updates and ordered immediate notification submission.

Validation consists of the crate library tests, all HIDShift host tests, the
ESP32-S3 firmware build, and the direct BLE hardware E2E.
The crate's standalone `tests/gatt.rs` expects a platform-specific serial
adapter argument and is not a self-contained host test.
