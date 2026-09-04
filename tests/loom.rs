//! Loom model-checking tests for the circuit breaker state machine.
//!
//! The production lock (`parking_lot::RwLock`) is swapped for
//! `loom::sync::RwLock` under `--cfg loom` (see `src/lib.rs`), so these
//! models explore every bounded interleaving of state transitions and prove
//! the state machine never loses an update:
//!
//! - Model A: two threads record failures concurrently against a threshold
//!   of 2 — the circuit must trip exactly once and both failures must be
//!   counted (a lock bug would show up as a lost update or double trip).
//! - Model B: `trip()` races `record_success()` — under the documented
//!   total-ordering semantics (each record/trip is serialized by the state
//!   lock), the final state must be `Open` with exactly one transition.
//!
//! What loom does NOT cover: the async `call()` path's read-decide-await-
//! record window (loom models sync primitives; the await point is a
//! deliberate, documented design — results are recorded as they complete).
//!
//! Run with:
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom
//! ```

#![cfg(loom)]

use breaker::{CircuitBreaker, CircuitBreakerConfig, State};
use loom::sync::Arc;
use loom::thread;
use std::time::Duration;

/// Config with a huge wait duration so the Open -> HalfOpen timer never
/// fires during a model (keeps the models deterministic).
fn deterministic_config(failure_threshold: u32) -> CircuitBreakerConfig {
    CircuitBreakerConfig::builder()
        .failure_rate_threshold(failure_threshold)
        .wait_duration(Duration::from_secs(3600))
        .build()
}

/// Model A: concurrent failure recording must not lose updates.
///
/// Both threads observe `Closed` and each records one failure. Under the
/// state lock every update is serialized, so the circuit trips exactly once
/// when the second failure lands and the failure totals are exact.
#[test]
fn loom_concurrent_failures_no_lost_updates() {
    loom::model(|| {
        let cb = Arc::new(CircuitBreaker::new(deterministic_config(2)));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let cb = Arc::clone(&cb);
            handles.push(thread::spawn(move || {
                cb.record_failure();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let m = cb.metrics();
        assert_eq!(m.state, State::Open);
        assert_eq!(m.total_failures, 2, "lost failure update");
        assert_eq!(m.transitions, 1, "circuit must trip exactly once");
        assert_eq!(m.failure_rate, 1.0);
    });
}

/// Model B: concurrent trip + success recording.
///
/// `record_success` on an `Open` circuit is a no-op (no transition), and
/// `trip` always forces `Open`. Whatever order the two operations execute
/// in, the serialized final state must be `Open` with exactly one
/// transition, and the success must still be counted in the totals.
#[test]
fn loom_concurrent_trip_and_success_serialized() {
    loom::model(|| {
        let cb = CircuitBreaker::new(deterministic_config(10));
        let cb2 = cb.clone();

        let handle = thread::spawn(move || {
            cb2.record_success();
        });
        cb.trip();
        handle.join().unwrap();

        let m = cb.metrics();
        assert_eq!(m.state, State::Open);
        assert_eq!(m.transitions, 1);
        assert_eq!(m.total_successes, 1);
        assert_eq!(m.total_failures, 0);
    });
}
