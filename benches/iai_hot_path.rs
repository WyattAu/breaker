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
//! Unlike [`criterion`](../call_overhead/index.html) (wall-clock, noisy,
//! human-readable trend), iai-callgrind counts CPU instructions under
//! Valgrind and is reproducible across runs on the same binary — fit for a
//! CI gate. The criterion benches in `benches/call_overhead.rs` remain the
//! source of the SLO wall-clock numbers in PERF-SLO.md; this file is the
//! pass/fail gate.
//!
//! Workflow:
//!
//! - main: `cargo iai-callgrind --bench iai_hot_path --save-baseline main`
//!   (done by the `perf-gate` CI job; baselines update intentionally on
//!   every main push).
//! - PRs: `cargo iai-callgrind --bench iai_hot_path --baseline main
//!   --fail-fast` — any instruction-count regression fails the job.
//! - Locally this needs `valgrind` installed (`apt install valgrind`);
//!   without it, compile-check only: `cargo bench --no-run --bench iai_hot_path`.

use std::convert::Infallible;
use std::hint::black_box;

use breaker::{CircuitBreaker, CircuitBreakerConfig};
use iai_callgrind::{library_benchmark, library_benchmark_group, main};

type Rt = tokio::runtime::Runtime;

fn current_thread_rt() -> Rt {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

async fn ok_op() -> Result<u8, Infallible> {
    Ok(0)
}

fn setup_allowed() -> (Rt, CircuitBreaker) {
    let rt = current_thread_rt();
    let brk = CircuitBreaker::new(CircuitBreakerConfig::standard());
    // Warm one call so the measured call is the steady-state path (the
    // first call may page in lazy process state; Valgrind counts those
    // cold effects otherwise).
    rt.block_on(brk.call(ok_op)).ok();
    (rt, brk)
}

fn setup_rejected() -> (Rt, CircuitBreaker) {
    let rt = current_thread_rt();
    let brk = CircuitBreaker::new(CircuitBreakerConfig::standard());
    brk.trip(); // steady-state open: every call short-circuits
    (rt, brk)
}

// Allowed (closed-circuit) fast path: read-lock state check + write-lock
// success record, no transition. This is the path the PERF-SLO covers.
#[library_benchmark]
#[bench::allowed(setup = setup_allowed)]
fn call_allowed(env: (Rt, CircuitBreaker)) -> u8 {
    let (rt, brk) = env;
    rt.block_on(async { black_box(brk.call(ok_op).await).unwrap_or(0) })
}

// Rejected (open-circuit) short-circuit path: read-lock state check, no
// user future is polled.
#[library_benchmark]
#[bench::rejected(setup = setup_rejected)]
fn call_rejected(env: (Rt, CircuitBreaker)) -> u8 {
    let (rt, brk) = env;
    rt.block_on(async {
        black_box(brk.call(|| async { Ok::<u8, Infallible>(0u8) }).await).unwrap_or(0)
    })
}

library_benchmark_group!(
    name = iai_hot_path;
    benchmarks = call_allowed, call_rejected
);

main!(library_benchmark_groups = iai_hot_path);
