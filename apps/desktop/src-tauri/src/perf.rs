//! In-process startup and turn latency diagnostics.

use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_TURN_SAMPLES: usize = 512;

#[derive(Default)]
struct PerfSamples {
    startup_phases_microseconds: BTreeMap<String, u64>,
    turn_first_delta_microseconds: VecDeque<u64>,
}

static PERF_SAMPLES: OnceLock<Mutex<PerfSamples>> = OnceLock::new();
static NEXT_TURN_ID: AtomicU64 = AtomicU64::new(1);

fn samples() -> &'static Mutex<PerfSamples> {
    PERF_SAMPLES.get_or_init(|| Mutex::new(PerfSamples::default()))
}

fn duration_microseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Run one synchronous startup phase inside its tracing span and retain its duration.
pub(crate) fn startup_phase<T>(
    name: &'static str,
    span: &tracing::Span,
    operation: impl FnOnce() -> T,
) -> T {
    let started = Instant::now();
    span.in_scope(|| {
        let result = operation();
        record_startup_phase(name, started.elapsed());
        result
    })
}

/// Retain the last cold-start measurement for a named phase.
pub(crate) fn record_startup_phase(name: &'static str, duration: Duration) {
    let elapsed_microseconds = duration_microseconds(duration);
    if let Ok(mut samples) = samples().lock() {
        samples
            .startup_phases_microseconds
            .insert(name.to_owned(), elapsed_microseconds);
    }
    tracing::info!(
        phase = name,
        elapsed_microseconds,
        "startup phase completed"
    );
}

/// One chat turn's monotonic latency clock.
pub(crate) struct TurnTrace {
    id: u64,
    started: Instant,
    first_delta_recorded: bool,
}

impl TurnTrace {
    pub(crate) fn start() -> Self {
        Self {
            id: NEXT_TURN_ID.fetch_add(1, Ordering::Relaxed),
            started: Instant::now(),
            first_delta_recorded: false,
        }
    }

    pub(crate) const fn id(&self) -> u64 {
        self.id
    }

    /// Record the first text delta once, returning its elapsed time for span fields.
    pub(crate) fn record_first_delta(&mut self) -> Option<Duration> {
        if self.first_delta_recorded {
            return None;
        }
        self.first_delta_recorded = true;
        let elapsed = self.started.elapsed();
        let elapsed_microseconds = duration_microseconds(elapsed);
        if let Ok(mut samples) = samples().lock() {
            if samples.turn_first_delta_microseconds.len() == MAX_TURN_SAMPLES {
                samples.turn_first_delta_microseconds.pop_front();
            }
            samples
                .turn_first_delta_microseconds
                .push_back(elapsed_microseconds);
        }
        Some(elapsed)
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

fn percentile(sorted_samples: &[u64], percent: usize) -> Option<u64> {
    if sorted_samples.is_empty() {
        return None;
    }
    let rank = (sorted_samples.len() * percent).div_ceil(100);
    let index = rank.saturating_sub(1);
    sorted_samples.get(index).copied()
}

/// JSON payload embedded in diagnostics and emitted as `perf-report`.
pub(crate) fn report() -> Value {
    let Ok(samples) = samples().lock() else {
        return json!({"error": "performance samples lock is poisoned"});
    };
    let mut first_delta = samples
        .turn_first_delta_microseconds
        .iter()
        .copied()
        .collect::<Vec<_>>();
    first_delta.sort_unstable();
    json!({
        "measurement": "live-process-monotonic",
        "last_cold_start": {
            "phases_microseconds": samples.startup_phases_microseconds,
            "startup_native_setup_microseconds": samples
                .startup_phases_microseconds
                .get("native_setup")
                .copied(),
        },
        "turn_first_delta": {
            "p50_microseconds": percentile(&first_delta, 50),
            "p95_microseconds": percentile(&first_delta, 95),
            "maximum_microseconds": first_delta.last().copied(),
            "sample_count": first_delta.len(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank_and_handles_no_samples() {
        assert_eq!(percentile(&[], 95), None);
        assert_eq!(percentile(&[10], 95), Some(10));
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 50), Some(3));
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 95), Some(5));
        assert_eq!(percentile(&[1, 2, 3, 4], 50), Some(2));
        assert_eq!(percentile(&(1..=100).collect::<Vec<_>>(), 95), Some(95));
    }

    #[test]
    fn startup_phase_returns_operation_result() {
        let result = startup_phase(
            "perf_test",
            &tracing::info_span!("startup.perf_test"),
            || 42,
        );
        assert_eq!(result, 42);
        assert!(
            report()
                .pointer("/last_cold_start/phases_microseconds/perf_test")
                .is_some()
        );
    }
}
