use crate::state::State;

/// Snapshot of circuit breaker metrics.
#[derive(Debug, Clone)]
pub struct CircuitMetrics {
    /// Lifetime ratio of failures to total calls (0.0 – 1.0).
    pub failure_rate: f64,
    /// Failure fraction over the sliding window
    /// (`failures / min(window_size, filled)`; 0.0 when empty) — the
    /// quantity the rate-based trip decision evaluates.
    pub window_failure_rate: f32,
    /// Current circuit state.
    pub state: State,
    /// Total successful calls since creation.
    pub total_successes: u64,
    /// Total failed calls since creation.
    pub total_failures: u64,
    /// Number of state transitions.
    pub transitions: u64,
}
