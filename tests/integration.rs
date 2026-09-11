// Tests exercise failure paths directly; unwrap/expect, slicing, and
// panicking asserts are the test signal here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Integration tests for the breaker crate.
//!
//! Tests state transitions, metrics recording, config presets, the builder
//! pattern, sliding-window tripping, half-open probe permits, backoff,
//! failure predicates, and typed errors from the perspective of the public
//! API.

use breaker::{
    BackoffStrategy, CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError, CircuitMetrics,
    State,
};
use std::time::Duration;

/// Consecutive-tripping isolation: window rate disabled.
fn consecutive_config(failures: u32, wait: Duration) -> CircuitBreakerConfig {
    CircuitBreakerConfig::builder()
        .consecutive_failures(failures)
        .failure_rate_threshold(0.0)
        .backoff(BackoffStrategy::Fixed(wait))
        .build()
}

// ---------------------------------------------------------------------------
// State transitions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_lifecycle_closed_open_halfopen_closed() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(2)
        .failure_rate_threshold(0.0)
        .success_threshold(2)
        .half_open_max_calls(2)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(50)))
        .build();

    let cb = CircuitBreaker::new(config);

    // Starts Closed
    assert_eq!(cb.state(), State::Closed);
    assert!(cb.is_closed());

    // Fail twice -> Open
    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    assert_eq!(cb.state(), State::Closed);
    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    assert_eq!(cb.state(), State::Open);
    assert!(cb.is_open());

    // Wait for transition to HalfOpen
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(cb.state(), State::HalfOpen);
    assert!(cb.is_half_open());

    // Two successes in HalfOpen -> Closed
    let _ = cb
        .call(|| async { Ok::<_, String>("ok".to_string()) })
        .await;
    let _ = cb
        .call(|| async { Ok::<_, String>("ok".to_string()) })
        .await;
    assert_eq!(cb.state(), State::Closed);
}

#[tokio::test]
async fn half_open_failure_reopens_circuit() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(1)
        .failure_rate_threshold(0.0)
        .success_threshold(2)
        .half_open_max_calls(2)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(50)))
        .build();

    let cb = CircuitBreaker::new(config);

    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    assert_eq!(cb.state(), State::Open);

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(cb.state(), State::HalfOpen);

    // Fail in half-open -> back to Open
    let _ = cb.call(|| async { Err::<(), _>("fail again") }).await;
    assert_eq!(cb.state(), State::Open);
}

#[tokio::test]
async fn open_circuit_rejects_all_calls() {
    let config = consecutive_config(1, Duration::from_secs(60));
    let cb = CircuitBreaker::new(config);

    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    assert_eq!(cb.state(), State::Open);

    let result = cb
        .call(|| async { Ok::<_, String>("should not run".to_string()) })
        .await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        CircuitBreakerError::CircuitOpen
    ));
}

// ---------------------------------------------------------------------------
// Metrics recording
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_tracks_successes_and_failures() {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());

    let _ = cb
        .call(|| async { Ok::<_, String>("ok".to_string()) })
        .await;
    let _ = cb
        .call(|| async { Ok::<_, String>("ok".to_string()) })
        .await;
    let _ = cb
        .call(|| async { Ok::<_, String>("ok".to_string()) })
        .await;
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;

    let m = cb.metrics();
    assert_eq!(m.total_successes, 3);
    assert_eq!(m.total_failures, 1);
    assert!((m.failure_rate - 0.25).abs() < f64::EPSILON);
    assert!((m.window_failure_rate - 0.25).abs() < f32::EPSILON);
    assert_eq!(m.state, State::Closed);
}

#[test]
fn metrics_initial_state_zeroes() {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
    let m: CircuitMetrics = cb.metrics();
    assert_eq!(m.failure_rate, 0.0);
    assert_eq!(m.window_failure_rate, 0.0);
    assert_eq!(m.total_successes, 0);
    assert_eq!(m.total_failures, 0);
    assert_eq!(m.transitions, 0);
    assert_eq!(m.state, State::Closed);
}

#[test]
fn metrics_transitions_count() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(2)
        .failure_rate_threshold(0.0)
        .half_open_max_calls(1)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
        .build();
    let cb = CircuitBreaker::new(config);

    cb.record_failure(); // 1st failure, still Closed, no transition
    assert_eq!(cb.metrics().transitions, 0);

    cb.record_failure(); // 2nd failure -> Open (1 transition)
    assert_eq!(cb.metrics().transitions, 1);

    std::thread::sleep(Duration::from_millis(50));
    // record_success triggers: Open->HalfOpen (via maybe_transition_to_half_open),
    // then HalfOpen->Closed (success_threshold met) = 2 more transitions
    cb.record_success();
    assert_eq!(cb.metrics().transitions, 3);
}

// ---------------------------------------------------------------------------
// Sliding-window tripping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sliding_window_trips_on_rate_crossing() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(u32::MAX) // streak rule disabled
        .failure_rate_threshold(0.5)
        .sliding_window_size(4)
        .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
        .build();
    let cb = CircuitBreaker::new(config);

    // 2 ok, 2 err → window [0,0,1,1], filled = 4 >= minimum_calls,
    // rate = 0.5 >= 0.5 → trips with a max streak of 1.
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    assert!(cb.is_closed());
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    assert!(cb.is_open());
}

#[tokio::test]
async fn window_rate_is_evaluated_when_failures_are_recorded() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(u32::MAX)
        .failure_rate_threshold(0.5)
        .sliding_window_size(2)
        .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
        .build();
    let cb = CircuitBreaker::new(config);

    // err → 1/1, but below minimum_calls (2): rate not evaluated.
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    assert!(cb.is_closed());

    // ok → window [1,0]: the fraction is 0.5 ≥ threshold, but a success can
    // only *lower* the rate, so the trip check runs on failure recording
    // only (documented semantics). Still closed.
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    assert!(cb.is_closed());

    // Next failure evicts the oldest outcome (the first failure) and
    // records itself → [0,1] = 0.5 ≥ 0.5 → trips.
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    assert!(cb.is_open());
}

#[tokio::test]
async fn sliding_window_reset_on_close() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(u32::MAX)
        .failure_rate_threshold(0.5)
        .sliding_window_size(4)
        .success_threshold(1)
        .half_open_max_calls(2)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(20)))
        .build();
    let cb = CircuitBreaker::new(config);

    // Trip on 2/4 failures (interleaved with successes).
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    assert!(cb.is_open());

    // Recover → window must be empty again.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    assert!(cb.is_closed());
    assert!((cb.window_failure_rate() - 0.0).abs() < f32::EPSILON);

    // The pre-trip failures must not count: 2 fresh failures keep the
    // window below minimum_calls → still closed.
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    assert!(cb.is_closed());
}

#[tokio::test]
async fn early_warning_mode_evaluates_partially_filled_window() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(u32::MAX)
        .failure_rate_threshold(0.5)
        .sliding_window_size(4)
        .minimum_calls(1)
        .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
        .build();
    let cb = CircuitBreaker::new(config);

    // minimum_calls = 1: the literal failures/min(size, filled) formula —
    // the first failure is 1/1 = 1.0 >= 0.5 → trips immediately.
    let _ = cb.call(|| async { Err::<(), _>("err") }).await;
    assert!(cb.is_open());
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exponential_backoff_extends_each_open_episode() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(1)
        .failure_rate_threshold(0.0)
        .half_open_max_calls(1)
        .success_threshold(1)
        .backoff(BackoffStrategy::Exponential {
            initial: Duration::from_millis(40),
            max: Duration::from_millis(400),
            factor: 2.0,
        })
        .build();
    let cb = CircuitBreaker::new(config);

    // Trip 1: wait 40 ms before HalfOpen.
    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert_eq!(cb.state(), State::Open, "first episode still in backoff");
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(cb.state(), State::HalfOpen);

    // Probe fails → re-trip (attempt 2): wait 80 ms this time.
    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(cb.state(), State::Open, "second episode must wait longer");
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(cb.state(), State::HalfOpen);

    // Probe succeeds → closed; the attempt counter resets.
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    assert!(cb.is_closed());
}

#[tokio::test]
async fn jittered_backoff_still_opens_and_half_opens() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(1)
        .failure_rate_threshold(0.0)
        .half_open_max_calls(1)
        .success_threshold(1)
        .backoff(BackoffStrategy::ExponentialJitter {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(50),
            factor: 2.0,
            jitter: 1.0, // full jitter: wait ∈ [0, computed]
        })
        .build();
    let cb = CircuitBreaker::new(config);

    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    assert!(cb.is_open());
    // Full jitter waits at most the unjittered duration (10 ms for
    // attempt 1); by 60 ms the circuit must be probing.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(cb.state(), State::HalfOpen);
    let _ = cb.call(|| async { Ok::<(), &str>(()) }).await;
    assert!(cb.is_closed());
}

// ---------------------------------------------------------------------------
// Half-open probe permits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn half_open_rejects_when_no_probe_capacity() {
    use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};

    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(1)
        .failure_rate_threshold(0.0)
        .half_open_max_calls(1)
        .success_threshold(100)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
        .build();
    let cb = CircuitBreaker::new(config);

    cb.record_failure();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(cb.is_half_open());

    // Occupy the single permit with a slow in-flight probe.
    let entered = Arc::new(AtomicUsize::new(0));
    let handle = {
        let cb = cb.clone();
        let entered = entered.clone();
        tokio::spawn(async move {
            cb.call(|| async {
                entered.fetch_add(1, StdOrdering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok::<(), &str>(())
            })
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await; // let it acquire
    assert_eq!(entered.load(StdOrdering::SeqCst), 1);

    // No capacity: rejected immediately, not counted as a failure.
    let result = cb.call(|| async { Ok::<(), &str>(()) }).await;
    assert!(matches!(result, Err(CircuitBreakerError::Rejected)));
    assert_eq!(cb.metrics().total_failures, 1);

    // Probe completes → permit released → calls admitted again.
    handle.await.unwrap().unwrap();
    let result = cb.call(|| async { Ok::<(), &str>(()) }).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn cancelled_probe_releases_permit() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(1)
        .failure_rate_threshold(0.0)
        .half_open_max_calls(1)
        .success_threshold(100)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
        .build();
    let cb = CircuitBreaker::new(config);

    cb.record_failure();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Start a probe on a spawned task, then abort it mid-flight: the
    // cancelled future must drop its HalfOpenPermit and free the slot.
    let handle = {
        let cb = cb.clone();
        tokio::spawn(async move {
            cb.call(|| async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok::<(), &str>(())
            })
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.abort();
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Capacity must be available again despite the cancelled probe.
    let result = cb.call(|| async { Ok::<(), &str>(()) }).await;
    assert!(result.is_ok(), "cancelled probe must release its permit");
}

// ---------------------------------------------------------------------------
// Config presets
// ---------------------------------------------------------------------------

#[test]
fn config_standard_preset_values() {
    let c = CircuitBreakerConfig::standard();
    assert_eq!(c.failure_rate_threshold, 0.5);
    assert_eq!(c.consecutive_failures, 5);
    assert_eq!(c.sliding_window_size, 10);
    assert_eq!(c.minimum_calls, 10);
    assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(30)));
    assert_eq!(c.half_open_max_calls, 3);
}

#[test]
fn config_fast_fail_preset_values() {
    let c = CircuitBreakerConfig::fast_fail();
    assert_eq!(c.failure_rate_threshold, 1.0);
    assert_eq!(c.consecutive_failures, 1);
    assert_eq!(c.sliding_window_size, 5);
    assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(10)));
    assert_eq!(c.half_open_max_calls, 1);
}

#[test]
fn config_lenient_preset_values() {
    let c = CircuitBreakerConfig::lenient();
    assert_eq!(c.failure_rate_threshold, 0.5);
    assert_eq!(c.consecutive_failures, 10);
    assert_eq!(c.sliding_window_size, 20);
    assert_eq!(c.minimum_calls, 20);
    assert_eq!(c.backoff, BackoffStrategy::Fixed(Duration::from_secs(60)));
    assert_eq!(c.half_open_max_calls, 5);
}

// ---------------------------------------------------------------------------
// Builder pattern
// ---------------------------------------------------------------------------

#[test]
fn builder_sets_name() {
    let cb = CircuitBreaker::builder(CircuitBreakerConfig::standard())
        .name("my-service")
        .build();
    assert_eq!(cb.name(), "my-service");
}

#[test]
fn builder_default_name() {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
    assert_eq!(cb.name(), "default");
}

#[test]
fn builder_with_state_change_callback() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();

    let cb = CircuitBreaker::builder(
        CircuitBreakerConfig::builder()
            .consecutive_failures(1)
            .failure_rate_threshold(0.0)
            .build(),
    )
    .on_state_change(move |_prev, _next| {
        c.fetch_add(1, Ordering::SeqCst);
    })
    .build();

    cb.record_failure(); // Closed -> Open
    cb.reset(); // Open -> Closed
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn builder_config_fields() {
    let c = CircuitBreakerConfig::builder()
        .consecutive_failures(7)
        .failure_rate_threshold(0.8)
        .sliding_window_size(15)
        .minimum_calls(5)
        .backoff(BackoffStrategy::Exponential {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(45),
            factor: 2.0,
        })
        .half_open_max_calls(4)
        .success_threshold(6)
        .build();
    assert_eq!(c.consecutive_failures, 7);
    assert_eq!(c.failure_rate_threshold, 0.8);
    assert_eq!(c.sliding_window_size, 15);
    assert_eq!(c.minimum_calls, 5);
    assert_eq!(
        c.backoff,
        BackoffStrategy::Exponential {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(45),
            factor: 2.0,
        }
    );
    assert_eq!(c.half_open_max_calls, 4);
    assert_eq!(c.success_threshold, 6);
}

#[test]
fn trip_and_reset_forced_transitions() {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
    assert!(cb.is_closed());

    cb.trip();
    assert!(cb.is_open());
    assert_eq!(cb.metrics().transitions, 1);

    cb.reset();
    assert!(cb.is_closed());
    assert_eq!(cb.metrics().transitions, 2);
}

#[test]
fn error_display_all_variants() {
    assert_eq!(
        CircuitBreakerError::<String>::CircuitOpen.to_string(),
        "circuit breaker is open"
    );
    assert_eq!(
        CircuitBreakerError::<String>::Rejected.to_string(),
        "circuit breaker: half-open probe capacity exhausted"
    );
    assert_eq!(
        CircuitBreakerError::Failure("test".to_string()).to_string(),
        "test"
    );
}

#[test]
fn success_threshold_defaults_to_half_open_max() {
    let c = CircuitBreakerConfig::builder()
        .half_open_max_calls(9)
        .build();
    assert_eq!(c.success_threshold, 9);
}

#[tokio::test]
async fn call_returns_original_typed_error() {
    #[derive(Debug, PartialEq)]
    struct ApiError {
        status: u16,
        message: &'static str,
    }

    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
    let result = cb
        .call(|| async {
            Err::<(), _>(ApiError {
                status: 503,
                message: "unavailable",
            })
        })
        .await;
    match result.unwrap_err() {
        CircuitBreakerError::Failure(e) => {
            assert_eq!(e.status, 503);
            assert_eq!(e.message, "unavailable");
        }
        other => panic!("expected Failure, got {other:?}"),
    }
}

#[tokio::test]
async fn predicate_classifies_errors() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(2)
        .failure_rate_threshold(0.0)
        .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
        .failure_predicate(|e: &std::io::Error| e.kind() == std::io::ErrorKind::ConnectionRefused)
        .build();
    let cb = CircuitBreaker::new(config);

    // Predicate-false errors pass through and never trip…
    for _ in 0..5 {
        let result = cb
            .call(|| async {
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "bad request",
                ))
            })
            .await;
        assert!(matches!(result, Err(CircuitBreakerError::Failure(_))));
    }
    assert!(cb.is_closed());
    assert_eq!(cb.metrics().total_failures, 0);

    // …predicate-true errors count and trip at the threshold.
    for _ in 0..2 {
        let result = cb
            .call(|| async {
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "down",
                ))
            })
            .await;
        assert!(matches!(result, Err(CircuitBreakerError::Failure(_))));
    }
    assert!(cb.is_open());
    assert_eq!(cb.metrics().total_failures, 2);
}

#[test]
fn manual_record_success_and_failure() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(2)
        .failure_rate_threshold(0.0)
        .half_open_max_calls(1)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
        .build();
    let cb = CircuitBreaker::new(config);

    cb.record_failure();
    cb.record_failure();
    assert!(cb.is_open());

    std::thread::sleep(Duration::from_millis(50));
    assert!(cb.is_half_open());

    cb.record_success();
    assert!(cb.is_closed());
}

use std::sync::Arc;
