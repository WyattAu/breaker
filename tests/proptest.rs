// Property tests exercise hostile inputs directly; unwrap/expect, slicing,
// and panicking asserts are the test signal here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use breaker::{BackoffStrategy, CircuitBreaker, CircuitBreakerConfig};
use proptest::prelude::*;
use std::time::Duration;

/// Consecutive-tripping config (window rate disabled) so the streak
/// properties are tested in isolation.
fn arb_consecutive_config() -> impl Strategy<Value = CircuitBreakerConfig> {
    (1u32..100u32, 1u32..100u32).prop_map(|(fail_thresh, succ_thresh)| {
        CircuitBreakerConfig::builder()
            .consecutive_failures(fail_thresh)
            .failure_rate_threshold(0.0)
            .success_threshold(succ_thresh)
            .half_open_max_calls(succ_thresh)
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(1)))
            .build()
    })
}

/// Arbitrary sliding-window configs (rate enabled, streak disabled) for
/// window-level properties.
fn arb_window_config() -> impl Strategy<Value = CircuitBreakerConfig> {
    (1u32..16u32, 1u32..11u32).prop_map(|(window, min_calls)| {
        CircuitBreakerConfig::builder()
            .consecutive_failures(u32::MAX)
            .failure_rate_threshold(0.5)
            .sliding_window_size(window)
            .minimum_calls(min_calls.min(window))
            .backoff(BackoffStrategy::Fixed(Duration::from_millis(1)))
            .build()
    })
}

proptest! {
    #[test]
    fn starts_closed(config in arb_consecutive_config()) {
        let cb = CircuitBreaker::new(config);
        prop_assert!(cb.is_closed(), "should start in Closed state");
        prop_assert!(!cb.is_open());
        prop_assert!(!cb.is_half_open());
    }

    #[test]
    fn trip_makes_open(_dummy in 0..1u32) {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        cb.trip();
        prop_assert!(cb.is_open());
        prop_assert!(!cb.is_closed());
    }

    #[test]
    fn reset_after_trip_makes_closed(_dummy in 0..1u32) {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        cb.trip();
        prop_assert!(cb.is_open());
        cb.reset();
        prop_assert!(cb.is_closed());
        prop_assert!(!cb.is_open());
    }

    #[test]
    fn record_failure_threshold_opens(fail_thresh in 1u32..50u32) {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(fail_thresh)
            .failure_rate_threshold(0.0)
            .half_open_max_calls(1)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build();
        let cb = CircuitBreaker::new(config);

        for _ in 0..fail_thresh {
            cb.record_failure();
        }
        prop_assert!(cb.is_open(), "should be open after {} failures", fail_thresh);
    }

    #[test]
    fn success_resets_failure_count(threshold in 2u32..50u32) {
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(threshold)
            .failure_rate_threshold(0.0)
            .half_open_max_calls(1)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build();
        let cb = CircuitBreaker::new(config);

        // Record threshold - 1 failures
        for _ in 0..threshold - 1 {
            cb.record_failure();
        }
        prop_assert!(cb.is_closed(), "should still be closed");

        // A success resets the count
        cb.record_success();
        prop_assert!(cb.is_closed());

        // Record threshold - 1 more failures (should still be closed since count was reset)
        for _ in 0..threshold - 1 {
            cb.record_failure();
        }
        prop_assert!(cb.is_closed());
    }

    #[test]
    fn failure_rate_bounded(n_successes in 0u64..1000u64, n_failures in 0u64..1000u64) {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
        for _ in 0..n_successes {
            cb.record_success();
        }
        for _ in 0..n_failures {
            cb.record_failure();
        }
        let m = cb.metrics();
        prop_assert!(m.failure_rate >= 0.0);
        prop_assert!(m.failure_rate <= 1.0);
        prop_assert!((0.0..=1.0).contains(&m.window_failure_rate));
    }

    // --- 2.0.0: sliding-window properties -------------------------------

    /// The window holds at most `sliding_window_size` outcomes and its rate
    /// stays in [0, 1] under any outcome sequence. `total_failures` counts
    /// every failure even after the circuit trips (failures in Open are
    /// no-ops for the state machine, so the window stays frozen there).
    #[test]
    fn window_rate_stays_bounded(
        config in arb_window_config(),
        outcomes in proptest::collection::vec(proptest::bool::ANY, 0..64),
    ) {
        let cb = CircuitBreaker::new(config);
        for failed in outcomes {
            if failed {
                cb.record_failure();
            } else {
                cb.record_success();
            }
            let m = cb.metrics();
            prop_assert!((0.0..=1.0).contains(&m.window_failure_rate));
        }
    }

    /// Once the window is full, the recorded failure count in the window
    /// can never exceed the window capacity.
    #[test]
    fn window_filled_never_exceeds_capacity(
        config in arb_window_config(),
        outcomes in proptest::collection::vec(proptest::bool::ANY, 0..64),
    ) {
        let capacity = config.sliding_window_size;
        let cb = CircuitBreaker::new(config);
        for failed in outcomes {
            if failed {
                cb.record_failure();
                // A trip freezes the window; stop recording into it.
                if cb.is_open() {
                    break;
                }
            } else {
                cb.record_success();
            }
            prop_assert!(cb.window_filled() <= capacity);
        }
    }
}
