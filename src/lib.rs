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

use std::future::Future;
use std::sync::Arc;

use parking_lot::RwLock;
use state::{State, StateMachine};

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
            Err(_err) => {
                self.inner.state.write().record_failure(&self.inner.config);
                Err(CircuitBreakerError::Timeout)
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
