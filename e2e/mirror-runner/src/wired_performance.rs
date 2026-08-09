use std::collections::VecDeque;
use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hidshift::e2e::{E2eCommand, E2ePacket};
use serde::Serialize;
use serialport::SerialPort;

use super::{send_normalized, wait_for_line_containing};

const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const KEY_A: u16 = 30;
const REL_X: u16 = 0;
const MOUSE_STREAM_REPORTS: u16 = 1_000;
const MOUSE_STREAM_INTERVAL_US: u16 = 1_000;
const MOUSE_STREAM_X: i16 = 200;
const BATCHED_INTERVAL_MS: f64 = 0.5;
const MIN_MOUSE_STREAM_SOURCE_RATE_HZ: f64 = 900.0;
const MAX_MOUSE_STREAM_DELIVERY_MS: f64 = 1_500.0;
const MAX_MOUSE_STREAM_INTERARRIVAL_P95_MS: f64 = 5.0;

#[derive(Debug, Serialize)]
struct WiredPerformanceReport {
    schema_version: u8,
    unix_time_seconds: u64,
    path: &'static str,
    keyboard_linux_observed: LatencyStats,
    mouse_linux_observed: LatencyStats,
    mouse_stream: MouseStreamStats,
}

#[derive(Clone, Debug, Serialize)]
struct LatencyStats {
    samples: usize,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct MouseStreamStats {
    requested_reports: u16,
    expected_x: i64,
    observed_x: i64,
    evdev_events: usize,
    source_duration_ms: f64,
    source_rate_hz: f64,
    linux_delivery_duration_ms: f64,
    evdev_interarrival: LatencyStats,
    batched_intervals: usize,
}

#[derive(Clone, Copy, Debug)]
struct RawInputEvent {
    event_type: u16,
    code: u16,
    value: i32,
    kernel_time_us: i64,
}

#[derive(Clone, Copy, Debug)]
struct ObservedInputEvent {
    raw: RawInputEvent,
    received: Instant,
}

struct InputFile {
    file: File,
    pending: Vec<u8>,
}

struct InputObserver {
    inputs: Vec<InputFile>,
    events: VecDeque<ObservedInputEvent>,
}

impl InputObserver {
    fn new(files: Vec<File>) -> Self {
        Self {
            inputs: files
                .into_iter()
                .map(|file| InputFile {
                    file,
                    pending: Vec::new(),
                })
                .collect(),
            events: VecDeque::new(),
        }
    }

    fn drain(&mut self) -> Result<(), Box<dyn Error>> {
        self.events.clear();
        for input in &mut self.inputs {
            input.pending.clear();
            let mut bytes = [0; 256];
            loop {
                match input.file.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }

    fn wait_for(
        &mut self,
        event_type: u16,
        code: u16,
        value: i32,
        timeout: Duration,
    ) -> Result<ObservedInputEvent, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(event) = self.events.pop_front() {
                if (event.raw.event_type, event.raw.code, event.raw.value)
                    == (event_type, code, value)
                {
                    return Ok(event);
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timeout waiting for evdev type {event_type} code {code} value {value}"
                )
                .into());
            }
            self.read_available()?;
            if self.events.is_empty() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn collect_relative_sum(
        &mut self,
        code: u16,
        expected: i64,
        timeout: Duration,
    ) -> Result<Vec<ObservedInputEvent>, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        let mut total = 0i64;
        let mut events = Vec::new();
        while total != expected {
            if Instant::now() >= deadline {
                return Err(format!(
                    "timeout waiting for relative movement: observed {total}/{expected}"
                )
                .into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = self.wait_for_any(remaining).map_err(|error| {
                format!(
                    "{error}: observed relative movement {total}/{expected}"
                )
            })?;
            if (event.raw.event_type, event.raw.code) != (EV_REL, code) {
                continue;
            }
            total += i64::from(event.raw.value);
            events.push(event);
            if total > expected {
                return Err(format!(
                    "relative movement exceeded expectation: observed {total}/{expected}"
                )
                .into());
            }
        }
        Ok(events)
    }

    fn wait_for_any(&mut self, timeout: Duration) -> Result<ObservedInputEvent, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }
            if Instant::now() >= deadline {
                return Err("timeout waiting for evdev input".into());
            }
            self.read_available()?;
            if self.events.is_empty() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn read_available(&mut self) -> Result<(), Box<dyn Error>> {
        for input in &mut self.inputs {
            let mut bytes = [0; 256];
            loop {
                match input.file.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(length) => {
                        input.pending.extend_from_slice(&bytes[..length]);
                        let received = Instant::now();
                        self.events.extend(
                            decode_input_events(&mut input.pending)
                                .into_iter()
                                .map(|raw| ObservedInputEvent { raw, received }),
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }
}

pub(super) fn run(
    serial: &mut dyn SerialPort,
    files: Vec<File>,
    latency_samples: usize,
    results_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    if latency_samples == 0 {
        return Err("--latency-samples must be positive".into());
    }
    let mut input = InputObserver::new(files);
    let mut sequence = 20_000;
    send(serial, &mut sequence, E2eCommand::ReleaseAll)?;
    std::thread::sleep(Duration::from_millis(50));
    input.drain()?;

    let (keyboard, mouse) = measure_latency(serial, &mut input, &mut sequence, latency_samples)?;
    let mouse_stream = measure_mouse_stream(serial, &mut input, &mut sequence)?;
    let unix_time_seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let report = WiredPerformanceReport {
        schema_version: 1,
        unix_time_seconds,
        path: "Host normalized input -> SPI -> Device production firmware -> USB HID -> Linux evdev",
        keyboard_linux_observed: keyboard,
        mouse_linux_observed: mouse,
        mouse_stream,
    };
    fs::create_dir_all(results_dir)?;
    let result_path = results_dir.join(format!("{unix_time_seconds}-dual-wired.json"));
    fs::write(&result_path, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    println!("result: {}", result_path.display());
    if !mouse_stream_integrity_passes(&report.mouse_stream) {
        return Err(format!(
            "wired mouse stream failed: x={}/{}, source={:.1} Hz, delivery={:.1} ms, interarrival_p95={:.3} ms",
            report.mouse_stream.observed_x,
            report.mouse_stream.expected_x,
            report.mouse_stream.source_rate_hz,
            report.mouse_stream.linux_delivery_duration_ms,
            report.mouse_stream.evdev_interarrival.p95_ms,
        )
        .into());
    }
    Ok(())
}

fn measure_latency(
    serial: &mut dyn SerialPort,
    input: &mut InputObserver,
    sequence: &mut u32,
    samples: usize,
) -> Result<(LatencyStats, LatencyStats), Box<dyn Error>> {
    let mut keyboard = Vec::with_capacity(samples * 2);
    let mut mouse = Vec::with_capacity(samples);
    input.drain()?;
    for index in 0..samples {
        std::thread::sleep(sample_phase_delay(index * 3));
        let started = send(
            serial,
            sequence,
            E2eCommand::Keyboard {
                modifiers: 0,
                keys: [4, 0, 0, 0, 0, 0],
            },
        )?;
        let event = input
            .wait_for(EV_KEY, KEY_A, 1, Duration::from_secs(2))
            .map_err(|error| format!("keyboard press sample {index}: {error}"))?;
        keyboard.push(event.received.duration_since(started).as_secs_f64() * 1_000.0);

        std::thread::sleep(sample_phase_delay(index * 3 + 1));
        let started = send(
            serial,
            sequence,
            E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0; 6],
            },
        )?;
        let event = input
            .wait_for(EV_KEY, KEY_A, 0, Duration::from_secs(2))
            .map_err(|error| format!("keyboard release sample {index}: {error}"))?;
        keyboard.push(event.received.duration_since(started).as_secs_f64() * 1_000.0);

        let x = if index.is_multiple_of(2) { 1 } else { -1 };
        std::thread::sleep(sample_phase_delay(index * 3 + 2));
        let started = send(
            serial,
            sequence,
            E2eCommand::Mouse {
                buttons: 0,
                x,
                y: 0,
                wheel: 0,
                pan: 0,
            },
        )?;
        let event = input
            .wait_for(EV_REL, REL_X, i32::from(x), Duration::from_secs(2))
            .map_err(|error| format!("mouse sample {index}: {error}"))?;
        mouse.push(event.received.duration_since(started).as_secs_f64() * 1_000.0);
    }
    Ok((latency_stats(keyboard), latency_stats(mouse)))
}

fn measure_mouse_stream(
    serial: &mut dyn SerialPort,
    input: &mut InputObserver,
    sequence: &mut u32,
) -> Result<MouseStreamStats, Box<dyn Error>> {
    input.drain()?;
    let mut serial_reader = serial.try_clone()?;
    let expected_x = i64::from(MOUSE_STREAM_REPORTS) * i64::from(MOUSE_STREAM_X);
    let stream_sequence = *sequence;
    let started = send(
        serial,
        sequence,
        E2eCommand::MouseStream {
            reports: MOUSE_STREAM_REPORTS,
            interval_us: MOUSE_STREAM_INTERVAL_US,
            x: MOUSE_STREAM_X,
            y: 0,
        },
    )?;
    let events = match input.collect_relative_sum(REL_X, expected_x, Duration::from_secs(10)) {
        Ok(events) => events,
        Err(error) => {
            let marker = format!("@HIDSHIFT-E2E:STREAM,{stream_sequence},");
            let source = wait_for_line_containing(
                &mut *serial_reader,
                marker.as_bytes(),
                Duration::from_secs(1),
            )
            .map_or_else(|_| "no source completion marker".to_owned(), |line| line);
            return Err(format!("1 kHz mouse stream: {error}; {source}").into());
        }
    };
    let delivered = events
        .last()
        .map(|event| event.received.duration_since(started))
        .unwrap_or_default();
    let marker = format!("@HIDSHIFT-E2E:STREAM,{stream_sequence},");
    let line = wait_for_line_containing(&mut *serial_reader, marker.as_bytes(), Duration::from_secs(5))?;
    let source_duration_us = parse_stream_duration(&line, stream_sequence)?;
    let interarrival = events
        .windows(2)
        .map(|pair| {
            (pair[1].raw.kernel_time_us - pair[0].raw.kernel_time_us).max(0) as f64 / 1_000.0
        })
        .collect::<Vec<_>>();
    let batched_intervals = interarrival
        .iter()
        .filter(|duration| **duration < BATCHED_INTERVAL_MS)
        .count();
    let observed_x = events.iter().map(|event| i64::from(event.raw.value)).sum();
    Ok(MouseStreamStats {
        requested_reports: MOUSE_STREAM_REPORTS,
        expected_x,
        observed_x,
        evdev_events: events.len(),
        source_duration_ms: source_duration_us as f64 / 1_000.0,
        source_rate_hz: f64::from(MOUSE_STREAM_REPORTS) * 1_000_000.0 / source_duration_us as f64,
        linux_delivery_duration_ms: delivered.as_secs_f64() * 1_000.0,
        evdev_interarrival: latency_stats(interarrival),
        batched_intervals,
    })
}

fn send(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
    command: E2eCommand,
) -> Result<Instant, Box<dyn Error>> {
    let started = Instant::now();
    send_normalized(
        serial,
        E2ePacket {
            sequence: *sequence,
            command,
        },
    )?;
    *sequence = sequence.wrapping_add(1);
    Ok(started)
}

fn sample_phase_delay(index: usize) -> Duration {
    Duration::from_micros(((index as u64 * 379) % 1_000) + 31)
}

fn parse_stream_duration(line: &str, sequence: u32) -> Result<u64, Box<dyn Error>> {
    let marker = format!("@HIDSHIFT-E2E:STREAM,{sequence},");
    let body = line
        .split_once(&marker)
        .map(|(_, body)| body)
        .ok_or("mouse stream completion did not match its sequence")?;
    let mut fields = body.trim().split(',');
    let reports = fields
        .next()
        .ok_or("missing stream report count")?
        .parse::<u16>()?;
    if reports != MOUSE_STREAM_REPORTS {
        return Err(format!("stream completed {reports}/{MOUSE_STREAM_REPORTS} reports").into());
    }
    let duration = fields
        .next()
        .ok_or("missing stream duration")?
        .parse::<u64>()?;
    if duration == 0 || fields.next().is_some() {
        return Err("invalid stream duration response".into());
    }
    Ok(duration)
}

fn decode_input_events(pending: &mut Vec<u8>) -> Vec<RawInputEvent> {
    let event_len = std::mem::size_of::<libc::timeval>() + 8;
    let complete = pending.len() / event_len;
    let mut events = Vec::with_capacity(complete);
    for event in pending[..complete * event_len].chunks_exact(event_len) {
        let offset = std::mem::size_of::<libc::timeval>();
        events.push(RawInputEvent {
            event_type: u16::from_ne_bytes([event[offset], event[offset + 1]]),
            code: u16::from_ne_bytes([event[offset + 2], event[offset + 3]]),
            value: i32::from_ne_bytes([
                event[offset + 4],
                event[offset + 5],
                event[offset + 6],
                event[offset + 7],
            ]),
            kernel_time_us: input_event_time_us(event),
        });
    }
    pending.drain(..complete * event_len);
    events
}

fn input_event_time_us(event: &[u8]) -> i64 {
    // SAFETY: decode_input_events passes one complete Linux input_event. The
    // compact byte buffer may be unaligned, so read_unaligned is required.
    let timestamp = unsafe { std::ptr::read_unaligned(event.as_ptr().cast::<libc::timeval>()) };
    timestamp.tv_sec.saturating_mul(1_000_000) + timestamp.tv_usec
}

fn latency_stats(mut values: Vec<f64>) -> LatencyStats {
    values.sort_by(f64::total_cmp);
    let samples = values.len();
    LatencyStats {
        samples,
        mean_ms: values.iter().sum::<f64>() / samples.max(1) as f64,
        p50_ms: percentile(&values, 0.50),
        p95_ms: percentile(&values, 0.95),
        p99_ms: percentile(&values, 0.99),
        max_ms: values.last().copied().unwrap_or_default(),
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn mouse_stream_integrity_passes(stats: &MouseStreamStats) -> bool {
    stats.observed_x == stats.expected_x
        && stats.source_rate_hz >= MIN_MOUSE_STREAM_SOURCE_RATE_HZ
        && stats.linux_delivery_duration_ms <= MAX_MOUSE_STREAM_DELIVERY_MS
        && stats.evdev_interarrival.p95_ms <= MAX_MOUSE_STREAM_INTERARRIVAL_P95_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_event(event_type: u16, code: u16, value: i32) -> Vec<u8> {
        let mut bytes = vec![0; std::mem::size_of::<libc::timeval>() + 8];
        let offset = std::mem::size_of::<libc::timeval>();
        bytes[offset..offset + 2].copy_from_slice(&event_type.to_ne_bytes());
        bytes[offset + 2..offset + 4].copy_from_slice(&code.to_ne_bytes());
        bytes[offset + 4..offset + 8].copy_from_slice(&value.to_ne_bytes());
        bytes
    }

    #[test]
    fn input_parser_preserves_partial_event_until_the_next_read() {
        let event = input_event(2, 0, 200);
        let split = event.len() - 3;
        let mut pending = event[..split].to_vec();

        assert!(decode_input_events(&mut pending).is_empty());
        pending.extend_from_slice(&event[split..]);
        let decoded = decode_input_events(&mut pending);

        assert_eq!(decoded.len(), 1);
        assert_eq!(
            (decoded[0].event_type, decoded[0].code, decoded[0].value),
            (2, 0, 200)
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn latency_stats_report_tail_percentiles() {
        let stats = latency_stats(vec![1.0, 2.0, 3.0, 4.0, 100.0]);

        assert_eq!(stats.p50_ms, 3.0);
        assert_eq!(stats.p95_ms, 100.0);
        assert_eq!(stats.p99_ms, 100.0);
        assert_eq!(stats.max_ms, 100.0);
    }

    #[test]
    fn stream_completion_requires_matching_report_count() {
        assert_eq!(
            parse_stream_duration("INFO @HIDSHIFT-E2E:STREAM,42,1000,999123", 42).unwrap(),
            999_123
        );
        assert!(parse_stream_duration("INFO @HIDSHIFT-E2E:STREAM,42,999,999123", 42).is_err());
    }

    #[test]
    fn mouse_stream_gate_requires_all_movement_at_the_requested_rate() {
        let stats = MouseStreamStats {
            requested_reports: 1_000,
            expected_x: 200_000,
            observed_x: 200_000,
            evdev_events: 250,
            source_duration_ms: 1_000.0,
            source_rate_hz: 1_000.0,
            linux_delivery_duration_ms: 1_010.0,
            evdev_interarrival: latency_stats(Vec::new()),
            batched_intervals: 0,
        };
        assert!(mouse_stream_integrity_passes(&stats));
        assert!(!mouse_stream_integrity_passes(&MouseStreamStats {
            source_rate_hz: 899.9,
            ..stats.clone()
        }));
        assert!(!mouse_stream_integrity_passes(&MouseStreamStats {
            observed_x: 199_999,
            ..stats.clone()
        }));
        assert!(!mouse_stream_integrity_passes(&MouseStreamStats {
            linux_delivery_duration_ms: 1_500.1,
            ..stats.clone()
        }));
        assert!(!mouse_stream_integrity_passes(&MouseStreamStats {
            evdev_interarrival: LatencyStats {
                p95_ms: 5.1,
                ..latency_stats(Vec::new())
            },
            ..stats
        }));
    }
}
