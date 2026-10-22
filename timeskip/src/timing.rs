use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

const ENGINE_METRIC: &str = "execution_layer_request_times";

/// Per-slot duration samples, keyed by phase name.
#[derive(Default)]
pub struct PhaseTimings {
    phases: BTreeMap<&'static str, Vec<Duration>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseSummary {
    pub count: usize,
    pub total_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub max_ms: f64,
}

impl PhaseTimings {
    pub fn record(&mut self, phase: &'static str, elapsed: Duration) {
        self.phases.entry(phase).or_default().push(elapsed);
    }

    pub fn summary(&self) -> BTreeMap<String, PhaseSummary> {
        self.phases
            .iter()
            .map(|(phase, samples)| ((*phase).to_string(), summarize(samples)))
            .collect()
    }
}

fn summarize(samples: &[Duration]) -> PhaseSummary {
    let mut sorted: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e3).collect();
    sorted.sort_by(f64::total_cmp);
    let total: f64 = sorted.iter().sum();
    let count = sorted.len();
    let percentile = |p: f64| -> f64 {
        if count == 0 {
            return 0.0;
        }
        let rank = ((count as f64 - 1.0) * p).round() as usize;
        sorted.get(rank).copied().unwrap_or_default()
    };

    PhaseSummary {
        count,
        total_ms: total,
        mean_ms: if count == 0 {
            0.0
        } else {
            total / count as f64
        },
        p50_ms: percentile(0.5),
        p90_ms: percentile(0.9),
        max_ms: sorted.last().copied().unwrap_or_default(),
    }
}

/// Cumulative engine API call counts and durations from Lighthouse's `ExecutionLayer` metrics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EngineCalls {
    pub methods: BTreeMap<String, EngineCallSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EngineCallSummary {
    pub count: u64,
    pub total_ms: f64,
    pub mean_ms: f64,
}

impl EngineCalls {
    pub fn snapshot() -> Self {
        let mut methods = BTreeMap::new();
        for family in metrics::gather() {
            if family.get_name() != ENGINE_METRIC {
                continue;
            }
            for metric in family.get_metric() {
                let method = metric
                    .get_label()
                    .iter()
                    .find(|label| label.get_name() == "method")
                    .map(|label| label.get_value().to_string())
                    .unwrap_or_default();
                let histogram = metric.get_histogram();
                methods.insert(
                    method,
                    EngineCallSummary {
                        count: histogram.get_sample_count(),
                        total_ms: histogram.get_sample_sum() * 1e3,
                        mean_ms: 0.0,
                    },
                );
            }
        }
        Self { methods }
    }

    /// Calls made since `earlier` was taken.
    pub fn since(&self, earlier: &Self) -> Self {
        let methods = self
            .methods
            .iter()
            .map(|(method, now)| {
                let before = earlier.methods.get(method).cloned().unwrap_or_default();
                let count = now.count.saturating_sub(before.count);
                let total_ms = now.total_ms - before.total_ms;
                let mean_ms = if count == 0 {
                    0.0
                } else {
                    total_ms / count as f64
                };
                (
                    method.clone(),
                    EngineCallSummary {
                        count,
                        total_ms,
                        mean_ms,
                    },
                )
            })
            .filter(|(_, summary)| summary.count > 0)
            .collect();
        Self { methods }
    }
}
