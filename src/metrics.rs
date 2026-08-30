use crate::state::State;

/// Snapshot of circuit breaker metrics.
#[derive(Debug, Clone)]
pub struct CircuitMetrics {
    /// Ratio of failures to total calls (0.0 – 1.0).
    pub failure_rate: f64,
    /// Current circuit state.
    pub state: State,
    /// Total successful calls since creation.
    pub total_successes: u64,
    /// Total failed calls since creation.
    pub total_failures: u64,
    /// Number of state transitions.
    pub transitions: u64,
}
