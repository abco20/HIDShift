use serde::{Deserialize, Serialize};

const BRIDGE_GAME_LATENCY_P95_MAX_MS: f64 = 9.999;
const BRIDGE_GAME_LATENCY_P99_MAX_MS: f64 = 15.0;
const BLE_GAME_LATENCY_P95_MAX_MS: f64 = 15.0;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LatencyStats {
    pub samples: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BaselineComparison {
    pub baseline_p95_ms: f64,
    pub current_p95_ms: f64,
    pub change_percent: f64,
    pub passed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PerformanceBaseline {
    pub schema_version: u8,
    pub metric: String,
    pub keyboard: LatencyStats,
    pub mouse: LatencyStats,
}

pub fn bridge_game_latency_passes(stats: &LatencyStats) -> bool {
    stats.p95_ms <= BRIDGE_GAME_LATENCY_P95_MAX_MS && stats.p99_ms <= BRIDGE_GAME_LATENCY_P99_MAX_MS
}

pub fn ble_game_latency_passes(stats: &LatencyStats) -> bool {
    stats.p95_ms <= BLE_GAME_LATENCY_P95_MAX_MS
}

pub fn merge_latency(a: &LatencyStats, b: &LatencyStats) -> LatencyStats {
    LatencyStats {
        samples: a.samples + b.samples,
        mean_ms: (a.mean_ms * a.samples as f64 + b.mean_ms * b.samples as f64)
            / (a.samples + b.samples).max(1) as f64,
        p50_ms: a.p50_ms.max(b.p50_ms),
        p95_ms: a.p95_ms.max(b.p95_ms),
        p99_ms: a.p99_ms.max(b.p99_ms),
        max_ms: a.max_ms.max(b.max_ms),
    }
}

pub fn latency_stats(mut values: Vec<f64>) -> LatencyStats {
    values.sort_by(f64::total_cmp);
    let samples = values.len();
    let mean_ms = values.iter().sum::<f64>() / samples.max(1) as f64;
    LatencyStats {
        samples,
        mean_ms,
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

pub fn compare_baseline(baseline: &LatencyStats, current: &LatencyStats) -> BaselineComparison {
    let change_percent = if baseline.p95_ms == 0.0 {
        0.0
    } else {
        (current.p95_ms / baseline.p95_ms - 1.0) * 100.0
    };
    let allowed = baseline.p95_ms * 1.25 + 2.0;
    BaselineComparison {
        baseline_p95_ms: baseline.p95_ms,
        current_p95_ms: current.p95_ms,
        change_percent,
        passed: current.p95_ms <= allowed,
    }
}

pub fn latency_advisory(stats: &LatencyStats) -> &'static str {
    if stats.p95_ms <= 20.0 && stats.p99_ms <= 30.0 {
        "good: unlikely to be perceptible for ordinary keyboard/mouse use"
    } else if stats.p95_ms <= 30.0 && stats.p99_ms <= 50.0 {
        "acceptable: generally usable, but fast gaming users may notice it"
    } else {
        "investigate: tail latency is likely perceptible"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(p95_ms: f64, p99_ms: f64) -> LatencyStats {
        LatencyStats {
            samples: 1,
            mean_ms: 0.0,
            p50_ms: 0.0,
            p95_ms,
            p99_ms,
            max_ms: p99_ms,
        }
    }

    #[test]
    fn percentile_and_baseline_gate_cover_tail_regression() {
        let current = latency_stats(vec![1.0, 2.0, 3.0, 4.0, 100.0]);
        assert_eq!(current.p50_ms, 3.0);
        assert_eq!(current.p95_ms, 100.0);
        assert!(!compare_baseline(&sample(10.0, 100.0), &current).passed);
    }

    #[test]
    fn bridge_gate_requires_sub_10ms_p95_and_15ms_p99() {
        assert!(bridge_game_latency_passes(&sample(9.9, 14.9)));
        assert!(!bridge_game_latency_passes(&sample(10.0, 14.9)));
        assert!(!bridge_game_latency_passes(&sample(9.9, 15.1)));
    }

    #[test]
    fn ble_gate_allows_15ms_p95_without_an_espnow_p99_limit() {
        assert!(ble_game_latency_passes(&sample(15.0, 30.0)));
        assert!(!ble_game_latency_passes(&sample(15.001, 15.001)));
    }
}
