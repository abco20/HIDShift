use std::env;
use std::error::Error;
use std::io::{Read, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use btleplug::api::{Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use clap::{ArgGroup, Parser, Subcommand};
use futures_util::StreamExt;
use hidshift::{
    MANAGEMENT_REQUEST_UUID, MANAGEMENT_RESPONSE_LEN, MANAGEMENT_RESPONSE_UUID,
    MANAGEMENT_SERVICE_UUID, ManagementCommand, ManagementHostStatus, ManagementOutputTarget,
    ManagementResponse, ManagementResponsePayload, ManagementResult, MirrorCandidateId,
    SETTING_DESCRIPTORS, SettingScope, SettingTarget, setting_by_key,
};
use hidshift_client::{
    ManagementClient, PendingRequest, SerialResponseDecoder, encode_serial_request,
};
use serialport::{FlowControl, SerialPort};
use uuid::Uuid;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Transport {
    Serial(String),
    Ble(Option<String>),
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Arguments {
    transport: Transport,
    command: CliCommand,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CliCommand {
    Request(ManagementCommand),
    Devices,
    Diagnostics,
    History,
    MirrorList,
    SettingsList,
    SettingDescribe {
        key: String,
    },
    SettingGet {
        key: String,
        slot: Option<u8>,
    },
    SettingSet {
        key: String,
        slot: Option<u8>,
        value: i32,
    },
    Overview,
}

#[derive(Debug, Parser)]
#[command(name = "hidshiftctl", version, about = "HIDShiftをUSBまたはBluetooth経由で管理します", long_about = None)]
#[command(group(ArgGroup::new("transport").multiple(false).args(["serial", "ble"])))]
struct CliArgs {
    /// USB Serialポート
    #[arg(long, value_name = "PORT")]
    serial: Option<String>,
    /// BluetoothでHIDShiftを自動検出
    #[arg(long)]
    ble: bool,
    /// 接続するBluetoothアドレス（省略時はHIDShiftを自動検出）
    #[arg(long, requires = "ble", value_name = "ADDRESS")]
    address: Option<String>,
    #[command(subcommand)]
    command: CommandArgs,
}

#[derive(Debug, Subcommand)]
enum CommandArgs {
    /// 接続先・USB入力・診断・履歴をまとめて表示
    Overview,
    /// 現在の接続状態を表示
    Status,
    /// 接続中のUSB入力機器を表示
    Devices,
    /// 再起動や通信エラーの診断情報を表示
    Diagnostics,
    /// 接続イベントの履歴を表示
    History,
    /// 入力の送信先を切り替え
    Select {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: u8,
    },
    /// USBまたはBLEの出力先を選択・確認
    Target {
        #[command(subcommand)]
        command: TargetArgs,
    },
    /// Dynamic USB Mirrorの対象を選択・解除
    Mirror {
        #[command(subcommand)]
        command: MirrorArgs,
    },
    /// 新しいPCやスマートフォンのペアリングを開始
    Pair {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: u8,
    },
    /// 実行中のペアリングを中止
    PairCancel,
    /// スロットに登録された機器を削除
    Forget {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: u8,
    },
    /// スロットの機器名と状態を表示
    Info {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: u8,
    },
    /// スロットの最終接続情報を表示
    Timing {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: u8,
    },
    /// スロットの表示名を変更（空文字列で自動名に戻す）
    Name {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: u8,
        name: String,
    },
    /// 動作設定を確認・変更
    Settings {
        #[command(subcommand)]
        command: SettingsArgs,
    },
}

#[derive(Debug, Subcommand)]
enum TargetArgs {
    /// Device S3経由のWired USBを選択
    Usb,
    /// BLE Host slotを選択
    Ble {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: u8,
    },
    /// 選択中・稼働中の出力先とReady状態を表示
    Status,
}

#[derive(Debug, Subcommand)]
enum MirrorArgs {
    /// 登録済みMirror candidateを一覧表示
    List,
    /// 登録済みMirror candidateを選択
    Select { candidate: u8 },
    /// Mirror設定を解除
    Clear,
    /// Mirror設定と現在のUSB presentationを表示
    Status,
}

#[derive(Debug, Subcommand)]
enum SettingsArgs {
    /// 現在値を説明付きで一覧表示
    List,
    /// 設定の用途・範囲・選択肢を表示
    Describe { key: String },
    /// 設定値を表示
    Get {
        key: String,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: Option<u8>,
    },
    /// 設定値を変更（例: on, slot-2, jis, 125%）
    Set {
        key: String,
        value: String,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=4))]
        slot: Option<u8>,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        match error.downcast::<clap::Error>() {
            Ok(error) => error.exit(),
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments(env::args().skip(1))?;
    match arguments.command {
        CliCommand::Request(command) => {
            let response = request(&arguments.transport, command).await?;
            print_response(response);
            ensure_ok(response)
        }
        CliCommand::Devices => print_devices(&arguments.transport).await,
        CliCommand::Diagnostics => {
            let response = request(&arguments.transport, ManagementCommand::GetDiagnostics).await?;
            print_response(response);
            ensure_ok(response)
        }
        CliCommand::History => print_history(&arguments.transport).await,
        CliCommand::MirrorList => print_mirror_candidates(&arguments.transport).await,
        CliCommand::SettingsList => print_settings(&arguments.transport).await,
        CliCommand::SettingDescribe { key } => {
            print_setting_description(&key)?;
            Ok(())
        }
        CliCommand::SettingGet { key, slot } => {
            let command = setting_command(&key, slot, None)?;
            let response = request(&arguments.transport, command).await?;
            print_response(response);
            ensure_ok(response)
        }
        CliCommand::SettingSet { key, slot, value } => {
            let command = setting_command(&key, slot, Some(value))?;
            let response = request(&arguments.transport, command).await?;
            print_response(response);
            ensure_ok(response)
        }
        CliCommand::Overview => {
            for command in [
                ManagementCommand::GetStatus,
                ManagementCommand::GetDiagnostics,
            ] {
                print_response(request(&arguments.transport, command).await?);
            }
            print_devices(&arguments.transport).await?;
            print_history(&arguments.transport).await
        }
    }
}

async fn request(
    transport: &Transport,
    command: ManagementCommand,
) -> Result<ManagementResponse, Box<dyn Error>> {
    let request_id = SystemTime::now().duration_since(UNIX_EPOCH)?.subsec_nanos() as u8;
    let mut client = ManagementClient::new(request_id);
    let request = client
        .begin(command)
        .map_err(|error| format!("could not start request: {error:?}"))?;

    let bytes = match transport {
        Transport::Serial(port) => serial_request(port, request, DEFAULT_TIMEOUT)?,
        Transport::Ble(address) => {
            ble_request(address.as_deref(), request, DEFAULT_TIMEOUT).await?
        }
        Transport::None => {
            return Err("接続方法を指定してください（--serial PORT または --ble）".into());
        }
    };
    let response = client
        .accept(&bytes)
        .map_err(|error| format!("invalid firmware response: {error:?}"))?;
    Ok(response)
}

fn ensure_ok(response: ManagementResponse) -> Result<(), Box<dyn Error>> {
    (response.result == ManagementResult::Ok)
        .then_some(())
        .ok_or_else(|| result_message(response.result).into())
}

fn serial_request(
    port_name: &str,
    request: PendingRequest,
    timeout: Duration,
) -> Result<[u8; MANAGEMENT_RESPONSE_LEN], Box<dyn Error>> {
    let mut port = open_management_serial(port_name)?;
    std::thread::sleep(Duration::from_secs(2));
    port.write_all(&encode_serial_request(request))?;
    port.flush()?;

    let deadline = Instant::now() + timeout;
    let mut reader = port.try_clone()?;
    let mut decoder = SerialResponseDecoder::default();
    let mut chunk = [0u8; 128];
    let mut diagnostic_tail = Vec::with_capacity(512);
    let mut boot_diagnostic = None;
    while Instant::now() < deadline {
        let length = match reader.read(&mut chunk) {
            Ok(0) => continue,
            Ok(length) => length,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(error.into()),
        };
        diagnostic_tail.extend_from_slice(&chunk[..length]);
        boot_diagnostic = boot_diagnostic.or_else(|| serial_boot_diagnostic(&diagnostic_tail));
        if diagnostic_tail.len() > 512 {
            diagnostic_tail.drain(..diagnostic_tail.len() - 512);
        }
        for response in decoder.push(&chunk[..length]) {
            if response[1] == request.request().request_id {
                return Ok(response);
            }
        }
    }
    Err(boot_diagnostic
        .unwrap_or("timed out waiting for HIDShift on the serial port")
        .into())
}

fn open_management_serial(port_name: &str) -> Result<Box<dyn SerialPort>, Box<dyn Error>> {
    let mut port = serialport::new(port_name, 115_200)
        .timeout(Duration::from_millis(200))
        .open()?;
    let _ = port.set_flow_control(FlowControl::None);
    // CH340 auto-reset wiring can otherwise leave EN/BOOT asserted for the
    // lifetime of the process or after an interrupted management command.
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);
    Ok(port)
}

fn serial_boot_diagnostic(bytes: &[u8]) -> Option<&'static str> {
    bytes.windows(b"BROWNOUT_RST".len())
        .any(|window| window == b"BROWNOUT_RST")
        .then_some(
            "HIDShiftが電圧低下（brownout）で再起動しています。電源容量、USBケーブル、USB機器への給電を確認してください",
        )
}

async fn ble_request(
    requested_address: Option<&str>,
    request: PendingRequest,
    timeout_duration: Duration,
) -> Result<[u8; MANAGEMENT_RESPONSE_LEN], Box<dyn Error>> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or("no Bluetooth adapter found")?;
    let service_uuid = Uuid::parse_str(MANAGEMENT_SERVICE_UUID)?;
    adapter
        .start_scan(ScanFilter {
            services: vec![service_uuid],
        })
        .await?;

    let peripheral = tokio::time::timeout(
        timeout_duration,
        find_peripheral(&adapter, requested_address),
    )
    .await
    .map_err(|_| "timed out scanning for HIDShift")??;
    adapter.stop_scan().await?;

    if !peripheral.is_connected().await? {
        peripheral.connect().await?;
    }
    peripheral.discover_services().await?;
    let request_uuid = Uuid::parse_str(MANAGEMENT_REQUEST_UUID)?;
    let response_uuid = Uuid::parse_str(MANAGEMENT_RESPONSE_UUID)?;
    let characteristics = peripheral.characteristics();
    let request_characteristic = characteristics
        .iter()
        .find(|characteristic| characteristic.uuid == request_uuid)
        .ok_or("management request characteristic not found")?;
    let response_characteristic = characteristics
        .iter()
        .find(|characteristic| characteristic.uuid == response_uuid)
        .ok_or("management response characteristic not found")?;
    if !response_characteristic
        .properties
        .contains(CharPropFlags::NOTIFY)
    {
        return Err("management response characteristic cannot notify".into());
    }

    let mut notifications = peripheral.notifications().await?;
    peripheral.subscribe(response_characteristic).await?;
    peripheral
        .write(
            request_characteristic,
            &request.encode(),
            WriteType::WithResponse,
        )
        .await?;

    tokio::time::timeout(timeout_duration, async {
        while let Some(notification) = notifications.next().await {
            if notification.uuid != response_uuid
                || notification.value.len() != MANAGEMENT_RESPONSE_LEN
            {
                continue;
            }
            let mut response = [0u8; MANAGEMENT_RESPONSE_LEN];
            response.copy_from_slice(&notification.value);
            if response[1] == request.request().request_id {
                return Ok(response);
            }
        }
        Err("Bluetooth notification stream ended".into())
    })
    .await
    .map_err(|_| "timed out waiting for the Bluetooth response")?
}

async fn find_peripheral(
    adapter: &Adapter,
    requested_address: Option<&str>,
) -> Result<Peripheral, Box<dyn Error>> {
    loop {
        for peripheral in adapter.peripherals().await? {
            let properties = peripheral.properties().await?;
            let address_matches = requested_address.is_some_and(|address| {
                peripheral
                    .address()
                    .to_string()
                    .eq_ignore_ascii_case(address)
            });
            let name_matches = requested_address.is_none()
                && properties
                    .as_ref()
                    .and_then(|properties| properties.local_name.as_deref())
                    == Some("HIDShift");
            if address_matches || name_matches {
                return Ok(peripheral);
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn print_response(response: ManagementResponse) {
    match response.payload {
        ManagementResponsePayload::HostInfo(info) => {
            let name = std::str::from_utf8(info.name.as_bytes()).unwrap_or("?");
            println!(
                "slot {}: {} [{}] ({})",
                info.host_id.0,
                name,
                match info.name_source {
                    1 => "auto",
                    2 => "manual",
                    _ => "unknown",
                },
                display_slot(info.status)
            );
        }
        ManagementResponsePayload::Status(status) => {
            println!(
                "active:  {}",
                display_host(status.active_host.map(|host| host.0))
            );
            println!(
                "pairing: {}",
                display_host(status.pairing_host.map(|host| host.0))
            );
            println!(
                "usb:     {} device(s), {} HID interface(s), {} keyboard(s)",
                status.usb.device_count, status.usb.interface_count, status.usb.keyboard_count
            );
            println!("slots:");
            for (index, host_status) in status
                .hosts
                .iter()
                .take(status.host_count as usize)
                .enumerate()
            {
                println!("  {}: {}", index + 1, display_slot(*host_status));
            }
        }
        ManagementResponsePayload::Diagnostics(value) => println!(
            "firmware: {}s uptime, reset=0x{:02x}, brownouts={}, BLE disconnects={}, notify failures={}, USB errors={}, flash writes={}, flash failures={}",
            value.uptime_seconds,
            value.reset_reason,
            value.brownout_count,
            value.ble_disconnect_count,
            value.ble_notify_failure_count,
            value.usb_error_count,
            value.flash_write_count,
            value.flash_failure_count
        ),
        ManagementResponsePayload::History(event) => println!(
            "#{:04} +{}s {} subject={} detail=0x{:02x} {:04x}:{:04x}",
            event.sequence,
            event.timestamp_seconds,
            history_kind(event.kind),
            event.subject,
            event.detail,
            event.vendor_id,
            event.product_id
        ),
        ManagementResponsePayload::Schema(schema) => println!(
            "firmware {}.{}.{} schema v{} settings={} history={} usb={} hash={:08x}",
            schema.firmware_major,
            schema.firmware_minor,
            schema.firmware_patch,
            schema.version,
            schema.setting_count,
            schema.history_capacity,
            schema.usb_capacity,
            schema.hash
        ),
        ManagementResponsePayload::Setting(setting) => {
            let descriptor = hidshift::setting_descriptor(setting.id);
            println!(
                "{:<20} {}  ({}, {})",
                descriptor.label,
                display_setting_value(descriptor, setting.value),
                descriptor.key,
                display_target(setting.target)
            );
        }
        ManagementResponsePayload::HostTiming(timing) => println!(
            "slot {}: last connected +{}s, last disconnected +{}s, reason=0x{:02x}",
            timing.host_id.0,
            timing.last_connected_seconds,
            timing.last_disconnected_seconds,
            timing.last_disconnect_reason
        ),
        ManagementResponsePayload::UsbDevice(device) => println!(
            "usb[{}] device={} {:04x}:{:04x} flags=0x{:02x} name-part={}",
            device.index,
            device.device_id,
            device.vendor_id,
            device.product_id,
            device.flags,
            String::from_utf8_lossy(device.name_chunk())
        ),
        ManagementResponsePayload::OutputTargetStatus(status) => {
            println!("selected: {}", display_output_target(status.selected));
            println!(
                "active:   {}",
                status
                    .active
                    .map(display_output_target)
                    .unwrap_or_else(|| "none".to_owned())
            );
            println!("state:    {:?}", status.availability);
            println!(
                "wired:   {}",
                if status.wired_ready {
                    "ready"
                } else {
                    "not ready"
                }
            );
            println!("ble ready mask: 0x{:02x}", status.ready_ble_mask);
            println!("presentation: {:?}", status.effective_presentation);
            println!("mirror configured: {}", status.mirror_configured);
            println!("operation: {}", status.operation_id);
        }
        ManagementResponsePayload::MirrorCandidate(candidate) => {
            println!(
                "mirror[{}] {:04x}:{:04x} profile={:08x} descriptor={:08x} source={}{}{}{}",
                candidate.candidate.0,
                candidate.vendor_id,
                candidate.product_id,
                candidate.profile_hash,
                candidate.descriptor_hash,
                candidate
                    .source_device
                    .map(|device| device.to_string())
                    .unwrap_or_else(|| "synthetic".to_owned()),
                if candidate.selected() {
                    " selected"
                } else {
                    ""
                },
                if candidate.active() { " active" } else { "" },
                if candidate.synthetic() {
                    " synthetic"
                } else {
                    ""
                },
            );
        }
        ManagementResponsePayload::None => {}
    }
}

fn display_output_target(target: ManagementOutputTarget) -> String {
    match target {
        ManagementOutputTarget::Wired => "usb".to_owned(),
        ManagementOutputTarget::Ble(host_id) => format!("ble {}", host_id.0),
    }
}

async fn print_devices(transport: &Transport) -> Result<(), Box<dyn Error>> {
    let status = request(transport, ManagementCommand::GetStatus).await?;
    ensure_ok(status)?;
    let ManagementResponsePayload::Status(status) = status.payload else {
        return Err("status payload missing".into());
    };
    if status.usb.device_count == 0 {
        println!("no USB HID devices");
    }
    for index in 0..status.usb.device_count {
        let mut offset = 0u8;
        let mut name = Vec::new();
        let mut first = None;
        loop {
            let response = request(
                transport,
                ManagementCommand::GetUsbDevice {
                    index,
                    name_offset: offset,
                },
            )
            .await?;
            ensure_ok(response)?;
            let ManagementResponsePayload::UsbDevice(device) = response.payload else {
                return Err("USB device payload missing".into());
            };
            first.get_or_insert(device);
            name.extend_from_slice(device.name_chunk());
            offset = offset.saturating_add(device.name_chunk_len);
            if offset >= device.name_len || device.name_chunk_len == 0 {
                break;
            }
        }
        let device = first.unwrap();
        println!(
            "usb[{}]: {} ({:04x}:{:04x}) device={}{}{}{}",
            index,
            if name.is_empty() {
                "unknown".into()
            } else {
                String::from_utf8_lossy(&name).into_owned()
            },
            device.vendor_id,
            device.product_id,
            device.device_id,
            if device.flags & 0x02 != 0 {
                " keyboard"
            } else {
                ""
            },
            if device.flags & 0x04 != 0 {
                " mouse"
            } else {
                ""
            },
            if device.flags & 0x08 != 0 {
                " consumer"
            } else {
                ""
            },
        );
    }
    Ok(())
}

async fn print_mirror_candidates(transport: &Transport) -> Result<(), Box<dyn Error>> {
    let mut found = 0usize;
    for candidate in 0..4 {
        let response = request(
            transport,
            ManagementCommand::GetMirrorCandidate(MirrorCandidateId(candidate)),
        )
        .await?;
        if response.result == ManagementResult::NotFound {
            continue;
        }
        ensure_ok(response)?;
        print_response(response);
        found += 1;
    }
    if found == 0 {
        println!("no mirror candidates");
    }
    Ok(())
}

async fn print_history(transport: &Transport) -> Result<(), Box<dyn Error>> {
    for index in 0..16 {
        let response = request(transport, ManagementCommand::GetHistory { index }).await?;
        ensure_ok(response)?;
        if matches!(response.payload, ManagementResponsePayload::None) {
            break;
        }
        print_response(response);
    }
    Ok(())
}

async fn print_settings(transport: &Transport) -> Result<(), Box<dyn Error>> {
    let schema = request(transport, ManagementCommand::GetSchema).await?;
    ensure_ok(schema)?;
    let ManagementResponsePayload::Schema(schema_info) = schema.payload else {
        return Err("schema payload missing".into());
    };
    if schema_info.version != hidshift::SETTINGS_SCHEMA_VERSION
        || schema_info.setting_count as usize != hidshift::SETTING_COUNT
        || schema_info.hash != hidshift::SETTINGS_SCHEMA_HASH
    {
        return Err("firmware settings schema does not match this CLI".into());
    }
    println!(
        "HIDShift settings (firmware {}.{}.{})",
        schema_info.firmware_major, schema_info.firmware_minor, schema_info.firmware_patch
    );
    for descriptor in SETTING_DESCRIPTORS {
        match descriptor.scope {
            SettingScope::Global => print_response(
                request(
                    transport,
                    ManagementCommand::GetSetting {
                        id: descriptor.id,
                        target: SettingTarget::Global,
                    },
                )
                .await?,
            ),
            SettingScope::Host => {
                for slot in 1..=4 {
                    print_response(
                        request(
                            transport,
                            ManagementCommand::GetSetting {
                                id: descriptor.id,
                                target: SettingTarget::Host(hidshift::HostId(slot)),
                            },
                        )
                        .await?,
                    );
                }
            }
        }
    }
    Ok(())
}

fn print_setting_description(key: &str) -> Result<(), Box<dyn Error>> {
    let descriptor = setting_by_key(key).ok_or_else(|| unknown_setting_message(key))?;
    println!("{} ({})", descriptor.label, descriptor.key);
    println!("  {}", descriptor.description);
    println!(
        "  対象: {}",
        if descriptor.scope == SettingScope::Global {
            "本体全体"
        } else {
            "接続先スロット（--slotが必要）"
        }
    );
    if descriptor.choices.is_empty() {
        println!(
            "  値: {}{}〜{}{}（刻み {}{}）",
            descriptor.min,
            descriptor.unit,
            descriptor.max,
            descriptor.unit,
            descriptor.step,
            descriptor.unit
        );
    } else {
        println!("  選択肢:");
        for choice in descriptor.choices {
            println!(
                "    {:<10} {}",
                choice_cli_name(descriptor.id, choice.value),
                choice.label
            );
        }
    }
    println!(
        "  初期値: {}",
        display_setting_value(descriptor, descriptor.default)
    );
    if descriptor.restart_required {
        println!("  注意: 変更はHIDShiftの再起動後に反映されます");
    }
    Ok(())
}

fn display_setting_value(descriptor: &hidshift::SettingDescriptor, value: i32) -> String {
    if descriptor.kind == hidshift::SettingValueKind::Bool {
        return if value == 0 { "オフ" } else { "オン" }.into();
    }
    if let Some(choice) = descriptor
        .choices
        .iter()
        .find(|choice| choice.value == value)
    {
        return format!("{} ({})", choice.label, value);
    }
    format!("{}{}", value, descriptor.unit)
}

fn parse_cli_setting_value(
    descriptor: &hidshift::SettingDescriptor,
    input: &str,
) -> Result<i32, Box<dyn Error>> {
    let normalized = input.trim();
    let value = if descriptor.kind == hidshift::SettingValueKind::Bool {
        match normalized.to_ascii_lowercase().as_str() {
            "on" | "true" | "yes" | "enable" => Some(1),
            "off" | "false" | "no" | "disable" => Some(0),
            _ => normalized.parse().ok(),
        }
    } else if descriptor.kind == hidshift::SettingValueKind::Choice {
        choice_alias(descriptor.id, normalized).or_else(|| normalized.parse().ok())
    } else {
        normalized
            .strip_suffix(descriptor.unit)
            .unwrap_or(normalized)
            .trim()
            .parse()
            .ok()
    };
    value.filter(|value| (descriptor.min..=descriptor.max).contains(value)).ok_or_else(|| {
        format!("'{}' は {} の有効な値ではありません。`settings describe {}` で候補を確認してください", input, descriptor.label, descriptor.key).into()
    })
}

fn choice_alias(id: hidshift::SettingId, input: &str) -> Option<i32> {
    use hidshift::SettingId;
    let input = input.to_ascii_lowercase();
    Some(match (id, input.as_str()) {
        (SettingId::BootTarget, "last") => 0,
        (SettingId::BootTarget, "slot-1" | "slot1") => 1,
        (SettingId::BootTarget, "slot-2" | "slot2") => 2,
        (SettingId::BootTarget, "slot-3" | "slot3") => 3,
        (SettingId::BootTarget, "slot-4" | "slot4") => 4,
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            "none",
        ) => 0,
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            "next",
        ) => 1,
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            "pair",
        ) => 2,
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            "forget",
        ) => 3,
        (SettingId::KeyboardLayout, "raw" | "none") => 0,
        (SettingId::KeyboardLayout, "us") => 1,
        (SettingId::KeyboardLayout, "jis" | "jp") => 2,
        (SettingId::LogLevel, "error") => 0,
        (SettingId::LogLevel, "warn") => 1,
        (SettingId::LogLevel, "info") => 2,
        (SettingId::LogLevel, "debug") => 3,
        (SettingId::LogLevel, "trace") => 4,
        _ => return None,
    })
}

fn choice_cli_name(id: hidshift::SettingId, value: i32) -> &'static str {
    use hidshift::SettingId;
    match (id, value) {
        (SettingId::BootTarget, 0) => "last",
        (SettingId::BootTarget, 1) => "slot-1",
        (SettingId::BootTarget, 2) => "slot-2",
        (SettingId::BootTarget, 3) => "slot-3",
        (SettingId::BootTarget, 4) => "slot-4",
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            0,
        ) => "none",
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            1,
        ) => "next",
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            2,
        ) => "pair",
        (
            SettingId::ButtonShortAction
            | SettingId::ButtonLongAction
            | SettingId::ButtonVeryLongAction,
            3,
        ) => "forget",
        (SettingId::KeyboardLayout, 0) => "raw",
        (SettingId::KeyboardLayout, 1) => "us",
        (SettingId::KeyboardLayout, 2) => "jis",
        (SettingId::LogLevel, 0) => "error",
        (SettingId::LogLevel, 1) => "warn",
        (SettingId::LogLevel, 2) => "info",
        (SettingId::LogLevel, 3) => "debug",
        (SettingId::LogLevel, 4) => "trace",
        _ => "?",
    }
}

fn unknown_setting_message(key: &str) -> String {
    let available = SETTING_DESCRIPTORS
        .iter()
        .map(|item| item.key)
        .collect::<Vec<_>>()
        .join(", ");
    format!("設定 '{key}' はありません。利用可能: {available}")
}

fn setting_command(
    key: &str,
    slot: Option<u8>,
    value: Option<i32>,
) -> Result<ManagementCommand, Box<dyn Error>> {
    let descriptor = setting_by_key(key).ok_or("unknown setting key")?;
    let target = match (descriptor.scope, slot) {
        (SettingScope::Global, None) => SettingTarget::Global,
        (SettingScope::Host, Some(1..=4)) => SettingTarget::Host(hidshift::HostId(slot.unwrap())),
        (SettingScope::Global, Some(_)) => return Err("global setting does not take SLOT".into()),
        (SettingScope::Host, None) => return Err("host setting requires SLOT".into()),
        _ => return Err("slot must be between 1 and 4".into()),
    };
    Ok(match value {
        Some(value) => ManagementCommand::SetSetting {
            id: descriptor.id,
            target,
            value,
        },
        None => ManagementCommand::GetSetting {
            id: descriptor.id,
            target,
        },
    })
}

fn display_target(target: SettingTarget) -> String {
    match target {
        SettingTarget::Global => "global".into(),
        SettingTarget::Host(host) => format!("slot {}", host.0),
    }
}

fn history_kind(kind: u8) -> &'static str {
    match kind {
        1 => "BLE connected",
        2 => "BLE disconnected",
        3 => "USB connected",
        4 => "USB disconnected",
        5 => "target selected",
        6 => "pairing started",
        _ => "event",
    }
}

fn display_host(host: Option<u8>) -> String {
    host.map(|host| host.to_string())
        .unwrap_or_else(|| "-".to_owned())
}

fn display_slot(status: ManagementHostStatus) -> String {
    let mut states = Vec::new();
    if status.known {
        states.push("registered");
    }
    if status.connected {
        states.push("connected");
    }
    if status.encrypted {
        states.push("encrypted");
    }
    if status.bonded {
        states.push("bonded");
    }
    if states.is_empty() {
        "empty".to_owned()
    } else {
        states.join(", ")
    }
}

const fn result_message(result: ManagementResult) -> &'static str {
    match result {
        ManagementResult::Ok => "ok",
        ManagementResult::InvalidHost => "invalid host slot",
        ManagementResult::HostNotFound => "host slot is not registered",
        ManagementResult::HostAlreadyBonded => "host slot is already bonded; forget it first",
        ManagementResult::InternalError => "firmware internal error",
        ManagementResult::InvalidName => "invalid host name",
        ManagementResult::InvalidSetting => "invalid setting, target, or value",
        ManagementResult::NotFound => "requested item was not found",
        ManagementResult::Unavailable => "requested feature is unavailable in this firmware",
    }
}

fn parse_arguments<I>(arguments: I) -> Result<Arguments, Box<dyn Error>>
where
    I: IntoIterator<Item = String>,
{
    let parsed =
        CliArgs::try_parse_from(std::iter::once("hidshiftctl".to_owned()).chain(arguments))?;
    let transport = if let Some(port) = parsed.serial {
        Transport::Serial(port)
    } else if parsed.ble {
        Transport::Ble(parsed.address)
    } else {
        Transport::None
    };
    let command = match parsed.command {
        CommandArgs::Overview => CliCommand::Overview,
        CommandArgs::Status => CliCommand::Request(ManagementCommand::GetStatus),
        CommandArgs::Devices => CliCommand::Devices,
        CommandArgs::Diagnostics => CliCommand::Diagnostics,
        CommandArgs::History => CliCommand::History,
        CommandArgs::Select { slot } => {
            CliCommand::Request(ManagementCommand::SelectHost(hidshift::HostId(slot)))
        }
        CommandArgs::Target { command } => match command {
            TargetArgs::Usb => CliCommand::Request(ManagementCommand::SelectOutputTarget(
                ManagementOutputTarget::Wired,
            )),
            TargetArgs::Ble { slot } => CliCommand::Request(ManagementCommand::SelectOutputTarget(
                ManagementOutputTarget::Ble(hidshift::HostId(slot)),
            )),
            TargetArgs::Status => CliCommand::Request(ManagementCommand::GetOutputTargetStatus),
        },
        CommandArgs::Mirror { command } => match command {
            MirrorArgs::List => CliCommand::MirrorList,
            MirrorArgs::Select { candidate } => CliCommand::Request(
                ManagementCommand::SetMirrorTarget(MirrorCandidateId(candidate)),
            ),
            MirrorArgs::Clear => CliCommand::Request(ManagementCommand::ClearMirrorTarget),
            MirrorArgs::Status => CliCommand::Request(ManagementCommand::GetOutputTargetStatus),
        },
        CommandArgs::Pair { slot } => {
            CliCommand::Request(ManagementCommand::StartPairing(hidshift::HostId(slot)))
        }
        CommandArgs::PairCancel => CliCommand::Request(ManagementCommand::CancelPairing),
        CommandArgs::Forget { slot } => {
            CliCommand::Request(ManagementCommand::ForgetHost(hidshift::HostId(slot)))
        }
        CommandArgs::Info { slot } => {
            CliCommand::Request(ManagementCommand::GetHostInfo(hidshift::HostId(slot)))
        }
        CommandArgs::Timing { slot } => {
            CliCommand::Request(ManagementCommand::GetHostTiming(hidshift::HostId(slot)))
        }
        CommandArgs::Name { slot, name } => CliCommand::Request(ManagementCommand::SetHostName {
            host_id: hidshift::HostId(slot),
            name: hidshift::ManagementHostName::from_ascii(&name)
                .map_err(|_| "名前は半角12文字以内で入力してください")?,
        }),
        CommandArgs::Settings { command } => match command {
            SettingsArgs::List => CliCommand::SettingsList,
            SettingsArgs::Describe { key } => CliCommand::SettingDescribe { key },
            SettingsArgs::Get { key, slot } => CliCommand::SettingGet { key, slot },
            SettingsArgs::Set { key, value, slot } => {
                let descriptor =
                    setting_by_key(&key).ok_or_else(|| unknown_setting_message(&key))?;
                CliCommand::SettingSet {
                    key,
                    slot,
                    value: parse_cli_setting_value(descriptor, &value)?,
                }
            }
        },
    };
    Ok(Arguments { transport, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serial_and_ble_commands() {
        assert_eq!(
            parse_arguments(["--serial", "/dev/ttyUSB0", "select", "3"].map(str::to_owned))
                .unwrap(),
            Arguments {
                transport: Transport::Serial("/dev/ttyUSB0".to_owned()),
                command: CliCommand::Request(ManagementCommand::SelectHost(hidshift::HostId(3))),
            }
        );
        assert_eq!(
            parse_arguments(["--ble", "status"].map(str::to_owned)).unwrap(),
            Arguments {
                transport: Transport::Ble(None),
                command: CliCommand::Request(ManagementCommand::GetStatus),
            }
        );
        assert_eq!(
            parse_arguments(["--ble", "name", "2", "Work PC"].map(str::to_owned)).unwrap(),
            Arguments {
                transport: Transport::Ble(None),
                command: CliCommand::Request(ManagementCommand::SetHostName {
                    host_id: hidshift::HostId(2),
                    name: hidshift::ManagementHostName::from_ascii("Work PC").unwrap(),
                }),
            }
        );
    }

    #[test]
    fn parses_wired_and_ble_output_target_commands() {
        assert_eq!(
            parse_arguments(["--serial", "/dev/ttyACM0", "target", "usb"].map(str::to_owned))
                .unwrap()
                .command,
            CliCommand::Request(ManagementCommand::SelectOutputTarget(
                ManagementOutputTarget::Wired
            ))
        );
        assert_eq!(
            parse_arguments(["--ble", "target", "ble", "4"].map(str::to_owned))
                .unwrap()
                .command,
            CliCommand::Request(ManagementCommand::SelectOutputTarget(
                ManagementOutputTarget::Ble(hidshift::HostId(4))
            ))
        );
        assert_eq!(
            parse_arguments(["--ble", "target", "status"].map(str::to_owned))
                .unwrap()
                .command,
            CliCommand::Request(ManagementCommand::GetOutputTargetStatus)
        );
        assert_eq!(
            parse_arguments(["--ble", "mirror", "list"].map(str::to_owned))
                .unwrap()
                .command,
            CliCommand::MirrorList
        );
    }

    #[test]
    fn rejects_invalid_cli_slots_and_extra_arguments() {
        assert!(
            parse_arguments(["--serial", "/dev/ttyUSB0", "select", "0"].map(str::to_owned))
                .is_err()
        );
        assert!(parse_arguments(["--ble", "pair", "5"].map(str::to_owned)).is_err());
        assert!(parse_arguments(["--ble", "status", "extra"].map(str::to_owned)).is_err());
    }

    #[test]
    fn parses_named_setting_values_and_named_slot_option() {
        assert_eq!(
            parse_arguments(
                [
                    "--serial",
                    "/dev/ttyACM0",
                    "settings",
                    "set",
                    "keyboard_layout",
                    "jis",
                    "--slot",
                    "2",
                ]
                .map(str::to_owned)
            )
            .unwrap()
            .command,
            CliCommand::SettingSet {
                key: "keyboard_layout".into(),
                slot: Some(2),
                value: 2,
            }
        );
        assert_eq!(
            parse_cli_setting_value(setting_by_key("auto_reconnect").unwrap(), "off").unwrap(),
            0
        );
        assert_eq!(
            parse_cli_setting_value(setting_by_key("mouse_sensitivity_percent").unwrap(), "125%")
                .unwrap(),
            125
        );
    }

    #[test]
    fn setting_value_errors_point_to_describe_command() {
        let error =
            parse_cli_setting_value(setting_by_key("mouse_sensitivity_percent").unwrap(), "500%")
                .unwrap_err()
                .to_string();
        assert!(error.contains("settings describe mouse_sensitivity_percent"));
    }

    #[test]
    fn setting_description_does_not_require_a_device_transport() {
        assert_eq!(
            parse_arguments(["settings", "describe", "keyboard_layout"].map(str::to_owned))
                .unwrap(),
            Arguments {
                transport: Transport::None,
                command: CliCommand::SettingDescribe {
                    key: "keyboard_layout".into(),
                },
            }
        );
    }

    #[test]
    fn serial_log_reports_brownout_instead_of_a_generic_timeout() {
        assert_eq!(
            serial_boot_diagnostic(b"rst:0xf (BROWNOUT_RST),boot:0x2b"),
            Some(
                "HIDShiftが電圧低下（brownout）で再起動しています。電源容量、USBケーブル、USB機器への給電を確認してください"
            )
        );
        assert_eq!(serial_boot_diagnostic(b"normal management response"), None);
    }
}
