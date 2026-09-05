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
//! Tests state transitions, metrics recording, config presets, and the builder
//! pattern from the perspective of the public API.

use breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError, CircuitMetrics, State};

// ---------------------------------------------------------------------------
// State transitions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_lifecycle_closed_open_halfopen_closed() {
    let config = CircuitBreakerConfig::builder()
        .failure_rate_threshold(2)
        .success_threshold(2)
        .half_open_max_calls(2)
        .wait_duration(std::time::Duration::from_millis(50))
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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
        .failure_rate_threshold(1)
        .success_threshold(2)
        .half_open_max_calls(2)
        .wait_duration(std::time::Duration::from_millis(50))
        .build();

    let cb = CircuitBreaker::new(config);

    let _ = cb.call(|| async { Err::<(), _>("fail") }).await;
    assert_eq!(cb.state(), State::Open);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(cb.state(), State::HalfOpen);

    // Fail in half-open -> back to Open
    let _ = cb.call(|| async { Err::<(), _>("fail again") }).await;
    assert_eq!(cb.state(), State::Open);
}

#[tokio::test]
async fn open_circuit_rejects_all_calls() {
    let config = CircuitBreakerConfig::builder()
        .failure_rate_threshold(1)
        .wait_duration(std::time::Duration::from_secs(60))
        .build();
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
    assert_eq!(m.state, State::Closed);
}

#[test]
fn metrics_initial_state_zeroes() {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
    let m: CircuitMetrics = cb.metrics();
    assert_eq!(m.failure_rate, 0.0);
    assert_eq!(m.total_successes, 0);
    assert_eq!(m.total_failures, 0);
    assert_eq!(m.transitions, 0);
    assert_eq!(m.state, State::Closed);
}

#[test]
fn metrics_transitions_count() {
    let config = CircuitBreakerConfig::builder()
        .failure_rate_threshold(2)
        .half_open_max_calls(1)
        .wait_duration(std::time::Duration::from_millis(10))
        .build();
    let cb = CircuitBreaker::new(config);

    cb.record_failure(); // 1st failure, still Closed, no transition
    assert_eq!(cb.metrics().transitions, 0);

    cb.record_failure(); // 2nd failure -> Open (1 transition)
    assert_eq!(cb.metrics().transitions, 1);

    std::thread::sleep(std::time::Duration::from_millis(50));
    // record_success triggers: Open->HalfOpen (via maybe_transition_to_half_open),
    // then HalfOpen->Closed (success_threshold met) = 2 more transitions
    cb.record_success();
    assert_eq!(cb.metrics().transitions, 3);
}

// ---------------------------------------------------------------------------
// Config presets
// ---------------------------------------------------------------------------

#[test]
fn config_standard_preset_values() {
    let c = CircuitBreakerConfig::standard();
    assert_eq!(c.failure_rate_threshold, 5);
    assert_eq!(c.sliding_window_size, 10);
    assert_eq!(c.wait_duration, std::time::Duration::from_secs(30));
    assert_eq!(c.half_open_max_calls, 3);
}

#[test]
fn config_fast_fail_preset_values() {
    let c = CircuitBreakerConfig::fast_fail();
    assert_eq!(c.failure_rate_threshold, 1);
    assert_eq!(c.sliding_window_size, 5);
    assert_eq!(c.wait_duration, std::time::Duration::from_secs(10));
    assert_eq!(c.half_open_max_calls, 1);
}

#[test]
fn config_lenient_preset_values() {
    let c = CircuitBreakerConfig::lenient();
    assert_eq!(c.failure_rate_threshold, 10);
    assert_eq!(c.sliding_window_size, 20);
    assert_eq!(c.wait_duration, std::time::Duration::from_secs(60));
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
            .failure_rate_threshold(1)
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
        .failure_rate_threshold(7)
        .sliding_window_size(15)
        .wait_duration(std::time::Duration::from_secs(45))
        .half_open_max_calls(4)
        .success_threshold(6)
        .build();
    assert_eq!(c.failure_rate_threshold, 7);
    assert_eq!(c.sliding_window_size, 15);
    assert_eq!(c.wait_duration, std::time::Duration::from_secs(45));
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
        CircuitBreakerError::CircuitOpen.to_string(),
        "circuit breaker is open"
    );
    assert_eq!(
        CircuitBreakerError::Timeout.to_string(),
        "circuit breaker: operation timed out or failed"
    );
    assert_eq!(
        CircuitBreakerError::Inner("test".into()).to_string(),
        "circuit breaker: inner error: test"
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
async fn call_returns_inner_error_value() {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
    let result = cb.call(|| async { Err::<(), _>("custom error msg") }).await;
    match result.unwrap_err() {
        CircuitBreakerError::Inner(msg) => assert_eq!(msg, "custom error msg"),
        other => panic!("expected Inner, got {:?}", other),
    }
}

#[test]
fn manual_record_success_and_failure() {
    let config = CircuitBreakerConfig::builder()
        .failure_rate_threshold(2)
        .half_open_max_calls(1)
        .wait_duration(std::time::Duration::from_millis(10))
        .build();
    let cb = CircuitBreaker::new(config);

    cb.record_failure();
    cb.record_failure();
    assert!(cb.is_open());

    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(cb.is_half_open());

    cb.record_success();
    assert!(cb.is_closed());
}
