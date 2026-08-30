use std::time::Duration;

/// Configuration for a [`CircuitBreaker`](crate::CircuitBreaker).
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before the circuit opens.
    pub failure_rate_threshold: u32,
    /// Size of the sliding window for failure tracking.
    pub sliding_window_size: u32,
    /// Duration to wait in the Open state before transitioning to HalfOpen.
    pub wait_duration: Duration,
    /// Number of successful calls in HalfOpen needed to close the circuit.
    pub half_open_max_calls: u32,
}

impl CircuitBreakerConfig {
    /// Sensible defaults: 5 failures, 10-window, 30 s wait, 3 half-open probes.
    pub fn standard() -> Self {
        Self::builder()
            .failure_rate_threshold(5)
            .sliding_window_size(10)
            .wait_duration(Duration::from_secs(30))
            .half_open_max_calls(3)
            .build()
    }

    /// Trip after a single failure, 10 s wait.
    pub fn fast_fail() -> Self {
        Self::builder()
            .failure_rate_threshold(1)
            .sliding_window_size(5)
            .wait_duration(Duration::from_secs(10))
            .half_open_max_calls(1)
            .build()
    }

    /// Forgiving: 10 failures, 60 s wait.
    pub fn lenient() -> Self {
        Self::builder()
            .failure_rate_threshold(10)
            .sliding_window_size(20)
            .wait_duration(Duration::from_secs(60))
            .half_open_max_calls(5)
            .build()
    }

    pub fn builder() -> CircuitBreakerConfigBuilder {
        CircuitBreakerConfigBuilder::default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CircuitBreakerConfigBuilder {
    failure_rate_threshold: Option<u32>,
    sliding_window_size: Option<u32>,
    wait_duration: Option<Duration>,
    half_open_max_calls: Option<u32>,
}

impl CircuitBreakerConfigBuilder {
    pub fn failure_rate_threshold(mut self, v: u32) -> Self {
        self.failure_rate_threshold = Some(v);
        self
    }

    pub fn sliding_window_size(mut self, v: u32) -> Self {
        self.sliding_window_size = Some(v);
        self
    }

    pub fn wait_duration(mut self, v: Duration) -> Self {
        self.wait_duration = Some(v);
        self
    }

    pub fn half_open_max_calls(mut self, v: u32) -> Self {
        self.half_open_max_calls = Some(v);
        self
    }

    pub fn build(self) -> CircuitBreakerConfig {
        let wait = self.wait_duration.unwrap_or(Duration::from_secs(30));
        CircuitBreakerConfig {
            failure_rate_threshold: self.failure_rate_threshold.unwrap_or(5),
            sliding_window_size: self.sliding_window_size.unwrap_or(10),
            half_open_max_calls: self.half_open_max_calls.unwrap_or(3),
            wait_duration: wait,
        }
    }
}
