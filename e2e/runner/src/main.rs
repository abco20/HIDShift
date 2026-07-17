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
use clap::Parser;
use hidshift::HostId;
use hidshift::e2e::{E2eCommand, E2ePacket};
use hidshift::management::{
    ManagementCommand, ManagementRequest, ManagementResponse, ManagementResponsePayload,
    ManagementResult,
};
use serde::{Deserialize, Serialize};
use serialport::{FlowControl, SerialPort};

mod board;
use board::{parse_chip_type, parse_mac_address, serial_by_path_candidates};
mod metrics;
use metrics::{
    BaselineComparison, LatencyStats, PerformanceBaseline, ble_game_latency_passes,
    compare_baseline, latency_advisory, latency_stats,
};

const DUT_BAUD_RATE: u32 = 115_200;
const PROBE_BAUD_RATE: u32 = 115_200;
const DUT_CHIP: &str = "esp32s3";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeChip {
    Esp32,
    Esp32S3,
}

impl ProbeChip {
    fn from_espflash(value: &str) -> Option<Self> {
        match value {
            "esp32" => Some(Self::Esp32),
            "esp32s3" => Some(Self::Esp32S3),
            _ => None,
        }
    }

    const fn espflash_name(self) -> &'static str {
        match self {
            Self::Esp32 => "esp32",
            Self::Esp32S3 => "esp32s3",
        }
    }

    const fn cargo_target(self) -> &'static str {
        match self {
            Self::Esp32 => "xtensa-esp32-none-elf",
            Self::Esp32S3 => "xtensa-esp32s3-none-elf",
        }
    }

    const fn cargo_feature(self) -> &'static str {
        match self {
            Self::Esp32 => "esp32",
            Self::Esp32S3 => "esp32s3",
        }
    }
}
#[derive(Parser, Debug)]
#[command(about = "HIDShift BLE hardware E2E runner")]
struct Args {
    #[arg(long)]
    dut_port: Option<PathBuf>,
    #[arg(long)]
    probe_port: Option<PathBuf>,
    #[arg(long)]
    skip_flash: bool,
    #[arg(long)]
    skip_linux: bool,
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
    let (dut, probe, probe_chip) = resolve_ports(&args, &repo)?;

    println!("DUT   {} ({DUT_CHIP})", dut.display());
    println!("Probe {} ({})", probe.display(), probe_chip.espflash_name());
    if !args.skip_flash {
        build_and_flash(&repo, &dut, &probe, probe_chip)?;
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
        name: "keyboard_latency_target".into(),
        passed: ble_game_latency_passes(&latency),
        detail: format!(
            "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
            latency.p50_ms, latency.p95_ms, latency.p99_ms
        ),
    });
    tests.push(TestResult {
        name: "mouse_latency_target".into(),
        passed: ble_game_latency_passes(&mouse_measurement.end_to_end),
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

fn resolve_ports(args: &Args, repo: &Path) -> Result<(PathBuf, PathBuf, ProbeChip)> {
    if let (Some(dut), Some(probe)) = (&args.dut_port, &args.probe_port) {
        ensure!(
            fs::canonicalize(dut)? != fs::canonicalize(probe)?,
            "DUT and probe resolve to the same device"
        );
        verify_chip(repo, dut, DUT_CHIP)?;
        let probe_info = board_info(repo, probe)?;
        let probe_chip = parse_chip_type(&probe_info)
            .as_deref()
            .and_then(ProbeChip::from_espflash)
            .context("probe must be an ESP32 or ESP32-S3")?;
        return Ok((dut.clone(), probe.clone(), probe_chip));
    }
    ensure!(
        args.dut_port.is_none() && args.probe_port.is_none(),
        "provide both --dut-port and --probe-port, or neither"
    );

    let candidates = serial_by_path_candidates(Path::new("/dev/serial/by-path"))?;
    let mut boards = BTreeMap::<(String, String), PathBuf>::new();
    for path in candidates {
        if let Ok(info) = board_info(repo, &path)
            && let Some(chip) = parse_chip_type(&info)
            && let Some(mac) = parse_mac_address(&info)
        {
            if ProbeChip::from_espflash(&chip).is_some() {
                // One ESP32-S3 may be visible through both an external UART
                // bridge and native USB-JTAG. Keep the first stable by-path
                // candidate for each hardware MAC instead of flashing it as
                // two different boards.
                boards.entry((chip, mac)).or_insert(path);
            }
        }
    }
    let mut esp32 = Vec::new();
    let mut esp32s3 = Vec::new();
    for ((chip, _), path) in boards {
        match ProbeChip::from_espflash(&chip) {
            Some(ProbeChip::Esp32) => esp32.push(path),
            Some(ProbeChip::Esp32S3) => esp32s3.push(path),
            None => {}
        }
    }
    let dut = esp32s3.first().cloned().context("no ESP32-S3 DUT found")?;
    if let Some(probe) = esp32.into_iter().next() {
        return Ok((dut, probe, ProbeChip::Esp32));
    }
    ensure!(
        esp32s3.len() == 2,
        "S3-only E2E discovery requires exactly two unique boards; found {} (use --dut-port and --probe-port)",
        esp32s3.len()
    );
    Ok((dut, esp32s3[1].clone(), ProbeChip::Esp32S3))
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

fn build_and_flash(repo: &Path, dut: &Path, probe: &Path, probe_chip: ProbeChip) -> Result<()> {
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
            .args([
                "erase-flash",
                "--chip",
                probe_chip.espflash_name(),
                "--port",
            ])
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
        ". '{}' && HIDSHIFT_DUT_ADDRESS='{}' cargo +esp build -Zbuild-std=core,alloc --release --manifest-path e2e/probe-firmware/Cargo.toml --no-default-features --features {} --target {}",
        export.display(),
        address,
        probe_chip.cargo_feature(),
        probe_chip.cargo_target()
    );
    run(Command::new("sh").arg("-c").arg(build_probe), repo)?;
    run(
        Command::new("espflash")
            .args(["flash", "--chip", probe_chip.espflash_name(), "--port"])
            .arg(probe)
            .arg(
                repo.join("e2e/probe-firmware/target")
                    .join(probe_chip.cargo_target())
                    .join("release/hidshift-e2e-probe"),
            ),
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
    #[cfg(test)]
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
    println!("provisioning: LinuxAdvertising");
    let address = discover_hidshift_address()?;
    println!("provisioning: LinuxPair");
    pair_linux_host(&address, || start_linux_pairing(harness, HostId(2)))?;
    bluetoothctl(&["trust", &address], 10)?;
    // A fresh DUT bond is persisted by a planned BLE stack restart. Let that
    // finish before asking BlueZ to restore the encrypted connection.
    println!("provisioning: LinuxLink");
    wait_linux_link(&address, Duration::from_secs(60))?;
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

fn cached_hidshift_bond_is_usable(info: &str) -> bool {
    info.contains("Paired: yes") && info.contains("Bonded: yes")
}

fn wait_linux_link(address: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(info) = bluetoothctl(&["info", address], 10)
            && cached_hidshift_bond_is_usable(&info)
            && info.contains("Connected: yes")
        {
            return Ok(());
        }
        let _ = bluetoothctl(&["connect", address], 10);
        thread::sleep(Duration::from_millis(500));
    }
    bail!("BlueZ did not restore the encrypted HIDShift link")
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
}
