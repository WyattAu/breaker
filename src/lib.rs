#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
//! Async circuit breaker for Rust.
//!
//! A state-machine based circuit breaker with sliding-window failure-rate
//! tripping, consecutive-failure tripping, half-open probe permits (stampede
//! protection), configurable backoff strategies, typed errors, and a real
//! Tower layer (behind the `tower` feature).
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
//! async fn main() -> Result<(), CircuitBreakerError<String>> {
//!     let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
//!
//!     cb.call(|| async {
//!         // Your fallible async operation here
//!         Ok::<_, String>("success".to_string())
//!     })
//!     .await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! # Tripping policy
//!
//! While `Closed`, the circuit trips on whichever fires first:
//!
//! - **consecutive failures** — [`CircuitBreakerConfig::consecutive_failures`]
//!   failures in a row;
//! - **sliding-window failure rate** — the fraction of failures over the
//!   last [`CircuitBreakerConfig::sliding_window_size`] outcomes reaches
//!   [`CircuitBreakerConfig::failure_rate_threshold`] (evaluated against
//!   `min(size, outcomes recorded so far)`).
//!
//! # Failure classification
//!
//! Configure [`failure_predicate`](CircuitBreakerConfigBuilder::failure_predicate)
//! so only errors you deem breaker-worthy count as failures; everything
//! else passes through to the caller with the **original, typed error**
//! ([`CircuitBreakerError::Failure`]) and never touches the state machine.
//!
//! # Backoff
//!
//! Each trip computes its Open-state wait from the configured
//! [`BackoffStrategy`] — fixed, exponential, or exponential-with-jitter —
//! using a monotonically increasing trip counter.
//!
//! # Half-open probes
//!
//! While `HalfOpen`, at most
//! [`half_open_max_calls`](CircuitBreakerConfig::half_open_max_calls)
//! concurrent probe calls are admitted; the rest are rejected immediately
//! ([`CircuitBreakerError::Rejected`], not counted as failures). Permits
//! are released when a probe completes or its future is dropped.

mod config;
mod error;
mod lock;
mod metrics;
mod state;
#[cfg(feature = "tower")]
pub mod tower;

pub use config::BackoffStrategy;
pub use config::CircuitBreakerConfig;
pub use config::CircuitBreakerConfigBuilder;
pub use error::CircuitBreakerError;
pub use metrics::CircuitMetrics;
pub use state::State;

#[cfg(feature = "tower")]
pub use tower::{BreakerLayer, BreakerService};

use std::future::Future;
use std::sync::Arc;

// Under `cfg(loom)` the state lock is swapped for loom's RwLock so the
// record_failure/record_success state machine can be model-checked
// (see tests/loom.rs and src/lock.rs). The atomic shim swaps the half-open
// permit counter the same way.
use lock::{AtomicUsize, Ordering, RwLock};
use state::StateMachine;

/// An async circuit breaker.
///
/// Wraps fallible operations and tripping the circuit when failures exceed
/// the configured thresholds (consecutive streak or sliding-window rate,
/// whichever fires first).
#[derive(Clone)]
pub struct CircuitBreaker {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("name", &self.inner.name)
            .field("state", &self.state())
            .field("metrics", &self.metrics())
            .finish_non_exhaustive()
    }
}

struct Inner {
    name: Arc<str>,
    config: CircuitBreakerConfig,
    state: RwLock<StateMachine>,
    state_change_callback: Option<Arc<dyn Fn(State, State) + Send + Sync>>,
    /// Half-open probe permits currently in flight. Acquired by `call()`
    /// before running an operation in `HalfOpen` (bounded by
    /// [`CircuitBreakerConfig::half_open_max_calls`] — stampede protection),
    /// released on probe completion *or* future cancellation via
    /// [`HalfOpenPermit`]'s `Drop`. Deliberately outside the state lock:
    /// admission is a lock-free CAS. Residual permits from a prior
    /// half-open episode (futures still being polled across a re-trip)
    /// only ever *reduce* the next episode's capacity, never exceed it.
    half_open_permits: AtomicUsize,
}

/// A held half-open probe permit; releases its slot on drop — including
/// when the probe future is cancelled mid-await.
struct HalfOpenPermit<'a> {
    permits: &'a AtomicUsize,
}

impl<'a> HalfOpenPermit<'a> {
    /// Acquire one probe slot, or `None` if all
    /// [`half_open_max_calls`](CircuitBreakerConfig::half_open_max_calls)
    /// slots are in use. Lock-free CAS loop: strictly bounded admissions,
    /// no momentary overshoot.
    fn acquire(permits: &'a AtomicUsize, max_calls: u32) -> Option<Self> {
        let max = max_calls as usize;
        let mut current = permits.load(Ordering::Acquire);
        loop {
            if current >= max {
                return None;
            }
            match permits.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self { permits }),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for HalfOpenPermit<'_> {
    fn drop(&mut self) {
        self.permits.fetch_sub(1, Ordering::Release);
    }
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self::builder(config).build()
    }

    /// Create a new [`CircuitBreakerBuilder`].
    pub fn builder(config: CircuitBreakerConfig) -> CircuitBreakerBuilder {
        CircuitBreakerBuilder {
            name: Arc::from("default"),
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
    /// Manual recording bypasses half-open permit accounting (permits are a
    /// `call()`-level admission mechanism).
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
        self.emit_failure_metrics();
        if let Some((prev, next)) = transition {
            self.fire_callback(prev, next);
        }
    }

    /// Metrics emitted on every recorded failure (shared by `call()` and
    /// the manual `record_failure()` entry point).
    #[cfg(feature = "metrics")]
    fn emit_failure_metrics(&self) {
        ::metrics::counter!("circuit_breaker_failures_total").increment(1);
        let m = self.inner.state.read();
        ::metrics::histogram!("circuit_breaker_failure_rate").record(m.failure_rate());
        ::metrics::histogram!("circuit_breaker_failure_count").record(m.failure_count() as f64);
    }

    /// No-op without the `metrics` feature.
    #[cfg(not(feature = "metrics"))]
    fn emit_failure_metrics(&self) {}

    fn fire_callback(&self, prev: State, next: State) {
        #[cfg(feature = "metrics")]
        {
            let transition = format!("{prev:?}->{next:?}");
            ::metrics::counter!("circuit_breaker_transitions_total", "transition" => transition)
                .increment(1);
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
    /// - [`CircuitBreakerError::CircuitOpen`] — circuit is open (rejected
    ///   before the operation runs).
    /// - [`CircuitBreakerError::Rejected`] — circuit is half-open and all
    ///   probe permits are taken (not counted as a failure).
    /// - [`CircuitBreakerError::Failure`] — the operation failed; the
    ///   original error value is preserved. Counted toward tripping unless
    ///   a [`failure_predicate`](CircuitBreakerConfigBuilder::failure_predicate)
    ///   classifies it otherwise.
    /// - [`CircuitBreakerError::Timeout`] (`timeout` feature) — the
    ///   operation exceeded the configured timeout; counted as a failure.
    pub async fn call<F, Fut, T, E>(&self, operation: F) -> Result<T, CircuitBreakerError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: 'static,
    {
        // Admission: state check under a read lock; half-open additionally
        // requires a probe permit (lock-free, strictly bounded). The permit
        // guard lives until the end of the call — through the operation's
        // await point — and releases on drop, so cancelled futures never
        // leak slots.
        let _permit = {
            let state = self.inner.state.read();
            match state.current() {
                State::Open => return Err(CircuitBreakerError::CircuitOpen),
                State::HalfOpen => match HalfOpenPermit::acquire(
                    &self.inner.half_open_permits,
                    self.inner.config.half_open_max_calls,
                ) {
                    Some(permit) => Some(permit),
                    None => {
                        #[cfg(feature = "metrics")]
                        ::metrics::counter!("circuit_breaker_rejected_total").increment(1);
                        return Err(CircuitBreakerError::Rejected);
                    }
                },
                State::Closed => None,
            }
        };

        let fut = operation();

        #[cfg(feature = "timeout")]
        let stepped = match self.inner.config.call_timeout {
            Some(limit) => match ::tokio::time::timeout(limit, fut).await {
                Ok(stepped) => stepped,
                // Timed out: counts as a failure; the permit (if held)
                // releases on return.
                Err(_elapsed) => {
                    self.record_failure();
                    return Err(CircuitBreakerError::Timeout);
                }
            },
            None => fut.await,
        };
        #[cfg(not(feature = "timeout"))]
        let stepped = fut.await;

        match stepped {
            Ok(value) => {
                self.record_success();
                Ok(value)
            }
            Err(err) => {
                if self.inner.config.is_breaker_failure(&err) {
                    self.record_failure();
                }
                // Original typed error preserved verbatim — no stringifying.
                Err(CircuitBreakerError::Failure(err))
            }
        }
    }

    /// Return a snapshot of the current circuit metrics.
    pub fn metrics(&self) -> CircuitMetrics {
        let state = self.inner.state.read();
        CircuitMetrics {
            failure_rate: state.failure_rate(),
            window_failure_rate: state.window_failure_rate(),
            state: state.current(),
            total_successes: state.total_successes(),
            total_failures: state.total_failures(),
            transitions: state.transitions(),
        }
    }

    /// Failure fraction over the sliding window — the exact quantity the
    /// rate-based trip decision evaluates.
    pub fn window_failure_rate(&self) -> f32 {
        self.inner.state.read().window_failure_rate()
    }

    /// Number of outcomes currently held in the sliding window (grows to
    /// [`sliding_window_size`](CircuitBreakerConfig::sliding_window_size),
    /// then evicts oldest-first).
    pub fn window_filled(&self) -> u32 {
        self.inner.state.read().window_filled()
    }

    /// Force the circuit into the `Open` state. Counts as a trip for the
    /// backoff attempt counter.
    pub fn trip(&self) {
        let prev = self.state();
        self.inner.state.write().force_open(&self.inner.config);
        #[cfg(feature = "metrics")]
        ::metrics::counter!("circuit_breaker_trips_total").increment(1);
        self.fire_callback(prev, State::Open);
    }

    /// Force the circuit back into the `Closed` state. Resets the sliding
    /// window and the backoff attempt counter.
    pub fn reset(&self) {
        let prev = self.state();
        self.inner.state.write().force_closed(&self.inner.config);
        self.fire_callback(prev, State::Closed);
    }
}

/// Builder for [`CircuitBreaker`].
pub struct CircuitBreakerBuilder {
    name: Arc<str>,
    config: CircuitBreakerConfig,
    state_change_callback: Option<Arc<dyn Fn(State, State) + Send + Sync>>,
}

impl CircuitBreakerBuilder {
    /// Set the name of the circuit breaker.
    /// Accepts `String`, `&str`, or `Arc<str>` — static names are zero-copy
    /// when passed as `&'static str` via `Cow` or `Arc`.
    pub fn name(mut self, name: impl Into<Arc<str>>) -> Self {
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
                state: RwLock::new(StateMachine::new(&self.config)),
                config: self.config,
                state_change_callback: self.state_change_callback,
                half_open_permits: AtomicUsize::new(0),
            }),
        }
    }
}

// Tests exercise failure paths and invariants directly; unwrap/expect,
// slicing, and panicking asserts are acceptable here — violations
// surface as test failures, not production panics.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CircuitBreakerConfig;
    use std::time::Duration;

    /// Consecutive-tripping isolation: window rate disabled.
    fn consecutive_config(failures: u32, wait: Duration) -> CircuitBreakerConfig {
        CircuitBreakerConfig::builder()
            .consecutive_failures(failures)
            .failure_rate_threshold(0.0)
            .backoff(BackoffStrategy::Fixed(wait))
            .build()
    }

    #[test]
    fn config_standard_preset() {
        let c = CircuitBreakerConfig::standard();
        assert_eq!(c.failure_rate_threshold, 0.5);
        assert_eq!(c.consecutive_failures, 5);
        assert_eq!(c.sliding_window_size, 10);
        assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(30)));
        assert_eq!(c.half_open_max_calls, 3);
    }

    #[test]
    fn config_fast_fail_preset() {
        let c = CircuitBreakerConfig::fast_fail();
        assert_eq!(c.failure_rate_threshold, 1.0);
        assert_eq!(c.consecutive_failures, 1);
        assert_eq!(c.half_open_max_calls, 1);
        assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(10)));
    }

    #[test]
    fn config_lenient_preset() {
        let c = CircuitBreakerConfig::lenient();
        assert_eq!(c.failure_rate_threshold, 0.5);
        assert_eq!(c.consecutive_failures, 10);
        assert_eq!(c.sliding_window_size, 20);
        assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(60)));
        assert_eq!(c.half_open_max_calls, 5);
    }

    #[test]
    fn config_builder_custom() {
        let c = CircuitBreakerConfig::builder()
            .consecutive_failures(3)
            .failure_rate_threshold(0.75)
            .sliding_window_size(7)
            .backoff(BackoffStrategy::Exponential {
                initial: Duration::from_millis(5),
                max: Duration::from_secs(5),
                factor: 2.0,
            })
            .half_open_max_calls(2)
            .build();
        assert_eq!(c.consecutive_failures, 3);
        assert_eq!(c.failure_rate_threshold, 0.75);
        assert_eq!(c.sliding_window_size, 7);
        assert_eq!(c.half_open_max_calls, 2);
        assert_eq!(
            c.backoff,
            BackoffStrategy::Exponential {
                initial: Duration::from_millis(5),
                max: Duration::from_secs(5),
                factor: 2.0,
            }
        );
    }

    #[test]
    fn wait_duration_convenience_maps_to_fixed_backoff() {
        let c = CircuitBreakerConfig::builder()
            .wait_duration(Duration::from_secs(5))
            .build();
        assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(5)));
    }

    #[test]
    fn backoff_overrides_wait_duration() {
        let c = CircuitBreakerConfig::builder()
            .wait_duration(Duration::from_secs(5))
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(9)))
            .build();
        assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(9)));
    }

    #[tokio::test]
    async fn starts_in_closed_state() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        assert_eq!(cb.metrics().state, State::Closed);
    }

    #[tokio::test]
    async fn closed_to_open_after_failures() {
        let config = consecutive_config(3, Duration::from_secs(60));
        let cb = CircuitBreaker::new(config);

        for _ in 0..3 {
            let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        }

        assert_eq!(cb.metrics().state, State::Open);
        assert_eq!(cb.metrics().total_failures, 3);
    }

    #[tokio::test]
    async fn open_rejects_requests() {
        let config = consecutive_config(1, Duration::from_secs(60));
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
        let config = consecutive_config(1, Duration::from_millis(50));
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert_eq!(cb.metrics().state, State::Open);

        tokio::time::sleep(Duration::from_millis(100)).await;

        let result = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn half_open_to_closed_after_successes() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .success_threshold(2)
            .half_open_max_calls(2)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
            .build();
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert_eq!(cb.metrics().state, State::Open);

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(cb.metrics().state, State::HalfOpen);
        let _ = cb
            .call(|| async { Ok::<_, String>("ok".to_string()) })
            .await;
        assert_eq!(cb.metrics().total_successes, 1);
    }

    #[tokio::test]
    async fn half_open_to_open_on_failure() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .success_threshold(2)
            .half_open_max_calls(2)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
            .build();
        let cb = CircuitBreaker::new(config);

        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

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
        assert!((m.window_failure_rate - 1.0 / 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn failure_rate_zero_initially() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        let m = cb.metrics();
        assert_eq!(m.failure_rate, 0.0);
        assert_eq!(m.window_failure_rate, 0.0);
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
        let config = consecutive_config(3, Duration::from_secs(60));
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
            CircuitBreakerError::<String>::CircuitOpen.to_string(),
            "circuit breaker is open"
        );
        assert_eq!(
            CircuitBreakerError::<String>::Rejected.to_string(),
            "circuit breaker: half-open probe capacity exhausted"
        );
        assert_eq!(
            CircuitBreakerError::Failure("boom".to_string()).to_string(),
            "boom"
        );
        #[cfg(feature = "timeout")]
        assert_eq!(
            CircuitBreakerError::<String>::Timeout.to_string(),
            "circuit breaker: operation timed out"
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
    async fn async_call_failure_preserves_typed_error() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        let result = cb
            .call(|| async { Err::<String, _>("something broke") })
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CircuitBreakerError::Failure(msg) => assert_eq!(msg, "something broke"),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    /// The old `call()` stringified user errors (`Inner(err.to_string())`);
    /// 2.0.0 must preserve the original value — even for error types with
    /// no `Display`/`Error` impl at all.
    #[tokio::test]
    async fn non_display_error_type_passes_through() {
        #[derive(Debug, PartialEq)]
        struct Opaque {
            code: u16,
        }

        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        let result = cb
            .call(|| async { Err::<(), _>(Opaque { code: 418 }) })
            .await;
        match result.unwrap_err() {
            CircuitBreakerError::Failure(e) => assert_eq!(e, Opaque { code: 418 }),
            other => panic!("expected Failure, got {other:?}"),
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
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .success_threshold(2)
            .half_open_max_calls(2)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
            .build();
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert!(cb.is_open());

        // Wait for half-open
        std::thread::sleep(Duration::from_millis(50));
        assert!(cb.is_half_open());

        cb.record_success();
        assert!(cb.is_half_open()); // need 2 successes

        cb.record_success();
        assert!(cb.is_closed());
    }

    #[test]
    fn record_failure_manual() {
        let config = consecutive_config(2, Duration::from_secs(60));
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert!(cb.is_closed());

        cb.record_failure();
        assert!(cb.is_open());
    }

    #[test]
    fn record_failure_in_half_open_opens_circuit() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .success_threshold(2)
            .half_open_max_calls(2)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
            .build();
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        std::thread::sleep(Duration::from_millis(50));
        assert!(cb.is_half_open());

        cb.record_failure();
        assert!(cb.is_open());
    }

    #[test]
    fn on_state_change_callback() {
        use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let cb = CircuitBreaker::builder(
            CircuitBreakerConfig::builder()
                .consecutive_failures(1)
                .failure_rate_threshold(0.0)
                .build(),
        )
        .on_state_change(move |_prev, _next| {
            count_clone.fetch_add(1, StdOrdering::SeqCst);
        })
        .build();

        cb.record_failure(); // Closed -> Open
        assert_eq!(count.load(StdOrdering::SeqCst), 1);

        cb.reset(); // Open -> Closed
        assert_eq!(count.load(StdOrdering::SeqCst), 2);
    }

    #[test]
    fn on_state_change_records_transition() {
        let transitions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let t_clone = transitions.clone();

        let cb = CircuitBreaker::builder(
            CircuitBreakerConfig::builder()
                .consecutive_failures(1)
                .failure_rate_threshold(0.0)
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
        use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let cb = CircuitBreaker::builder(CircuitBreakerConfig::standard())
            .on_state_change(move |_, _| {
                count_clone.fetch_add(1, StdOrdering::SeqCst);
            })
            .build();

        cb.record_success();
        cb.record_success();
        assert_eq!(count.load(StdOrdering::SeqCst), 0);
        assert!(cb.is_closed());
    }

    // --- 2.0.0: sliding-window tripping ---------------------------------

    /// Window rate crosses threshold with failures interleaved with
    /// successes — the case the 1.x consecutive-only policy missed.
    #[tokio::test]
    async fn sliding_window_rate_trips_interleaved_failures() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(u32::MAX) // streak rule out of the way
            .failure_rate_threshold(0.5)
            .sliding_window_size(4)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build();
        let cb = CircuitBreaker::new(config);

        // ok, err, ok → window [0,1,0]: 1/3, and below minimum_calls (4)
        // the rate isn't even evaluated. Still closed, streak is irrelevant.
        let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
        assert!(cb.is_closed());

        // 4th call fails → window full at 4 outcomes, 2/4 = 0.5 >= 0.5:
        // trips even though the consecutive streak is only 1.
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert!(cb.is_open());
        assert_eq!(cb.metrics().total_failures, 2);
        assert!((cb.window_failure_rate() - 0.5).abs() < f32::EPSILON);
    }

    /// Once the breaker recovers to Closed, the window starts empty — old
    /// failures cannot trip the fresh episode.
    #[tokio::test]
    async fn sliding_window_resets_after_recovery() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(u32::MAX)
            .failure_rate_threshold(1.0) // only a 100 % full window trips
            .sliding_window_size(4)
            .success_threshold(1)
            .half_open_max_calls(2)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(20)))
            .build();
        let cb = CircuitBreaker::new(config);

        // Trip: 4 failures fill the window at 100 %.
        for _ in 0..4 {
            let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        }
        assert!(cb.is_open());

        // Recover.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
        assert!(cb.is_closed());

        // Window cleared: rate is 0.0, and a short failure burst cannot
        // re-trip until the window refills to minimum_calls (4).
        assert!((cb.window_failure_rate() - 0.0).abs() < f32::EPSILON);
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert!(cb.is_closed());

        // Refill: 4th failure makes the window 100 % again → trips.
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert!(cb.is_open());
    }

    // --- 2.0.0: half-open probe permits ---------------------------------

    /// More concurrent probes than `half_open_max_calls` → exactly
    /// `half_open_max_calls` run; the rest are rejected immediately and do
    /// not count as failures.
    #[tokio::test]
    async fn half_open_permits_bound_concurrent_probes() {
        use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};

        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .half_open_max_calls(2)
            .success_threshold(100) // stay half-open for the test
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
            .build();
        let cb = CircuitBreaker::new(config);

        cb.record_failure(); // → Open
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(cb.is_half_open());

        let entered = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let cb = cb.clone();
            let entered = entered.clone();
            handles.push(tokio::spawn(async move {
                cb.call(|| async {
                    entered.fetch_add(1, StdOrdering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok::<(), &str>(())
                })
                .await
            }));
        }

        let mut admitted = 0;
        let mut rejected = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(()) => admitted += 1,
                Err(CircuitBreakerError::Rejected) => rejected += 1,
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }

        assert_eq!(admitted, 2, "exactly half_open_max_calls probes admitted");
        assert_eq!(rejected, 4, "excess probes rejected with Rejected");
        assert_eq!(entered.load(StdOrdering::SeqCst), 2);
        assert_eq!(cb.metrics().total_failures, 1); // rejections don't count
    }

    /// A probe that fails releases its permit: the next half-open episode
    /// has full capacity again.
    #[tokio::test]
    async fn half_open_permits_released_after_failure() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .half_open_max_calls(1)
            .success_threshold(1)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
            .build();
        let cb = CircuitBreaker::new(config);

        // Episode 1: probe fails → permit released on completion.
        cb.record_failure();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
        assert!(cb.is_open());

        // Episode 2: the single permit must be available again.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let result = cb.call(|| async { Ok::<(), &str>(()) }).await;
        assert!(
            result.is_ok(),
            "permit must be reusable after a failed probe"
        );
        assert!(cb.is_closed());
    }

    // --- 2.0.0: failure predicate ---------------------------------------

    /// Predicate-false errors pass through typed, uncounted, and without
    /// tripping the breaker.
    #[tokio::test]
    async fn predicate_false_errors_do_not_trip() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .failure_predicate(|e: &String| e.contains("permanent"))
            .build();
        let cb = CircuitBreaker::new(config);

        // Transient errors: returned to the caller with the original value,
        // but invisible to the state machine.
        for _ in 0..5 {
            let result = cb
                .call(|| async { Err::<(), _>("transient blip".to_string()) })
                .await;
            match result.unwrap_err() {
                CircuitBreakerError::Failure(e) => assert_eq!(e, "transient blip"),
                other => panic!("expected Failure, got {other:?}"),
            }
        }
        assert!(cb.is_closed());
        assert_eq!(cb.metrics().total_failures, 0);

        // A permanent error counts and trips (consecutive_failures = 1).
        let result = cb
            .call(|| async { Err::<(), _>("permanent outage".to_string()) })
            .await;
        match result.unwrap_err() {
            CircuitBreakerError::Failure(e) => assert_eq!(e, "permanent outage"),
            other => panic!("expected Failure, got {other:?}"),
        }
        assert!(cb.is_open());
        assert_eq!(cb.metrics().total_failures, 1);
    }

    // --- 2.0.0: timeout feature -----------------------------------------

    #[cfg(feature = "timeout")]
    #[tokio::test]
    async fn timeout_fires_and_counts_as_failure() {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .call_timeout(Some(Duration::from_millis(20)))
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build();
        let cb = CircuitBreaker::new(config);

        let result = cb
            .call(|| async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok::<(), &str>(())
            })
            .await;
        assert!(
            matches!(result, Err(CircuitBreakerError::Timeout)),
            "expected Timeout, got {result:?}"
        );
        assert!(cb.is_open()); // timeout counted as the (first) failure
        assert_eq!(cb.metrics().total_failures, 1);
    }

    #[cfg(feature = "timeout")]
    #[tokio::test]
    async fn timeout_disabled_passes_slow_calls() {
        let config = CircuitBreakerConfig::builder().call_timeout(None).build();
        let cb = CircuitBreaker::new(config);
        let result = cb
            .call(|| async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok::<(), &str>(())
            })
            .await;
        assert!(result.is_ok());
    }

    // --- Mutation-killing test (cargo-mutants triage) ---

    /// The `failure_count` histogram must carry the real consecutive-failure
    /// count. Kills the `StateMachine::failure_count() -> 0` and `-> 1`
    /// replacement mutants, which survive every state-based assertion
    /// (thresholds are checked on the internal field, not the getter).
    #[cfg(feature = "metrics")]
    #[test]
    fn failure_count_histogram_records_consecutive_failures() {
        mod capture {
            use ::metrics::{
                Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder,
                SharedString, Unit,
            };
            use std::sync::{Arc, Mutex};

            #[derive(Clone, Default)]
            pub struct HistogramCapture(Arc<Mutex<Vec<f64>>>);

            impl HistogramCapture {
                pub fn values(&self) -> Vec<f64> {
                    self.0
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone()
                }
            }

            struct Capturing(HistogramCapture);

            impl HistogramFn for Capturing {
                fn record(&self, value: f64) {
                    self.0
                        .0
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(value);
                }
            }

            impl Recorder for HistogramCapture {
                fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
                fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
                fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
                fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
                    Counter::noop()
                }
                fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
                    Gauge::noop()
                }
                fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
                    if key.name() == "circuit_breaker_failure_count" {
                        Histogram::from_arc(Arc::new(Capturing(self.clone())))
                    } else {
                        Histogram::noop()
                    }
                }
            }
        }

        use ::metrics::with_local_recorder;
        use capture::HistogramCapture;

        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(5)
            .failure_rate_threshold(0.0)
            .sliding_window_size(10)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build();
        let cb = CircuitBreaker::new(config);
        let recorder = HistogramCapture::default();

        // The local recorder is thread-local, so the future must be driven on
        // this thread: a current-thread runtime under the recorder scope.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        with_local_recorder(&recorder, || {
            rt.block_on(async {
                let _ = cb.call(|| async { Err::<(), _>("boom") }).await;
                let _ = cb.call(|| async { Err::<(), _>("boom") }).await;
            });
        });

        assert_eq!(recorder.values(), vec![1.0, 2.0]);
    }
}
