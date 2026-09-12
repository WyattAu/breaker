//! Config-knob behavior matrix for breaker.
//!
//! Every public knob must OBSERVABLY change behavior: each test pairs a
//! default with an alternate value and asserts the observable output
//! differs.
//!
//! Knobs covered (8 builder knobs + `call_timeout`):
//!   1. `failure_rate_threshold` — 0.0 disables rate tripping (below)
//!   2. `consecutive_failures` — streak resets on success (below)
//!   3. `sliding_window_size` — window bounds the rate sample; different
//!      sizes trip at different times (below). This is the knob behind
//!      the historic dead-knob incident: now provably wired.
//!   4. `minimum_calls` — early-warning vs full-window evaluation (below)
//!   5. `half_open_max_calls` — permit exhaustion yields `Rejected`
//!      (`half_open_rejects_when_no_probe_capacity` in
//!      `tests/integration.rs`; `concurrent_requests_respect_probe_permits`
//!      in `tests/tower.rs`)
//!   6. `success_threshold` — N probes required to close (below)
//!   7. `backoff` — Fixed/Exponential/Jitter sequences
//!      (`src/config.rs` unit tests) and open-episode lengths
//!      (`exponential_backoff_extends_each_open_episode`,
//!      `jittered_backoff_still_opens_and_half_opens` in
//!      `tests/integration.rs`)
//!   8. `failure_predicate` — false passes through uncounted, true counts
//!      and trips (`predicate_classifies_errors` in
//!      `tests/integration.rs`; type-level classification in
//!      `src/config.rs` unit tests)
//!   9. `call_timeout` (`timeout` feature) — slow calls time out, count
//!      as failures (below + `src/lib.rs` unit test)
//!
//! This file proves the threshold/window/streak/permit interactions with
//! contrast pairs; pure-strategy bounds and the predicate/type matrix are
//! cited where they already carry proof.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::time::Duration;

use breaker::{BackoffStrategy, CircuitBreaker, CircuitBreakerConfig, State};

#[cfg(feature = "timeout")]
fn fast_config() -> CircuitBreakerConfig {
    CircuitBreakerConfig::builder()
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(20)))
        .half_open_max_calls(2)
        .success_threshold(1)
        .build()
}

async fn fail() -> Result<(), &'static str> {
    Err("fail")
}

async fn win() -> Result<(), &'static str> {
    Ok(())
}

// ---------------------------------------------------------------------------
// failure_rate_threshold: 0.0 disables rate-based tripping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_rate_threshold_zero_disables_rate_tripping() {
    // Rate tripping off + streak effectively off: alternating outcomes
    // never trip...
    let calm = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.0)
            .consecutive_failures(u32::MAX)
            .sliding_window_size(4)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    );
    for _ in 0..10 {
        let _ = calm.call(fail).await;
        let _ = calm.call(win).await;
    }
    assert!(calm.is_closed());

    // ...while the most aggressive rate trips on the very first failure.
    let touchy = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(1.0)
            .minimum_calls(1)
            .consecutive_failures(u32::MAX)
            .sliding_window_size(10)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    );
    let _ = touchy.call(fail).await;
    assert!(
        touchy.is_open(),
        "rate 1.0 over a 1-call window must trip immediately"
    );
}

// ---------------------------------------------------------------------------
// consecutive_failures: any success resets the streak
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_consecutive_streak_resets_on_success() {
    let mk = || {
        CircuitBreaker::new(
            CircuitBreakerConfig::builder()
                .failure_rate_threshold(0.0) // streak decides alone
                .consecutive_failures(2)
                .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
                .build(),
        )
    };

    // Two in a row trips...
    let cb = mk();
    let _ = cb.call(fail).await;
    let _ = cb.call(fail).await;
    assert!(cb.is_open());

    // ...but an interleaved success restarts the count.
    let cb = mk();
    let _ = cb.call(fail).await;
    let _ = cb.call(win).await;
    let _ = cb.call(fail).await;
    assert!(
        cb.is_closed(),
        "success must reset the consecutive-failure streak"
    );
}

// ---------------------------------------------------------------------------
// sliding_window_size: the window bounds the rate sample (the incident knob)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_window_size_bounds_the_rate_sample() {
    // Window 1: the single most recent failure IS the sample → trips.
    let narrow = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.5)
            .minimum_calls(1)
            .consecutive_failures(u32::MAX)
            .sliding_window_size(1)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    );
    let _ = narrow.call(fail).await;
    assert!(narrow.is_open());

    // Window 4 over the same outcomes: old failures are diluted by
    // successes and the rate never reaches 0.5.
    let wide = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.5)
            .minimum_calls(1)
            .consecutive_failures(u32::MAX)
            .sliding_window_size(4)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    );
    for _ in 0..3 {
        let _ = wide.call(win).await;
    }
    let _ = wide.call(fail).await; // window [S,S,S,F] → rate 0.25
    assert!(
        wide.is_closed(),
        "window=4 must dilute one failure in four outcomes"
    );
}

// ---------------------------------------------------------------------------
// minimum_calls: early-warning (1) vs full-window (size) evaluation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_minimum_calls_switches_early_warning() {
    // minimum_calls=1: the very first failure evaluates 1/1 = 1.0.
    let eager = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.5)
            .minimum_calls(1)
            .consecutive_failures(u32::MAX)
            .sliding_window_size(4)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    );
    let _ = eager.call(fail).await;
    assert!(eager.is_open());

    // minimum_calls=4: one failure is not yet evidence.
    let patient = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.5)
            .minimum_calls(4)
            .consecutive_failures(u32::MAX)
            .sliding_window_size(4)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    );
    let _ = patient.call(fail).await;
    assert!(
        patient.is_closed(),
        "minimum_calls=4 must wait for a full window"
    );
}

// ---------------------------------------------------------------------------
// success_threshold: N half-open probes required to close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_success_threshold_counts_probes_to_close() {
    let cb = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.0)
            .consecutive_failures(1)
            .half_open_max_calls(2)
            .success_threshold(2)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(20)))
            .build(),
    );
    let _ = cb.call(fail).await;
    assert!(cb.is_open());
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(cb.state(), State::HalfOpen);

    // One probe is not enough...
    let _ = cb.call(win).await;
    assert_eq!(
        cb.state(),
        State::HalfOpen,
        "success_threshold=2 must stay half-open after one probe"
    );
    // ...two close the circuit.
    let _ = cb.call(win).await;
    assert!(cb.is_closed());
}

// ---------------------------------------------------------------------------
// backoff: Fixed alias + strategy-driven episode timing bounds
// ---------------------------------------------------------------------------

#[test]
fn knob_wait_duration_is_fixed_backoff_alias() {
    let via_alias = CircuitBreakerConfig::builder()
        .wait_duration(Duration::from_millis(250))
        .build();
    let via_strategy = CircuitBreakerConfig::builder()
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(250)))
        .build();
    assert_eq!(via_alias.backoff.initial_wait(), Duration::from_millis(250));
    assert_eq!(
        via_alias.backoff.wait_for(7),
        via_strategy.backoff.wait_for(7)
    );
}

#[test]
fn knob_backoff_strategies_bound_waits() {
    let fixed = BackoffStrategy::Fixed(Duration::from_millis(100));
    let exp = BackoffStrategy::Exponential {
        initial: Duration::from_millis(10),
        max: Duration::from_millis(100),
        factor: 2.0,
    };
    let jitter = BackoffStrategy::ExponentialJitter {
        initial: Duration::from_millis(10),
        max: Duration::from_millis(100),
        factor: 2.0,
        jitter: 1.0,
    };
    // Fixed is constant; exponential grows then caps; full jitter stays
    // within [0, unjittered].
    for attempt in 1..=6 {
        assert_eq!(fixed.wait_for(attempt), Duration::from_millis(100));
        assert!(exp.wait_for(attempt) <= Duration::from_millis(100));
        let base = exp.wait_for(attempt);
        for _ in 0..50 {
            let w = jitter.wait_for(attempt);
            assert!(w <= base, "jittered {w:?} exceeds base {base:?}");
        }
    }
    assert_eq!(exp.wait_for(1), Duration::from_millis(10));
    assert_eq!(exp.wait_for(5), Duration::from_millis(100)); // capped
}

// ---------------------------------------------------------------------------
// failure_predicate + call_timeout contrasts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_predicate_false_passes_through_uncounted() {
    // Compact contrast next to `predicate_classifies_errors`: string errors
    // the predicate rejects never touch the failure counters.
    let cb = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.0)
            .consecutive_failures(1)
            .failure_predicate(|e: &String| e.contains("fatal"))
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    );
    for _ in 0..3 {
        let r = cb
            .call(|| async { Err::<(), _>("transient".to_string()) })
            .await;
        assert!(r.is_err());
    }
    assert!(cb.is_closed());
    assert_eq!(cb.metrics().total_failures, 0);

    let r = cb
        .call(|| async { Err::<(), _>("fatal: disk gone".to_string()) })
        .await;
    assert!(r.is_err());
    assert!(cb.is_open(), "predicate-true error must trip");
    assert_eq!(cb.metrics().total_failures, 1);
}

#[cfg(feature = "timeout")]
#[tokio::test]
async fn knob_call_timeout_fires_and_counts_failure() {
    // No timeout (default): a slow call succeeds...
    let patient = CircuitBreaker::new(fast_config());
    let r = patient
        .call(|| async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok::<(), &'static str>(())
        })
        .await;
    assert!(r.is_ok());
    assert!(patient.is_closed());

    // 20 ms timeout: the same call times out and the timeout counts as a
    // failure (streak 1 → open).
    let strict = CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(0.0)
            .consecutive_failures(1)
            .half_open_max_calls(1)
            .success_threshold(1)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .call_timeout(Some(Duration::from_millis(20)))
            .build(),
    );
    let r = strict
        .call(|| async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok::<(), &'static str>(())
        })
        .await;
    assert!(
        matches!(r, Err(breaker::CircuitBreakerError::Timeout)),
        "slow call must time out, got: {r:?}"
    );
    assert!(strict.is_open());
    assert_eq!(strict.metrics().total_failures, 1);
}
