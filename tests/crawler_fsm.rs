// Tests exercise failure paths directly; unwrap/expect, slicing, and
// panicking asserts are the test signal here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Crawler-style FSM semantics, mirroring the per-domain circuit breaker
//! tests in crawlkit-engine (`src/circuit_breaker.rs`): pure
//! consecutive-failure tripping (sliding-window rate disabled), an explicit
//! half-open success threshold, and a fixed cooldown — driven entirely
//! through the manual `record_success`/`record_failure` entry points.

use breaker::{BackoffStrategy, CircuitBreaker, CircuitBreakerConfig, State};
use std::time::Duration;

/// Crawler semantics: consecutive-streak tripping only, fixed cooldown.
fn crawler_config(failures: u32, successes: u32, cooldown: Duration) -> CircuitBreakerConfig {
    CircuitBreakerConfig::builder()
        .consecutive_failures(failures)
        .failure_rate_threshold(0.0)
        .success_threshold(successes)
        .wait_duration(cooldown)
        .build()
}

/// Mirrors crawlkit's `test_circuit_breaker_closed_by_default`.
#[test]
fn circuit_breaker_closed_by_default() {
    let cb = CircuitBreaker::new(crawler_config(5, 3, Duration::from_secs(60)));
    assert_eq!(cb.state(), State::Closed);
    assert!(cb.is_allowed());
}

/// Mirrors crawlkit's `test_circuit_breaker_opens_after_failures`:
/// threshold transition exactly at the Nth consecutive failure.
#[test]
fn circuit_breaker_opens_after_failures() {
    let cb = CircuitBreaker::new(crawler_config(3, 3, Duration::from_secs(60)));

    cb.record_failure();
    assert_eq!(cb.state(), State::Closed);

    cb.record_failure();
    assert_eq!(cb.state(), State::Closed);

    cb.record_failure();
    assert_eq!(cb.state(), State::Open);
    assert!(!cb.is_allowed());
}

/// Mirrors crawlkit's `test_circuit_breaker_success_resets_count`
/// (no-flap): an interleaved success resets the streak, so the circuit
/// never trips on scattered failures.
#[test]
fn circuit_breaker_success_resets_count() {
    let cb = CircuitBreaker::new(crawler_config(3, 3, Duration::from_secs(60)));

    cb.record_failure();
    cb.record_failure();
    cb.record_success(); // resets the failure streak
    cb.record_failure();
    assert_eq!(cb.state(), State::Closed);
    assert_eq!(cb.failure_count(), 1);
}

/// Cooldown gate: while the fixed wait has not elapsed, an `Open` circuit
/// stays `Open` and rejects; once it has, reads as `HalfOpen` and admits.
#[test]
fn circuit_breaker_cooldown_gates_half_open() {
    let cb = CircuitBreaker::new(crawler_config(1, 3, Duration::from_secs(60)));

    cb.record_failure();
    assert_eq!(cb.state(), State::Open);
    assert!(!cb.is_allowed()); // inside the cooldown window

    // Past the wait, the upgrade is applied lazily on read.
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(cb.state(), State::Open);

    // Now the same breaker with a short wait: gate opens.
    let fast = CircuitBreaker::new(crawler_config(1, 3, Duration::from_millis(10)));
    fast.record_failure();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(fast.state(), State::HalfOpen);
    assert!(fast.is_allowed());
}

/// Half-open recovery: `success_threshold` successful probes close the
/// circuit (mirrors crawlkit's HalfOpen → Closed semantics).
#[test]
fn circuit_breaker_half_open_success_closes() {
    let cb = CircuitBreaker::new(crawler_config(1, 2, Duration::from_millis(10)));

    cb.record_failure();
    assert_eq!(cb.state(), State::Open);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(cb.state(), State::HalfOpen);

    cb.record_success();
    assert_eq!(cb.state(), State::HalfOpen); // need 2 successes

    cb.record_success();
    assert_eq!(cb.state(), State::Closed);
    assert!(cb.is_allowed());
    assert_eq!(cb.failure_count(), 0);
}

/// Half-open failure re-trips immediately (single-probe no-flap guard),
/// and the cooldown restarts from the re-trip.
#[test]
fn circuit_breaker_half_open_failure_reopens() {
    let cb = CircuitBreaker::new(crawler_config(1, 2, Duration::from_millis(10)));

    cb.record_failure();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(cb.state(), State::HalfOpen);

    cb.record_failure();
    assert_eq!(cb.state(), State::Open);
    assert!(!cb.is_allowed()); // fresh cooldown in effect
}
