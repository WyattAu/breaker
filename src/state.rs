use std::time::{Duration, Instant};

use crate::config::CircuitBreakerConfig;

/// The three states of the circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Circuit is closed — requests are allowed.
    Closed,
    /// Circuit is open — requests are rejected.
    Open,
    /// Circuit is half-open — probing for recovery.
    HalfOpen,
}

pub struct StateMachine {
    current: State,
    failure_count: u32,
    success_count: u32,
    total_successes: u64,
    total_failures: u64,
    transitions: u64,
    last_failure: Option<Instant>,
    open_since: Option<Instant>,
    wait_duration: Duration,
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            current: State::Closed,
            failure_count: 0,
            success_count: 0,
            total_successes: 0,
            total_failures: 0,
            transitions: 0,
            last_failure: None,
            open_since: None,
            wait_duration: Duration::from_secs(30),
        }
    }

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
        self.maybe_transition_to_half_open(config);
        let prev = self.current;

        match self.current {
            State::Closed => {
                self.failure_count = 0;
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
        self.last_failure = Some(Instant::now());
        self.maybe_transition_to_half_open(config);
        let prev = self.current;

        match self.current {
            State::Closed => {
                self.failure_count += 1;
                if self.failure_count >= config.failure_rate_threshold {
                    self.transition(State::Open, config);
                    Some((prev, State::Open))
                } else {
                    None
                }
            }
            State::HalfOpen => {
                self.transition(State::Open, config);
                Some((prev, State::Open))
            }
            State::Open => None,
        }
    }

    fn maybe_transition_to_half_open(&mut self, config: &CircuitBreakerConfig) {
        if self.current == State::Open {
            if let Some(open_since) = self.open_since {
                if open_since.elapsed() >= config.wait_duration {
                    self.transition(State::HalfOpen, config);
                }
            }
        }
    }

    fn transition(&mut self, next: State, config: &CircuitBreakerConfig) {
        self.current = next;
        self.transitions += 1;
        self.failure_count = 0;
        self.success_count = 0;
        self.wait_duration = config.wait_duration;

        if next == State::Open {
            self.open_since = Some(Instant::now());
        } else {
            self.open_since = None;
        }
    }

    pub fn force_open(&mut self) {
        self.transition(State::Open, &CircuitBreakerConfig::standard());
    }

    pub fn force_closed(&mut self) {
        self.transition(State::Closed, &CircuitBreakerConfig::standard());
    }

    pub fn failure_rate(&self) -> f64 {
        let total = self.total_successes + self.total_failures;
        if total == 0 {
            return 0.0;
        }
        self.total_failures as f64 / total as f64
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
