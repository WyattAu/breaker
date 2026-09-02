#![forbid(unsafe_code)]
#![deny(missing_docs)]
//! Async circuit breaker for Rust.
//!
//! A state-machine based circuit breaker with configurable failure thresholds,
//! sliding window metrics, and optional Tower layer integration.
//!
//! # States
//!
//! ```text
//! ┌────────┐  failure threshold  ┌──────┐  timeout/ probes  ┌──────────┐
//! │ Closed │ ──────────────────> │ Open │ ────────────────> │ HalfOpen │
//! └────────┘                     └──────┘                   └──────────┘
//!      ^                                                     │     │
//!      │              success                                │     │
//!      └─────────────────────────────────────────────────────┘     │
//!      │                    failure                                 │
//!      └───────────────────────────────────────────────────────────┘
//! ```
//!
//! # Quick Start
//!
//! ```no_run
//! use breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), CircuitBreakerError> {
//!     let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
//!
//!     cb.call(|| async {
//!         // Your fallible async operation here
//!         Ok::<_, String>("success")
//!     })
//!     .await?;
//!
//!     Ok(())
//! }
//! ```

mod config;
mod error;
mod metrics;
mod state;

pub use config::CircuitBreakerConfig;
pub use error::CircuitBreakerError;
pub use metrics::CircuitMetrics;
pub use state::State;

use std::future::Future;
use std::sync::Arc;

use parking_lot::RwLock;
use state::StateMachine;

/// An async circuit breaker.
///
/// Wraps fallible operations and tripping the circuit when failures exceed the
/// configured threshold.
#[derive(Clone)]
pub struct CircuitBreaker {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    config: CircuitBreakerConfig,
    state: RwLock<StateMachine>,
    state_change_callback: Option<Arc<dyn Fn(State, State) + Send + Sync>>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self::builder(config).build()
    }

    /// Create a new [`CircuitBreakerBuilder`].
    pub fn builder(config: CircuitBreakerConfig) -> CircuitBreakerBuilder {
        CircuitBreakerBuilder {
            name: String::from("default"),
            config,
            state_change_callback: None,
        }
    }

    /// Returns the name of this circuit breaker.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Returns the current state of the circuit.
    pub fn state(&self) -> State {
        self.inner.state.read().current()
    }

    /// Returns `true` if the circuit is open.
    pub fn is_open(&self) -> bool {
        matches!(self.state(), State::Open)
    }

    /// Returns `true` if the circuit is closed.
    pub fn is_closed(&self) -> bool {
        matches!(self.state(), State::Closed)
    }

    /// Returns `true` if the circuit is half-open.
    pub fn is_half_open(&self) -> bool {
        matches!(self.state(), State::HalfOpen)
    }

    /// Record a success manually, advancing the state machine.
    ///
    /// Use this to drive the circuit breaker without [`call`](Self::call).
    pub fn record_success(&self) {
        let transition;
        {
            let mut state = self.inner.state.write();
            transition = state.record_success(&self.inner.config);
        }
        #[cfg(feature = "metrics")]
        ::metrics::counter!("circuit_breaker_successes_total").increment(1);
        if let Some((prev, next)) = transition {
            self.fire_callback(prev, next);
        }
    }

    /// Record a failure manually, advancing the state machine.
    ///
    /// Use this to drive the circuit breaker without [`call`](Self::call).
    pub fn record_failure(&self) {
        let transition;
        {
            let mut state = self.inner.state.write();
            transition = state.record_failure(&self.inner.config);
        }
        #[cfg(feature = "metrics")]
        ::metrics::counter!("circuit_breaker_failures_total").increment(1);
        if let Some((prev, next)) = transition {
            self.fire_callback(prev, next);
        }
    }

    fn fire_callback(&self, prev: State, next: State) {
        #[cfg(feature = "metrics")]
        {
            let transition = format!("{prev:?}->{next:?}");
            ::metrics::counter!("circuit_breaker_transitions_total", "transition" => transition).increment(1);
            let state_val = match next {
                State::Closed => 0.0,
                State::Open => 1.0,
                State::HalfOpen => 2.0,
            };
            ::metrics::gauge!("circuit_breaker_state").set(state_val);
        }
        if let Some(ref cb) = self.inner.state_change_callback {
            cb(prev, next);
        }
    }

    /// Execute an operation through the circuit breaker.
    ///
    /// Returns [`CircuitBreakerError::CircuitOpen`] if the circuit is tripped.
    pub async fn call<F, Fut, T, E>(&self, operation: F) -> Result<T, CircuitBreakerError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        {
            let state = self.inner.state.read();
            if state.current() == State::Open {
                return Err(CircuitBreakerError::CircuitOpen);
            }
        }

        match operation().await {
            Ok(value) => {
                let transition;
                {
                    let mut state = self.inner.state.write();
                    transition = state.record_success(&self.inner.config);
                }
                #[cfg(feature = "metrics")]
                ::metrics::counter!("circuit_breaker_successes_total").increment(1);
                if let Some((prev, next)) = transition {
                    self.fire_callback(prev, next);
                }
                Ok(value)
            }
            Err(err) => {
                let transition;
                {
                    let mut state = self.inner.state.write();
                    transition = state.record_failure(&self.inner.config);
                }
                #[cfg(feature = "metrics")]
                {
                    ::metrics::counter!("circuit_breaker_failures_total").increment(1);
                    let m = self.inner.state.read();
                    ::metrics::histogram!("circuit_breaker_failure_rate").record(m.failure_rate());
                    ::metrics::histogram!("circuit_breaker_failure_count").record(m.failure_count() as f64);
                }
                if let Some((prev, next)) = transition {
                    self.fire_callback(prev, next);
                }
                Err(CircuitBreakerError::Inner(err.to_string()))
            }
        }
    }

    /// Return a snapshot of the current circuit metrics.
    pub fn metrics(&self) -> CircuitMetrics {
        let state = self.inner.state.read();
        CircuitMetrics {
            failure_rate: state.failure_rate(),
            state: state.current(),
            total_successes: state.total_successes(),
            total_failures: state.total_failures(),
            transitions: state.transitions(),
        }
    }

    /// Force the circuit into the `Open` state.
    pub fn trip(&self) {
        let prev = self.state();
        self.inner.state.write().force_open();
        #[cfg(feature = "metrics")]
        ::metrics::counter!("circuit_breaker_trips_total").increment(1);
        self.fire_callback(prev, State::Open);
    }

    /// Force the circuit back into the `Closed` state.
    pub fn reset(&self) {
        let prev = self.state();
        self.inner.state.write().force_closed();
        self.fire_callback(prev, State::Closed);
    }
}

/// Builder for [`CircuitBreaker`].
pub struct CircuitBreakerBuilder {
    name: String,
    config: CircuitBreakerConfig,
    state_change_callback: Option<Arc<dyn Fn(State, State) + Send + Sync>>,
}

impl CircuitBreakerBuilder {
    /// Set the name of the circuit breaker.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set a callback to invoke on state transitions.
    pub fn on_state_change<F>(mut self, f: F) -> Self
    where
        F: Fn(State, State) + Send + Sync + 'static,
    {
        self.state_change_callback = Some(Arc::new(f));
        self
    }

    /// Build the [`CircuitBreaker`].
    pub fn build(self) -> CircuitBreaker {
        CircuitBreaker {
            inner: Arc::new(Inner {
                name: self.name,
                config: self.config,
                state: RwLock::new(StateMachine::new()),
                state_change_callback: self.state_change_callback,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::CircuitBreakerConfig;

    #[test]
    fn config_standard_preset() {
        let c = CircuitBreakerConfig::standard();
        assert_eq!(c.failure_rate_threshold, 5);
        assert_eq!(c.sliding_window_size, 10);
        assert_eq!(c.wait_duration, std::time::Duration::from_secs(30));
        assert_eq!(c.half_open_max_calls, 3);
    }

    #[test]
    fn config_fast_fail_preset() {
        let c = CircuitBreakerConfig::fast_fail();
        assert_eq!(c.failure_rate_threshold, 1);
        assert_eq!(c.half_open_max_calls, 1);
        assert_eq!(c.wait_duration, std::time::Duration::from_secs(10));
    }

    #[test]
    fn config_lenient_preset() {
        let c = CircuitBreakerConfig::lenient();
        assert_eq!(c.failure_rate_threshold, 10);
        assert_eq!(c.sliding_window_size, 20);
        assert_eq!(c.wait_duration, std::time::Duration::from_secs(60));
        assert_eq!(c.half_open_max_calls, 5);
    }

    #[test]
    fn config_builder_custom() {
        let c = CircuitBreakerConfig::builder()
            .failure_rate_threshold(3)
            .sliding_window_size(7)
            .wait_duration(std::time::Duration::from_secs(5))
            .half_open_max_calls(2)
            .build();
        assert_eq!(c.failure_rate_threshold, 3);
        assert_eq!(c.sliding_window_size, 7);
        assert_eq!(c.half_open_max_calls, 2);
    }

    #[tokio::test]
    async fn starts_in_closed_state() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        assert_eq!(cb.metrics().state, State::Closed);
    }

    #[tokio::test]
    async fn closed_to_open_after_failures() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(3)
            .half_open_max_calls(1)
            .wait_duration(std::time::Duration::from_secs(60))
            .build();
        let cb = CircuitBreaker::new(config);

        for _ in 0..3 {
            let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        }

        assert_eq!(cb.metrics().state, State::Open);
        assert_eq!(cb.metrics().total_failures, 3);
    }

    #[tokio::test]
    async fn open_rejects_requests() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(1)
            .half_open_max_calls(1)
            .wait_duration(std::time::Duration::from_secs(60))
            .build();
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert_eq!(cb.metrics().state, State::Open);

        let result = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            CircuitBreakerError::CircuitOpen
        ));
    }

    #[tokio::test]
    async fn open_to_half_open_after_wait() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(1)
            .half_open_max_calls(1)
            .wait_duration(std::time::Duration::from_millis(50))
            .build();
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert_eq!(cb.metrics().state, State::Open);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let result = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn half_open_to_closed_after_successes() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(1)
            .half_open_max_calls(2)
            .wait_duration(std::time::Duration::from_millis(10))
            .build();
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert_eq!(cb.metrics().state, State::Open);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(cb.metrics().state, State::HalfOpen);
        let _ = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;
        assert_eq!(cb.metrics().total_successes, 1);
    }

    #[tokio::test]
    async fn half_open_to_open_on_failure() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(1)
            .half_open_max_calls(2)
            .wait_duration(std::time::Duration::from_millis(10))
            .build();
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(cb.metrics().state, State::HalfOpen);
        let _ = cb.call(|| async { Err::<(), _>("fail again") }).await;
        assert_eq!(cb.metrics().total_failures, 2);
    }

    #[tokio::test]
    async fn metrics_records_successes_and_failures() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());

        let _ = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;
        let _ = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;

        let m = cb.metrics();
        assert_eq!(m.total_successes, 2);
        assert_eq!(m.total_failures, 1);
        assert!((m.failure_rate - 1.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn failure_rate_zero_initially() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        let m = cb.metrics();
        assert_eq!(m.failure_rate, 0.0);
        assert_eq!(m.transitions, 0);
    }

    #[test]
    fn trip_and_reset() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        cb.trip();
        assert_eq!(cb.metrics().state, State::Open);

        cb.reset();
        assert_eq!(cb.metrics().state, State::Closed);
    }

    #[tokio::test]
    async fn success_resets_failure_count_in_closed() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(3)
            .half_open_max_calls(1)
            .wait_duration(std::time::Duration::from_secs(60))
            .build();
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert_eq!(cb.metrics().state, State::Closed);

        let _ = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert_eq!(cb.metrics().state, State::Closed);
    }

    #[test]
    fn error_display_messages() {
        assert_eq!(
            CircuitBreakerError::CircuitOpen.to_string(),
            "circuit breaker is open"
        );
        assert_eq!(
            CircuitBreakerError::Timeout.to_string(),
            "circuit breaker: operation timed out or failed"
        );
        assert_eq!(
            CircuitBreakerError::Inner("boom".into()).to_string(),
            "circuit breaker: inner error: boom"
        );
    }

    #[tokio::test]
    async fn async_call_success() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        let result = cb
            .call(|| async { Ok::<_, String>(42i32.to_string()) })
            .await;
        assert_eq!(result.unwrap(), "42");
    }

    #[tokio::test]
    async fn async_call_failure_propagates() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        let result = cb
            .call(|| async { Err::<String, _>("something broke") })
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CircuitBreakerError::Inner(msg) => assert_eq!(msg, "something broke"),
            other => panic!("expected Inner, got {:?}", other),
        }
    }

    #[test]
    fn name_returns_configured_name() {
        let cb = CircuitBreaker::builder(CircuitBreakerConfig::standard())
            .name("my-breaker")
            .build();
        assert_eq!(cb.name(), "my-breaker");
    }

    #[test]
    fn name_default() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        assert_eq!(cb.name(), "default");
    }

    #[test]
    fn is_open_is_closed_is_half_open() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        assert!(cb.is_closed());
        assert!(!cb.is_open());
        assert!(!cb.is_half_open());

        cb.trip();
        assert!(cb.is_open());
        assert!(!cb.is_closed());
        assert!(!cb.is_half_open());

        cb.reset();
        assert!(cb.is_closed());
        assert!(!cb.is_open());
    }

    #[test]
    fn state_method() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        assert_eq!(cb.state(), State::Closed);
        cb.trip();
        assert_eq!(cb.state(), State::Open);
    }

    #[test]
    fn record_success_manual() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(1)
            .success_threshold(2)
            .half_open_max_calls(2)
            .wait_duration(std::time::Duration::from_millis(10))
            .build();
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert!(cb.is_open());

        // Wait for half-open
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(cb.is_half_open());

        cb.record_success();
        assert!(cb.is_half_open()); // need 2 successes

        cb.record_success();
        assert!(cb.is_closed());
    }

    #[test]
    fn record_failure_manual() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(2)
            .build();
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert!(cb.is_closed());

        cb.record_failure();
        assert!(cb.is_open());
    }

    #[test]
    fn record_failure_in_half_open_opens_circuit() {
        let config = CircuitBreakerConfig::builder()
            .failure_rate_threshold(1)
            .success_threshold(2)
            .half_open_max_calls(2)
            .wait_duration(std::time::Duration::from_millis(10))
            .build();
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(cb.is_half_open());

        cb.record_failure();
        assert!(cb.is_open());
    }

    #[test]
    fn on_state_change_callback() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let cb = CircuitBreaker::builder(
            CircuitBreakerConfig::builder()
                .failure_rate_threshold(1)
                .build(),
        )
        .on_state_change(move |_prev, _next| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        })
        .build();

        cb.record_failure(); // Closed -> Open
        assert_eq!(count.load(Ordering::SeqCst), 1);

        cb.reset(); // Open -> Closed
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn on_state_change_records_transition() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let transitions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let t_clone = transitions.clone();

        let cb = CircuitBreaker::builder(
            CircuitBreakerConfig::builder()
                .failure_rate_threshold(1)
                .build(),
        )
        .on_state_change(move |prev, next| {
            t_clone.lock().unwrap().push((prev, next));
        })
        .build();

        cb.record_failure();
        let t = transitions.lock().unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0], (State::Closed, State::Open));
    }

    #[test]
    fn success_threshold_config() {
        let c = CircuitBreakerConfig::builder().success_threshold(5).build();
        assert_eq!(c.success_threshold, 5);
    }

    #[test]
    fn success_threshold_defaults_to_half_open_max_calls() {
        let c = CircuitBreakerConfig::builder()
            .half_open_max_calls(7)
            .build();
        assert_eq!(c.success_threshold, 7);
    }

    #[test]
    fn builder_creates_named_breaker() {
        let cb = CircuitBreaker::builder(CircuitBreakerConfig::standard())
            .name("http-breaker")
            .build();
        assert_eq!(cb.name(), "http-breaker");
        assert!(cb.is_closed());
    }

    #[test]
    fn record_success_in_closed_does_not_transition() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let cb = CircuitBreaker::builder(CircuitBreakerConfig::standard())
            .on_state_change(move |_, _| {
                count_clone.fetch_add(1, Ordering::SeqCst);
            })
            .build();

        cb.record_success();
        cb.record_success();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(cb.is_closed());
    }
}
