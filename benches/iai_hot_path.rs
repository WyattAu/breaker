// iai-callgrind benchmarks run once under Valgrind on fixed inputs; the
// harness measures instruction counts, so there is no "expected failure"
// recovery path — a panic aborts the run visibly, which is what we want.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Deterministic regression gate for the `CircuitBreaker::call` hot path.
//!
//! Unlike criterion (wall-clock, noisy, human-readable trend —
//! `benches/call_overhead.rs`), iai-callgrind counts CPU instructions under
//! Valgrind and is reproducible for a given binary — fit for a CI gate.
//! Criterion stays the source of the wall-clock trend; this file is the
//! pass/fail gate.

use breaker::{CircuitBreaker, CircuitBreakerConfig};
use iai_callgrind::{library_benchmark, library_benchmark_group, main};

type Rt = tokio::runtime::Runtime;

fn setup_call() -> (Rt, CircuitBreaker) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
    // Warm one call so the measured call is the steady-state allowed path.
    rt.block_on(cb.call(|| async { Ok::<(), std::convert::Infallible>(()) }))
        .ok();
    (rt, cb)
}

// Steady-state allowed call: state check under lock + success record, no
// transition — the path real traffic hits.
#[library_benchmark]
#[bench::steady_state(setup = setup_call)]
fn call_allowed(env: (Rt, CircuitBreaker)) -> bool {
    let (rt, cb) = env;
    rt.block_on(async {
        cb.call(|| async { Ok::<(), std::convert::Infallible>(()) })
            .await
            .is_ok()
    })
}

library_benchmark_group!(name = iai_hot_path; benchmarks = call_allowed);

main!(library_benchmark_groups = iai_hot_path);
