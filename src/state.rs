use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::config::CircuitBreakerConfig;

/// The three states of the circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Circuit is closed — requests are allowed.
    Closed,
    /// Circuit is open — requests are rejected.
    Open,
    /// Circuit is half-open — probing for recovery. At most
    /// [`CircuitBreakerConfig::half_open_max_calls`] concurrent probe calls
    /// are admitted (stampede protection); excess calls are rejected
    /// without counting as failures.
    HalfOpen,
}

/// Fixed-capacity ring buffer of the most recent call outcomes (1 = failure,
/// 0 = success) used for rate-based tripping. Allocated once at
/// construction (`with_capacity`); steady-state recording is O(1) with no
/// allocation, and the running failure count is maintained incrementally so
/// rate evaluation is O(1) too.
struct Window {
    outcomes: VecDeque<u8>,
    failures: u32,
    capacity: u32,
}

impl Window {
    fn new(capacity: u32) -> Self {
        Self {
            outcomes: VecDeque::with_capacity(capacity as usize),
            failures: 0,
            capacity: capacity.max(1),
        }
    }

    /// Record one outcome, evicting the oldest when full.
    fn record(&mut self, failed: bool) {
        if self.outcomes.len() as u32 >= self.capacity {
            if let Some(evicted) = self.outcomes.pop_front() {
                self.failures -= u32::from(evicted == 1);
            }
        }
        self.outcomes.push_back(u8::from(failed));
        if failed {
            self.failures += 1;
        }
    }

    /// Outcomes currently held (grows to `capacity`, then stays there).
    fn filled(&self) -> u32 {
        self.outcomes.len() as u32
    }

    /// Failure fraction over the recorded outcomes:
    /// `failures / min(capacity, filled)` (by construction `filled <=
    /// capacity`, so the denominator is just `filled`). `0.0` when empty.
    fn failure_rate(&self) -> f32 {
        let filled = self.filled();
        if filled == 0 {
            0.0
        } else {
            self.failures as f32 / filled as f32
        }
    }

    /// Drop all outcomes (fresh assessment after a HalfOpen → Closed
    /// recovery).
    fn clear(&mut self) {
        self.outcomes.clear();
        self.failures = 0;
    }
}

/// Drives the Closed/Open/HalfOpen state machine.
///
/// # Tripping (Closed)
///
/// Trips on whichever fires first (see
/// [`CircuitBreakerConfig`](crate::CircuitBreakerConfig) for the exact
/// semantics):
///
/// - **consecutive failures** — `failure_count >= consecutive_failures`;
/// - **sliding-window rate** — `Window::failure_rate() >=
///   failure_rate_threshold` (with the window's `min(size, filled)`
///   denominator this also subsumes the leading-edge "N early failures"
///   case).
///
/// # Backoff (Open)
///
/// Every trip increments `open_attempts` and recomputes the episode's wait
/// from the configured [`BackoffStrategy`]; `current()` upgrades Open →
/// HalfOpen once that episode's wait has elapsed.
///
/// # Concurrency contract
///
/// All mutation happens under the breaker's state lock (see `src/lock.rs`);
/// the half-open *probe permit* counter lives outside this struct (one
/// `AtomicUsize` on the breaker's shared inner state) so admission control
/// can proceed without a write lock.
pub struct StateMachine {
    current: State,
    /// Consecutive failures in the current Closed episode (reset by any
    /// success and by every transition).
    failure_count: u32,
    /// Successful probes toward `success_threshold` in HalfOpen.
    success_count: u32,
    total_successes: u64,
    total_failures: u64,
    transitions: u64,
    /// When the current Open episode began (drives the HalfOpen upgrade).
    open_since: Option<Instant>,
    /// Wait duration for the *current* Open episode, as computed from the
    /// backoff strategy for `open_attempts`. Read by `current()`.
    wait_duration: Duration,
    /// How many times the circuit has tripped (fed to the backoff strategy).
    open_attempts: u32,
    /// Sliding window of recent outcomes (Closed-state rate tripping).
    window: Window,
}

impl StateMachine {
    pub fn new(config: &CircuitBreakerConfig) -> Self {
        Self {
            current: State::Closed,
            failure_count: 0,
            success_count: 0,
            total_successes: 0,
            total_failures: 0,
            transitions: 0,
            open_since: None,
            wait_duration: config.backoff.initial_wait(),
            open_attempts: 0,
            window: Window::new(config.sliding_window_size),
        }
    }

    /// Effective current state. An `Open` circuit whose backoff wait has
    /// elapsed reads as `HalfOpen` (the upgrade is lazy — it is applied by
    /// the next state-mutating call).
    pub fn current(&self) -> State {
        if self.current == State::Open {
            if let Some(open_since) = self.open_since {
                if open_since.elapsed() >= self.wait_duration {
                    return State::HalfOpen;
                }
            }
        }
        self.current
    }

    pub fn record_success(&mut self, config: &CircuitBreakerConfig) -> Option<(State, State)> {
        self.total_successes += 1;
        self.maybe_transition_to_half_open();
        let prev = self.current;

        match self.current {
            State::Closed => {
                self.failure_count = 0;
                self.window.record(false);
                // A success can only lower the window rate and resets the
                // streak, so no trip check is needed here.
                None
            }
            State::HalfOpen => {
                self.success_count += 1;
                if self.success_count >= config.success_threshold {
                    self.transition(State::Closed, config);
                    Some((prev, State::Closed))
                } else {
                    None
                }
            }
            State::Open => None,
        }
    }

    pub fn record_failure(&mut self, config: &CircuitBreakerConfig) -> Option<(State, State)> {
        self.total_failures += 1;
        self.maybe_transition_to_half_open();
        let prev = self.current;

        match self.current {
            State::Closed => {
                self.failure_count += 1;
                self.window.record(true);
                if self.should_trip(config) {
                    self.transition(State::Open, config);
                    Some((prev, State::Open))
                } else {
                    None
                }
            }
            State::HalfOpen => {
                // Any failed probe re-trips the circuit immediately.
                self.transition(State::Open, config);
                Some((prev, State::Open))
            }
            State::Open => None,
        }
    }

    /// Trip check: consecutive-failure streak **or** window failure rate,
    /// whichever fires first. The rate is evaluated as
    /// `failures / min(capacity, filled)` — by construction `filled <=
    /// capacity` — but only once `filled >= minimum_calls`.
    fn should_trip(&self, config: &CircuitBreakerConfig) -> bool {
        if self.failure_count >= config.consecutive_failures {
            return true;
        }
        // A threshold of 0.0 disables rate-based tripping.
        config.failure_rate_threshold > 0.0
            && self.window.filled() >= config.minimum_calls
            && self.window.failure_rate() >= config.failure_rate_threshold
    }

    fn maybe_transition_to_half_open(&mut self) {
        if self.current == State::Open {
            if let Some(open_since) = self.open_since {
                if open_since.elapsed() >= self.wait_duration {
                    self.current = State::HalfOpen;
                    self.transitions += 1;
                    self.failure_count = 0;
                    self.success_count = 0;
                    self.open_since = None;
                }
            }
        }
    }

    /// Apply a transition. Tripping to `Open` increments `open_attempts`
    /// and computes this episode's wait from the backoff strategy;
    /// recovering to `Closed` resets the attempt counter and the sliding
    /// window so the next episode is assessed from a clean slate.
    fn transition(&mut self, next: State, config: &CircuitBreakerConfig) {
        self.current = next;
        self.transitions += 1;
        self.failure_count = 0;
        self.success_count = 0;

        match next {
            State::Open => {
                self.open_attempts = self.open_attempts.saturating_add(1);
                self.open_since = Some(Instant::now());
                self.wait_duration = config.backoff.wait_for(self.open_attempts);
            }
            State::Closed => {
                self.open_attempts = 0;
                self.open_since = None;
                self.window.clear();
            }
            State::HalfOpen => {
                self.open_since = None;
            }
        }
    }

    pub fn force_open(&mut self, config: &CircuitBreakerConfig) {
        self.transition(State::Open, config);
    }

    pub fn force_closed(&mut self, config: &CircuitBreakerConfig) {
        self.transition(State::Closed, config);
    }

    /// Lifetime failure rate (all calls ever recorded). For the *windowed*
    /// rate that drives tripping, see [`Self::window_failure_rate`].
    pub fn failure_rate(&self) -> f64 {
        let total = self.total_successes + self.total_failures;
        if total == 0 {
            return 0.0;
        }
        self.total_failures as f64 / total as f64
    }

    /// Failure fraction over the sliding window (`min(size, filled)`
    /// denominator) — the quantity the rate-based trip decision uses.
    pub fn window_failure_rate(&self) -> f32 {
        self.window.failure_rate()
    }

    /// Outcomes currently held in the sliding window.
    pub fn window_filled(&self) -> u32 {
        self.window.filled()
    }

    pub fn total_successes(&self) -> u64 {
        self.total_successes
    }

    pub fn total_failures(&self) -> u64 {
        self.total_failures
    }

    pub fn transitions(&self) -> u64 {
        self.transitions
    }

    /// Consecutive failures recorded in the current `Closed` episode.
    /// Consumed by the `metrics` feature's failure-count histogram.
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    pub fn failure_count(&self) -> u32 {
        self.failure_count
    }
}

// Tests exercise the window and backoff arithmetic directly; unwrap/expect,
// slicing, and panicking asserts are acceptable here — violations surface
// as test failures, not production panics.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
#[cfg(test)]
mod tests {
    use super::*;

    fn config(window: u32, rate: f32, consecutive: u32) -> CircuitBreakerConfig {
        CircuitBreakerConfig::builder()
            .sliding_window_size(window)
            .failure_rate_threshold(rate)
            .consecutive_failures(consecutive)
            .build()
    }

    #[test]
    fn window_evicts_oldest_and_tracks_failures() {
        let mut w = Window::new(3);
        w.record(true);
        w.record(false);
        w.record(true);
        assert_eq!(w.failure_rate(), 2.0 / 3.0);
        // Window full: recording another success evicts the first failure.
        w.record(false);
        assert_eq!(w.filled(), 3);
        assert_eq!(w.failure_rate(), 1.0 / 3.0);
        w.record(true);
        w.record(true);
        assert_eq!(w.failure_rate(), 2.0 / 3.0); // 2 failures of last 3
    }

    #[test]
    fn window_capacity_is_clamped_to_one() {
        let mut w = Window::new(0);
        w.record(true);
        w.record(false);
        assert_eq!(w.filled(), 1);
        assert_eq!(w.failure_rate(), 0.0);
    }

    #[test]
    fn window_clear_resets() {
        let mut w = Window::new(4);
        w.record(true);
        w.record(true);
        w.clear();
        assert_eq!(w.filled(), 0);
        assert_eq!(w.failure_rate(), 0.0);
    }

    #[test]
    fn rate_trip_when_threshold_crossed() {
        let config = config(4, 0.5, 100);
        assert_eq!(config.minimum_calls, 4); // defaults to the window size
        let mut sm = StateMachine::new(&config);
        // Window below minimum_calls: rate not evaluated yet.
        assert!(sm.record_failure(&config).is_none());
        assert!(sm.record_failure(&config).is_none());
        assert!(sm.record_failure(&config).is_none());
        assert_eq!(sm.current(), State::Closed);
        // 4th failure: filled = 4, rate = 4/4 >= 0.5 → trips.
        assert!(sm.record_failure(&config).is_some());
        assert_eq!(sm.current(), State::Open);
    }

    #[test]
    fn rate_early_warning_with_minimum_calls_one() {
        // minimum_calls = 1: the literal `failures / min(size, filled)`
        // formula — a single failure in a fresh window is 1/1 and trips
        // any threshold <= 1.0.
        let config = CircuitBreakerConfig::builder()
            .sliding_window_size(4)
            .minimum_calls(1)
            .failure_rate_threshold(0.5)
            .consecutive_failures(100)
            .build();
        let mut sm = StateMachine::new(&config);
        assert!(sm.record_failure(&config).is_some());
        assert_eq!(sm.current(), State::Open);
    }

    #[test]
    fn rate_no_trip_below_threshold() {
        let config = config(4, 0.75, 100);
        let mut sm = StateMachine::new(&config);
        sm.record_success(&config);
        sm.record_success(&config);
        sm.record_success(&config);
        assert!(sm.record_failure(&config).is_none()); // 1/4 = 0.25
        assert!(sm.record_failure(&config).is_none()); // 2/4 = 0.50
        assert!(sm.record_failure(&config).is_some()); // 3/4 = 0.75 >= 0.75
        assert_eq!(sm.current(), State::Open);
    }

    #[test]
    fn consecutive_trip_with_rate_disabled() {
        let config = config(10, 0.0, 3);
        let mut sm = StateMachine::new(&config);
        sm.record_failure(&config);
        sm.record_failure(&config);
        assert_eq!(sm.current(), State::Closed);
        sm.record_failure(&config);
        assert_eq!(sm.current(), State::Open);
    }

    #[test]
    fn window_clears_on_recovery_to_closed() {
        let config = config(4, 0.5, 100);
        let mut sm = StateMachine::new(&config);
        sm.record_failure(&config);
        sm.record_failure(&config);
        sm.record_failure(&config);
        sm.record_failure(&config); // 4/4 trips
        assert_eq!(sm.current(), State::Open);
        sm.force_closed(&config);
        assert_eq!(sm.window_filled(), 0);
        // Fresh window: the stale failures are gone, so the same burst
        // pattern starts over from below minimum_calls instead of
        // immediately re-tripping.
        sm.record_failure(&config);
        sm.record_failure(&config);
        assert_eq!(sm.current(), State::Closed);
    }

    #[test]
    fn open_attempts_grow_and_backoff_applies() {
        let config = CircuitBreakerConfig::builder()
            .backoff(crate::config::BackoffStrategy::Exponential {
                initial: Duration::from_millis(10),
                max: Duration::from_millis(1000),
                factor: 2.0,
            })
            .build();
        let mut sm = StateMachine::new(&config);
        sm.force_open(&config);
        assert_eq!(sm.open_attempts, 1);
        assert_eq!(sm.wait_duration, Duration::from_millis(10));
        sm.force_closed(&config);
        sm.force_open(&config);
        assert_eq!(sm.open_attempts, 1); // recovery resets the counter
        sm.transition(State::Open, &config); // re-trip without recovery
        assert_eq!(sm.open_attempts, 2);
        assert_eq!(sm.wait_duration, Duration::from_millis(20));
    }

    #[test]
    fn half_open_probe_successes_close() {
        let config = CircuitBreakerConfig::builder()
            .success_threshold(2)
            .half_open_max_calls(2)
            .failure_rate_threshold(0.0)
            .consecutive_failures(100)
            .build();
        let mut sm = StateMachine::new(&config);
        sm.force_open(&config);
        sm.force_closed(&config); // reset attempts; go through half-open manually
        sm.current = State::HalfOpen;
        assert!(sm.record_success(&config).is_none());
        assert_eq!(sm.current(), State::HalfOpen);
        assert!(sm.record_success(&config).is_some());
        assert_eq!(sm.current(), State::Closed);
        assert_eq!(sm.window_filled(), 0);
    }
}
