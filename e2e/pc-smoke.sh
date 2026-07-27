#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
esp_export="$repo_root/.mise/esp/export-esp.sh"
dut_port="/dev/serial/by-id/usb-1a86_USB_Single_Serial_5C84329179-if00"

if [[ ! -f "$esp_export" ]]; then
  echo "ESP toolchain is not installed. Run: mise run esp:install" >&2
  exit 1
fi
if [[ ! -e "$dut_port" ]]; then
  echo "DUT USB identity is not connected: $dut_port" >&2
  exit 1
fi

source "$esp_export"
cd "$repo_root"

cargo +esp build \
  --locked \
  -Zbuild-std=core,alloc \
  --release \
  --manifest-path firmware/Cargo.toml \
  --bin firmware \
  --features hardware-e2e \
  --target xtensa-esp32s3-none-elf

espflash flash \
  --chip esp32s3 \
  --port "$dut_port" \
  --partition-table partitions/bridge.csv \
  --target-app-partition ota_0 \
  target/xtensa-esp32s3-none-elf/release/firmware

cargo build --locked --release --manifest-path tools/hidshiftctl/Cargo.toml
ctl="$repo_root/tools/hidshiftctl/target/release/hidshiftctl"

"$ctl" --serial "$dut_port" --json status
"$ctl" --serial "$dut_port" --json input list
"$ctl" --serial "$dut_port" --json support status
