#![cfg(kani)]
//! Kani bounded model-checking harnesses for the circuit breaker state
//! machine (see `tests/loom.rs` for the concurrency-side models).
//!
//! The harnesses drive the public [`CircuitBreaker`] API with nondeterministic
//! success/failure sequences (bounded to 3 unrolled steps) and check the key
//! safety properties after every step:
//!
//! 1. `state ∈ {Closed, Open, HalfOpen}` always — the state machine never
//!    leaves the documented state space, no matter the op sequence.
//! 2. If the circuit is `Open`, then at least `failure_rate_threshold`
//!    failures have been recorded in total. Justification: a `Closed → Open`
//!    trip requires exactly `threshold` consecutive failures (each of which
//!    is also a total failure), and a `HalfOpen → Open` re-trip can only
//!    increase `total_failures`. So `total_failures >= threshold` covers both
//!    disjuncts of "tripped at threshold OR re-opened via the half-open
//!    path". (`failures_at_open` itself is the internal consecutive counter,
//!    which the trip transition resets to 0 — it is not observable through
//!    the public API, hence the `total_failures` proxy.)
//!
//! # Time is frozen
//!
//! Kani cannot execute `clock_gettime` (the FFI under `Instant::now`,
//! model-checking/kani#2423), so the harness stubs `Instant::now` with a
//! clock frozen at the epoch: every read returns `Instant::ZERO` and
//! `elapsed()` is deterministically `Duration::ZERO`. Consequences:
//! - with `wait_duration = 0`, the `Open → HalfOpen` upgrade fires on the
//!   next recorded op — the half-open semantics (probe successes closing the
//!   circuit, probe failures re-tripping it) are fully exercised;
//! - with a long `wait_duration`, `Open` never times out — the first
//!   harness pins the trip/threshold accounting without half-open retries.
//!
//! The stub lives in this file (not `src/`) because the lib forbids
//! `unsafe_code`; the frozen clock needs one `zeroed()` Instant, which is a
//! valid Linux `Timespec { tv_sec: 0, tv_nsec: 0 }`.
//!
//! # Symex-cost controls (do not "fix" these back)
//!
//! - Per-step results are accumulated into a single final assertion: every
//!   potentially-failing `kani::assert` drags the panic/stderr-formatting
//!   machinery into symbolic execution, where its drop-glue recursion
//!   explodes the state graph (observed: CBMC stuck at recursion depth 100+).
//! - 3 steps: every overflow-checked counter increment inside `record_*`
//!   (Kani mandates overflow checks) is another potentially-panicking
//!   branch feeding that machinery, so cost grows multiplicatively with
//!   step count (observed with parking_lot locks: >15 GB RSS, no result).
//! - The parking_lot state lock is swapped for std's under `cfg(kani)`
//!   (see `src/lock.rs`): Kani models std sync primitives natively, and
//!   parking_lot's futex/spin internals are intractable symbolically.
//!   With all three controls, each harness verifies in <60 s.
//!
//! Run with (stubbing is behind Kani's unstable feature gate):
//! ```text
//! cargo kani --tests -Z stubbing
//! ```

use breaker::{CircuitBreaker, CircuitBreakerConfig, State};
use std::time::Instant;

/// Frozen clock: a valid zeroed Linux `Timespec { tv_sec: 0, tv_nsec: 0 }`.
/// Every `Instant::now()` in the harness's reachability returns this value,
/// so `elapsed()` is always `Duration::ZERO`.
fn frozen_now() -> Instant {
    // SAFETY: `Instant` is a plain Copy struct (Timespec: seconds + nanos)
    // on Linux; all-zero bits are a valid instant (the epoch), and no code
    // constructs an `Instant` any other way in these harnesses.
    unsafe { std::mem::zeroed() }
}

/// Advance the breaker by one abstract step; returns `true` iff both safety
/// invariants hold after the step.
fn step_ok(cb: &CircuitBreaker, fail: bool, threshold: u32) -> bool {
    if fail {
        cb.record_failure();
    } else {
        cb.record_success();
    }

    let m = cb.metrics();
    matches!(m.state, State::Closed | State::Open | State::HalfOpen)
        && (m.state != State::Open || m.total_failures >= threshold as u64)
}

/// Arbitrary success/failure sequences (3 bounded steps) against a breaker
/// with `threshold = 2` and a long wait: checks the state-space invariant
/// and the trip-at-threshold invariant after every step.
#[kani::proof]
#[kani::unwind(15)]
#[kani::stub(std::time::Instant::now, frozen_now)]
fn kani_breaker_invariants_under_arbitrary_sequences() {
    let config = CircuitBreakerConfig::builder()
        .failure_rate_threshold(2)
        .success_threshold(2)
        .wait_duration(std::time::Duration::from_secs(3600))
        .build();
    let cb = CircuitBreaker::new(config);

    let mut ok = true;
    ok &= step_ok(&cb, kani::any(), 2);
    ok &= step_ok(&cb, kani::any(), 2);
    ok &= step_ok(&cb, kani::any(), 2);

    kani::assert(
        ok,
        "state ∈ {Closed,Open,HalfOpen} always; Open implies >= threshold total failures",
    );
}

/// `wait_duration = 0` makes the `Open → HalfOpen` upgrade happen on the very
/// next recorded op, so the half-open semantics are deterministically
/// exercised — including the edge case `threshold = 1` (trip on the first
/// failure).
#[kani::proof]
#[kani::unwind(15)]
#[kani::stub(std::time::Instant::now, frozen_now)]
fn kani_breaker_half_open_path_invariants() {
    let config = CircuitBreakerConfig::builder()
        .failure_rate_threshold(1)
        .success_threshold(1)
        .wait_duration(std::time::Duration::from_secs(0))
        .build();
    let cb = CircuitBreaker::new(config);

    let mut ok = true;
    ok &= step_ok(&cb, kani::any(), 1);
    ok &= step_ok(&cb, kani::any(), 1);
    ok &= step_ok(&cb, kani::any(), 1);

    kani::assert(
        ok,
        "half-open path: state invariants and re-trip accounting hold",
    );
}
