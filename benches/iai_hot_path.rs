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

use breaker::CircuitBreaker;
use iai_callgrind::{library_benchmark, library_benchmark_group, main};

#[library_benchmark]
fn call_allowed() {
    let cb = CircuitBreaker::builder().build();
    let _ = cb.call(|| Ok::<(), std::convert::Infallible>(()));
}

library_benchmark_group!(name = iai_hot_path; benchmarks = call_allowed);

main!(library_benchmark_groups = iai_hot_path);
