# Local patches

Base crate: `embassy-usb-synopsys-otg` 0.4.0.

Upstream source: <https://github.com/embassy-rs/embassy>

License: `MIT OR Apache-2.0`. This copy is redistributed under the MIT option;
see [LICENSE-MIT](LICENSE-MIT).

`src/host.rs` carries the USB Host fixes required by this firmware:

- retain events for multiple active host channels instead of overwriting them
- clean up a transfer when its async future is cancelled
- preserve periodic endpoint scheduling from `bInterval`
- retry transient periodic transaction errors without losing channel state

The repository uses this copy through `[patch.crates-io]` in the root
`Cargo.toml`. Host stress testing with a one-level hub, composite keyboard, and
full-speed mouse exercises these changes.
