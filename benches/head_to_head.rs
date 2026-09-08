// Benchmarks run on fixed, known-good inputs; unwrap failures abort the
// bench run visibly, which is the desired behavior here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Head-to-head comparison: breaker vs failsafe 1.3, the most-used dedicated
//! circuit-breaker crate on crates.io (~16M downloads).
//!
//! Measured operations, kept symmetric on both sides:
//!
//! 1. Allowed-path call: `call(<always-succeed op>).await` through a
//!    healthy (closed) breaker. Ours takes a closure returning a future,
//!    failsafe's futures API takes a future value directly; the constructed
//!    future and recorded outcome are identical.
//! 2. State check: `is_closed()` vs `is_call_permitted()` — the cheapest
//!    decision both crates expose, isolating lock/state-machine overhead
//!    from future plumbing.
//!
//! Both crates record successes through their default policy/state machine;
//! neither transitions during the run, so this is steady-state closed-path
//! overhead.

use std::convert::Infallible;
use std::hint::black_box;

use breaker::{CircuitBreaker, CircuitBreakerConfig};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

fn bench_breaker_call_allowed(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let brk = CircuitBreaker::new(CircuitBreakerConfig::standard());
    let mut group = c.benchmark_group("comparison");
    group.throughput(Throughput::Elements(1));
    group.bench_function("breaker_call_ok", |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            rt.block_on(async {
                for _ in 0..iters {
                    let _ = black_box(brk.call(|| async { Ok::<u8, Infallible>(0u8) }).await);
                }
            });
            start.elapsed()
        });
    });
    group.finish();
}

fn bench_failsafe_call_allowed(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Default config: default failure-accrual policy + state machine,
    // the equivalent of our `CircuitBreakerConfig::standard()` baseline.
    let brk = failsafe::Config::new().build();
    let mut group = c.benchmark_group("comparison");
    group.throughput(Throughput::Elements(1));
    group.bench_function("failsafe_call_ok", |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            rt.block_on(async {
                for _ in 0..iters {
                    let _ = black_box(
                        failsafe::futures::CircuitBreaker::call(&brk, async {
                            Ok::<u8, Infallible>(0u8)
                        })
                        .await,
                    );
                }
            });
            start.elapsed()
        });
    });
    group.finish();
}

fn bench_state_checks(c: &mut Criterion) {
    let brk = CircuitBreaker::new(CircuitBreakerConfig::standard());
    let fs = failsafe::Config::new().build();

    let mut group = c.benchmark_group("comparison");
    group.bench_function("breaker_is_closed", |b| {
        b.iter(|| black_box(brk.is_closed()));
    });
    group.bench_function("failsafe_is_call_permitted", |b| {
        b.iter(|| black_box(failsafe::futures::CircuitBreaker::is_call_permitted(&fs)));
    });
    group.finish();
}

criterion_group!(
    comparison,
    bench_breaker_call_allowed,
    bench_failsafe_call_allowed,
    bench_state_checks,
);
criterion_main!(comparison);
