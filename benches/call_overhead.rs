// Benchmarks run on fixed, known-good inputs; unwrap failures abort the
// bench run visibly, which is the desired behavior here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Hot-path latency: `CircuitBreaker::call` overhead on the allowed (closed)
//! fast path, vs. the rejected (open) path, measured with criterion.
//!
//! Each iteration performs `n` calls; `Throughput::Elements(n)` makes the
//! reported time **per call**. Sustained success (no state transition) is the
//! steady-state path real services live on.

use std::convert::Infallible;
use std::hint::black_box;

use breaker::{CircuitBreaker, CircuitBreakerConfig};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

fn breaker() -> CircuitBreaker {
    CircuitBreaker::new(CircuitBreakerConfig::standard())
}

/// Always-succeed operation: the pre-allowed fast path (read-lock state
/// check + write-lock success record, no transition).
async fn ok_op() -> Result<u8, Infallible> {
    Ok(0)
}

fn bench_call_allowed(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("call_allowed");
    for n in [1usize, 100, 1000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("call_ok_{n}"), |b| {
            let brk = breaker();
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                rt.block_on(async {
                    for _ in 0..iters {
                        for _ in 0..n {
                            let _ = black_box(brk.call(ok_op).await);
                        }
                    }
                });
                start.elapsed()
            });
        });
    }
    group.finish();
}

fn bench_call_rejected(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("call_rejected");
    for n in [1usize, 100, 1000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("call_open_{n}"), |b| {
            let brk = breaker();
            brk.trip(); // steady-state open: every call short-circuits
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                rt.block_on(async {
                    for _ in 0..iters {
                        for _ in 0..n {
                            let _ = black_box(
                                brk.call(|| async { Ok::<u8, Infallible>(0u8) }).await,
                            );
                        }
                    }
                });
                start.elapsed()
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_call_allowed, bench_call_rejected);
criterion_main!(benches);
