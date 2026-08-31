#![forbid(unsafe_code)]
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
    config: CircuitBreakerConfig,
    state: RwLock<StateMachine>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                state: RwLock::new(StateMachine::new()),
            }),
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
                self.inner.state.write().record_success(&self.inner.config);
                Ok(value)
            }
            Err(err) => {
                self.inner.state.write().record_failure(&self.inner.config);
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
        self.inner.state.write().force_open();
    }

    /// Force the circuit back into the `Closed` state.
    pub fn reset(&self) {
        self.inner.state.write().force_closed();
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

        let result = cb.call(|| async { Ok::<_, String>("ok".to_string()) }).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CircuitBreakerError::CircuitOpen));
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

        let result = cb.call(|| async { Ok::<_, String>("ok".to_string()) }).await;
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
        let _ = cb.call(|| async { Ok::<_, String>("ok".to_string()) }).await;
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

        let _ = cb.call(|| async { Ok::<_, String>("ok".to_string()) }).await;
        let _ = cb.call(|| async { Ok::<_, String>("ok".to_string()) }).await;
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

        let _ = cb.call(|| async { Ok::<_, String>("ok".to_string()) }).await;

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
        let result = cb.call(|| async { Ok::<_, String>(42i32.to_string()) }).await;
        assert_eq!(result.unwrap(), "42");
    }

    #[tokio::test]
    async fn async_call_failure_propagates() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        let result = cb.call(|| async { Err::<String, _>("something broke") }).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CircuitBreakerError::Inner(msg) => assert_eq!(msg, "something broke"),
            other => panic!("expected Inner, got {:?}", other),
        }
    }
}
