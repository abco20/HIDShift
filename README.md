# HIDShift

HIDShift turns an ESP32-S3 into a USB-to-BLE bridge for keyboards, mice, and
consumer controls. Input is sent only to the selected BLE host.

The standard build uses one ESP32-S3. An optional two-board build can also
switch between BLE and wired USB and mirror a USB HID device.

## Quick start

Hardware:

- ESP32-S3
- USB OTG D- on GPIO19 and D+ on GPIO20
- Active-low control button on GPIO0
- A stable 5 V supply for the board and attached USB devices

Install the tools, build, and flash the standard image:

```sh
mise install
mise run esp:install
mise run firmware:flash
```

Direct HID devices, composite devices, and one level of USB hub are supported.

## Controls

Actions occur when GPIO0 is released.

| Hold | Action |
| --- | --- |
| Less than 3 seconds | Select the next ready destination |
| 3–8 seconds | Pair a new destination |
| 8 seconds or longer | Forget the active destination |

## Management

The device can be managed over BLE or USB serial:

- `tools/hidshiftctl`: command-line interface
- `web`: Web Bluetooth / Web Serial interface

```sh
cargo run --release --manifest-path tools/hidshiftctl/Cargo.toml -- status
mise run web:serve
```

## Development

The bridge core is `no_std` and tested on the host:

```sh
mise run host:ci
mise run firmware:check
```

Hardware tests are split by the equipment they require:

```sh
mise run e2e:pc
mise run e2e:radio
mise run e2e:dual
```

## Documentation

- [Management protocol](docs/management-protocol.md)
- [Dual-S3 wiring and behavior](docs/dual-s3.md)
- [Hardware E2E suites](e2e/README.md)
- [Change history](CHANGELOG.md)

## License

HIDShift is licensed under the MIT License. See [LICENSE](LICENSE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
