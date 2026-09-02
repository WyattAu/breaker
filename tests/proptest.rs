use breaker::{CircuitBreaker, CircuitBreakerConfig};
use proptest::prelude::*;

fn arb_config() -> impl Strategy<Value = CircuitBreakerConfig> {
    (1u32..100u32, 1u32..100u32).prop_map(|(fail_thresh, succ_thresh)| {
        CircuitBreakerConfig::builder()
            .failure_rate_threshold(fail_thresh)
            .success_threshold(succ_thresh)
            .half_open_max_calls(succ_thresh)
            .wait_duration(std::time::Duration::from_millis(1))
            .build()
    })
}

proptest! {
    #[test]
    fn starts_closed(config in arb_config()) {
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
            .failure_rate_threshold(fail_thresh)
            .half_open_max_calls(1)
            .wait_duration(std::time::Duration::from_secs(60))
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
            .failure_rate_threshold(threshold)
            .half_open_max_calls(1)
            .wait_duration(std::time::Duration::from_secs(60))
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
    }
}
