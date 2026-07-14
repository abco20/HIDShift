use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use hidshift::HostId;
use hidshift::e2e::{E2eCommand, E2eInputLane, E2ePacket};
use hidshift::espnow_pairing::EspNowRole;
use hidshift::management::{
    ManagementCommand, ManagementRequest, ManagementResponse, ManagementResponsePayload,
    ManagementResult,
};
use serde::{Deserialize, Serialize};
use serialport::{FlowControl, SerialPort};

mod board;
use board::{assign_bridge_roles, parse_chip_type, serial_by_path_candidates};
mod metrics;
use metrics::{
    BaselineComparison, LatencyStats, PerformanceBaseline, ble_game_latency_passes,
    bridge_game_latency_passes, compare_baseline, latency_advisory, latency_stats, merge_latency,
};

const DUT_BAUD_RATE: u32 = 115_200;
const PROBE_BAUD_RATE: u32 = 115_200;
const DUT_CHIP: &str = "esp32s3";
const PROBE_CHIP: &str = "esp32";
const BRIDGE_ESPNOW_TX_P95_MAX_MS: f64 = 15.0;
// This functional fault-injection timer starts before the 115200-baud UART
// command reaches Host ingress. The latency gate below uses synchronized Host
// ingress timestamps and must not be conflated with this recovery timeout.
const BRIDGE_IDLE_RECOVERY_MAX_MS: f64 = 50.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Mode {
    Ble,
    Bridge,
    Coexist,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BridgeProvisioningStage {
    Flash,
    EspNowPair,
    HostReady,
    LinuxAdvertising,
    LinuxPair,
    LinuxLink,
}

impl BridgeProvisioningStage {
    fn announce(self) {
        println!("provisioning: {self:?}");
    }
}

#[derive(Parser, Debug)]
#[command(about = "HIDShift two-ESP hardware E2E runner")]
struct Args {
    #[arg(long, value_enum, default_value_t = Mode::Ble)]
    mode: Mode,
    #[arg(long)]
    dut_port: Option<PathBuf>,
    #[arg(long)]
    probe_port: Option<PathBuf>,
    #[arg(long)]
    host_port: Option<PathBuf>,
    #[arg(long)]
    device_port: Option<PathBuf>,
    #[arg(long)]
    skip_flash: bool,
    #[arg(long)]
    skip_linux: bool,
    /// Ask the running bridge Device to enter its ROM USB download loader.
    #[arg(long)]
    enter_device_download: bool,
    #[arg(long, default_value_t = 6)]
    espnow_channel: u8,
    #[arg(long, default_value_t = 500)]
    latency_samples: usize,
    #[arg(long, default_value_t = 120)]
    stability_seconds: u64,
    #[arg(long, default_value = "e2e/baseline.json")]
    baseline: PathBuf,
    #[arg(long)]
    write_baseline: bool,
    #[arg(long, default_value = "e2e/results")]
    results_dir: PathBuf,
    #[arg(long, default_value = "e2e/bridge-baseline.json")]
    bridge_baseline: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StabilityStats {
    duration_seconds: f64,
    reports_sent: u64,
    reports_received: u64,
    mismatches: u64,
    timeouts: u64,
    probe_sequence_gaps: u64,
    last_timeout: Option<String>,
    dut_inputs: u32,
    dut_ble_queued: u32,
    dut_notify_done: u32,
    dut_counter_reset: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u8,
    unix_time_seconds: u64,
    dut_port: String,
    probe_port: String,
    tests: Vec<TestResult>,
    /// Probe-clock-synchronized latency. This excludes the probe's UART
    /// formatting and USB-serial delivery delay.
    latency: LatencyStats,
    /// Wall-clock latency observed when the complete probe line reaches Linux.
    host_observed_latency: LatencyStats,
    pipeline_latency: PipelineLatencyStats,
    mouse_latency: LatencyStats,
    mouse_host_observed_latency: LatencyStats,
    mouse_pipeline_latency: PipelineLatencyStats,
    clock_sync: ClockSyncStats,
    stability: StabilityStats,
    baseline_comparison: Option<BaselineComparison>,
    experiential_latency_advisory: String,
}

#[derive(Debug, Serialize)]
struct TestResult {
    name: String,
    passed: bool,
    detail: String,
}

#[derive(Clone, Debug, Serialize)]
struct ClockSyncStats {
    samples_per_device: usize,
    probe_best_round_trip_ms: f64,
    dut_best_round_trip_ms: f64,
    combined_uncertainty_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct PipelineLatencyStats {
    ingress_to_runtime: LatencyStats,
    runtime_processing: LatencyStats,
    runtime_queueing: LatencyStats,
    ble_queue_to_receive: LatencyStats,
    ble_dispatch: LatencyStats,
    notify_call: LatencyStats,
    notify_done_to_hci_dequeue: LatencyStats,
    hci_dequeue_to_credit: LatencyStats,
    hci_credit_to_submit: LatencyStats,
    notify_done_to_hci_submit: LatencyStats,
    hci_submit_to_probe: LatencyStats,
    notify_done_to_probe: LatencyStats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct BleLinkTelemetry {
    connected: bool,
    connection_interval_us: u32,
    peripheral_latency: u16,
    supervision_timeout_ms: u32,
    tx_phy: u8,
    rx_phy: u8,
    parameter_updates: u32,
    phy_updates: u32,
}

struct LatencyMeasurement {
    end_to_end: LatencyStats,
    host_observed: LatencyStats,
    pipeline: PipelineLatencyStats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DutInputTimestamps {
    query_sequence: u32,
    input_sequence: u32,
    ingress_us: u64,
    runtime_us: u64,
    runtime_dispatch_us: u64,
    ble_queued_us: u64,
    ble_receive_us: u64,
    notify_start_us: u64,
    notify_done_us: u64,
    hci_submit_us: u64,
    hci_dequeue_us: u64,
    hci_credit_us: u64,
    input_count: u32,
    ble_queued_count: u32,
    notify_done_count: u32,
    ble_link: BleLinkTelemetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Source {
    Dut,
    Probe,
}

#[derive(Debug)]
struct SerialLine {
    source: Source,
    received: Instant,
    text: String,
}

#[derive(Debug, Eq, PartialEq)]
struct ProbeNotification {
    sequence: Option<u32>,
    kind: String,
    probe_us: Option<u64>,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct DeviceClockSync {
    host_anchor: Instant,
    probe_anchor_us: u64,
    round_trip: Duration,
}

#[derive(Clone, Copy, Debug)]
struct BridgeClockSync {
    clock: DeviceClockSync,
    host_session_id: u32,
    device_session_id: u32,
}

struct Harness {
    writer: Box<dyn SerialPort>,
    probe_writer: Box<dyn SerialPort>,
    lines: Receiver<SerialLine>,
    sequence: u32,
    probe_diagnostics: Mutex<ProbeDiagnostics>,
}

#[derive(Default)]
struct ProbeDiagnostics {
    last_sequence: Option<u32>,
    gaps: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("runner must live below the repository root")?
        .to_path_buf();
    if matches!(args.mode, Mode::Bridge | Mode::Coexist) {
        return run_bridge(&args, &repo);
    }
    let (dut, probe) = resolve_ports(&args, &repo)?;

    println!("DUT   {} ({DUT_CHIP})", dut.display());
    println!("Probe {} ({PROBE_CHIP})", probe.display());
    if !args.skip_flash {
        build_and_flash(&repo, &dut, &probe)?;
    } else {
        let _ = bluetoothctl(&["power", "off"], 10);
    }

    let mut harness = open_harness(&dut, &probe)?;
    // The DUT can finish booting while the probe image is still being built,
    // so its one-shot READY log is not a reliable synchronization point. The
    // sequence-tagged Hello acknowledgement below proves the live UART path.
    let (hello_sequence, _) = harness.send(E2eCommand::Hello)?;
    harness.wait_dut_sequence("QUEUED", hello_sequence, Duration::from_secs(3))?;
    if !args.skip_flash {
        harness.wait_marker(
            Source::Probe,
            "@HIDSHIFT-PROBE:SUBSCRIBED",
            Duration::from_secs(35),
        )?;
    }
    // Fresh pairing performs a planned BLE restart for the critical bond
    // write. A restored bond does not. Wait past that window, then prove the
    // current subscription with a real report instead of inferring state from
    // boot logs that may still be buffered by a USB/UART bridge.
    drain_for(&harness.lines, Duration::from_secs(7));
    harness.wait_transport_ready(Duration::from_secs(45))?;

    let mut tests = run_functional_raw_tests(&mut harness)?;
    let clock_sync = synchronize_probe_clock(&mut harness, 20)?;
    let dut_clock_sync = synchronize_dut_clock(&mut harness, 20)?;
    let measurement = run_latency_test(
        &mut harness,
        args.latency_samples,
        clock_sync,
        dut_clock_sync,
    )?;
    let mouse_measurement = run_mouse_latency_test(
        &mut harness,
        args.latency_samples,
        clock_sync,
        dut_clock_sync,
    )?;
    let latency = measurement.end_to_end;
    let host_observed_latency = measurement.host_observed;
    let stability = run_stability_test(&mut harness, Duration::from_secs(args.stability_seconds))?;

    if args.skip_linux {
        tests.push(TestResult {
            name: "linux_evdev".into(),
            passed: true,
            detail: "skipped by --skip-linux".into(),
        });
    } else {
        tests.extend(run_linux_evdev_test(&mut harness)?);
    }

    let baseline = read_baseline(&repo.join(&args.baseline))?;
    let baseline_comparison =
        baseline.map(|baseline| compare_baseline(&baseline.keyboard, &latency));
    if let Some(comparison) = &baseline_comparison {
        tests.push(TestResult {
            name: "latency_baseline_regression".into(),
            passed: comparison.passed,
            detail: format!(
                "p95 {:.3} ms -> {:.3} ms ({:+.1}%)",
                comparison.baseline_p95_ms, comparison.current_p95_ms, comparison.change_percent
            ),
        });
    }
    tests.push(TestResult {
        name: "short_stability".into(),
        passed: stability.timeouts == 0 && stability.mismatches == 0,
        detail: format!(
            "{}/{} reports, mismatches={}, timeouts={}",
            stability.reports_received,
            stability.reports_sent,
            stability.mismatches,
            stability.timeouts
        ),
    });

    tests.push(TestResult {
        name: "mouse_latency_target".into(),
        passed: mouse_measurement.end_to_end.p95_ms <= 20.0
            && mouse_measurement.end_to_end.p99_ms <= 25.0,
        detail: format!(
            "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
            mouse_measurement.end_to_end.p50_ms,
            mouse_measurement.end_to_end.p95_ms,
            mouse_measurement.end_to_end.p99_ms
        ),
    });

    let advisory = latency_advisory(if mouse_measurement.end_to_end.p95_ms > latency.p95_ms {
        &mouse_measurement.end_to_end
    } else {
        &latency
    })
    .to_owned();
    let report = Report {
        schema_version: 2,
        unix_time_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        dut_port: dut.display().to_string(),
        probe_port: probe.display().to_string(),
        tests,
        latency,
        host_observed_latency,
        pipeline_latency: measurement.pipeline,
        mouse_latency: mouse_measurement.end_to_end,
        mouse_host_observed_latency: mouse_measurement.host_observed,
        mouse_pipeline_latency: mouse_measurement.pipeline,
        clock_sync: ClockSyncStats {
            samples_per_device: 20,
            probe_best_round_trip_ms: clock_sync.round_trip.as_secs_f64() * 1_000.0,
            dut_best_round_trip_ms: dut_clock_sync.round_trip.as_secs_f64() * 1_000.0,
            combined_uncertainty_ms: (clock_sync.round_trip + dut_clock_sync.round_trip)
                .as_secs_f64()
                * 500.0,
        },
        stability,
        baseline_comparison,
        experiential_latency_advisory: advisory,
    };

    let results_dir = repo.join(&args.results_dir);
    fs::create_dir_all(&results_dir)?;
    let result_path = results_dir.join(format!("{}.json", report.unix_time_seconds));
    fs::write(&result_path, serde_json::to_vec_pretty(&report)?)?;
    if args.write_baseline {
        let baseline_path = repo.join(&args.baseline);
        if let Some(parent) = baseline_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &baseline_path,
            serde_json::to_vec_pretty(&PerformanceBaseline {
                schema_version: 2,
                metric: "dut_ingress_to_probe_clock_synchronized".into(),
                keyboard: report.latency.clone(),
                mouse: report.mouse_latency.clone(),
            })?,
        )?;
    }

    println!("\n{}", serde_json::to_string_pretty(&report)?);
    println!("result: {}", result_path.display());
    ensure!(
        report.tests.iter().all(|test| test.passed),
        "one or more E2E tests failed"
    );
    Ok(())
}

#[derive(Debug, Serialize)]
struct BridgeReport {
    schema_version: u8,
    mode: &'static str,
    unix_time_seconds: u64,
    host_port: String,
    device_flash_port: String,
    tests: Vec<TestResult>,
    keyboard_latency: LatencyStats,
    mouse_latency: LatencyStats,
    keyboard_espnow_tx: LatencyStats,
    mouse_espnow_tx: LatencyStats,
    keyboard_host_pipeline: BridgeHostPipelineStats,
    mouse_host_pipeline: BridgeHostPipelineStats,
    keyboard_device_pipeline: BridgeDevicePipelineStats,
    mouse_device_pipeline: BridgeDevicePipelineStats,
    host_ingress_to_espnow_tx: LatencyStats,
    stability: BridgeStabilityStats,
    ble_peer_address: Option<String>,
    ble_link: Option<BleLinkTelemetry>,
    ble_keyboard_latency: Option<LatencyStats>,
    ble_mouse_latency: Option<LatencyStats>,
    ble_keyboard_host_observed_latency: Option<LatencyStats>,
    ble_mouse_host_observed_latency: Option<LatencyStats>,
    ble_keyboard_pipeline: Option<PipelineLatencyStats>,
    ble_mouse_pipeline: Option<PipelineLatencyStats>,
    baseline_keyboard: Option<BaselineComparison>,
    baseline_mouse: Option<BaselineComparison>,
}

struct CoexistConnection {
    address: String,
    evdev_paths: Vec<PathBuf>,
}

struct CoexistBleResult {
    address: String,
    link: BleLinkTelemetry,
    keyboard: LatencyMeasurement,
    mouse: LatencyMeasurement,
    tests: Vec<TestResult>,
}

#[derive(Debug, Serialize)]
struct BridgeHostPipelineStats {
    ingress_to_enqueue: LatencyStats,
    enqueue_to_dequeue: LatencyStats,
    dequeue_to_send_start: LatencyStats,
    send_start_to_callback: LatencyStats,
}

impl BridgeHostPipelineStats {
    fn zero() -> Self {
        let zero = latency_stats(vec![0.0]);
        Self {
            ingress_to_enqueue: zero.clone(),
            enqueue_to_dequeue: zero.clone(),
            dequeue_to_send_start: zero.clone(),
            send_start_to_callback: zero,
        }
    }
}

struct BridgeHostPipelineSamples {
    ingress_to_enqueue: Vec<f64>,
    enqueue_to_dequeue: Vec<f64>,
    dequeue_to_send_start: Vec<f64>,
    send_start_to_callback: Vec<f64>,
}

impl BridgeHostPipelineSamples {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            ingress_to_enqueue: Vec::with_capacity(capacity),
            enqueue_to_dequeue: Vec::with_capacity(capacity),
            dequeue_to_send_start: Vec::with_capacity(capacity),
            send_start_to_callback: Vec::with_capacity(capacity),
        }
    }

    fn finish(self) -> BridgeHostPipelineStats {
        BridgeHostPipelineStats {
            ingress_to_enqueue: latency_stats(self.ingress_to_enqueue),
            enqueue_to_dequeue: latency_stats(self.enqueue_to_dequeue),
            dequeue_to_send_start: latency_stats(self.dequeue_to_send_start),
            send_start_to_callback: latency_stats(self.send_start_to_callback),
        }
    }
}

#[derive(Debug, Serialize)]
struct BridgeDevicePipelineStats {
    radio_rx_to_reassembly: LatencyStats,
    reassembly_to_hid_write: LatencyStats,
    radio_rx_to_hid_write: LatencyStats,
}

impl BridgeDevicePipelineStats {
    fn zero() -> Self {
        let zero = latency_stats(vec![0.0]);
        Self {
            radio_rx_to_reassembly: zero.clone(),
            reassembly_to_hid_write: zero.clone(),
            radio_rx_to_hid_write: zero,
        }
    }
}

struct BridgeDevicePipelineSamples {
    radio_rx_to_reassembly: Vec<f64>,
    reassembly_to_hid_write: Vec<f64>,
    radio_rx_to_hid_write: Vec<f64>,
}

impl BridgeDevicePipelineSamples {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            radio_rx_to_reassembly: Vec::with_capacity(capacity),
            reassembly_to_hid_write: Vec::with_capacity(capacity),
            radio_rx_to_hid_write: Vec::with_capacity(capacity),
        }
    }

    fn finish(self) -> BridgeDevicePipelineStats {
        BridgeDevicePipelineStats {
            radio_rx_to_reassembly: latency_stats(self.radio_rx_to_reassembly),
            reassembly_to_hid_write: latency_stats(self.reassembly_to_hid_write),
            radio_rx_to_hid_write: latency_stats(self.radio_rx_to_hid_write),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct BridgeStabilityStats {
    duration_seconds: f64,
    reports_sent: u64,
    reports_received: u64,
    timeouts: u64,
    host_session_before: u32,
    host_session_after: u32,
    device_session_before: u32,
    device_session_after: u32,
}

struct BridgeHarness {
    writer: Box<dyn SerialPort>,
    lines: Receiver<SerialLine>,
    sequence: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BridgeTimestamp {
    host_session_id: u32,
    input_sequence: u32,
    ingress_us: u64,
    enqueue_us: u64,
    dequeue_us: u64,
    send_start_us: u64,
    tx_done_us: u64,
    device_sequence: u32,
    device_radio_rx_us: u64,
    device_reassembled_us: u64,
    device_hid_write_us: u64,
}

fn run_bridge(args: &Args, repo: &Path) -> Result<()> {
    let coexist = args.mode == Mode::Coexist;
    let (host, device) = resolve_bridge_ports(args, repo)?;
    println!("Host   {} (ESP32-S3, keyboard/mouse side)", host.display());
    println!("Device {} (ESP32-S3, PC USB HID side)", device.display());
    if args.enter_device_download {
        ensure!(
            args.skip_flash,
            "--enter-device-download requires --skip-flash"
        );
        let mut harness = open_bridge_harness(&host)?;
        let (sequence, _) = harness.send(E2eCommand::EnterDeviceDownload)?;
        harness.wait_dut_sequence("QUEUED", sequence, Duration::from_secs(3))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !device.exists() {
            thread::sleep(Duration::from_millis(100));
        }
        ensure!(device.exists(), "Device USB-JTAG port did not appear");
        println!("Device entered ROM download mode: {}", device.display());
        return Ok(());
    }
    if !args.skip_flash {
        BridgeProvisioningStage::Flash.announce();
        if coexist {
            build_and_flash_coexist(repo, &host, &device)?;
        } else {
            build_and_flash_bridge(repo, &host, &device)?;
        }
        BridgeProvisioningStage::EspNowPair.announce();
        pair_bridge_boards(repo, &host, &device, args.espnow_channel)?;
    }
    if coexist && !args.skip_linux && !args.skip_flash {
        remove_cached_hidshift_devices();
    }
    if coexist && !args.skip_linux {
        bluetoothctl(&["power", "on"], 10)?;
    }

    let mut harness = open_bridge_harness(&host)?;
    let mut hello_ready = false;
    for _ in 0..10 {
        let hello = match harness.send(E2eCommand::Hello) {
            Ok((sequence, _)) => sequence,
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        match harness.wait_dut_sequence("QUEUED", hello, Duration::from_millis(750)) {
            Ok(_) => {
                hello_ready = true;
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
    ensure!(hello_ready, "bridge Host did not acknowledge E2E hello");
    BridgeProvisioningStage::HostReady.announce();
    let mut host_clock = synchronize_bridge_host_clock(&mut harness, 20)
        .context("bridge Host clock synchronization")?;
    let coexist_connection = if coexist && !args.skip_linux {
        BridgeProvisioningStage::LinuxAdvertising.announce();
        let connection = connect_coexist_linux(&mut harness)?;
        // Persisting a new BLE bond may intentionally restart the Host BLE
        // controller. Re-synchronize after LinuxLink so the subsequent
        // latency phase treats that planned reboot as the current session.
        host_clock = synchronize_bridge_host_clock(&mut harness, 20)
            .context("bridge Host clock re-synchronization after Linux pairing")?;
        Some(connection)
    } else {
        None
    };
    if coexist {
        select_bridge_transport(&mut harness, hidshift::InputTransport::EspNow)?;
    }
    let paths = if args.skip_linux {
        Vec::new()
    } else {
        find_evdevs("ESP-NOW HID Bridge", Duration::from_secs(20))?
    };
    let mut inputs = paths
        .iter()
        .map(|path| open_nonblocking(path))
        .collect::<Result<Vec<_>>>()?;
    for input in &mut inputs {
        drain_file(input);
    }

    let mut tests = Vec::new();
    if args.skip_linux {
        tests.push(TestResult {
            name: "bridge_linux_evdev".into(),
            passed: true,
            detail: "skipped by --skip-linux".into(),
        });
    } else {
        let hidraw_path = find_hidraw("ESP-NOW HID Bridge", Duration::from_secs(10))?;
        let mut hidraw = open_hidraw(&hidraw_path)?;
        tests.extend(run_bridge_functional_tests(
            &mut harness,
            &mut inputs,
            paths.len(),
            &mut hidraw,
        )?);
    }
    for input in &mut inputs {
        drain_file(input);
    }
    thread::sleep(Duration::from_millis(100));

    let (
        keyboard_latency,
        mouse_latency,
        keyboard_tx,
        mouse_tx,
        keyboard_host_pipeline,
        mouse_host_pipeline,
        keyboard_device_pipeline,
        mouse_device_pipeline,
    ) = if args.skip_linux {
        (
            latency_stats(vec![0.0]),
            latency_stats(vec![0.0]),
            latency_stats(vec![0.0]),
            latency_stats(vec![0.0]),
            BridgeHostPipelineStats::zero(),
            BridgeHostPipelineStats::zero(),
            BridgeDevicePipelineStats::zero(),
            BridgeDevicePipelineStats::zero(),
        )
    } else {
        run_bridge_interleaved_latency(&mut harness, &mut inputs, host_clock, args.latency_samples)?
    };
    let tx_latency = merge_latency(&keyboard_tx, &mouse_tx);
    let stability = if args.skip_linux {
        BridgeStabilityStats {
            duration_seconds: 0.0,
            reports_sent: 0,
            reports_received: 0,
            timeouts: 0,
            host_session_before: host_clock.host_session_id,
            host_session_after: host_clock.host_session_id,
            device_session_before: host_clock.device_session_id,
            device_session_after: host_clock.device_session_id,
        }
    } else {
        run_bridge_stability(
            &mut harness,
            &mut inputs,
            Duration::from_secs(args.stability_seconds),
            host_clock,
        )?
    };

    tests.push(TestResult {
        name: "bridge_keyboard_game_latency".into(),
        passed: args.skip_linux || bridge_game_latency_passes(&keyboard_latency),
        detail: format!(
            "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
            keyboard_latency.p50_ms, keyboard_latency.p95_ms, keyboard_latency.p99_ms
        ),
    });
    tests.push(TestResult {
        name: "bridge_mouse_game_latency".into(),
        passed: args.skip_linux || bridge_game_latency_passes(&mouse_latency),
        detail: format!(
            "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
            mouse_latency.p50_ms, mouse_latency.p95_ms, mouse_latency.p99_ms
        ),
    });
    tests.push(TestResult {
        name: "espnow_tx_latency".into(),
        passed: args.skip_linux || tx_latency.p95_ms <= BRIDGE_ESPNOW_TX_P95_MAX_MS,
        detail: format!(
            "Host ingress through ESP-NOW send callback p95={:.3} ms p99={:.3} ms",
            tx_latency.p95_ms, tx_latency.p99_ms
        ),
    });
    for (name, observed) in [
        (
            "bridge_keyboard_device_telemetry_coverage",
            keyboard_device_pipeline.radio_rx_to_hid_write.samples,
        ),
        (
            "bridge_mouse_device_telemetry_coverage",
            mouse_device_pipeline.radio_rx_to_hid_write.samples,
        ),
    ] {
        let required = bridge_device_telemetry_samples_required(args.latency_samples);
        tests.push(TestResult {
            name: name.into(),
            passed: args.skip_linux || observed >= required,
            detail: format!("{observed} sampled Device stages, required at least {required}"),
        });
    }
    tests.push(TestResult {
        name: "bridge_stability".into(),
        passed: bridge_stability_passes(&stability),
        detail: format!(
            "{}/{} reports, timeouts={}, Host session {}->{}, Device session {}->{}",
            stability.reports_received,
            stability.reports_sent,
            stability.timeouts,
            stability.host_session_before,
            stability.host_session_after,
            stability.device_session_before,
            stability.device_session_after
        ),
    });

    let mut coexist_ble = if let Some(connection) = coexist_connection {
        Some(run_coexist_ble_tests(
            &mut harness,
            &mut inputs,
            connection,
            host_clock,
            args.latency_samples,
        )?)
    } else {
        None
    };
    if let Some(result) = &mut coexist_ble {
        tests.append(&mut result.tests);
    } else if coexist {
        tests.push(TestResult {
            name: "coexist_ble".into(),
            passed: args.skip_linux,
            detail: "skipped by --skip-linux".into(),
        });
    }

    let baseline = read_baseline(&repo.join(&args.bridge_baseline))?;
    let baseline_keyboard = baseline
        .as_ref()
        .map(|baseline| compare_baseline(&baseline.keyboard, &keyboard_latency));
    let baseline_mouse = baseline
        .as_ref()
        .map(|baseline| compare_baseline(&baseline.mouse, &mouse_latency));
    let report = BridgeReport {
        schema_version: 3,
        mode: if coexist {
            "espnow-ble-coexist"
        } else {
            "espnow-usb-bridge"
        },
        unix_time_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        host_port: host.display().to_string(),
        device_flash_port: device.display().to_string(),
        tests,
        keyboard_latency,
        mouse_latency,
        keyboard_espnow_tx: keyboard_tx,
        mouse_espnow_tx: mouse_tx,
        keyboard_host_pipeline,
        mouse_host_pipeline,
        keyboard_device_pipeline,
        mouse_device_pipeline,
        host_ingress_to_espnow_tx: tx_latency,
        stability,
        ble_peer_address: coexist_ble.as_ref().map(|result| result.address.clone()),
        ble_link: coexist_ble.as_ref().map(|result| result.link),
        ble_keyboard_latency: coexist_ble
            .as_ref()
            .map(|result| result.keyboard.end_to_end.clone()),
        ble_mouse_latency: coexist_ble
            .as_ref()
            .map(|result| result.mouse.end_to_end.clone()),
        ble_keyboard_host_observed_latency: coexist_ble
            .as_ref()
            .map(|result| result.keyboard.host_observed.clone()),
        ble_mouse_host_observed_latency: coexist_ble
            .as_ref()
            .map(|result| result.mouse.host_observed.clone()),
        ble_keyboard_pipeline: coexist_ble
            .as_ref()
            .map(|result| result.keyboard.pipeline.clone()),
        ble_mouse_pipeline: coexist_ble
            .as_ref()
            .map(|result| result.mouse.pipeline.clone()),
        baseline_keyboard,
        baseline_mouse,
    };
    let results_dir = repo.join(&args.results_dir);
    fs::create_dir_all(&results_dir)?;
    let result_path = results_dir.join(format!(
        "{}-{}.json",
        if coexist { "coexist" } else { "bridge" },
        report.unix_time_seconds
    ));
    fs::write(&result_path, serde_json::to_vec_pretty(&report)?)?;
    if args.write_baseline {
        let baseline_path = repo.join(&args.bridge_baseline);
        fs::write(
            &baseline_path,
            serde_json::to_vec_pretty(&PerformanceBaseline {
                schema_version: 2,
                metric: "host_ingress_to_linux_evdev_espnow_usb".into(),
                keyboard: report.keyboard_latency.clone(),
                mouse: report.mouse_latency.clone(),
            })?,
        )?;
    }
    println!("\n{}", serde_json::to_string_pretty(&report)?);
    println!("result: {}", result_path.display());
    ensure!(
        report.tests.iter().all(|test| test.passed),
        "one or more bridge E2E tests failed"
    );
    Ok(())
}

fn select_bridge_transport(
    harness: &mut BridgeHarness,
    transport: hidshift::InputTransport,
) -> Result<()> {
    let (sequence, _) = harness.send(E2eCommand::SelectTransport { transport })?;
    harness.wait_dut_sequence("QUEUED", sequence, Duration::from_secs(3))?;
    Ok(())
}

fn connect_coexist_linux(harness: &mut BridgeHarness) -> Result<CoexistConnection> {
    let address = match discover_hidshift_address() {
        Ok(address) => address,
        Err(discovery_error) => {
            // An unbonded Host may advertise only while its explicit pairing
            // window is open. Open that window before giving up on a fresh
            // controller scan; this is also recoverable after a BLE reboot.
            println!(
                "provisioning: LinuxAdvertising did not find HIDShift ({discovery_error}); opening pairing window"
            );
            start_linux_pairing(harness, HostId(1))?;
            discover_hidshift_address().context("discover HIDShift after opening pairing window")?
        }
    };
    let cached_bond = bluetoothctl(&["info", &address], 10)
        .map(|info| cached_hidshift_bond_is_usable(&info))
        .unwrap_or(false);
    if !cached_bond {
        pair_linux_host(&address, || start_linux_pairing(harness, HostId(1)))?;
    }
    bluetoothctl(&["trust", &address], 10)?;

    // Bond persistence intentionally rebuilds the BLE controller. Wait for
    // BlueZ to restore the encrypted link before opening its evdev nodes.
    BridgeProvisioningStage::LinuxLink.announce();
    wait_coexist_linux_link(&address, Duration::from_secs(60))?;
    let evdev_paths = find_evdevs_exact("HIDShift", Duration::from_secs(15))?;
    Ok(CoexistConnection {
        address,
        evdev_paths,
    })
}

fn wait_coexist_linux_link(address: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut connected = false;
    let mut last_info = String::new();
    let mut last_connect_attempt = Instant::now() - Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(info) = bluetoothctl(&["info", &address], 10) {
            if coexist_linux_link_is_ready(&info) {
                connected = true;
                break;
            }
            last_info = info;
        }
        if last_connect_attempt.elapsed() >= Duration::from_secs(5) {
            let _ = bluetoothctl(&["connect", &address], 12);
            last_connect_attempt = Instant::now();
        }
        thread::sleep(Duration::from_millis(500));
    }
    ensure!(
        connected,
        "BlueZ did not restore the coexist HID connection; final info:\n{last_info}"
    );
    Ok(())
}

fn cached_hidshift_bond_is_usable(info: &str) -> bool {
    info.lines().any(|line| line.trim() == "Paired: yes")
        && info.lines().any(|line| line.trim() == "Bonded: yes")
}

fn coexist_linux_link_is_ready(info: &str) -> bool {
    cached_hidshift_bond_is_usable(info) && info.lines().any(|line| line.trim() == "Connected: yes")
}

struct CoexistBleSamples {
    end_to_end: Vec<f64>,
    host_observed: Vec<f64>,
    ingress_to_runtime: Vec<f64>,
    runtime_processing: Vec<f64>,
    runtime_queueing: Vec<f64>,
    ble_queue_to_receive: Vec<f64>,
    ble_dispatch: Vec<f64>,
    notify_call: Vec<f64>,
    notify_done_to_hci_dequeue: Vec<f64>,
    hci_dequeue_to_credit: Vec<f64>,
    hci_credit_to_submit: Vec<f64>,
    notify_done_to_hci_submit: Vec<f64>,
    hci_submit_to_evdev: Vec<f64>,
    notify_done_to_evdev: Vec<f64>,
    link: Option<BleLinkTelemetry>,
}

impl CoexistBleSamples {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            end_to_end: Vec::with_capacity(capacity),
            host_observed: Vec::with_capacity(capacity),
            ingress_to_runtime: Vec::with_capacity(capacity),
            runtime_processing: Vec::with_capacity(capacity),
            runtime_queueing: Vec::with_capacity(capacity),
            ble_queue_to_receive: Vec::with_capacity(capacity),
            ble_dispatch: Vec::with_capacity(capacity),
            notify_call: Vec::with_capacity(capacity),
            notify_done_to_hci_dequeue: Vec::with_capacity(capacity),
            hci_dequeue_to_credit: Vec::with_capacity(capacity),
            hci_credit_to_submit: Vec::with_capacity(capacity),
            notify_done_to_hci_submit: Vec::with_capacity(capacity),
            hci_submit_to_evdev: Vec::with_capacity(capacity),
            notify_done_to_evdev: Vec::with_capacity(capacity),
            link: None,
        }
    }

    fn record(
        &mut self,
        observed: Instant,
        started: Instant,
        host_clock: BridgeClockSync,
        dut: DutInputTimestamps,
    ) -> Result<()> {
        ensure!(
            dut.ingress_us <= dut.runtime_us
                && dut.runtime_us <= dut.runtime_dispatch_us
                && dut.runtime_dispatch_us <= dut.ble_queued_us
                && dut.ble_queued_us <= dut.ble_receive_us
                && dut.ble_receive_us <= dut.notify_start_us
                && dut.notify_start_us <= dut.notify_done_us
                && dut.notify_done_us <= dut.hci_dequeue_us
                && dut.hci_dequeue_us <= dut.hci_credit_us
                && dut.hci_credit_us <= dut.hci_submit_us
                && dut.notify_done_us <= dut.hci_submit_us,
            "coexist BLE pipeline timestamps are incomplete or unordered: {dut:?}"
        );
        ensure!(
            dut.ble_link.connected,
            "BLE link disconnected during latency sample"
        );
        if let Some(previous) = self.link {
            ensure!(
                ble_link_configuration_matches(previous, dut.ble_link),
                "BLE link configuration changed during latency measurement: {previous:?} -> {:?}",
                dut.ble_link
            );
        }
        self.link = Some(dut.ble_link);
        let ingress = host_clock
            .clock
            .host_time(dut.ingress_us)
            .context("coexist BLE ingress timestamp is outside synchronized clock range")?;
        let notify_done = host_clock
            .clock
            .host_time(dut.notify_done_us)
            .context("coexist BLE notify timestamp is outside synchronized clock range")?;
        let hci_submit = host_clock
            .clock
            .host_time(dut.hci_submit_us)
            .context("coexist BLE HCI timestamp is outside synchronized clock range")?;
        self.end_to_end.push(
            host_clock
                .clock
                .duration_since_synced(observed, ingress)
                .context("coexist BLE evdev event preceded DUT ingress")?
                .as_secs_f64()
                * 1_000.0,
        );
        self.host_observed
            .push(observed.duration_since(started).as_secs_f64() * 1_000.0);
        self.ingress_to_runtime
            .push((dut.runtime_us - dut.ingress_us) as f64 / 1_000.0);
        self.runtime_processing
            .push((dut.runtime_dispatch_us - dut.runtime_us) as f64 / 1_000.0);
        self.runtime_queueing
            .push((dut.ble_queued_us - dut.runtime_dispatch_us) as f64 / 1_000.0);
        self.ble_queue_to_receive
            .push((dut.ble_receive_us - dut.ble_queued_us) as f64 / 1_000.0);
        self.ble_dispatch
            .push((dut.notify_start_us - dut.ble_receive_us) as f64 / 1_000.0);
        self.notify_call
            .push((dut.notify_done_us - dut.notify_start_us) as f64 / 1_000.0);
        self.notify_done_to_hci_dequeue
            .push((dut.hci_dequeue_us - dut.notify_done_us) as f64 / 1_000.0);
        self.hci_dequeue_to_credit
            .push((dut.hci_credit_us - dut.hci_dequeue_us) as f64 / 1_000.0);
        self.hci_credit_to_submit
            .push((dut.hci_submit_us - dut.hci_credit_us) as f64 / 1_000.0);
        self.notify_done_to_hci_submit
            .push((dut.hci_submit_us - dut.notify_done_us) as f64 / 1_000.0);
        self.hci_submit_to_evdev.push(
            host_clock
                .clock
                .duration_since_synced(observed, hci_submit)
                .context(
                    "coexist BLE evdev event preceded HCI submission beyond clock uncertainty",
                )?
                .as_secs_f64()
                * 1_000.0,
        );
        self.notify_done_to_evdev.push(
            host_clock
                .clock
                .duration_since_synced(observed, notify_done)
                .context(
                    "coexist BLE evdev event preceded notify completion beyond clock uncertainty",
                )?
                .as_secs_f64()
                * 1_000.0,
        );
        Ok(())
    }

    fn finish(self) -> Result<(LatencyMeasurement, BleLinkTelemetry)> {
        let link = self
            .link
            .context("BLE latency measurement captured no link telemetry")?;
        let measurement = finish_latency_measurement(
            self.end_to_end,
            self.host_observed,
            self.ingress_to_runtime,
            self.runtime_processing,
            self.runtime_queueing,
            self.ble_queue_to_receive,
            self.ble_dispatch,
            self.notify_call,
            self.notify_done_to_hci_dequeue,
            self.hci_dequeue_to_credit,
            self.hci_credit_to_submit,
            self.notify_done_to_hci_submit,
            self.hci_submit_to_evdev,
            self.notify_done_to_evdev,
        );
        Ok((measurement, link))
    }
}

const fn ble_link_configuration_matches(a: BleLinkTelemetry, b: BleLinkTelemetry) -> bool {
    a.connected == b.connected
        && a.connection_interval_us == b.connection_interval_us
        && a.peripheral_latency == b.peripheral_latency
        && a.supervision_timeout_ms == b.supervision_timeout_ms
        && a.tx_phy == b.tx_phy
        && a.rx_phy == b.rx_phy
}

const fn coexist_ble_link_is_low_latency(link: BleLinkTelemetry) -> bool {
    let timing = hidshift::low_latency_ble_connection_timing();
    link.connected
        && link.connection_interval_us <= timing.interval_max_us
        && link.peripheral_latency == timing.peripheral_latency
        && link.tx_phy == 2
        && link.rx_phy == 2
}

fn run_coexist_ble_tests(
    harness: &mut BridgeHarness,
    espnow_inputs: &mut [File],
    connection: CoexistConnection,
    host_clock: BridgeClockSync,
    samples: usize,
) -> Result<CoexistBleResult> {
    ensure!(samples > 0, "--latency-samples must be positive");
    wait_coexist_linux_link(&connection.address, Duration::from_secs(20))?;
    let mut ble_inputs = open_evdevs_exact_stable("HIDShift", Duration::from_secs(15))?;

    select_bridge_transport(harness, hidshift::InputTransport::EspNow)?;
    harness.send(E2eCommand::ReleaseAll)?;
    thread::sleep(Duration::from_millis(20));
    for input in espnow_inputs.iter_mut().chain(&mut ble_inputs) {
        drain_file(input);
    }
    harness.send(E2eCommand::Keyboard {
        modifiers: 0,
        keys: [4, 0, 0, 0, 0, 0],
    })?;
    wait_bridge_input_event(espnow_inputs, 1, 30, 1, Duration::from_secs(1))?;
    let espnow_leaked_to_ble =
        wait_bridge_input_event(&mut ble_inputs, 1, 30, 1, Duration::from_millis(75)).is_ok();
    harness.send(E2eCommand::Keyboard {
        modifiers: 0,
        keys: [0; 6],
    })?;
    wait_bridge_input_event(espnow_inputs, 1, 30, 0, Duration::from_secs(1))?;

    select_bridge_transport(harness, hidshift::InputTransport::Ble)?;
    // BlueZ may have re-enumerated HID while ESP-NOW was being measured. Do
    // not retain stale event-node file descriptors across the route switch.
    wait_coexist_linux_link(&connection.address, Duration::from_secs(20))?;
    ble_inputs = open_evdevs_exact_stable("HIDShift", Duration::from_secs(15))?;
    harness.send(E2eCommand::ReleaseAll)?;
    thread::sleep(Duration::from_millis(20));
    for input in espnow_inputs.iter_mut().chain(&mut ble_inputs) {
        drain_file(input);
    }
    harness.send(E2eCommand::Keyboard {
        modifiers: 0,
        keys: [5, 0, 0, 0, 0, 0],
    })?;
    wait_bridge_input_event(&mut ble_inputs, 1, 48, 1, Duration::from_secs(1))?;
    let ble_leaked_to_espnow =
        wait_bridge_input_event(espnow_inputs, 1, 48, 1, Duration::from_millis(75)).is_ok();
    harness.send(E2eCommand::Keyboard {
        modifiers: 0,
        keys: [0; 6],
    })?;
    wait_bridge_input_event(&mut ble_inputs, 1, 48, 0, Duration::from_secs(1))?;

    let (keyboard, keyboard_link) =
        measure_coexist_ble_keyboard(harness, &mut ble_inputs, host_clock, samples)?;
    let (mouse, mouse_link) =
        measure_coexist_ble_mouse(harness, &mut ble_inputs, host_clock, samples)?;
    ensure!(
        ble_link_configuration_matches(keyboard_link, mouse_link),
        "BLE link configuration changed between keyboard and mouse measurements: {keyboard_link:?} -> {mouse_link:?}"
    );
    let tests = vec![
        TestResult {
            name: "coexist_ble_pair_and_reconnect".into(),
            passed: true,
            detail: format!(
                "paired {} and opened {} BLE evdev nodes after bond persistence",
                connection.address,
                connection.evdev_paths.len()
            ),
        },
        TestResult {
            name: "coexist_route_isolation".into(),
            passed: !espnow_leaked_to_ble && !ble_leaked_to_espnow,
            detail: format!(
                "ESP-NOW->BLE leak={} BLE->ESP-NOW leak={}",
                espnow_leaked_to_ble, ble_leaked_to_espnow
            ),
        },
        TestResult {
            name: "coexist_ble_low_latency_link".into(),
            passed: coexist_ble_link_is_low_latency(mouse_link),
            detail: format!(
                "connected={} interval={}us latency={} timeout={}ms PHY={}/{} updates={}/{}",
                mouse_link.connected,
                mouse_link.connection_interval_us,
                mouse_link.peripheral_latency,
                mouse_link.supervision_timeout_ms,
                mouse_link.tx_phy,
                mouse_link.rx_phy,
                mouse_link.parameter_updates,
                mouse_link.phy_updates
            ),
        },
        TestResult {
            name: "coexist_ble_keyboard_game_latency".into(),
            passed: ble_game_latency_passes(&keyboard.end_to_end),
            detail: format!(
                "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
                keyboard.end_to_end.p50_ms, keyboard.end_to_end.p95_ms, keyboard.end_to_end.p99_ms
            ),
        },
        TestResult {
            name: "coexist_ble_mouse_game_latency".into(),
            passed: ble_game_latency_passes(&mouse.end_to_end),
            detail: format!(
                "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
                mouse.end_to_end.p50_ms, mouse.end_to_end.p95_ms, mouse.end_to_end.p99_ms
            ),
        },
    ];

    // Release on the old route before switching so neither PC can retain a
    // key or button that the newly selected route cannot clear.
    harness.send(E2eCommand::ReleaseAll)?;
    thread::sleep(Duration::from_millis(20));
    select_bridge_transport(harness, hidshift::InputTransport::EspNow)?;
    Ok(CoexistBleResult {
        address: connection.address,
        link: mouse_link,
        keyboard,
        mouse,
        tests,
    })
}

fn measure_coexist_ble_keyboard(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    host_clock: BridgeClockSync,
    samples: usize,
) -> Result<(LatencyMeasurement, BleLinkTelemetry)> {
    let mut values = CoexistBleSamples::with_capacity(samples);
    for index in 0..samples {
        thread::sleep(sample_phase_delay(index * 2));
        for input in inputs.iter_mut() {
            drain_file(input);
        }
        let (sequence, started) = harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: [4, 0, 0, 0, 0, 0],
        })?;
        let observed = wait_bridge_input_event(inputs, 1, 30, 1, Duration::from_millis(150))?;
        let timestamp = harness.dut_input_timestamp(sequence)?;
        values.record(observed, started, host_clock, timestamp)?;
        harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: [0; 6],
        })?;
        wait_bridge_input_event(inputs, 1, 30, 0, Duration::from_millis(150))?;
    }
    values.finish()
}

fn measure_coexist_ble_mouse(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    host_clock: BridgeClockSync,
    samples: usize,
) -> Result<(LatencyMeasurement, BleLinkTelemetry)> {
    let mut values = CoexistBleSamples::with_capacity(samples);
    for index in 0..samples {
        thread::sleep(sample_phase_delay(index + samples * 2));
        for input in inputs.iter_mut() {
            drain_file(input);
        }
        let delta = if index % 2 == 0 { 7 } else { -7 };
        let (sequence, started) = harness.send(E2eCommand::Mouse {
            buttons: 0,
            x: delta,
            y: 0,
            wheel: 0,
            pan: 0,
        })?;
        let observed =
            wait_bridge_input_event(inputs, 2, 0, i32::from(delta), Duration::from_millis(150))?;
        let timestamp = harness.dut_input_timestamp(sequence)?;
        values.record(observed, started, host_clock, timestamp)?;
    }
    values.finish()
}

fn resolve_bridge_ports(args: &Args, repo: &Path) -> Result<(PathBuf, PathBuf)> {
    let (host, device) = match (&args.host_port, &args.device_port) {
        (Some(host), Some(device)) => (host.clone(), device.clone()),
        (None, None) => {
            let directory = Path::new("/dev/serial/by-path");
            let mut discovered = Vec::new();
            for path in serial_by_path_candidates(directory)? {
                if let Ok(Some(role)) = probe_bridge_role(&path) {
                    discovered.push((path, role));
                }
            }
            assign_bridge_roles(discovered)?
        }
        _ => bail!("provide both --host-port and --device-port, or neither"),
    };
    ensure!(
        fs::canonicalize(&host)? != fs::canonicalize(&device)?,
        "Host and Device resolve to the same board"
    );
    if !args.skip_flash {
        verify_chip(repo, &host, DUT_CHIP)?;
        verify_chip(repo, &device, DUT_CHIP)?;
    }
    Ok((host, device))
}

fn probe_bridge_role(port: &Path) -> Result<Option<EspNowRole>> {
    // Role probing is intentionally synchronous. Using BridgeHarness here
    // would spawn a detached line-reader thread that keeps the serial device
    // open after this function returns, preventing the actual E2E harness
    // from acquiring its exclusive lock.
    let mut port = open_serial(port, DUT_BAUD_RATE)?;
    thread::sleep(Duration::from_millis(2_000));
    let request = ManagementRequest {
        request_id: 1,
        command: ManagementCommand::GetEspNowInfo,
    };
    port.write_all(b"@HIDSHIFT:")?;
    for byte in request.encode() {
        write!(port, "{byte:02X}")?;
    }
    port.write_all(b"\n")?;
    let mut reader = BufReader::new(port);
    let deadline = Instant::now() + Duration::from_secs(3);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => thread::sleep(Duration::from_millis(5)),
            Ok(_) => {
                if let Some(ManagementResponse {
                    payload: ManagementResponsePayload::EspNowInfo(info),
                    ..
                }) = parse_management_response(line.trim())
                {
                    return Ok(Some(info.role));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                if remaining.is_zero() {
                    break;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn build_and_flash_bridge(repo: &Path, host: &Path, device: &Path) -> Result<()> {
    build_and_flash_bridge_images(repo, host, device, "hardware-e2e,espnow", false)
}

fn build_and_flash_coexist(repo: &Path, host: &Path, device: &Path) -> Result<()> {
    build_and_flash_bridge_images(repo, host, device, "hardware-e2e,espnow", true)
}

fn pair_bridge_boards(repo: &Path, host: &Path, device: &Path, channel: u8) -> Result<()> {
    run(
        Command::new("cargo")
            .args([
                "run",
                "--offline",
                "--manifest-path",
                "tools/hidshiftctl/Cargo.toml",
                "--",
                "espnow-pair",
                "--host-serial",
            ])
            .arg(host)
            .arg("--device-serial")
            .arg(device)
            .arg("--channel")
            .arg(channel.to_string()),
        repo,
    )?;
    for port in [device, host] {
        run(
            Command::new("espflash")
                .args(["reset", "--chip", DUT_CHIP, "--port"])
                .arg(port)
                .args(["--after", "hard-reset"]),
            repo,
        )?;
    }
    Ok(())
}

fn build_and_flash_bridge_images(
    repo: &Path,
    host: &Path,
    device: &Path,
    features: &str,
    erase_host_storage: bool,
) -> Result<()> {
    let export = esp_export_path()?;
    let esp_home = export.parent().context("export-esp.sh has no parent")?;
    let cargo = esp_home.join(".rustup/toolchains/esp/bin/cargo");
    let rustc = esp_home.join(".rustup/toolchains/esp/bin/rustc");
    let rustdoc = esp_home.join(".rustup/toolchains/esp/bin/rustdoc");
    let device_features = features.replace("espnow", "espnow-device");
    for (bin, feature_set) in [("firmware", features), ("bridge-device", &device_features)] {
        let build = format!(
            ". '{}' && RUSTC='{}' RUSTDOC='{}' '{}' build -Zbuild-std=core,alloc --release --manifest-path firmware/Cargo.toml {} --bin '{}' --features '{}' --target xtensa-esp32s3-none-elf",
            export.display(),
            rustc.display(),
            rustdoc.display(),
            cargo.display(),
            if bin == "bridge-device" {
                "--no-default-features"
            } else {
                ""
            },
            bin,
            feature_set
        );
        run(Command::new("sh").arg("-c").arg(build), repo)?;
    }

    run(
        Command::new("espflash")
            .args(["flash", "--chip", DUT_CHIP, "--port"])
            .arg(device)
            .args([
                "--partition-table",
                "partitions/bridge.csv",
                "--target-app-partition",
                "factory",
                "target/xtensa-esp32s3-none-elf/release/bridge-device",
            ]),
        repo,
    )?;
    run(
        Command::new("espflash")
            .args(["flash", "--chip", DUT_CHIP, "--port"])
            .arg(host)
            .args([
                "--partition-table",
                "partitions/bridge.csv",
                "--target-app-partition",
                "factory",
                "target/xtensa-esp32s3-none-elf/release/firmware",
            ]),
        repo,
    )?;
    if erase_host_storage {
        run(
            Command::new("espflash")
                .args(["erase-parts", "--chip", DUT_CHIP, "--port"])
                .arg(host)
                .args(["--partition-table", "partitions/bridge.csv", "bridge"]),
            repo,
        )?;
    }
    Ok(())
}

fn open_bridge_harness(host: &Path) -> Result<BridgeHarness> {
    let port = open_serial(host, DUT_BAUD_RATE)?;
    let writer = port.try_clone()?;
    let (sender, lines) = mpsc::channel();
    spawn_line_reader(Source::Dut, port, sender);
    // Opening the CH340 toggles the auto-reset lines on this board. Give the
    // Host time to finish booting before sending the first packet; otherwise
    // the very first UART write can fail with a transient EIO/timeout.
    thread::sleep(Duration::from_millis(2_000));
    Ok(BridgeHarness {
        writer,
        lines,
        sequence: 1,
    })
}

impl BridgeHarness {
    fn send(&mut self, command: E2eCommand) -> Result<(u32, Instant)> {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        let line = E2ePacket { sequence, command }.encode_line();
        let started = Instant::now();
        let mut last_error = None;
        for attempt in 0..4 {
            match (|| -> std::io::Result<()> {
                self.writer.write_all(&line)?;
                self.writer.write_all(b"\n")?;
                Ok(())
            })() {
                Ok(()) => return Ok((sequence, started)),
                Err(error) if attempt < 3 => {
                    last_error = Some(error);
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(last_error
            .expect("send retry loop must record an error")
            .into())
    }

    fn send_management(&mut self, command: ManagementCommand) -> Result<u8> {
        let request_id = self.sequence as u8;
        self.sequence = self.sequence.wrapping_add(1);
        let request = ManagementRequest {
            request_id,
            command,
        };
        self.writer.write_all(b"@HIDSHIFT:")?;
        for byte in request.encode() {
            write!(self.writer, "{byte:02X}")?;
        }
        self.writer.write_all(b"\n")?;
        Ok(request_id)
    }

    fn wait_management_response(
        &self,
        request_id: u8,
        timeout: Duration,
    ) -> Result<ManagementResponse> {
        let deadline = Instant::now() + timeout;
        let mut diagnostics = String::new();
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    bail!(
                        "timed out waiting for management response {request_id}; recent DUT lines:\n{diagnostics}"
                    )
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!(
                        "bridge UART reader disconnected while waiting for management response {request_id}"
                    )
                }
            };
            if diagnostics.len() > 4_096 {
                diagnostics.clear();
            }
            diagnostics.push_str(&line.text);
            diagnostics.push('\n');
            if let Some(response) = parse_management_response(&line.text)
                && response.request_id == request_id
            {
                return Ok(response);
            }
        }
        bail!("timed out waiting for management response {request_id}")
    }

    fn dut_input_timestamp(&mut self, expected_sequence: u32) -> Result<DutInputTimestamps> {
        let (query_sequence, _) = self.send(E2eCommand::ReadTimestamp {
            target_sequence: expected_sequence,
        })?;
        let deadline = Instant::now() + Duration::from_secs(1);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let line = self.lines.recv_timeout(remaining)?;
            if let Some(timestamp) = parse_dut_input_timestamp(&line.text)
                && timestamp.query_sequence == query_sequence
            {
                ensure!(
                    timestamp.input_sequence == expected_sequence,
                    "DUT timestamp sequence mismatch: expected {expected_sequence}, got {}",
                    timestamp.input_sequence
                );
                return Ok(timestamp);
            }
        }
        bail!("timed out waiting for BLE timestamp of input {expected_sequence}")
    }

    fn wait_dut_sequence(
        &self,
        event: &str,
        sequence: u32,
        timeout: Duration,
    ) -> Result<SerialLine> {
        let marker = format!("@HIDSHIFT-E2E:{event},{sequence},");
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let line = self.lines.recv_timeout(remaining)?;
            if line.text.contains(&marker) {
                return Ok(line);
            }
        }
        bail!("timed out waiting for {marker}")
    }

    fn wait_bridge_clock(&self, sequence: u32, timeout: Duration) -> Result<SerialLine> {
        let marker = format!("@HIDSHIFT-BRIDGE:CLOCK,{sequence},");
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let line = self.lines.recv_timeout(remaining)?;
            if line.text.contains(&marker) {
                return Ok(line);
            }
        }
        bail!("timed out waiting for {marker}")
    }

    fn bridge_timestamp(
        &mut self,
        expected_sequence: u32,
        expected_host_session_id: u32,
    ) -> Result<BridgeTimestamp> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut latest_host_timestamp = None;
        let mut last_seen_timestamp = None;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let (query, _) = self.send(E2eCommand::ReadTimestamp {
                target_sequence: expected_sequence,
            })?;
            let query_deadline = Instant::now() + remaining.min(Duration::from_millis(50));
            while let Some(wait) = query_deadline.checked_duration_since(Instant::now()) {
                let line = match self.lines.recv_timeout(wait) {
                    Ok(line) => line,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        bail!("bridge UART reader disconnected")
                    }
                };
                if let Some((session_id, reset_reason, brownout)) = parse_bridge_boot(&line.text)
                    && session_id != expected_host_session_id
                {
                    bail!(
                        "bridge Host rebooted while measuring input {expected_sequence}: \
                         expected session {expected_host_session_id}, observed session \
                         {session_id}, reset_reason=0x{reset_reason:02x}, brownout={brownout}"
                    );
                }
                if let Some((query_sequence, timestamp)) = parse_bridge_timestamp(&line.text)
                    && query_sequence == query
                {
                    last_seen_timestamp = Some(timestamp);
                    if timestamp.host_session_id != expected_host_session_id {
                        bail!(
                            "bridge Host rebooted while measuring input {expected_sequence}: \
                             expected session {expected_host_session_id}, observed session {}",
                            timestamp.host_session_id
                        );
                    }
                    if timestamp.input_sequence != expected_sequence {
                        continue;
                    }
                    if bridge_host_timestamp_complete(&timestamp)
                        && timestamp.device_sequence == expected_sequence
                    {
                        return Ok(timestamp);
                    }
                    if bridge_host_timestamp_complete(&timestamp) {
                        latest_host_timestamp = Some(timestamp);
                    }
                }
            }
            if let Some(timestamp) = latest_host_timestamp {
                return Ok(timestamp);
            }
        }
        bail!(
            "bridge timestamp query for input {expected_sequence} timed out; last={last_seen_timestamp:?}"
        )
    }
}

fn parse_bridge_boot(line: &str) -> Option<(u32, u8, bool)> {
    let body = line.split_once("@HIDSHIFT-BRIDGE:BOOT,")?.1;
    let mut fields = body.split(',');
    let session_id = fields.next()?.parse().ok()?;
    let reset_reason = fields.next()?.parse().ok()?;
    let brownout = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    fields
        .next()
        .is_none()
        .then_some((session_id, reset_reason, brownout))
}

const fn bridge_host_timestamp_complete(timestamp: &BridgeTimestamp) -> bool {
    timestamp.enqueue_us != 0
        && timestamp.dequeue_us != 0
        && timestamp.send_start_us != 0
        && timestamp.tx_done_us != 0
}

fn parse_bridge_timestamp(line: &str) -> Option<(u32, BridgeTimestamp)> {
    let body = line.split_once("@HIDSHIFT-BRIDGE:STAMP,")?.1;
    let mut fields = body.split(',');
    let query_sequence = fields.next()?.parse().ok()?;
    let host_session_id = fields.next()?.parse().ok()?;
    let input_sequence = fields.next()?.parse().ok()?;
    let ingress_us = fields.next()?.parse().ok()?;
    let enqueue_us = fields.next()?.parse().ok()?;
    let dequeue_us = fields.next()?.parse().ok()?;
    let send_start_us = fields.next()?.parse().ok()?;
    let tx_done_us = fields.next()?.parse().ok()?;
    let device_sequence = fields.next()?.parse().ok()?;
    let device_radio_rx_us = fields.next()?.parse().ok()?;
    let device_reassembled_us = fields.next()?.parse().ok()?;
    let device_hid_write_us = fields.next()?.parse().ok()?;
    Some((
        query_sequence,
        BridgeTimestamp {
            host_session_id,
            input_sequence,
            ingress_us,
            enqueue_us,
            dequeue_us,
            send_start_us,
            tx_done_us,
            device_sequence,
            device_radio_rx_us,
            device_reassembled_us,
            device_hid_write_us,
        },
    ))
}

fn parse_bridge_clock(line: &str) -> Option<(u32, u32, u32, u64)> {
    let body = line.split_once("@HIDSHIFT-BRIDGE:CLOCK,")?.1;
    let mut fields = body.split(',');
    let parsed = (
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    );
    fields.next().is_none().then_some(parsed)
}

fn synchronize_bridge_host_clock(
    harness: &mut BridgeHarness,
    samples: usize,
) -> Result<BridgeClockSync> {
    let mut best = None;
    for _ in 0..samples {
        let (sequence, started) = harness
            .send(E2eCommand::Hello)
            .context("bridge clock synchronization Hello send")?;
        let line = harness.wait_bridge_clock(sequence, Duration::from_secs(1))?;
        let (_, host_session_id, device_session_id, host_us) = parse_bridge_clock(&line.text)
            .context("invalid Host clock synchronization response")?;
        let round_trip = line.received.duration_since(started);
        let candidate = BridgeClockSync {
            clock: DeviceClockSync {
                host_anchor: started + round_trip / 2,
                probe_anchor_us: host_us,
                round_trip,
            },
            host_session_id,
            device_session_id,
        };
        if best.is_some_and(|current: BridgeClockSync| {
            current.host_session_id != host_session_id
                || current.device_session_id != device_session_id
        }) {
            bail!("Host or Device rebooted while synchronizing the bridge clock");
        }
        if best.is_none_or(|current: BridgeClockSync| round_trip < current.clock.round_trip) {
            best = Some(candidate);
        }
    }
    best.context("Host clock synchronization produced no samples")
}

fn run_bridge_functional_tests(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    device_count: usize,
    hidraw: &mut File,
) -> Result<Vec<TestResult>> {
    harness.send(E2eCommand::ReleaseAll)?;
    for input in inputs.iter_mut() {
        drain_file(input);
    }
    send_bridge_functional_event(
        harness,
        inputs,
        E2eCommand::Keyboard {
            modifiers: 0,
            keys: [4, 0, 0, 0, 0, 0],
        },
        1,
        30,
        1,
    )?;
    send_bridge_functional_event(harness, inputs, E2eCommand::ReleaseAll, 1, 30, 0)?;
    send_bridge_functional_event(
        harness,
        inputs,
        E2eCommand::Mouse {
            buttons: 0,
            x: 11,
            y: 0,
            wheel: 0,
            pan: 0,
        },
        2,
        0,
        11,
    )?;
    send_bridge_functional_event(
        harness,
        inputs,
        E2eCommand::Consumer { usage: 0x00e9 },
        1,
        115,
        1,
    )?;
    send_bridge_functional_event(harness, inputs, E2eCommand::ReleaseAll, 1, 115, 0)?;
    // Drop the primary final-release broadcast. With no later physical input,
    // a fresh timed snapshot must recover it without ACK/NACK feedback.
    send_bridge_functional_event(
        harness,
        inputs,
        E2eCommand::Keyboard {
            modifiers: 0,
            keys: [4, 0, 0, 0, 0, 0],
        },
        1,
        30,
        1,
    )?;
    harness.send(E2eCommand::DropNextInput {
        lane: E2eInputLane::Critical,
    })?;
    let recovery_started = Instant::now();
    harness.send(bridge_keyboard_release_command())?;
    wait_bridge_input_event(inputs, 1, 30, 0, Duration::from_millis(200))?;
    let critical_recovery_ms = recovery_started.elapsed().as_secs_f64() * 1_000.0;

    // During active input, the next motion state frame carries the critical
    // journal. A lost release must therefore recover immediately instead of
    // waiting for the deliberately slower idle refresh.
    send_bridge_functional_event(
        harness,
        inputs,
        E2eCommand::Keyboard {
            modifiers: 0,
            keys: [5, 0, 0, 0, 0, 0],
        },
        1,
        48,
        1,
    )?;
    harness.send(E2eCommand::DropNextInput {
        lane: E2eInputLane::Critical,
    })?;
    let active_recovery_started = Instant::now();
    harness.send(bridge_keyboard_release_command())?;
    harness.send(E2eCommand::Mouse {
        buttons: 0,
        x: 5,
        y: 0,
        wheel: 0,
        pan: 0,
    })?;
    wait_bridge_input_event(inputs, 1, 48, 0, Duration::from_millis(100))?;
    let active_recovery_ms = active_recovery_started.elapsed().as_secs_f64() * 1_000.0;

    // Suppress both the primary press and its timed recovery. The following
    // release snapshot must carry enough transition history to reconstruct a
    // short tap which was never visible as a standalone radio packet.
    for input in inputs.iter_mut() {
        drain_file(input);
    }
    harness.send(E2eCommand::DropNextInputBurst {
        lane: E2eInputLane::Critical,
    })?;
    harness.send(E2eCommand::Keyboard {
        modifiers: 0,
        keys: [5, 0, 0, 0, 0, 0],
    })?;
    harness.send(bridge_keyboard_release_command())?;
    wait_bridge_key_tap(inputs, 48, Duration::from_millis(200))?;

    // Motion is cumulative: losing one packet must preserve its delta when
    // the next packet arrives, without retransmitting the dropped packet.
    for input in inputs.iter_mut() {
        drain_file(input);
    }
    harness.send(E2eCommand::DropNextInput {
        lane: E2eInputLane::Motion,
    })?;
    harness.send(E2eCommand::Mouse {
        buttons: 0,
        x: 7,
        y: 0,
        wheel: 0,
        pan: 0,
    })?;
    thread::sleep(Duration::from_millis(40));
    harness.send(E2eCommand::Mouse {
        buttons: 0,
        x: 11,
        y: 0,
        wheel: 0,
        pan: 0,
    })?;
    wait_bridge_input_event_cumulative(inputs, 2, 0, 18, Duration::from_millis(200))
        .context("cumulative motion did not recover the intentionally dropped frame")?;
    drain_file(hidraw);
    harness.send(E2eCommand::VendorInput {
        len: 63,
        seed: 0xa0,
    })?;
    let vendor = wait_hidraw_vendor_pattern(hidraw, 4, 0xa0, Duration::from_millis(750))?;
    ensure!(
        vendor.len() == 64,
        "vendor input length was {}",
        vendor.len()
    );
    let mut output = [0u8; 64];
    output[0] = 4;
    for (index, byte) in output[1..].iter_mut().enumerate() {
        *byte = 0x20u8.wrapping_add(index as u8);
    }
    hidraw.write_all(&output)?;
    hidraw.flush()?;
    thread::sleep(Duration::from_millis(20));
    let mut feature = output;
    feature[1] = 0x5a;
    hidraw_set_feature(hidraw, &mut feature)?;
    let feature = hidraw_get_feature_retry(hidraw, 4, 64, Duration::from_secs(1))?;
    ensure!(
        feature.len() == 64 && feature[0] == 4 && feature[1] == 0x5a,
        "feature response len={} prefix={:02x?}",
        feature.len(),
        &feature[..feature.len().min(8)]
    );

    // Generate the burst inside Host firmware so this stresses the realtime
    // scheduler without conflating radio pressure with diagnostic UART loss.
    for input in inputs.iter_mut() {
        drain_file(input);
    }
    harness.send(E2eCommand::MouseBurst {
        count: 16,
        x: 1,
        y: -1,
    })?;
    // A subsequent state, including a zero delta, must recover any coalesced
    // or lost intermediate motion without retransmitting old packets.
    harness.send(E2eCommand::Mouse {
        buttons: 0,
        x: 0,
        y: 0,
        wheel: 0,
        pan: 0,
    })?;
    wait_bridge_input_event_cumulative(inputs, 2, 0, 16, Duration::from_millis(500))
        .context("cumulative motion burst did not reach its final state")?;

    // Exercise all three evdev domains repeatedly under mixed traffic. Each
    // critical transition is observed so this also catches stuck state.
    for round in 0..24 {
        let key = if round % 2 == 0 { 4 } else { 5 };
        let code = if key == 4 { 30 } else { 48 };
        send_bridge_functional_event(
            harness,
            inputs,
            E2eCommand::Keyboard {
                modifiers: 0,
                keys: [key, 0, 0, 0, 0, 0],
            },
            1,
            code,
            1,
        )?;
        harness.send(E2eCommand::Mouse {
            buttons: 0,
            x: 3,
            y: -2,
            wheel: 1,
            pan: -1,
        })?;
        send_bridge_functional_event(
            harness,
            inputs,
            bridge_keyboard_release_command(),
            1,
            code,
            0,
        )?;
        if round % 4 == 0 {
            send_bridge_functional_event(
                harness,
                inputs,
                E2eCommand::Consumer { usage: 0x00e9 },
                1,
                115,
                1,
            )?;
            send_bridge_functional_event(
                harness,
                inputs,
                E2eCommand::Consumer { usage: 0 },
                1,
                115,
                0,
            )?;
        }
    }
    Ok(vec![
        TestResult {
            name: "bridge_dynamic_usb_enumeration".into(),
            passed: true,
            detail: format!("{device_count} ESP-NOW HID Bridge evdev nodes"),
        },
        TestResult {
            name: "bridge_keyboard_mouse_consumer".into(),
            passed: true,
            detail: "keyboard press/release, mouse REL_X and consumer press/release verified"
                .into(),
        },
        TestResult {
            name: "bridge_broadcast_idle_tail_recovery".into(),
            passed: critical_recovery_ms <= BRIDGE_IDLE_RECOVERY_MAX_MS,
            detail: format!(
                "dropped primary key release recovered from a fresh snapshot in {critical_recovery_ms:.3} ms"
            ),
        },
        TestResult {
            name: "bridge_broadcast_short_tap_history_recovery".into(),
            passed: true,
            detail: "dropped press burst recovered as ordered press/release from the next snapshot"
                .into(),
        },
        TestResult {
            name: "bridge_motion_piggybacks_critical_recovery".into(),
            passed: active_recovery_ms <= 25.0,
            detail: format!(
                "dropped key release recovered from the next cumulative motion state in {active_recovery_ms:.3} ms"
            ),
        },
        TestResult {
            name: "bridge_motion_cumulative_loss_recovery".into(),
            passed: true,
            detail: "dropped +7 motion recovered by the following +11 cumulative report".into(),
        },
        TestResult {
            name: "bridge_vendor_hidraw_bidirectional".into(),
            passed: true,
            detail: "63-byte input/output and feature SET/GET crossed ESP-NOW".into(),
        },
        TestResult {
            name: "bridge_queue_pressure_and_mixed_input".into(),
            passed: true,
            detail: "16-report motion burst and 24 keyboard/mouse/consumer rounds recovered".into(),
        },
    ])
}

fn send_bridge_functional_event(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    command: E2eCommand,
    expected_type: u16,
    expected_code: u16,
    expected_value: i32,
) -> Result<()> {
    for attempt in 0..3 {
        harness.send(command)?;
        if wait_bridge_input_event(
            inputs,
            expected_type,
            expected_code,
            expected_value,
            Duration::from_millis(750),
        )
        .is_ok()
        {
            return Ok(());
        }
        if attempt < 2 {
            thread::sleep(Duration::from_millis(50));
        }
    }
    bail!(
        "functional bridge event was not observed type={expected_type} code={expected_code} value={expected_value}"
    )
}

fn run_bridge_interleaved_latency(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    host_clock: BridgeClockSync,
    samples: usize,
) -> Result<(
    LatencyStats,
    LatencyStats,
    LatencyStats,
    LatencyStats,
    BridgeHostPipelineStats,
    BridgeHostPipelineStats,
    BridgeDevicePipelineStats,
    BridgeDevicePipelineStats,
)> {
    let mut keyboard_end_to_end = Vec::with_capacity(samples);
    let mut keyboard_tx = Vec::with_capacity(samples);
    let mut mouse_end_to_end = Vec::with_capacity(samples);
    let mut mouse_tx = Vec::with_capacity(samples);
    let mut keyboard_device = BridgeDevicePipelineSamples::with_capacity(samples);
    let mut mouse_device = BridgeDevicePipelineSamples::with_capacity(samples);
    let mut keyboard_host = BridgeHostPipelineSamples::with_capacity(samples);
    let mut mouse_host = BridgeHostPipelineSamples::with_capacity(samples);
    for index in 0..samples {
        // Alternate which device is measured first. This prevents a whole
        // keyboard or mouse block from observing a different radio phase.
        if bridge_measurement_keyboard_first(index) {
            measure_bridge_keyboard_sample(
                harness,
                inputs,
                host_clock,
                index,
                &mut keyboard_end_to_end,
                &mut keyboard_tx,
                &mut keyboard_host,
                &mut keyboard_device,
            )?;
            measure_bridge_mouse_sample(
                harness,
                inputs,
                host_clock,
                index,
                &mut mouse_end_to_end,
                &mut mouse_tx,
                &mut mouse_host,
                &mut mouse_device,
            )?;
        } else {
            measure_bridge_mouse_sample(
                harness,
                inputs,
                host_clock,
                index,
                &mut mouse_end_to_end,
                &mut mouse_tx,
                &mut mouse_host,
                &mut mouse_device,
            )?;
            measure_bridge_keyboard_sample(
                harness,
                inputs,
                host_clock,
                index,
                &mut keyboard_end_to_end,
                &mut keyboard_tx,
                &mut keyboard_host,
                &mut keyboard_device,
            )?;
        }
    }
    Ok((
        latency_stats(keyboard_end_to_end),
        latency_stats(mouse_end_to_end),
        latency_stats(keyboard_tx),
        latency_stats(mouse_tx),
        keyboard_host.finish(),
        mouse_host.finish(),
        keyboard_device.finish(),
        mouse_device.finish(),
    ))
}

const fn bridge_measurement_keyboard_first(round: usize) -> bool {
    round % 2 == 0
}

fn measure_bridge_keyboard_sample(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    host_clock: BridgeClockSync,
    index: usize,
    end_to_end: &mut Vec<f64>,
    tx: &mut Vec<f64>,
    host: &mut BridgeHostPipelineSamples,
    device: &mut BridgeDevicePipelineSamples,
) -> Result<()> {
    thread::sleep(sample_phase_delay(index * 2));
    let (usage, code) = if index % 2 == 0 { (4, 30) } else { (5, 48) };
    let mut failures = Vec::new();
    for attempt in 0..3 {
        for input in inputs.iter_mut() {
            drain_file(input);
        }
        let (sequence, _) = harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: [usage, 0, 0, 0, 0, 0],
        })?;
        let observed = match wait_bridge_input_event(inputs, 1, code, 1, Duration::from_millis(100))
        {
            Ok(observed) => observed,
            Err(error) => {
                failures.push(format!("attempt {} input: {error}", attempt + 1));
                // A press may arrive just after the observation deadline. A
                // repeated full-state press would then produce no new evdev
                // edge and poison all retries, so always return the lane to a
                // known released state before trying another sample.
                harness.send(bridge_keyboard_release_command())?;
                let _ = wait_bridge_input_event(inputs, 1, code, 0, Duration::from_millis(100));
                for input in inputs.iter_mut() {
                    drain_file(input);
                }
                continue;
            }
        };
        let stamp = match harness.bridge_timestamp(sequence, host_clock.host_session_id) {
            Ok(stamp) => stamp,
            Err(error) => {
                failures.push(format!("attempt {} telemetry: {error}", attempt + 1));
                harness.send(bridge_keyboard_release_command())?;
                drain_for(&harness.lines, Duration::from_millis(20));
                continue;
            }
        };
        push_bridge_latency_sample(
            observed, stamp, host_clock, "keyboard", end_to_end, tx, host, device,
        )?;
        harness.send(bridge_keyboard_release_command())?;
        wait_bridge_input_event(inputs, 1, code, 0, Duration::from_secs(1))?;
        // The latency gate measures an isolated interaction. Let the release
        // ACK/retry exchange finish before the next keyboard/mouse sample;
        // continuous-load behavior is covered separately by stability.
        thread::sleep(Duration::from_millis(10));
        return Ok(());
    }
    bail!(
        "keyboard latency sample was not recovered after three reports: {}",
        failures.join("; ")
    )
}

const fn bridge_keyboard_release_command() -> E2eCommand {
    E2eCommand::Keyboard {
        modifiers: 0,
        keys: [0; 6],
    }
}

fn measure_bridge_mouse_sample(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    host_clock: BridgeClockSync,
    index: usize,
    end_to_end: &mut Vec<f64>,
    tx: &mut Vec<f64>,
    host: &mut BridgeHostPipelineSamples,
    device: &mut BridgeDevicePipelineSamples,
) -> Result<()> {
    thread::sleep(sample_phase_delay(index * 2 + 1));
    let delta = 7;
    for _ in 0..3 {
        for input in inputs.iter_mut() {
            drain_file(input);
        }
        let (sequence, _) = harness.send(E2eCommand::Mouse {
            buttons: 0,
            x: delta,
            y: 0,
            wheel: 0,
            pan: 0,
        })?;
        let Ok(observed) = wait_bridge_input_event_matching(
            inputs,
            2,
            0,
            |value| bridge_mouse_value_matches(value, delta),
            Duration::from_millis(100),
        ) else {
            continue;
        };
        let Ok(stamp) = harness.bridge_timestamp(sequence, host_clock.host_session_id) else {
            continue;
        };
        return push_bridge_latency_sample(
            observed, stamp, host_clock, "mouse", end_to_end, tx, host, device,
        );
    }
    bail!("mouse latency sample was not recovered after three cumulative reports")
}

const fn bridge_mouse_value_matches(observed: i32, requested: i16) -> bool {
    if requested >= 0 {
        observed >= requested as i32
    } else {
        observed <= requested as i32
    }
}

fn push_bridge_latency_sample(
    observed: Instant,
    stamp: BridgeTimestamp,
    host_clock: BridgeClockSync,
    kind: &str,
    end_to_end: &mut Vec<f64>,
    tx: &mut Vec<f64>,
    host: &mut BridgeHostPipelineSamples,
    device: &mut BridgeDevicePipelineSamples,
) -> Result<()> {
    ensure!(
        stamp.ingress_us <= stamp.enqueue_us
            && stamp.enqueue_us <= stamp.dequeue_us
            && stamp.dequeue_us <= stamp.send_start_us
            && stamp.send_start_us <= stamp.tx_done_us,
        "{kind} Host timestamps are not monotonic: ingress={} enqueue={} dequeue={} send={} done={}",
        stamp.ingress_us,
        stamp.enqueue_us,
        stamp.dequeue_us,
        stamp.send_start_us,
        stamp.tx_done_us
    );
    ensure!(
        stamp.host_session_id == host_clock.host_session_id,
        "{kind} Host session changed during measurement: synchronized={} timestamp={}",
        host_clock.host_session_id,
        stamp.host_session_id
    );
    let ingress = host_clock
        .clock
        .host_time(stamp.ingress_us)
        .context("Host ingress clock mapping failed")?;
    end_to_end.push(
        host_clock
            .clock
            .duration_since_synced(observed, ingress)
            .with_context(|| format!("{kind} event preceded mapped Host ingress"))?
            .as_secs_f64()
            * 1_000.0,
    );
    tx.push((stamp.tx_done_us - stamp.ingress_us) as f64 / 1_000.0);
    host.ingress_to_enqueue
        .push((stamp.enqueue_us - stamp.ingress_us) as f64 / 1_000.0);
    host.enqueue_to_dequeue
        .push((stamp.dequeue_us - stamp.enqueue_us) as f64 / 1_000.0);
    host.dequeue_to_send_start
        .push((stamp.send_start_us - stamp.dequeue_us) as f64 / 1_000.0);
    host.send_start_to_callback
        .push((stamp.tx_done_us - stamp.send_start_us) as f64 / 1_000.0);
    if stamp.device_sequence == stamp.input_sequence {
        ensure!(
            stamp.device_radio_rx_us <= stamp.device_reassembled_us
                && stamp.device_reassembled_us <= stamp.device_hid_write_us,
            "{kind} Device timestamps are not monotonic: rx={} reassembled={} hid={}",
            stamp.device_radio_rx_us,
            stamp.device_reassembled_us,
            stamp.device_hid_write_us
        );
        device
            .radio_rx_to_reassembly
            .push((stamp.device_reassembled_us - stamp.device_radio_rx_us) as f64 / 1_000.0);
        device
            .reassembly_to_hid_write
            .push((stamp.device_hid_write_us - stamp.device_reassembled_us) as f64 / 1_000.0);
        device
            .radio_rx_to_hid_write
            .push((stamp.device_hid_write_us - stamp.device_radio_rx_us) as f64 / 1_000.0);
    }
    Ok(())
}

fn run_bridge_stability(
    harness: &mut BridgeHarness,
    inputs: &mut [File],
    duration: Duration,
    initial_clock: BridgeClockSync,
) -> Result<BridgeStabilityStats> {
    let started = Instant::now();
    let mut sent = 0;
    let mut received = 0;
    let mut timeouts = 0;
    while started.elapsed() < duration {
        let pressed = sent % 2 == 0;
        harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: if pressed { [4, 0, 0, 0, 0, 0] } else { [0; 6] },
        })?;
        sent += 1;
        match wait_bridge_input_event(
            inputs,
            1,
            30,
            if pressed { 1 } else { 0 },
            Duration::from_millis(250),
        ) {
            Ok(_) => received += 1,
            Err(_) => timeouts += 1,
        }
    }
    let final_clock =
        synchronize_bridge_host_clock(harness, 5).context("post-stability bridge session check")?;
    Ok(BridgeStabilityStats {
        duration_seconds: started.elapsed().as_secs_f64(),
        reports_sent: sent,
        reports_received: received,
        timeouts,
        host_session_before: initial_clock.host_session_id,
        host_session_after: final_clock.host_session_id,
        device_session_before: initial_clock.device_session_id,
        device_session_after: final_clock.device_session_id,
    })
}

fn bridge_stability_passes(stability: &BridgeStabilityStats) -> bool {
    stability.timeouts == 0
        && stability.reports_sent == stability.reports_received
        && stability.host_session_before == stability.host_session_after
        && stability.device_session_before != 0
        && stability.device_session_before == stability.device_session_after
}

fn bridge_device_telemetry_samples_required(latency_samples: usize) -> usize {
    latency_samples.div_ceil(32).max(1)
}

fn wait_bridge_input_event(
    files: &mut [File],
    expected_type: u16,
    expected_code: u16,
    expected_value: i32,
    timeout: Duration,
) -> Result<Instant> {
    wait_bridge_input_event_matching(
        files,
        expected_type,
        expected_code,
        |value| value == expected_value,
        timeout,
    )
}

fn wait_bridge_input_event_matching(
    files: &mut [File],
    expected_type: u16,
    expected_code: u16,
    mut value_matches: impl FnMut(i32) -> bool,
    timeout: Duration,
) -> Result<Instant> {
    let deadline = Instant::now() + timeout;
    let event_len = std::mem::size_of::<libc::timeval>() + 8;
    let mut pending = vec![Vec::<u8>::new(); files.len()];
    while Instant::now() < deadline {
        for (file, pending) in files.iter_mut().zip(&mut pending) {
            let mut buffer = [0; 256];
            match file.read(&mut buffer) {
                Ok(count) => pending.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            while pending.len() >= event_len {
                let offset = std::mem::size_of::<libc::timeval>();
                let event_type = u16::from_ne_bytes([pending[offset], pending[offset + 1]]);
                let code = u16::from_ne_bytes([pending[offset + 2], pending[offset + 3]]);
                let value = i32::from_ne_bytes([
                    pending[offset + 4],
                    pending[offset + 5],
                    pending[offset + 6],
                    pending[offset + 7],
                ]);
                pending.drain(..event_len);
                if (event_type, code) == (expected_type, expected_code) && value_matches(value) {
                    return Ok(Instant::now());
                }
            }
        }
        thread::sleep(Duration::from_micros(100));
    }
    bail!(
        "timed out waiting for bridge evdev type={expected_type} code={expected_code} matching value"
    )
}

fn wait_bridge_input_event_cumulative(
    files: &mut [File],
    expected_type: u16,
    expected_code: u16,
    target: i32,
    timeout: Duration,
) -> Result<Instant> {
    let mut accumulated = 0i64;
    let result = wait_bridge_input_event_matching(
        files,
        expected_type,
        expected_code,
        |value| cumulative_delta_reached(&mut accumulated, value, target),
        timeout,
    );
    result.with_context(|| {
        format!("observed cumulative delta {accumulated}, expected to reach {target}")
    })
}

fn cumulative_delta_reached(accumulated: &mut i64, value: i32, target: i32) -> bool {
    *accumulated += i64::from(value);
    if target >= 0 {
        *accumulated >= i64::from(target)
    } else {
        *accumulated <= i64::from(target)
    }
}

fn resolve_ports(args: &Args, repo: &Path) -> Result<(PathBuf, PathBuf)> {
    if let (Some(dut), Some(probe)) = (&args.dut_port, &args.probe_port) {
        ensure!(
            fs::canonicalize(dut)? != fs::canonicalize(probe)?,
            "DUT and probe resolve to the same device"
        );
        if !args.skip_flash {
            verify_chip(repo, dut, DUT_CHIP)?;
            verify_chip(repo, probe, PROBE_CHIP)?;
        }
        return Ok((dut.clone(), probe.clone()));
    }
    ensure!(
        args.dut_port.is_none() && args.probe_port.is_none(),
        "provide both --dut-port and --probe-port, or neither"
    );

    let candidates = serial_by_path_candidates(Path::new("/dev/serial/by-path"))?;
    let mut chips = BTreeMap::<String, PathBuf>::new();
    for path in candidates {
        if let Ok(info) = board_info(repo, &path)
            && let Some(chip) = parse_chip_type(&info)
        {
            if chip == DUT_CHIP || chip == PROBE_CHIP {
                chips.entry(chip.to_owned()).or_insert(path);
            }
        }
    }
    let dut = chips.remove(DUT_CHIP).context("no ESP32-S3 DUT found")?;
    let probe = chips
        .remove(PROBE_CHIP)
        .context("no classic ESP32 probe found")?;
    Ok((dut, probe))
}

fn verify_chip(repo: &Path, port: &Path, expected: &str) -> Result<()> {
    let info = board_info(repo, port)?;
    let actual = parse_chip_type(&info).context("espflash did not report a chip type")?;
    ensure!(
        actual == expected,
        "{} is {actual}, expected {expected}",
        port.display()
    );
    Ok(())
}

fn board_info(repo: &Path, port: &Path) -> Result<String> {
    let output = run(
        Command::new("espflash")
            .arg("board-info")
            .arg("--port")
            .arg(port),
        repo,
    )?;
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn build_and_flash(repo: &Path, dut: &Path, probe: &Path) -> Result<()> {
    let export = esp_export_path()?;
    remove_cached_hidshift_devices();
    let linux_address = linux_controller_address()?;
    let _ = bluetoothctl(&["power", "off"], 10);
    let build_dut = format!(
        ". '{}' && HIDSHIFT_E2E_LINUX_ADDRESS='{}' cargo +esp build -Zbuild-std=core,alloc --release --manifest-path firmware/Cargo.toml --bin firmware --features hardware-e2e --target xtensa-esp32s3-none-elf",
        export.display(),
        linux_address
    );
    run(Command::new("sh").arg("-c").arg(build_dut), repo)?;
    // Stop any probe firmware from a previous run before the freshly erased
    // DUT opens its initial pairing window. Otherwise the old probe can create
    // and persist a bond while the replacement probe image is still building.
    run(
        Command::new("espflash")
            .args(["erase-flash", "--chip", PROBE_CHIP, "--port"])
            .arg(probe),
        repo,
    )?;
    run(
        Command::new("espflash")
            .args(["erase-flash", "--chip", DUT_CHIP, "--port"])
            .arg(dut),
        repo,
    )?;
    run(
        Command::new("espflash")
            .args(["flash", "--chip", DUT_CHIP, "--port"])
            .arg(dut)
            .args([
                "--partition-table",
                "partitions/bridge.csv",
                "--target-app-partition",
                "factory",
                "target/xtensa-esp32s3-none-elf/release/firmware",
            ]),
        repo,
    )?;
    run(
        Command::new("espflash")
            .args(["erase-parts", "--chip", DUT_CHIP, "--port"])
            .arg(dut)
            .args(["--partition-table", "partitions/bridge.csv", "bridge"]),
        repo,
    )?;

    let address = read_dut_ble_address(dut, Duration::from_secs(15))?;
    let build_probe = format!(
        ". '{}' && HIDSHIFT_DUT_ADDRESS='{}' cargo +esp build -Zbuild-std=core,alloc --release --manifest-path e2e/probe-firmware/Cargo.toml --target xtensa-esp32-none-elf",
        export.display(),
        address
    );
    run(Command::new("sh").arg("-c").arg(build_probe), repo)?;
    run(
        Command::new("espflash")
            .args(["flash", "--chip", PROBE_CHIP, "--port"])
            .arg(probe)
            .arg("e2e/probe-firmware/target/xtensa-esp32-none-elf/release/hidshift-e2e-probe"),
        repo,
    )?;
    Ok(())
}

fn linux_controller_address() -> Result<String> {
    let output = bluetoothctl(&["show"], 10)?;
    output
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Controller ")
                .and_then(|rest| rest.split_whitespace().next())
                .filter(|address| address.len() == 17)
                .map(str::to_owned)
        })
        .context("bluetoothctl did not report a Linux controller address")
}

fn read_dut_ble_address(port: &Path, timeout: Duration) -> Result<String> {
    let port = open_serial(port, DUT_BAUD_RATE)?;
    let mut reader = BufReader::new(port);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {}
            Ok(_) => {
                if let Some(address) = device_address_from_log(&line) {
                    return Ok(address);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("DUT did not report its BLE controller address")
}

fn device_address_from_log(line: &str) -> Option<String> {
    let address = line.split_once("[host] Device Address ")?.1.trim();
    (address.len() == 17
        && address.as_bytes().iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        }))
    .then(|| address.to_owned())
}

fn esp_export_path() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("HIDSHIFT_ESP_EXPORT") {
        let explicit = PathBuf::from(explicit);
        ensure!(explicit.exists(), "{} not found", explicit.display());
        return Ok(explicit);
    }
    let home = std::env::var_os("HOME").context("HOME is unset")?;
    let candidate = PathBuf::from(home).join("export-esp.sh");
    ensure!(
        candidate.exists(),
        "{} not found; set HIDSHIFT_ESP_EXPORT",
        candidate.display()
    );
    Ok(candidate)
}

fn run(command: &mut Command, directory: &Path) -> Result<Output> {
    let debug = format!("{command:?}");
    let output = command.current_dir(directory).output()?;
    if !output.status.success() {
        bail!(
            "command failed: {debug}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

fn open_harness(dut: &Path, probe: &Path) -> Result<Harness> {
    let dut_port = open_serial(dut, DUT_BAUD_RATE)?;
    let writer = dut_port.try_clone()?;
    let probe_port = open_serial(probe, PROBE_BAUD_RATE)?;
    let probe_writer = probe_port.try_clone()?;
    let (sender, receiver) = mpsc::channel();
    spawn_line_reader(Source::Dut, dut_port, sender.clone());
    spawn_line_reader(Source::Probe, probe_port, sender);
    Ok(Harness {
        writer,
        probe_writer,
        lines: receiver,
        sequence: 1,
        probe_diagnostics: Mutex::new(ProbeDiagnostics::default()),
    })
}

fn open_serial(path: &Path, baud_rate: u32) -> Result<Box<dyn SerialPort>> {
    let mut port = serialport::new(path.to_string_lossy(), baud_rate)
        .timeout(Duration::from_millis(500))
        .open()
        .with_context(|| format!("open serial port {}", path.display()))?;
    // CH340 modem-control state can leave the transmitter waiting after a
    // flash/reset sequence. Explicitly disable hardware flow control so the
    // provisioning state machine can continue using its bounded send retries.
    let _ = port.set_flow_control(FlowControl::None);
    // Keep the USB-UART auto-reset controls inactive after opening the port.
    // Some CH340 boards otherwise remain in reset until the host application
    // changes the modem-control state.
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(false);
    Ok(port)
}

fn spawn_line_reader(source: Source, port: Box<dyn SerialPort>, sender: mpsc::Sender<SerialLine>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(port);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => thread::sleep(Duration::from_millis(5)),
                Ok(_) => {
                    let _ = sender.send(SerialLine {
                        source,
                        received: Instant::now(),
                        text: line.trim().to_owned(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => {
                    // A CH340 can briefly return EIO while the ESP32 resets
                    // as the port is opened. Keep the reader alive so the
                    // post-boot E2E handshake is not lost.
                    let _ = error.kind();
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
    });
}

impl Harness {
    fn reset_probe_diagnostics(&self) {
        if let Ok(mut diagnostics) = self.probe_diagnostics.lock() {
            *diagnostics = ProbeDiagnostics::default();
        }
    }

    fn observe_probe_notification(&self, notification: &ProbeNotification) {
        let Some(sequence) = notification.sequence else {
            return;
        };
        if let Ok(mut diagnostics) = self.probe_diagnostics.lock() {
            if let Some(previous) = diagnostics.last_sequence {
                diagnostics.gaps += sequence.wrapping_sub(previous).saturating_sub(1) as u64;
            }
            diagnostics.last_sequence = Some(sequence);
        }
    }

    fn probe_sequence_gaps(&self) -> u64 {
        self.probe_diagnostics.lock().map(|d| d.gaps).unwrap_or(0)
    }

    fn wait_marker(&self, source: Source, marker: &str, timeout: Duration) -> Result<SerialLine> {
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("serial readers disconnected while waiting for {marker}")
                }
            };
            println!("{:?}: {}", line.source, line.text);
            if line.source == Source::Probe
                && let Some(notification) = parse_probe_notification(&line.text)
            {
                self.observe_probe_notification(&notification);
            }
            if line.source == source && line.text.contains(marker) {
                return Ok(line);
            }
        }
        bail!("timed out waiting for {marker}")
    }

    fn send(&mut self, command: E2eCommand) -> Result<(u32, Instant)> {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        let line = E2ePacket { sequence, command }.encode_line();
        let started = Instant::now();
        self.writer.write_all(&line)?;
        self.writer.write_all(b"\n")?;
        Ok((sequence, started))
    }

    fn send_management(&mut self, command: ManagementCommand) -> Result<u8> {
        let request_id = self.sequence as u8;
        let request = ManagementRequest {
            request_id,
            command,
        };
        self.sequence = self.sequence.wrapping_add(1);
        self.writer.write_all(b"@HIDSHIFT:")?;
        for byte in request.encode() {
            write!(self.writer, "{byte:02X}")?;
        }
        self.writer.write_all(b"\n")?;
        Ok(request_id)
    }

    fn wait_management_response(
        &self,
        request_id: u8,
        timeout: Duration,
    ) -> Result<ManagementResponse> {
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let line = self.lines.recv_timeout(remaining)?;
            if line.source == Source::Dut
                && let Some(response) = parse_management_response(&line.text)
                && response.request_id == request_id
            {
                return Ok(response);
            }
        }
        bail!("timed out waiting for management response {request_id}")
    }

    fn wait_dut_sequence(
        &self,
        event: &str,
        sequence: u32,
        timeout: Duration,
    ) -> Result<SerialLine> {
        let marker = format!("@HIDSHIFT-E2E:{event},{sequence},");
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    if line.source == Source::Probe
                        && let Some(notification) = parse_probe_notification(&line.text)
                    {
                        self.observe_probe_notification(&notification);
                    }
                    if line.source == Source::Dut && line.text.contains(&marker) {
                        return Ok(line);
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("serial readers disconnected while waiting for {marker}")
                }
            }
        }
        bail!("timed out waiting for {marker}")
    }

    fn wait_transport_ready(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut attempt = 0u8;
        while Instant::now() < deadline {
            // Change the key on every retry so a buffered notification from a
            // pre-persist connection cannot be mistaken for current transport.
            let key = 4 + attempt % 100;
            attempt = attempt.wrapping_add(1);
            self.send(E2eCommand::Keyboard {
                modifiers: 0,
                keys: [key, 0, 0, 0, 0, 0],
            })?;
            if self
                .await_notification(
                    "keyboard",
                    &[0, 0, key, 0, 0, 0, 0, 0],
                    Duration::from_secs(3),
                )
                .is_ok()
            {
                self.send(E2eCommand::Keyboard {
                    modifiers: 0,
                    keys: [0; 6],
                })?;
                if self
                    .await_notification("keyboard", &[0; 8], Duration::from_secs(3))
                    .is_ok()
                {
                    return Ok(());
                }
            }
            thread::sleep(Duration::from_millis(250));
        }
        bail!("BLE transport did not become ready")
    }

    fn await_notification(
        &self,
        kind: &str,
        expected: &[u8],
        timeout: Duration,
    ) -> Result<SerialLine> {
        let deadline = Instant::now() + timeout;
        let mut last_probe_line = None;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("serial readers disconnected while waiting for {kind} {expected:02x?}")
                }
            };
            if line.source == Source::Dut
                && (line.text.contains("runtime owner error")
                    || line.text.contains("runtime drive error")
                    || line.text.contains("notify failed")
                    || line.text.contains("@HIDSHIFT-E2E:ERROR")
                    || line.text.contains("@HIDSHIFT-E2E:NOTIFY"))
            {
                println!("Dut: {}", line.text);
            }
            if line.source != Source::Probe {
                continue;
            }
            last_probe_line = Some(line.text.clone());
            if let Some(notification) = parse_probe_notification(&line.text) {
                self.observe_probe_notification(&notification);
                if notification.kind == kind {
                    if notification.bytes == expected {
                        return Ok(line);
                    }
                    bail!(
                        "received unexpected {kind} report {:02x?}, expected {:02x?}",
                        notification.bytes,
                        expected
                    );
                }
            }
        }
        bail!(
            "timed out waiting for {kind} {expected:02x?}; last probe line: {}",
            last_probe_line.as_deref().unwrap_or("<none>")
        )
    }
}

fn parse_probe_notification(line: &str) -> Option<ProbeNotification> {
    if let Some(body) = line.strip_prefix("@N:") {
        let mut fields = body.split(':');
        let first = fields.next()?;
        let (sequence, kind, second, third) = if matches!(first, "k" | "m" | "c") {
            (None, first, fields.next()?, fields.next())
        } else {
            (
                Some(u32::from_str_radix(first, 16).ok()?),
                fields.next()?,
                fields.next()?,
                fields.next(),
            )
        };
        if fields.next().is_some() {
            return None;
        }
        let (probe_us, encoded) = match third {
            Some(encoded) => (Some(u64::from_str_radix(second, 16).ok()?), encoded),
            None => (None, second),
        };
        let kind = match kind {
            "k" => "keyboard",
            "m" => "mouse",
            "c" => "consumer",
            _ => return None,
        }
        .to_owned();
        return Some(ProbeNotification {
            sequence,
            kind,
            probe_us,
            bytes: decode_hex(encoded)?,
        });
    }
    if let Some(body) = line.split_once("@HIDSHIFT-PROBE:N,").map(|parts| parts.1) {
        let mut fields = body.splitn(3, ',');
        let kind = match fields.next()? {
            "k" => "keyboard",
            "m" => "mouse",
            "c" => "consumer",
            _ => return None,
        }
        .to_owned();
        let _probe_us = fields.next()?.parse::<u64>().ok()?;
        let encoded = fields.next()?.trim();
        return Some(ProbeNotification {
            sequence: None,
            kind,
            probe_us: Some(_probe_us),
            bytes: decode_hex(encoded)?,
        });
    }
    let marker = "@HIDSHIFT-PROBE:NOTIFY,";
    let body = line.split_once(marker)?.1;
    let mut fields = body.splitn(3, ',');
    let kind = fields.next()?.to_owned();
    let _probe_us = fields.next()?.parse::<u64>().ok()?;
    let bytes = fields
        .next()?
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter(|field| !field.trim().is_empty())
        .map(|field| u8::from_str_radix(field.trim(), 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(ProbeNotification {
        sequence: None,
        kind,
        probe_us: Some(_probe_us),
        bytes,
    })
}

fn parse_probe_clock_response(line: &str) -> Option<(u32, u64)> {
    let body = line.strip_prefix("@T:")?;
    let (sequence, probe_us) = body.split_once(':')?;
    Some((
        u32::from_str_radix(sequence, 16).ok()?,
        u64::from_str_radix(probe_us, 16).ok()?,
    ))
}

fn decode_hex(encoded: &str) -> Option<Vec<u8>> {
    if encoded.len() % 2 != 0 {
        return None;
    }
    (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn parse_management_response(line: &str) -> Option<ManagementResponse> {
    let encoded = line.split_once("@HIDSHIFT:")?.1.trim();
    let bytes = decode_hex(encoded)?;
    ManagementResponse::decode(&bytes).ok()
}

fn run_functional_raw_tests(harness: &mut Harness) -> Result<Vec<TestResult>> {
    harness.send(E2eCommand::ReleaseAll)?;
    drain_for(&harness.lines, Duration::from_millis(100));
    let cases = [
        (
            "raw_keyboard_press",
            E2eCommand::Keyboard {
                modifiers: 0,
                keys: [4, 0, 0, 0, 0, 0],
            },
            "keyboard",
            vec![0, 0, 4, 0, 0, 0, 0, 0],
        ),
        (
            "raw_keyboard_modifier_6kro",
            E2eCommand::Keyboard {
                modifiers: 0x02,
                keys: [4, 5, 6, 7, 8, 9],
            },
            "keyboard",
            vec![0x02, 0, 4, 5, 6, 7, 8, 9],
        ),
        (
            "raw_mouse",
            E2eCommand::Mouse {
                buttons: 3,
                x: 10,
                y: -7,
                wheel: 2,
                pan: -1,
            },
            "mouse",
            vec![3, 10, 249, 2, 255],
        ),
        (
            "raw_consumer",
            E2eCommand::Consumer { usage: 0x00e9 },
            "consumer",
            vec![0xe9, 0],
        ),
    ];
    let mut results = Vec::new();
    for (name, command, kind, expected) in cases {
        harness.send(command)?;
        harness.await_notification(kind, &expected, Duration::from_secs(2))?;
        results.push(TestResult {
            name: name.into(),
            passed: true,
            detail: format!("received {kind} {expected:02x?}"),
        });
    }
    harness.send(E2eCommand::ReleaseAll)?;
    Ok(results)
}

fn run_latency_test(
    harness: &mut Harness,
    samples: usize,
    probe_clock_sync: DeviceClockSync,
    dut_clock_sync: DeviceClockSync,
) -> Result<LatencyMeasurement> {
    ensure!(samples > 0, "--latency-samples must be positive");
    let mut synchronized_values = Vec::with_capacity(samples * 2);
    let mut observed_values = Vec::with_capacity(samples * 2);
    let mut ingress_to_runtime_values = Vec::with_capacity(samples * 2);
    let mut runtime_processing_values = Vec::with_capacity(samples * 2);
    let mut runtime_queueing_values = Vec::with_capacity(samples * 2);
    let mut runtime_to_ble_values = Vec::with_capacity(samples * 2);
    let mut ble_dispatch_values = Vec::with_capacity(samples * 2);
    let mut notify_call_values = Vec::with_capacity(samples * 2);
    let mut notify_done_to_hci_dequeue_values = Vec::with_capacity(samples * 2);
    let mut hci_dequeue_to_credit_values = Vec::with_capacity(samples * 2);
    let mut hci_credit_to_submit_values = Vec::with_capacity(samples * 2);
    let mut notify_done_to_hci_submit_values = Vec::with_capacity(samples * 2);
    let mut hci_submit_to_probe_values = Vec::with_capacity(samples * 2);
    let mut notify_to_probe_values = Vec::with_capacity(samples * 2);
    for index in 0..samples {
        let key = 4 + (index % 100) as u8;
        thread::sleep(sample_phase_delay(index * 2));
        let (input_sequence, started) = harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: [key, 0, 0, 0, 0, 0],
        })?;
        let line = harness.await_notification(
            "keyboard",
            &[0, 0, key, 0, 0, 0, 0, 0],
            Duration::from_secs(2),
        )?;
        let dut_timestamps = read_dut_input_timestamp(harness, input_sequence)?;
        record_latency_sample(
            &line,
            started,
            probe_clock_sync,
            dut_clock_sync,
            dut_timestamps,
            &mut synchronized_values,
            &mut observed_values,
            &mut ingress_to_runtime_values,
            &mut runtime_processing_values,
            &mut runtime_queueing_values,
            &mut runtime_to_ble_values,
            &mut ble_dispatch_values,
            &mut notify_call_values,
            &mut notify_done_to_hci_dequeue_values,
            &mut hci_dequeue_to_credit_values,
            &mut hci_credit_to_submit_values,
            &mut notify_done_to_hci_submit_values,
            &mut hci_submit_to_probe_values,
            &mut notify_to_probe_values,
        )?;

        thread::sleep(sample_phase_delay(index * 2 + 1));
        let (input_sequence, started) = harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: [0; 6],
        })?;
        let line = harness.await_notification("keyboard", &[0; 8], Duration::from_secs(2))?;
        let dut_timestamps = read_dut_input_timestamp(harness, input_sequence)?;
        record_latency_sample(
            &line,
            started,
            probe_clock_sync,
            dut_clock_sync,
            dut_timestamps,
            &mut synchronized_values,
            &mut observed_values,
            &mut ingress_to_runtime_values,
            &mut runtime_processing_values,
            &mut runtime_queueing_values,
            &mut runtime_to_ble_values,
            &mut ble_dispatch_values,
            &mut notify_call_values,
            &mut notify_done_to_hci_dequeue_values,
            &mut hci_dequeue_to_credit_values,
            &mut hci_credit_to_submit_values,
            &mut notify_done_to_hci_submit_values,
            &mut hci_submit_to_probe_values,
            &mut notify_to_probe_values,
        )?;
    }
    Ok(finish_latency_measurement(
        synchronized_values,
        observed_values,
        ingress_to_runtime_values,
        runtime_processing_values,
        runtime_queueing_values,
        runtime_to_ble_values,
        ble_dispatch_values,
        notify_call_values,
        notify_done_to_hci_dequeue_values,
        hci_dequeue_to_credit_values,
        hci_credit_to_submit_values,
        notify_done_to_hci_submit_values,
        hci_submit_to_probe_values,
        notify_to_probe_values,
    ))
}

fn run_mouse_latency_test(
    harness: &mut Harness,
    samples: usize,
    probe_clock_sync: DeviceClockSync,
    dut_clock_sync: DeviceClockSync,
) -> Result<LatencyMeasurement> {
    ensure!(samples > 0, "--latency-samples must be positive");
    let mut synchronized_values = Vec::with_capacity(samples);
    let mut observed_values = Vec::with_capacity(samples);
    let mut ingress_to_runtime_values = Vec::with_capacity(samples);
    let mut runtime_processing_values = Vec::with_capacity(samples);
    let mut runtime_queueing_values = Vec::with_capacity(samples);
    let mut runtime_to_ble_values = Vec::with_capacity(samples);
    let mut ble_dispatch_values = Vec::with_capacity(samples);
    let mut notify_call_values = Vec::with_capacity(samples);
    let mut notify_done_to_hci_dequeue_values = Vec::with_capacity(samples);
    let mut hci_dequeue_to_credit_values = Vec::with_capacity(samples);
    let mut hci_credit_to_submit_values = Vec::with_capacity(samples);
    let mut notify_done_to_hci_submit_values = Vec::with_capacity(samples);
    let mut hci_submit_to_probe_values = Vec::with_capacity(samples);
    let mut notify_to_probe_values = Vec::with_capacity(samples);
    for index in 0..samples {
        let x = if index % 2 == 0 { 1 } else { -1 };
        thread::sleep(sample_phase_delay(index + samples * 2));
        let (input_sequence, started) = harness.send(E2eCommand::Mouse {
            buttons: 0,
            x,
            y: 0,
            wheel: 0,
            pan: 0,
        })?;
        let expected_x = x as i8 as u8;
        let line = match harness.await_notification(
            "mouse",
            &[0, expected_x, 0, 0, 0],
            Duration::from_secs(2),
        ) {
            Ok(line) => line,
            Err(error) => {
                let telemetry = read_dut_input_timestamp(harness, input_sequence);
                bail!("{error}; DUT telemetry after mouse timeout: {telemetry:?}")
            }
        };
        let dut_timestamps = read_dut_input_timestamp(harness, input_sequence)?;
        record_latency_sample(
            &line,
            started,
            probe_clock_sync,
            dut_clock_sync,
            dut_timestamps,
            &mut synchronized_values,
            &mut observed_values,
            &mut ingress_to_runtime_values,
            &mut runtime_processing_values,
            &mut runtime_queueing_values,
            &mut runtime_to_ble_values,
            &mut ble_dispatch_values,
            &mut notify_call_values,
            &mut notify_done_to_hci_dequeue_values,
            &mut hci_dequeue_to_credit_values,
            &mut hci_credit_to_submit_values,
            &mut notify_done_to_hci_submit_values,
            &mut hci_submit_to_probe_values,
            &mut notify_to_probe_values,
        )?;
    }
    Ok(finish_latency_measurement(
        synchronized_values,
        observed_values,
        ingress_to_runtime_values,
        runtime_processing_values,
        runtime_queueing_values,
        runtime_to_ble_values,
        ble_dispatch_values,
        notify_call_values,
        notify_done_to_hci_dequeue_values,
        hci_dequeue_to_credit_values,
        hci_credit_to_submit_values,
        notify_done_to_hci_submit_values,
        hci_submit_to_probe_values,
        notify_to_probe_values,
    ))
}

fn finish_latency_measurement(
    synchronized_values: Vec<f64>,
    observed_values: Vec<f64>,
    ingress_to_runtime_values: Vec<f64>,
    runtime_processing_values: Vec<f64>,
    runtime_queueing_values: Vec<f64>,
    runtime_to_ble_values: Vec<f64>,
    ble_dispatch_values: Vec<f64>,
    notify_call_values: Vec<f64>,
    notify_done_to_hci_dequeue_values: Vec<f64>,
    hci_dequeue_to_credit_values: Vec<f64>,
    hci_credit_to_submit_values: Vec<f64>,
    notify_done_to_hci_submit_values: Vec<f64>,
    hci_submit_to_probe_values: Vec<f64>,
    notify_to_probe_values: Vec<f64>,
) -> LatencyMeasurement {
    LatencyMeasurement {
        end_to_end: latency_stats(synchronized_values),
        host_observed: latency_stats(observed_values),
        pipeline: PipelineLatencyStats {
            ingress_to_runtime: latency_stats(ingress_to_runtime_values),
            runtime_processing: latency_stats(runtime_processing_values),
            runtime_queueing: latency_stats(runtime_queueing_values),
            ble_queue_to_receive: latency_stats(runtime_to_ble_values),
            ble_dispatch: latency_stats(ble_dispatch_values),
            notify_call: latency_stats(notify_call_values),
            notify_done_to_hci_dequeue: latency_stats(notify_done_to_hci_dequeue_values),
            hci_dequeue_to_credit: latency_stats(hci_dequeue_to_credit_values),
            hci_credit_to_submit: latency_stats(hci_credit_to_submit_values),
            notify_done_to_hci_submit: latency_stats(notify_done_to_hci_submit_values),
            hci_submit_to_probe: latency_stats(hci_submit_to_probe_values),
            notify_done_to_probe: latency_stats(notify_to_probe_values),
        },
    }
}

/// Prevent the lockstep harness from repeatedly injecting at the same point
/// in the 7.5 ms BLE connection interval. Without this, the probe UART return
/// time phase-locks every sample just after an event and measures a synthetic
/// two-event worst case rather than normal asynchronous HID traffic.
fn sample_phase_delay(index: usize) -> Duration {
    Duration::from_micros(((index as u64 * 2_713) % 7_500) + 137)
}

fn record_latency_sample(
    line: &SerialLine,
    started: Instant,
    probe_clock_sync: DeviceClockSync,
    dut_clock_sync: DeviceClockSync,
    dut: DutInputTimestamps,
    synchronized_values: &mut Vec<f64>,
    observed_values: &mut Vec<f64>,
    ingress_to_runtime_values: &mut Vec<f64>,
    runtime_processing_values: &mut Vec<f64>,
    runtime_queueing_values: &mut Vec<f64>,
    runtime_to_ble_values: &mut Vec<f64>,
    ble_dispatch_values: &mut Vec<f64>,
    notify_call_values: &mut Vec<f64>,
    notify_done_to_hci_dequeue_values: &mut Vec<f64>,
    hci_dequeue_to_credit_values: &mut Vec<f64>,
    hci_credit_to_submit_values: &mut Vec<f64>,
    notify_done_to_hci_submit_values: &mut Vec<f64>,
    hci_submit_to_probe_values: &mut Vec<f64>,
    notify_to_probe_values: &mut Vec<f64>,
) -> Result<()> {
    ensure!(
        dut.ingress_us <= dut.runtime_us
            && dut.runtime_us <= dut.runtime_dispatch_us
            && dut.runtime_dispatch_us <= dut.ble_queued_us
            && dut.ble_queued_us <= dut.ble_receive_us
            && dut.ble_receive_us <= dut.notify_start_us
            && dut.notify_start_us <= dut.notify_done_us
            && dut.notify_done_us <= dut.hci_dequeue_us
            && dut.hci_dequeue_us <= dut.hci_credit_us
            && dut.hci_credit_us <= dut.hci_submit_us
            && dut.notify_done_us <= dut.hci_submit_us,
        "DUT pipeline timestamps are incomplete or unordered: {dut:?}"
    );
    let notification = parse_probe_notification(&line.text)
        .context("matched probe notification could not be decoded")?;
    let probe_us = notification
        .probe_us
        .context("probe notification is missing its receive timestamp")?;
    let probe_received = probe_clock_sync
        .host_time(probe_us)
        .context("probe clock timestamp is out of range")?;
    let dut_ingress = dut_clock_sync
        .host_time(dut.ingress_us)
        .context("DUT clock timestamp is out of range")?;
    let dut_notify_done = dut_clock_sync
        .host_time(dut.notify_done_us)
        .context("DUT notify completion timestamp is out of range")?;
    let dut_hci_submit = dut_clock_sync
        .host_time(dut.hci_submit_us)
        .context("DUT HCI submit timestamp is out of range")?;
    let synchronized = probe_received
        .checked_duration_since(dut_ingress)
        .context("cross-device clock synchronization produced a negative latency")?;
    synchronized_values.push(synchronized.as_secs_f64() * 1_000.0);
    observed_values.push(line.received.duration_since(started).as_secs_f64() * 1_000.0);
    ingress_to_runtime_values.push((dut.runtime_us - dut.ingress_us) as f64 / 1_000.0);
    runtime_processing_values.push((dut.runtime_dispatch_us - dut.runtime_us) as f64 / 1_000.0);
    runtime_queueing_values.push((dut.ble_queued_us - dut.runtime_dispatch_us) as f64 / 1_000.0);
    runtime_to_ble_values.push((dut.ble_receive_us - dut.ble_queued_us) as f64 / 1_000.0);
    ble_dispatch_values.push((dut.notify_start_us - dut.ble_receive_us) as f64 / 1_000.0);
    notify_call_values.push((dut.notify_done_us - dut.notify_start_us) as f64 / 1_000.0);
    notify_done_to_hci_dequeue_values
        .push((dut.hci_dequeue_us - dut.notify_done_us) as f64 / 1_000.0);
    hci_dequeue_to_credit_values.push((dut.hci_credit_us - dut.hci_dequeue_us) as f64 / 1_000.0);
    hci_credit_to_submit_values.push((dut.hci_submit_us - dut.hci_credit_us) as f64 / 1_000.0);
    notify_done_to_hci_submit_values
        .push((dut.hci_submit_us - dut.notify_done_us) as f64 / 1_000.0);
    hci_submit_to_probe_values.push(
        probe_received
            .checked_duration_since(dut_hci_submit)
            .context("BLE HCI-submit-to-probe latency was negative")?
            .as_secs_f64()
            * 1_000.0,
    );
    notify_to_probe_values.push(
        probe_received
            .checked_duration_since(dut_notify_done)
            .context("BLE notify-done-to-probe latency was negative")?
            .as_secs_f64()
            * 1_000.0,
    );
    Ok(())
}

fn synchronize_probe_clock(harness: &mut Harness, samples: usize) -> Result<DeviceClockSync> {
    ensure!(
        samples > 0,
        "clock synchronization needs at least one sample"
    );
    let mut best = None;
    for sequence in 0..samples as u32 {
        let started = Instant::now();
        writeln!(harness.probe_writer, "@T:{sequence:08x}")?;
        harness.probe_writer.flush()?;
        let deadline = Instant::now() + Duration::from_secs(1);
        let response = loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("probe clock response timed out")?;
            let line = harness.lines.recv_timeout(remaining)?;
            if line.source == Source::Probe
                && let Some((received_sequence, probe_us)) = parse_probe_clock_response(&line.text)
                && received_sequence == sequence
            {
                break (line.received, probe_us);
            }
        };
        let round_trip = response.0.duration_since(started);
        let candidate = DeviceClockSync {
            host_anchor: started + round_trip / 2,
            probe_anchor_us: response.1,
            round_trip,
        };
        if best.is_none_or(|current: DeviceClockSync| round_trip < current.round_trip) {
            best = Some(candidate);
        }
        thread::sleep(Duration::from_millis(5));
    }
    best.context("probe clock synchronization produced no samples")
}

fn synchronize_dut_clock(harness: &mut Harness, samples: usize) -> Result<DeviceClockSync> {
    ensure!(
        samples > 0,
        "DUT clock synchronization needs at least one sample"
    );
    let mut best = None;
    for _ in 0..samples {
        let (sequence, started) = harness.send(E2eCommand::Hello)?;
        let line = harness.wait_dut_sequence("QUEUED", sequence, Duration::from_secs(1))?;
        let (_, dut_us) = parse_dut_queued_timestamp(&line.text)
            .context("invalid DUT clock synchronization response")?;
        let round_trip = line.received.duration_since(started);
        let candidate = DeviceClockSync {
            host_anchor: started + round_trip / 2,
            probe_anchor_us: dut_us,
            round_trip,
        };
        if best.is_none_or(|current: DeviceClockSync| round_trip < current.round_trip) {
            best = Some(candidate);
        }
        thread::sleep(Duration::from_millis(5));
    }
    best.context("DUT clock synchronization produced no samples")
}

fn read_dut_input_timestamp(
    harness: &mut Harness,
    expected_sequence: u32,
) -> Result<DutInputTimestamps> {
    let timestamps = read_dut_snapshot(harness)?;
    ensure!(
        timestamps.input_sequence == expected_sequence,
        "DUT timestamp sequence mismatch: expected {expected_sequence}, got {}",
        timestamps.input_sequence
    );
    Ok(timestamps)
}

fn read_dut_snapshot(harness: &mut Harness) -> Result<DutInputTimestamps> {
    let (query_sequence, _) = harness.send(E2eCommand::ReadTimestamp { target_sequence: 0 })?;
    let line = harness.wait_dut_sequence("STAMP", query_sequence, Duration::from_secs(1))?;
    parse_dut_input_timestamp(&line.text).context("invalid DUT input timestamp response")
}

fn parse_dut_queued_timestamp(line: &str) -> Option<(u32, u64)> {
    let body = line.split_once("@HIDSHIFT-E2E:QUEUED,")?.1;
    let mut fields = body.split(',');
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

fn parse_dut_input_timestamp(line: &str) -> Option<DutInputTimestamps> {
    let body = line.split_once("@HIDSHIFT-E2E:STAMP,")?.1;
    let mut fields = body.split(',');
    let timestamps = DutInputTimestamps {
        query_sequence: fields.next()?.parse().ok()?,
        input_sequence: fields.next()?.parse().ok()?,
        ingress_us: fields.next()?.parse().ok()?,
        runtime_us: fields.next()?.parse().ok()?,
        runtime_dispatch_us: fields.next()?.parse().ok()?,
        ble_queued_us: fields.next()?.parse().ok()?,
        ble_receive_us: fields.next()?.parse().ok()?,
        notify_start_us: fields.next()?.parse().ok()?,
        notify_done_us: fields.next()?.parse().ok()?,
        input_count: fields.next()?.parse().ok()?,
        ble_queued_count: fields.next()?.parse().ok()?,
        notify_done_count: fields.next()?.parse().ok()?,
        ble_link: BleLinkTelemetry {
            connected: match fields.next()? {
                "0" => false,
                "1" => true,
                _ => return None,
            },
            connection_interval_us: fields.next()?.parse().ok()?,
            peripheral_latency: fields.next()?.parse().ok()?,
            supervision_timeout_ms: fields.next()?.parse().ok()?,
            tx_phy: fields.next()?.parse().ok()?,
            rx_phy: fields.next()?.parse().ok()?,
            parameter_updates: fields.next()?.parse().ok()?,
            phy_updates: fields.next()?.parse().ok()?,
        },
        hci_submit_us: fields.next()?.parse().ok()?,
        hci_dequeue_us: fields.next()?.parse().ok()?,
        hci_credit_us: fields.next()?.parse().ok()?,
    };
    fields.next().is_none().then_some(timestamps)
}

impl DeviceClockSync {
    fn host_time(self, probe_us: u64) -> Option<Instant> {
        if probe_us >= self.probe_anchor_us {
            self.host_anchor
                .checked_add(Duration::from_micros(probe_us - self.probe_anchor_us))
        } else {
            self.host_anchor
                .checked_sub(Duration::from_micros(self.probe_anchor_us - probe_us))
        }
    }

    /// Measures from a timestamp mapped through this synchronization sample
    /// to a directly observed host instant. The midpoint estimate can place a
    /// mapped timestamp at most half the synchronization round trip after the
    /// real event; clamp only that bounded diagnostic artifact to zero.
    fn duration_since_synced(self, observed: Instant, synchronized: Instant) -> Option<Duration> {
        if let Some(duration) = observed.checked_duration_since(synchronized) {
            return Some(duration);
        }
        let apparent_lead = synchronized.checked_duration_since(observed)?;
        (apparent_lead <= self.round_trip / 2).then_some(Duration::ZERO)
    }
}

fn run_stability_test(harness: &mut Harness, duration: Duration) -> Result<StabilityStats> {
    let started = Instant::now();
    harness.reset_probe_diagnostics();
    let dut_before = read_dut_snapshot(harness)?;
    let mut sent = 0u64;
    let mut received = 0u64;
    let mut mismatches = 0u64;
    let mut timeouts = 0u64;
    let mut index = 0u8;
    let mut last_timeout = None;
    while started.elapsed() < duration {
        let key = 4 + index % 100;
        index = index.wrapping_add(1);
        for keys in [[key, 0, 0, 0, 0, 0], [0; 6]] {
            harness.send(E2eCommand::Keyboard { modifiers: 0, keys })?;
            sent += 1;
            let mut expected = [0; 8];
            expected[2..].copy_from_slice(&keys);
            match harness.await_notification("keyboard", &expected, Duration::from_millis(500)) {
                Ok(_) => received += 1,
                Err(error) => {
                    let error_text = error.to_string();
                    if error_text.contains("received unexpected") {
                        mismatches += 1;
                    } else {
                        timeouts += 1;
                    }
                    let telemetry = harness
                        .send(E2eCommand::ReadTimestamp { target_sequence: 0 })
                        .and_then(|(query_sequence, _)| {
                            let line = harness.wait_dut_sequence(
                                "STAMP",
                                query_sequence,
                                Duration::from_secs(1),
                            )?;
                            Ok(line.text)
                        })
                        .unwrap_or_else(|query_error| {
                            format!("timestamp query failed: {query_error}")
                        });
                    last_timeout = Some(format!("{error_text}; DUT telemetry: {telemetry}"));
                }
            }
        }
    }
    if received + timeouts + mismatches != sent {
        mismatches += sent.saturating_sub(received + timeouts + mismatches);
    }
    let dut_after = read_dut_snapshot(harness)?;
    let dut_counter_reset = dut_after.input_count < dut_before.input_count
        || dut_after.ble_queued_count < dut_before.ble_queued_count
        || dut_after.notify_done_count < dut_before.notify_done_count;
    Ok(StabilityStats {
        duration_seconds: started.elapsed().as_secs_f64(),
        reports_sent: sent,
        reports_received: received,
        mismatches,
        timeouts,
        probe_sequence_gaps: harness.probe_sequence_gaps(),
        last_timeout,
        dut_inputs: if dut_counter_reset {
            dut_after.input_count
        } else {
            dut_after.input_count - dut_before.input_count
        },
        dut_ble_queued: if dut_counter_reset {
            dut_after.ble_queued_count
        } else {
            dut_after.ble_queued_count - dut_before.ble_queued_count
        },
        dut_notify_done: if dut_counter_reset {
            dut_after.notify_done_count
        } else {
            dut_after.notify_done_count - dut_before.notify_done_count
        },
        dut_counter_reset,
    })
}

fn read_baseline(path: &Path) -> Result<Option<PerformanceBaseline>> {
    match fs::read(path) {
        Ok(bytes) => {
            let baseline: PerformanceBaseline = serde_json::from_slice(&bytes).context(
                "baseline uses the legacy latency schema; regenerate it with --write-baseline",
            )?;
            ensure!(baseline.schema_version == 2, "unsupported baseline schema");
            Ok(Some(baseline))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

trait ManagementHarness {
    fn send_management(&mut self, command: ManagementCommand) -> Result<u8>;

    fn wait_management_response(
        &self,
        request_id: u8,
        timeout: Duration,
    ) -> Result<ManagementResponse>;
}

impl ManagementHarness for Harness {
    fn send_management(&mut self, command: ManagementCommand) -> Result<u8> {
        Harness::send_management(self, command)
    }

    fn wait_management_response(
        &self,
        request_id: u8,
        timeout: Duration,
    ) -> Result<ManagementResponse> {
        Harness::wait_management_response(self, request_id, timeout)
    }
}

impl ManagementHarness for BridgeHarness {
    fn send_management(&mut self, command: ManagementCommand) -> Result<u8> {
        BridgeHarness::send_management(self, command)
    }

    fn wait_management_response(
        &self,
        request_id: u8,
        timeout: Duration,
    ) -> Result<ManagementResponse> {
        BridgeHarness::wait_management_response(self, request_id, timeout)
    }
}

fn start_linux_pairing<H: ManagementHarness>(harness: &mut H, host_id: HostId) -> Result<()> {
    let request_id = harness.send_management(ManagementCommand::StartPairing(host_id))?;
    let response = harness.wait_management_response(request_id, Duration::from_secs(3))?;
    ensure!(
        response.result == ManagementResult::Ok,
        "DUT rejected Linux pairing request for host {host_id:?}: {:?}",
        response.result
    );
    ensure!(
        matches!(
            response.payload,
            ManagementResponsePayload::Status(status)
                if status.pairing_host == Some(host_id)
        ),
        "DUT did not enter pairing mode for host {host_id:?}"
    );
    Ok(())
}

fn pair_linux_host<F>(address: &str, mut reopen_pairing: F) -> Result<()>
where
    F: FnMut() -> Result<()>,
{
    let mut last_failure = String::from("no pairing attempt was made");
    for attempt in 1..=3 {
        reopen_pairing()?;
        println!("provisioning: LinuxPair attempt {attempt}/3 ({address})");
        // Explicitly select the interactive agent for this attempt. A stale
        // agent from a previous failed run is otherwise a common source of
        // org.bluez.Error.AuthenticationCanceled.
        let _ = bluetoothctl(&["agent", "on"], 5);
        let _ = bluetoothctl(&["default-agent"], 5);
        let mut agent = Command::new("bluetoothctl")
            .args(["--agent", "DisplayYesNo"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start BlueZ confirmation agent")?;
        let mut agent_input = agent.stdin.take().context("open BlueZ agent stdin")?;
        let agent_responder = thread::spawn(move || {
            for _ in 0..160 {
                if agent_input.write_all(b"yes\n").is_err() || agent_input.flush().is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
        });
        thread::sleep(Duration::from_millis(750));
        let pairing = bluetoothctl(&["--timeout", "25", "pair", address], 30);
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = agent_responder.join();
        match pairing {
            Ok(output)
                if output.contains("Pairing successful") || output.contains("already paired") =>
            {
                return Ok(());
            }
            Ok(output) => last_failure = output,
            Err(error) => last_failure = error.to_string(),
        }
        thread::sleep(Duration::from_secs(1));
    }
    bail!("BlueZ did not complete pairing after 3 attempts: {last_failure}")
}

fn run_linux_evdev_test(harness: &mut Harness) -> Result<Vec<TestResult>> {
    bluetoothctl(&["power", "on"], 10)?;
    // A fresh Probe bond is persisted through a planned BLE-stack restart.
    // trouble-host 0.6 releases its single global pairing state on that
    // disconnect, so wait for the bonded Probe transport before pairing the
    // second host.
    thread::sleep(Duration::from_secs(10));
    harness.wait_transport_ready(Duration::from_secs(30))?;
    start_linux_pairing(harness, HostId(2))?;
    BridgeProvisioningStage::LinuxAdvertising.announce();
    let address = discover_hidshift_address()?;
    BridgeProvisioningStage::LinuxPair.announce();
    pair_linux_host(&address, || start_linux_pairing(harness, HostId(2)))?;
    bluetoothctl(&["trust", &address], 10)?;
    // A fresh DUT bond is persisted by a planned BLE stack restart. Let that
    // finish before asking BlueZ to restore the encrypted connection.
    BridgeProvisioningStage::LinuxLink.announce();
    wait_coexist_linux_link(&address, Duration::from_secs(60))?;
    let request_id = harness.send_management(ManagementCommand::SelectHost(HostId(2)))?;
    let response = harness.wait_management_response(request_id, Duration::from_secs(3))?;
    ensure!(
        response.result == ManagementResult::Ok,
        "DUT rejected host 2 selection: {:?}",
        response.result
    );
    thread::sleep(Duration::from_secs(2));

    let devices = find_evdevs("HIDShift", Duration::from_secs(10))?;
    let mut inputs = devices
        .iter()
        .map(|path| open_nonblocking(path))
        .collect::<Result<Vec<_>>>()?;
    for input in &mut inputs {
        drain_file(input);
    }
    harness.send(E2eCommand::Keyboard {
        modifiers: 0,
        keys: [4, 0, 0, 0, 0, 0],
    })?;
    let event = wait_input_event_any(&mut inputs, 1, 30, 1, Duration::from_secs(3))?;
    harness.send(E2eCommand::ReleaseAll)?;
    Ok(vec![TestResult {
        name: "linux_evdev_keyboard".into(),
        passed: true,
        detail: format!(
            "{} HIDShift event devices; type={} code={} value={}",
            devices.len(),
            event.0,
            event.1,
            event.2
        ),
    }])
}

fn discover_hidshift_address() -> Result<String> {
    for attempt in 1..=5 {
        if let Ok(scan) = bluetoothctl(&["--timeout", "8", "scan", "on"], 12)
            && let Some(address) = hidshift_address_from_bluetoothctl(&scan)
        {
            return Ok(address);
        }
        if let Ok(devices) = bluetoothctl(&["devices"], 10)
            && let Some(address) = hidshift_address_from_bluetoothctl(&devices)
            && let Ok(info) = bluetoothctl(&["info", &address], 10)
            && cached_hidshift_bond_is_usable(&info)
        {
            // A cached, bonded device can be restored without a fresh scan.
            // An unbonded cache entry is not an advertisement and must not be
            // handed to `pair`, otherwise BlueZ reports "not available".
            return Ok(address);
        }
        println!("provisioning: LinuxAdvertising retry {attempt}/5");
        thread::sleep(Duration::from_millis(500));
    }
    bail!("bluetoothctl did not discover HIDShift")
}

fn hidshift_address_from_bluetoothctl(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        if !line.contains("HIDShift") {
            return None;
        }
        line.split_whitespace()
            .find(|field| {
                field.len() == 17
                    && field.as_bytes().iter().enumerate().all(|(index, byte)| {
                        if matches!(index, 2 | 5 | 8 | 11 | 14) {
                            *byte == b':'
                        } else {
                            byte.is_ascii_hexdigit()
                        }
                    })
            })
            .map(str::to_owned)
    })
}

fn remove_cached_hidshift_devices() {
    if let Ok(output) = bluetoothctl(&["devices"], 10) {
        for line in output.lines().filter(|line| line.contains("HIDShift")) {
            if let Some(address) = line.split_whitespace().nth(1) {
                let _ = bluetoothctl(&["remove", address], 10);
            }
        }
    }
}

fn bluetoothctl(args: &[&str], timeout_seconds: u64) -> Result<String> {
    let output = Command::new("timeout")
        .arg(format!("{timeout_seconds}s"))
        .arg("bluetoothctl")
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "bluetoothctl {:?} failed:\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn find_evdevs(name: &str, timeout: Duration) -> Result<Vec<PathBuf>> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut devices = Vec::new();
        for entry in fs::read_dir("/sys/class/input")? {
            let entry = entry?;
            let filename = entry.file_name();
            let filename = filename.to_string_lossy();
            if !filename.starts_with("event") {
                continue;
            }
            let device_name =
                fs::read_to_string(entry.path().join("device/name")).unwrap_or_default();
            if device_name.contains(name) {
                devices.push(Path::new("/dev/input").join(filename.as_ref()));
            }
        }
        if !devices.is_empty() {
            devices.sort();
            return Ok(devices);
        }
        if Instant::now() >= deadline {
            bail!("no evdev device containing {name}")
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn find_evdevs_exact(name: &str, timeout: Duration) -> Result<Vec<PathBuf>> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut devices = Vec::new();
        for entry in fs::read_dir("/sys/class/input")? {
            let entry = entry?;
            let filename = entry.file_name();
            let filename = filename.to_string_lossy();
            if !filename.starts_with("event") {
                continue;
            }
            let device_name =
                fs::read_to_string(entry.path().join("device/name")).unwrap_or_default();
            if device_name.trim() == name {
                devices.push(Path::new("/dev/input").join(filename.as_ref()));
            }
        }
        if !devices.is_empty() {
            devices.sort();
            return Ok(devices);
        }
        if Instant::now() >= deadline {
            bail!("no evdev device exactly named {name}")
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn open_nonblocking(path: &Path) -> Result<File> {
    let file = OpenOptions::new().read(true).open(path)?;
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    ensure!(flags >= 0, "F_GETFL failed");
    ensure!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "F_SETFL failed"
    );
    Ok(file)
}

fn open_evdevs_exact_stable(name: &str, timeout: Duration) -> Result<Vec<File>> {
    let deadline = Instant::now() + timeout;
    let mut last_open_error = None;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if let Ok(paths) = find_evdevs_exact(name, remaining.min(Duration::from_secs(1))) {
            match paths
                .iter()
                .map(|path| open_nonblocking(path))
                .collect::<Result<Vec<_>>>()
            {
                Ok(files) if !files.is_empty() => return Ok(files),
                Ok(_) => {}
                Err(error) => last_open_error = Some(error),
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    match last_open_error {
        Some(error) => Err(error).with_context(|| format!("open stable {name} evdev devices")),
        None => bail!("no stable evdev device exactly named {name}"),
    }
}

fn find_hidraw(name: &str, timeout: Duration) -> Result<PathBuf> {
    let deadline = Instant::now() + timeout;
    loop {
        for entry in fs::read_dir("/sys/class/hidraw")? {
            let entry = entry?;
            let uevent = fs::read_to_string(entry.path().join("device/uevent")).unwrap_or_default();
            if uevent.lines().any(|line| {
                line.strip_prefix("HID_NAME=")
                    .is_some_and(|value| value.contains(name))
            }) {
                return Ok(Path::new("/dev").join(entry.file_name()));
            }
        }
        if Instant::now() >= deadline {
            bail!("no hidraw device named {name}")
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn open_hidraw(path: &Path) -> Result<File> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    ensure!(flags >= 0, "F_GETFL failed for {}", path.display());
    ensure!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "F_SETFL failed for {}",
        path.display()
    );
    Ok(file)
}

fn wait_hidraw_report(file: &mut File, report_id: u8, timeout: Duration) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut report = [0u8; 256];
    while Instant::now() < deadline {
        match file.read(&mut report) {
            Ok(len) if len != 0 && report[0] == report_id => return Ok(report[..len].to_vec()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(1));
    }
    bail!("timed out waiting for hidraw report ID {report_id}")
}

fn wait_hidraw_vendor_pattern(
    file: &mut File,
    report_id: u8,
    seed: u8,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let report = wait_hidraw_report(file, report_id, remaining)?;
        if report.len() == 64
            && report[1..]
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == seed.wrapping_add(index as u8))
        {
            return Ok(report);
        }
    }
    bail!("timed out waiting for vendor payload seed {seed:#04x}")
}

const fn hidraw_ioctl(direction: u64, number: u64, size: usize) -> libc::c_ulong {
    ((direction << 30) | ((b'H' as u64) << 8) | number | ((size as u64) << 16)) as libc::c_ulong
}

fn hidraw_set_feature(file: &File, report: &mut [u8]) -> Result<()> {
    let result = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            hidraw_ioctl(3, 0x06, report.len()),
            report.as_mut_ptr(),
        )
    };
    ensure!(
        result >= 0,
        "HIDIOCSFEATURE failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

fn hidraw_get_feature_retry(
    file: &File,
    report_id: u8,
    len: usize,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut report = vec![0u8; len];
        report[0] = report_id;
        let result = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                hidraw_ioctl(3, 0x07, len),
                report.as_mut_ptr(),
            )
        };
        if result >= 0 {
            report.truncate(result as usize);
            return Ok(report);
        }
        if Instant::now() >= deadline {
            bail!("HIDIOCGFEATURE failed: {}", std::io::Error::last_os_error())
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn drain_file(file: &mut File) {
    let mut buffer = [0; 256];
    while file.read(&mut buffer).is_ok() {}
}

fn wait_input_event(
    file: &mut File,
    expected_type: u16,
    expected_code: u16,
    expected_value: i32,
    timeout: Duration,
) -> Result<(u16, u16, i32)> {
    let deadline = Instant::now() + timeout;
    let event_len = std::mem::size_of::<libc::timeval>() + 8;
    let mut pending = Vec::new();
    while Instant::now() < deadline {
        let mut buffer = [0; 256];
        match file.read(&mut buffer) {
            Ok(count) => pending.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error.into()),
        }
        while pending.len() >= event_len {
            let offset = std::mem::size_of::<libc::timeval>();
            let event_type = u16::from_ne_bytes([pending[offset], pending[offset + 1]]);
            let code = u16::from_ne_bytes([pending[offset + 2], pending[offset + 3]]);
            let value = i32::from_ne_bytes([
                pending[offset + 4],
                pending[offset + 5],
                pending[offset + 6],
                pending[offset + 7],
            ]);
            pending.drain(..event_len);
            if (event_type, code, value) == (expected_type, expected_code, expected_value) {
                return Ok((event_type, code, value));
            }
        }
    }
    bail!(
        "timed out waiting for evdev type={expected_type} code={expected_code} value={expected_value}"
    )
}

fn wait_input_event_any(
    files: &mut [File],
    expected_type: u16,
    expected_code: u16,
    expected_value: i32,
    timeout: Duration,
) -> Result<(u16, u16, i32)> {
    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        for file in files.iter_mut() {
            if let Ok(event) = wait_input_event(
                file,
                expected_type,
                expected_code,
                expected_value,
                remaining.min(Duration::from_millis(10)),
            ) {
                return Ok(event);
            }
        }
    }
    bail!(
        "timed out waiting on all evdev devices for type={expected_type} code={expected_code} value={expected_value}"
    )
}

fn wait_bridge_key_tap(files: &mut [File], expected_code: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let event_len = std::mem::size_of::<libc::timeval>() + 8;
    let mut pending = (0..files.len()).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut pressed = false;
    while Instant::now() < deadline {
        for (index, file) in files.iter_mut().enumerate() {
            let mut buffer = [0; 256];
            match file.read(&mut buffer) {
                Ok(count) => pending[index].extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            while pending[index].len() >= event_len {
                let offset = std::mem::size_of::<libc::timeval>();
                let event_type =
                    u16::from_ne_bytes([pending[index][offset], pending[index][offset + 1]]);
                let code =
                    u16::from_ne_bytes([pending[index][offset + 2], pending[index][offset + 3]]);
                let value = i32::from_ne_bytes([
                    pending[index][offset + 4],
                    pending[index][offset + 5],
                    pending[index][offset + 6],
                    pending[index][offset + 7],
                ]);
                pending[index].drain(..event_len);
                if event_type == 1 && code == expected_code {
                    if value == 1 {
                        pressed = true;
                    } else if value == 0 && pressed {
                        return Ok(());
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    bail!("timed out waiting for recovered key tap code={expected_code}, pressed={pressed}")
}

fn drain_for(receiver: &Receiver<SerialLine>, duration: Duration) {
    let deadline = Instant::now() + duration;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if receiver.recv_timeout(remaining).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_notification_parser_ignores_logger_prefix() {
        assert_eq!(
            parse_probe_notification(
                "INFO - @HIDSHIFT-PROBE:NOTIFY,keyboard,12345,[00, 00, 04, 00]"
            ),
            Some(ProbeNotification {
                sequence: None,
                kind: "keyboard".into(),
                probe_us: Some(12345),
                bytes: vec![0, 0, 4, 0],
            })
        );
        assert_eq!(
            parse_probe_notification("@HIDSHIFT-PROBE:N,k,12345,00000400"),
            Some(ProbeNotification {
                sequence: None,
                kind: "keyboard".into(),
                probe_us: Some(12345),
                bytes: vec![0, 0, 4, 0],
            })
        );
        assert_eq!(
            parse_probe_notification("@N:k:00000400"),
            Some(ProbeNotification {
                sequence: None,
                kind: "keyboard".into(),
                probe_us: None,
                bytes: vec![0, 0, 4, 0],
            })
        );
        assert_eq!(
            parse_probe_notification("@N:k:000000000001e240:00000400"),
            Some(ProbeNotification {
                sequence: None,
                kind: "keyboard".into(),
                probe_us: Some(123456),
                bytes: vec![0, 0, 4, 0],
            })
        );
        assert_eq!(
            parse_probe_notification("@N:0000002a:k:000000000001e240:00000400"),
            Some(ProbeNotification {
                sequence: Some(42),
                kind: "keyboard".into(),
                probe_us: Some(123456),
                bytes: vec![0, 0, 4, 0],
            })
        );
    }

    #[test]
    fn probe_clock_parser_and_mapping_use_probe_receive_time() {
        assert_eq!(
            parse_probe_clock_response("@T:0000002a:000000000001e240"),
            Some((42, 123456))
        );
        let host_anchor = Instant::now();
        let sync = DeviceClockSync {
            host_anchor,
            probe_anchor_us: 1_000_000,
            round_trip: Duration::from_millis(2),
        };
        assert_eq!(
            sync.host_time(1_007_500),
            Some(host_anchor + Duration::from_micros(7_500))
        );
        assert_eq!(
            sync.host_time(999_000),
            Some(host_anchor - Duration::from_millis(1))
        );
        assert_eq!(
            sync.duration_since_synced(
                host_anchor + Duration::from_micros(900),
                host_anchor + Duration::from_millis(1),
            ),
            Some(Duration::ZERO)
        );
        assert_eq!(
            sync.duration_since_synced(
                host_anchor + Duration::from_micros(1_100),
                host_anchor + Duration::from_millis(1),
            ),
            Some(Duration::from_micros(100))
        );
        assert_eq!(
            sync.duration_since_synced(
                host_anchor - Duration::from_micros(1_001),
                host_anchor + Duration::from_micros(1),
            ),
            None
        );
    }

    #[test]
    fn latency_sample_phase_spans_one_connection_interval() {
        let delays = (0..20)
            .map(|index| sample_phase_delay(index).as_micros())
            .collect::<Vec<_>>();
        assert!(delays.iter().all(|delay| *delay >= 137 && *delay < 7_637));
        assert!(delays.iter().copied().min().unwrap() < 1_000);
        assert!(delays.iter().copied().max().unwrap() > 7_000);
    }

    #[test]
    fn coexist_link_gate_requires_7500us_latency_19_and_2m_phy() {
        let low_latency = BleLinkTelemetry {
            connected: true,
            connection_interval_us: 7_500,
            peripheral_latency: 19,
            supervision_timeout_ms: 2_000,
            tx_phy: 2,
            rx_phy: 2,
            parameter_updates: 1,
            phy_updates: 1,
        };
        assert!(coexist_ble_link_is_low_latency(low_latency));
        assert!(!coexist_ble_link_is_low_latency(BleLinkTelemetry {
            connection_interval_us: 15_000,
            ..low_latency
        }));
        assert!(!coexist_ble_link_is_low_latency(BleLinkTelemetry {
            peripheral_latency: 18,
            ..low_latency
        }));
        assert!(!coexist_ble_link_is_low_latency(BleLinkTelemetry {
            rx_phy: 1,
            ..low_latency
        }));
    }

    #[test]
    fn dut_timestamp_parsers_accept_logger_prefixes() {
        assert_eq!(
            parse_dut_queued_timestamp("INFO - @HIDSHIFT-E2E:QUEUED,42,123456"),
            Some((42, 123456))
        );
        assert_eq!(
            parse_dut_input_timestamp(
                "INFO - @HIDSHIFT-E2E:STAMP,43,42,123400,123450,123460,123465,123470,123480,123490,7,6,5,1,7500,0,2000,2,2,1,1,123495,123491,123493"
            ),
            Some(DutInputTimestamps {
                query_sequence: 43,
                input_sequence: 42,
                ingress_us: 123400,
                runtime_us: 123450,
                runtime_dispatch_us: 123460,
                ble_queued_us: 123465,
                ble_receive_us: 123470,
                notify_start_us: 123480,
                notify_done_us: 123490,
                hci_submit_us: 123495,
                hci_dequeue_us: 123491,
                hci_credit_us: 123493,
                input_count: 7,
                ble_queued_count: 6,
                notify_done_count: 5,
                ble_link: BleLinkTelemetry {
                    connected: true,
                    connection_interval_us: 7_500,
                    peripheral_latency: 0,
                    supervision_timeout_ms: 2_000,
                    tx_phy: 2,
                    rx_phy: 2,
                    parameter_updates: 1,
                    phy_updates: 1,
                },
            })
        );
    }

    #[test]
    fn bridge_timestamp_parser_includes_device_pipeline_timestamps() {
        assert_eq!(
            parse_bridge_timestamp(
                "INFO - @HIDSHIFT-BRIDGE:STAMP,9,77,8,100,101,102,103,110,8,200,201,202"
            ),
            Some((
                9,
                BridgeTimestamp {
                    host_session_id: 77,
                    input_sequence: 8,
                    ingress_us: 100,
                    enqueue_us: 101,
                    dequeue_us: 102,
                    send_start_us: 103,
                    tx_done_us: 110,
                    device_sequence: 8,
                    device_radio_rx_us: 200,
                    device_reassembled_us: 201,
                    device_hid_write_us: 202,
                }
            ))
        );
    }

    #[test]
    fn bridge_clock_parser_includes_boot_session() {
        assert_eq!(
            parse_bridge_clock("INFO - @HIDSHIFT-BRIDGE:CLOCK,9,77,88,123456"),
            Some((9, 77, 88, 123456))
        );
        assert_eq!(parse_bridge_clock("@HIDSHIFT-BRIDGE:CLOCK,9,77"), None);
    }

    #[test]
    fn bridge_stability_rejects_a_reboot_even_when_all_reports_arrive() {
        let stable = BridgeStabilityStats {
            duration_seconds: 120.0,
            reports_sent: 4_000,
            reports_received: 4_000,
            timeouts: 0,
            host_session_before: 10,
            host_session_after: 10,
            device_session_before: 20,
            device_session_after: 20,
        };
        assert!(bridge_stability_passes(&stable));

        let rebooted_host = BridgeStabilityStats {
            host_session_after: 11,
            ..stable
        };
        assert!(!bridge_stability_passes(&rebooted_host));
        let rebooted_device = BridgeStabilityStats {
            device_session_after: 21,
            ..stable
        };
        assert!(!bridge_stability_passes(&rebooted_device));
    }

    #[test]
    fn bridge_device_telemetry_coverage_scales_with_latency_sample_count() {
        assert_eq!(bridge_device_telemetry_samples_required(1), 1);
        assert_eq!(bridge_device_telemetry_samples_required(200), 7);
        assert_eq!(bridge_device_telemetry_samples_required(500), 16);
    }

    #[test]
    fn bridge_boot_parser_preserves_reset_reason_and_brownout_flag() {
        assert_eq!(
            parse_bridge_boot("INFO - @HIDSHIFT-BRIDGE:BOOT,77,15,1"),
            Some((77, 0x0f, true))
        );
        assert_eq!(parse_bridge_boot("@HIDSHIFT-BRIDGE:BOOT,77,3,2"), None);
    }

    #[test]
    fn bridge_timestamp_is_incomplete_until_send_callback_finishes() {
        let incomplete = BridgeTimestamp {
            host_session_id: 77,
            input_sequence: 8,
            ingress_us: 100,
            enqueue_us: 101,
            dequeue_us: 102,
            send_start_us: 103,
            tx_done_us: 0,
            device_sequence: 8,
            device_radio_rx_us: 200,
            device_reassembled_us: 201,
            device_hid_write_us: 202,
        };
        assert!(!bridge_host_timestamp_complete(&incomplete));
        assert!(bridge_host_timestamp_complete(&BridgeTimestamp {
            tx_done_us: 110,
            ..incomplete
        }));
    }

    #[test]
    fn wired_management_response_parser_checks_protocol_bytes() {
        let expected = ManagementResponse {
            request_id: 9,
            result: ManagementResult::Ok,
            payload: ManagementResponsePayload::None,
        };
        let encoded = expected
            .encode()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            parse_management_response(&format!("INFO - @HIDSHIFT:{encoded}")),
            Some(expected)
        );
    }

    #[test]
    fn bluetoothctl_scan_parser_accepts_colored_event_prefixes() {
        assert_eq!(
            hidshift_address_from_bluetoothctl("[NEW] Device 02:00:00:00:00:01 HIDShift"),
            Some("02:00:00:00:00:01".into())
        );
    }

    #[test]
    fn coexist_skip_flash_reuses_only_a_complete_linux_bond() {
        let complete = "Paired: yes\nBonded: yes\nConnected: no\n";
        assert!(cached_hidshift_bond_is_usable(complete));
        assert!(!coexist_linux_link_is_ready(complete));
        assert!(coexist_linux_link_is_ready(
            "Paired: yes\nBonded: yes\nConnected: yes\n"
        ));
        assert!(!cached_hidshift_bond_is_usable("Paired: yes\nBonded: no\n"));
        assert!(!cached_hidshift_bond_is_usable("Paired: no\nBonded: yes\n"));
    }

    #[test]
    fn dut_log_parser_extracts_controller_address() {
        assert_eq!(
            device_address_from_log("INFO - [host] Device Address 02:00:00:00:00:01\r\n"),
            Some("02:00:00:00:00:01".into())
        );
    }

    #[test]
    fn linux_controller_parser_uses_public_controller_line() {
        let output = "Controller 02:00:00:00:00:02 host [default]";
        let address = output.lines().find_map(|line| {
            line.strip_prefix("Controller ")
                .and_then(|rest| rest.split_whitespace().next())
        });
        assert_eq!(address, Some("02:00:00:00:00:02"));
    }

    #[test]
    fn bridge_latency_measurement_reverses_first_device_each_round() {
        assert!(bridge_measurement_keyboard_first(0));
        assert!(!bridge_measurement_keyboard_first(1));
        assert!(bridge_measurement_keyboard_first(2));
        assert!(!bridge_measurement_keyboard_first(3));
    }

    #[test]
    fn bridge_latency_releases_only_the_keyboard_lane() {
        assert_eq!(
            bridge_keyboard_release_command(),
            E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0; 6],
            }
        );
    }

    #[test]
    fn bridge_mouse_latency_accepts_cumulative_recovery() {
        assert!(bridge_mouse_value_matches(7, 7));
        assert!(bridge_mouse_value_matches(14, 7));
        assert!(!bridge_mouse_value_matches(6, 7));
        assert!(bridge_mouse_value_matches(-14, -7));
        assert!(!bridge_mouse_value_matches(-6, -7));
    }

    #[test]
    fn cumulative_motion_recovery_accepts_split_evdev_deltas() {
        let mut positive = 0;
        assert!(!cumulative_delta_reached(&mut positive, 7, 18));
        assert!(cumulative_delta_reached(&mut positive, 11, 18));

        let mut negative = 0;
        assert!(!cumulative_delta_reached(&mut negative, -7, -18));
        assert!(cumulative_delta_reached(&mut negative, -11, -18));
    }
}
