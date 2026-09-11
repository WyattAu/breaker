use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

/// Strategy for how long the circuit stays `Open` between tripping and
/// probing again. Applied on **every** trip: the first `Closed → Open`
/// transition uses attempt 1, each subsequent trip (including
/// `HalfOpen → Open` re-trips) increments the attempt counter, so repeated
/// failures back off progressively instead of retry-hammering at a fixed
/// cadence.
///
/// The default (used by [`CircuitBreakerConfig::standard`] and friends) is
/// [`BackoffStrategy::Fixed`], which reproduces the 1.x `wait_duration`
/// behavior.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackoffStrategy {
    /// Constant wait, identical on every trip (1.x behavior).
    Fixed(Duration),
    /// Exponential backoff: `initial * factor^(attempt - 1)`, capped at
    /// `max`. Attempt 1 waits `initial`.
    Exponential {
        /// Wait after the first trip.
        initial: Duration,
        /// Upper bound on the computed wait.
        max: Duration,
        /// Growth multiplier per trip (`>= 1.0` recommended; values `< 1.0`
        /// shrink the wait and are clamped at `max`/`initial` by the same
        /// formula).
        factor: f64,
    },
    /// Exponential backoff with randomized jitter. The computed duration is
    /// the [`BackoffStrategy::Exponential`] formula, then scaled by
    /// `(1.0 - jitter * rand())` where `rand()` is uniform in `[0, 1)`.
    ///
    /// `jitter = 1.0` yields *full jitter* — a uniform pick from
    /// `[0, computed]` — which spreads retries of synchronized callers
    /// across the whole window and prevents retry stampedes.
    /// `jitter = 0.0` degenerates to plain [`BackoffStrategy::Exponential`].
    ///
    /// The randomness source is a non-cryptographic thread-local xorshift
    /// generator seeded from the wall clock; suitable for retry scheduling,
    /// not for security.
    ExponentialJitter {
        /// Wait after the first trip (before jitter).
        initial: Duration,
        /// Upper bound on the computed wait (before jitter).
        max: Duration,
        /// Growth multiplier per trip.
        factor: f64,
        /// Jitter fraction in `[0.0, 1.0]`; `1.0` = full jitter.
        jitter: f64,
    },
}

impl BackoffStrategy {
    /// Wait duration for the given trip attempt (1-based). Pure function:
    /// deterministic for [`BackoffStrategy::Fixed`] and
    /// [`BackoffStrategy::Exponential`]; randomized (but bounded) for
    /// [`BackoffStrategy::ExponentialJitter`].
    pub fn wait_for(&self, attempt: u32) -> Duration {
        match *self {
            BackoffStrategy::Fixed(d) => d,
            BackoffStrategy::Exponential {
                initial,
                max,
                factor,
            } => exponential_wait(initial, max, factor, attempt),
            BackoffStrategy::ExponentialJitter {
                initial,
                max,
                factor,
                jitter,
            } => {
                let base = exponential_wait(initial, max, factor, attempt);
                let jitter = jitter.clamp(0.0, 1.0);
                let scale = 1.0 - jitter * jitter_rand();
                base.mul_f64(scale.clamp(0.0, 1.0))
            }
        }
    }

    /// The wait before the very first trip is observed (used to initialize
    /// the state machine before any transition has occurred). For all
    /// strategies this equals `wait_for(1)`.
    pub fn initial_wait(&self) -> Duration {
        self.wait_for(1)
    }
}

/// `initial * factor^(attempt - 1)`, capped at `max`. Computed in `f64` and
/// clamped *before* conversion so overflow (huge attempts/factors) saturates
/// at `max` instead of panicking.
fn exponential_wait(initial: Duration, max: Duration, factor: f64, attempt: u32) -> Duration {
    let initial = initial.as_secs_f64();
    let max = max.as_secs_f64();
    // Cap the exponent: `factor^1024` already overflows to infinity for any
    // factor > 1 (and the result is clamped to `max` regardless), and this
    // avoids the i32-cast wraparound of large u32 attempts.
    let exp = attempt.saturating_sub(1).min(1024) as f64;
    let computed = initial * factor.powf(exp);
    // `min` guards infinity/NaN from absurd exponents; `max_f64` is finite
    // and positive (validated by the builder), so `from_secs_f64` cannot
    // panic.
    Duration::from_secs_f64(computed.min(max).max(0.0))
}

/// Non-cryptographic uniform `[0, 1)` sample for [`BackoffStrategy::
/// ExponentialJitter`]. Thread-local xorshift64* seeded from the wall clock;
/// no external RNG dependency, no `unsafe`.
fn jitter_rand() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0) };
    }
    STATE.with(|state| {
        let mut x = state.get();
        if x == 0 {
            x = seed();
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        state.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

fn seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    // xorshift requires a nonzero state.
    n | 1
}

/// Classifier signature: given a type-erased error, does it count as a
/// breaker failure?
type Classifier = Arc<dyn Fn(&dyn Any) -> bool + Send + Sync>;

/// Type-erased failure classifier. Built from a user closure via
/// [`CircuitBreakerConfigBuilder::failure_predicate`]; consulted by
/// [`CircuitBreaker::call`](crate::CircuitBreaker::call) to decide whether an
/// operation error counts as a breaker failure or passes through
/// un-counted.
///
/// The closure's error type is erased via `dyn Any`; if the runtime error
/// type does not match the type the predicate was built for, the default is
/// *conservative*: the error counts as a failure (same as having no
/// predicate).
#[derive(Clone)]
pub(crate) struct FailurePredicate {
    classify: Classifier,
}

impl FailurePredicate {
    pub(crate) fn new<E: 'static>(f: impl Fn(&E) -> bool + Send + Sync + 'static) -> Self {
        Self {
            classify: Arc::new(move |err: &dyn Any| match err.downcast_ref::<E>() {
                Some(e) => f(e),
                // Predicate built for a different error type than the one
                // this breaker is being called with: default to "counts as
                // failure" (matches the no-predicate behavior).
                None => true,
            }),
        }
    }

    pub(crate) fn is_failure(&self, err: &dyn Any) -> bool {
        (self.classify)(err)
    }
}

impl std::fmt::Debug for FailurePredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FailurePredicate(<closure>)")
    }
}

/// Configuration for a [`CircuitBreaker`](crate::CircuitBreaker).
///
/// # Tripping policy (2.0.0)
///
/// While `Closed`, the circuit trips when **either** condition fires first:
///
/// 1. **Consecutive failures** — [`Self::consecutive_failures`] failures
///    in a row (any success resets the streak).
/// 2. **Sliding-window failure rate** — over the last
///    [`Self::sliding_window_size`] recorded outcomes (0 = success, 1 =
///    failure), the failure fraction reaches
///    [`Self::failure_rate_threshold`]. The rate is evaluated as
///    `window_failures / min(window_size, filled)` where `filled` is the
///    number of outcomes recorded since the window last reset — but only
///    once `filled >= minimum_calls`. Defaults make the denominator the
///    full window: *5 failures within the last 10 calls* for the
///    `standard` preset. Setting
///    [`minimum_calls`](CircuitBreakerConfigBuilder::minimum_calls) to 1
///    enables the aggressive early-warning variant where every outcome is
///    evaluated against a partially-filled window (so the very first
///    failure — a rate of 1/1 — trips any threshold ≤ 1.0).
///
/// Set [`Self::failure_rate_threshold`] to `0.0` to disable rate-based
/// tripping and rely on consecutive failures alone (or vice versa:
/// `consecutive_failures = u32::MAX` disables the streak rule).
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Failure fraction (0.0 – 1.0) in the sliding window at which the
    /// circuit trips. `0.0` disables rate-based tripping.
    pub failure_rate_threshold: f32,
    /// Consecutive failures (streak, any success resets it) that trip the
    /// circuit. Evaluated in addition to the window rate; whichever fires
    /// first wins. Use `u32::MAX` to disable.
    pub consecutive_failures: u32,
    /// Capacity of the sliding window of recent call outcomes (successes
    /// and failures) used for rate-based tripping. Allocation happens once
    /// at breaker construction; recording is O(1) with no allocation.
    pub sliding_window_size: u32,
    /// Minimum number of outcomes that must be in the sliding window before
    /// the failure rate is evaluated. Defaults to the full
    /// [`Self::sliding_window_size`] (rate = failures / window size);
    /// `1` gives the aggressive early-warning variant (rate = failures /
    /// outcomes so far). Clamped to `<= sliding_window_size`.
    pub minimum_calls: u32,
    /// Maximum number of concurrent probe calls admitted while `HalfOpen`.
    /// This is the stampede guard: when the circuit half-opens, at most
    /// this many calls test the protected service at once; the rest are
    /// rejected immediately with
    /// [`CircuitBreakerError::Rejected`](crate::CircuitBreakerError::Rejected)
    /// (which does not count as a failure).
    pub half_open_max_calls: u32,
    /// Number of successful probe calls in HalfOpen needed to close the
    /// circuit.
    pub success_threshold: u32,
    /// How long to stay `Open` after each trip (see [`BackoffStrategy`]).
    pub backoff: BackoffStrategy,
    /// Optional classifier: only errors for which this returns `true` count
    /// as breaker failures; others pass through to the caller unchanged and
    /// leave the state machine untouched. `None` (default) counts every
    /// error as a failure.
    pub(crate) failure_predicate: Option<FailurePredicate>,
    /// Per-call timeout (`timeout` feature). A call that exceeds it is
    /// recorded as a failure and surfaces as
    /// [`CircuitBreakerError::Timeout`](crate::CircuitBreakerError::Timeout).
    /// `None` (default) applies no timeout.
    #[cfg(feature = "timeout")]
    pub call_timeout: Option<Duration>,
}

impl CircuitBreakerConfig {
    /// Sensible defaults: 50 % window rate, 5-failure streak, 10-window,
    /// fixed 30 s wait, 3 half-open probes.
    pub fn standard() -> Self {
        Self::builder()
            .failure_rate_threshold(0.5)
            .consecutive_failures(5)
            .sliding_window_size(10)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(30)))
            .half_open_max_calls(3)
            .build()
    }

    /// Trip on the first failure, fixed 10 s wait.
    pub fn fast_fail() -> Self {
        Self::builder()
            .failure_rate_threshold(1.0)
            .consecutive_failures(1)
            .sliding_window_size(5)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(10)))
            .half_open_max_calls(1)
            .build()
    }

    /// Forgiving: 50 % window rate over 20 outcomes, 10-failure streak,
    /// fixed 60 s wait.
    pub fn lenient() -> Self {
        Self::builder()
            .failure_rate_threshold(0.5)
            .consecutive_failures(10)
            .sliding_window_size(20)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .half_open_max_calls(5)
            .build()
    }

    /// Create a new builder with default values.
    pub fn builder() -> CircuitBreakerConfigBuilder {
        CircuitBreakerConfigBuilder::default()
    }

    /// Classify an operation error: does it count as a breaker failure?
    pub(crate) fn is_breaker_failure(&self, err: &dyn Any) -> bool {
        match &self.failure_predicate {
            Some(p) => p.is_failure(err),
            None => true,
        }
    }
}

/// Builder for [`CircuitBreakerConfig`]. Every setter clamps out-of-range
/// values (documented per method); unspecified fields fall back to the
/// defaults listed there.
#[derive(Debug, Clone, Default)]
pub struct CircuitBreakerConfigBuilder {
    failure_rate_threshold: Option<f32>,
    consecutive_failures: Option<u32>,
    sliding_window_size: Option<u32>,
    minimum_calls: Option<u32>,
    wait_duration: Option<Duration>,
    backoff: Option<BackoffStrategy>,
    half_open_max_calls: Option<u32>,
    success_threshold: Option<u32>,
    failure_predicate: Option<FailurePredicate>,
    #[cfg(feature = "timeout")]
    call_timeout: Option<Duration>,
}

impl CircuitBreakerConfigBuilder {
    /// Failure fraction (0.0 – 1.0) in the sliding window that trips the
    /// circuit. Values are clamped to `[0.0, 1.0]`; `0.0` disables
    /// rate-based tripping. Default: `0.5`.
    pub fn failure_rate_threshold(mut self, v: f32) -> Self {
        self.failure_rate_threshold = Some(v.clamp(0.0, 1.0));
        self
    }

    /// Consecutive failures that trip the circuit (whichever fires first
    /// with the window rate). Clamped to at least 1. Default: `5`.
    pub fn consecutive_failures(mut self, v: u32) -> Self {
        self.consecutive_failures = Some(v.max(1));
        self
    }

    /// Sliding-window capacity (recent outcomes) for rate tripping. Clamped
    /// to at least 1. Default: `10`.
    pub fn sliding_window_size(mut self, v: u32) -> Self {
        self.sliding_window_size = Some(v.max(1));
        self
    }

    /// Minimum outcomes in the window before the failure rate is evaluated.
    /// Defaults to the sliding-window size; clamped to
    /// `[1, sliding_window_size]`.
    pub fn minimum_calls(mut self, v: u32) -> Self {
        self.minimum_calls = Some(v.max(1));
        self
    }

    /// Backoff strategy applied on every trip. Overrides any
    /// [`wait_duration`](Self::wait_duration) set earlier (and vice versa:
    /// each setter replaces the other). Default: `Fixed(30 s)`.
    pub fn backoff(mut self, v: BackoffStrategy) -> Self {
        self.backoff = Some(v);
        self
    }

    /// Convenience for [`backoff`](Self::backoff)`(BackoffStrategy::Fixed(d))` —
    /// constant wait, the 1.x behavior. Default: 30 s.
    pub fn wait_duration(self, d: Duration) -> Self {
        self.backoff(BackoffStrategy::Fixed(d))
    }

    /// Maximum concurrent HalfOpen probe calls (stampede protection).
    /// Clamped to at least 1. Default: `3`.
    pub fn half_open_max_calls(mut self, v: u32) -> Self {
        self.half_open_max_calls = Some(v.max(1));
        self
    }

    /// Successful probes needed in HalfOpen to close the circuit. Clamped
    /// to at least 1; defaults to [`half_open_max_calls`](Self::half_open_max_calls).
    pub fn success_threshold(mut self, v: u32) -> Self {
        self.success_threshold = Some(v.max(1));
        self
    }

    /// Only errors matching this predicate count as breaker failures;
    /// others pass through to the caller with the original error value
    /// preserved (typed, not stringified) and do not touch the state
    /// machine.
    pub fn failure_predicate<E: 'static>(
        mut self,
        f: impl Fn(&E) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.failure_predicate = Some(FailurePredicate::new(f));
        self
    }

    /// Per-call timeout (`timeout` feature). `None` disables (default).
    /// Timed-out calls count as failures.
    #[cfg(feature = "timeout")]
    pub fn call_timeout(mut self, d: Option<Duration>) -> Self {
        self.call_timeout = d.filter(|d| !d.is_zero());
        self
    }

    /// Build the configuration, applying defaults and clamping.
    pub fn build(self) -> CircuitBreakerConfig {
        let half_open_max = self.half_open_max_calls.unwrap_or(3).max(1);
        let window = self.sliding_window_size.unwrap_or(10).max(1);
        let backoff = self.backoff.unwrap_or(BackoffStrategy::Fixed(
            self.wait_duration.unwrap_or(Duration::from_secs(30)),
        ));
        CircuitBreakerConfig {
            failure_rate_threshold: self.failure_rate_threshold.unwrap_or(0.5).clamp(0.0, 1.0),
            consecutive_failures: self.consecutive_failures.unwrap_or(5).max(1),
            sliding_window_size: window,
            minimum_calls: self.minimum_calls.unwrap_or(window).max(1).min(window),
            half_open_max_calls: half_open_max,
            success_threshold: self.success_threshold.unwrap_or(half_open_max).max(1),
            backoff,
            failure_predicate: self.failure_predicate,
            #[cfg(feature = "timeout")]
            call_timeout: self.call_timeout,
        }
    }
}

// Tests exercise validation edges directly; unwrap/expect, slicing, and
// panicking asserts are acceptable here — violations surface as test
// failures, not production panics.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_clamps_invalid_values() {
        let c = CircuitBreakerConfig::builder()
            .failure_rate_threshold(7.5) // clamped to 1.0
            .consecutive_failures(0) // clamped to 1
            .sliding_window_size(0) // clamped to 1
            .half_open_max_calls(0) // clamped to 1
            .success_threshold(0) // clamped to 1
            .build();
        assert_eq!(c.failure_rate_threshold, 1.0);
        assert_eq!(c.consecutive_failures, 1);
        assert_eq!(c.sliding_window_size, 1);
        assert_eq!(c.half_open_max_calls, 1);
        assert_eq!(c.success_threshold, 1);
    }

    #[test]
    fn negative_rate_clamps_to_zero_disables_rate() {
        let c = CircuitBreakerConfig::builder()
            .failure_rate_threshold(-1.0)
            .build();
        assert_eq!(c.failure_rate_threshold, 0.0);
    }

    #[test]
    fn minimum_calls_defaults_to_window_and_is_clamped() {
        let c = CircuitBreakerConfig::builder().build();
        assert_eq!(c.minimum_calls, c.sliding_window_size);

        let c = CircuitBreakerConfig::builder()
            .sliding_window_size(4)
            .minimum_calls(99) // clamped to the window size
            .build();
        assert_eq!(c.minimum_calls, 4);

        let c = CircuitBreakerConfig::builder()
            .sliding_window_size(4)
            .minimum_calls(0) // clamped to 1 (early-warning mode)
            .build();
        assert_eq!(c.minimum_calls, 1);
    }

    #[test]
    fn backoff_fixed_wait() {
        let b = BackoffStrategy::Fixed(Duration::from_millis(250));
        for attempt in 1..=5 {
            assert_eq!(b.wait_for(attempt), Duration::from_millis(250));
        }
    }

    #[test]
    fn backoff_exponential_sequence() {
        let b = BackoffStrategy::Exponential {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(100),
            factor: 2.0,
        };
        assert_eq!(b.wait_for(1), Duration::from_millis(10));
        assert_eq!(b.wait_for(2), Duration::from_millis(20));
        assert_eq!(b.wait_for(3), Duration::from_millis(40));
        assert_eq!(b.wait_for(4), Duration::from_millis(80));
        assert_eq!(b.wait_for(5), Duration::from_millis(100)); // capped
        assert_eq!(b.wait_for(50), Duration::from_millis(100)); // stays capped
        assert_eq!(b.initial_wait(), Duration::from_millis(10));
    }

    #[test]
    fn backoff_exponential_saturates_on_huge_attempts() {
        let b = BackoffStrategy::Exponential {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            factor: 2.0,
        };
        // factor^u32::MAX is infinite in f64; must saturate at `max`, not
        // panic in from_secs_f64.
        assert_eq!(b.wait_for(u32::MAX), Duration::from_secs(60));
    }

    #[test]
    fn backoff_zero_jitter_matches_exponential() {
        let base = BackoffStrategy::Exponential {
            initial: Duration::from_millis(10),
            max: Duration::from_secs(1),
            factor: 3.0,
        };
        let j = BackoffStrategy::ExponentialJitter {
            initial: Duration::from_millis(10),
            max: Duration::from_secs(1),
            factor: 3.0,
            jitter: 0.0,
        };
        for attempt in 1..=10 {
            assert_eq!(j.wait_for(attempt), base.wait_for(attempt));
        }
    }

    #[test]
    fn backoff_full_jitter_bounds() {
        let initial = Duration::from_millis(10);
        let j = BackoffStrategy::ExponentialJitter {
            initial,
            max: Duration::from_secs(1),
            factor: 2.0,
            jitter: 1.0, // full jitter: uniform [0, computed]
        };
        for attempt in 1..=6u32 {
            let computed = BackoffStrategy::Exponential {
                initial,
                max: Duration::from_secs(1),
                factor: 2.0,
            }
            .wait_for(attempt);
            for _ in 0..200 {
                let w = j.wait_for(attempt);
                assert!(
                    w <= computed,
                    "jittered wait {w:?} exceeds base {computed:?}"
                );
                assert!(w >= Duration::ZERO);
            }
        }
    }

    #[test]
    fn predicate_config_classifies_by_type() {
        let c = CircuitBreakerConfig::builder()
            .failure_predicate(|e: &String| e.contains("permanent"))
            .build();
        let perm: &dyn Any = &"permanent failure".to_string();
        let transient: &dyn Any = &"transient".to_string();
        assert!(c.is_breaker_failure(perm));
        assert!(!c.is_breaker_failure(transient));
    }

    #[test]
    fn predicate_mismatch_defaults_to_failure() {
        // Predicate built for String, consulted with i32: conservative
        // default is "counts as failure".
        let c = CircuitBreakerConfig::builder()
            .failure_predicate(|_: &String| false)
            .build();
        let other: &dyn Any = &42i32;
        assert!(c.is_breaker_failure(other));
    }

    #[test]
    fn no_predicate_counts_everything() {
        let c = CircuitBreakerConfig::standard();
        let anything: &dyn Any = &"whatever";
        assert!(c.is_breaker_failure(anything));
    }

    #[cfg(feature = "timeout")]
    #[test]
    fn call_timeout_zero_is_disabled() {
        let c = CircuitBreakerConfig::builder()
            .call_timeout(Some(Duration::ZERO))
            .build();
        assert_eq!(c.call_timeout, None);
    }
}
