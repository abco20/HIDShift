#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
esp_export="$repo_root/.mise/esp/export-esp.sh"
: "${HIDSHIFT_DUT_PORT:?Set HIDSHIFT_DUT_PORT to the DUT serial path}"
dut_port="$HIDSHIFT_DUT_PORT"

if [[ ! -f "$esp_export" ]]; then
  echo "ESP toolchain is not installed. Run: mise run esp:install" >&2
  exit 1
fi
if [[ ! -e "$dut_port" ]]; then
  echo "DUT serial path is not connected: $dut_port" >&2
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

cargo run --locked --release --manifest-path e2e/runner/Cargo.toml \
  --bin serial_management_smoke -- "$dut_port"
