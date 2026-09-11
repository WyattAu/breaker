# breaker

Async circuit breaker for Rust — sliding-window failure-rate tripping,
half-open probe permits (stampede protection), backoff strategies, typed
errors, and a real [Tower](https://docs.rs/tower) layer.

[![docs.rs](https://docs.rs/breaker/badge.svg)](https://docs.rs/breaker)
[![CI](https://github.com/WyattAu/breaker/actions/workflows/ci.yml/badge.svg)](https://github.com/WyattAu/breaker/actions)
[![crates.io](https://img.shields.io/crates/v/breaker)](https://crates.io/crates/breaker)
[![license](https://img.shields.io/crates/l/breaker)](LICENSE-MIT)

## Feature Flags

| Feature | Default | Description |
|---|---|---|
| `std` | ✅ | Standard-library support. |
| `metrics` | — | Emit `circuit_breaker_*` counters/histograms via the [`metrics`](https://docs.rs/metrics) facade. |
| `tower` | — | Real Tower middleware: `BreakerLayer`/`BreakerService` over any `tower::Service` (deps: `tower` + `util`, `tower-layer`). Axum/Tonic-compatible. |
| `timeout` | — | Optional per-call timeout via `tokio::time::timeout`; timed-out calls count as failures. This is the crate's only runtime dependency (`tokio`, time-only). |

## Features

- Three-state machine: **Closed → Open → HalfOpen → Closed**
- **Trips on whichever fires first:**
  - *consecutive failures* (the classic streak rule), or
  - *sliding-window failure rate* — a fixed-capacity ring buffer of the
    last N outcomes, tripping when `failures / min(size, filled) ≥
    failure_rate_threshold` once `minimum_calls` outcomes are recorded
- **Half-open probe permits**: at most `half_open_max_calls` concurrent
  probes test the protected service during recovery — the rest are rejected
  instantly (retry-stampede protection), without counting as failures
- **Backoff strategies**: `Fixed`, `Exponential { initial, max, factor }`,
  `ExponentialJitter { …, jitter }` (full jitter = uniform `[0, computed]`),
  applied per trip via an attempt counter
- **Typed errors**: `CircuitBreakerError<E>` preserves the original error
  value — no stringification; `E` needs no `Display`
- **Failure predicate**: classify which errors count as failures; the rest
  pass through untouched and leave the state machine alone
- Builder with `.standard()`, `.fast_fail()`, `.lenient()` presets
- Per-call metrics: lifetime + window failure rate, state, transitions
- `on_state_change` hook, `trip()` / `reset()` manual control
- Model-checked (loom), proof-harnessed (kani), 0-alloc fast path

## State Machine

```text
┌────────┐  threshold / rate   ┌──────┐  backoff wait   ┌──────────┐
│ Closed │ ──────────────────> │ Open │ ──────────────> │ HalfOpen │
└────────┘                     └──────┘                 └──────────┘
     ^                          re-trip                    │  │
     └─────────────────────────────────────────────────┘  │
                  success_threshold probes ───────────────┘
```

## Quick Start

```rust,no_run
use breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError};

#[tokio::main]
async fn main() -> Result<(), CircuitBreakerError<String>> {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());

    let body: String = cb
        .call(|| async {
            Ok::<_, String>("payload".to_string())
        })
        .await?;

    let m = cb.metrics();
    println!("state: {:?}, failures: {}", m.state, m.total_failures);

    Ok(())
}
```

## Tripping policy

While `Closed`, the circuit opens when **either** rule fires:

| Rule | Config | Default | Meaning |
|---|---|---|---|
| Consecutive failures | `consecutive_failures` | `5` | N failures in a row (any success resets the streak). |
| Window failure rate | `failure_rate_threshold` + `sliding_window_size` + `minimum_calls` | `0.5`, `10`, `10` | Over the last `sliding_window_size` outcomes, `failures / min(size, filled) ≥ threshold`, evaluated once `filled ≥ minimum_calls`. |

The default `minimum_calls = sliding_window_size` means *"5 failures within
the last 10 calls"* — set `minimum_calls(1)` for the aggressive
early-warning variant (every failure evaluated against a partially filled
window, so even the first failure trips). Set `failure_rate_threshold(0.0)`
to disable the window rule entirely.

## Half-open probes (stampede protection)

When the backoff wait elapses, the circuit reads as `HalfOpen`. At most
`half_open_max_calls` concurrent probe calls are admitted; excess calls get
`CircuitBreakerError::Rejected` immediately — they never touch the
protected service and never count as failures. Permits are acquired with a
lock-free CAS and released when the probe completes **or its future is
dropped** (cancellation-safe). After `success_threshold` successful probes
the circuit closes (and the window resets); a single failed probe re-trips
it, with the backoff strategy extending the next wait.

## Backoff strategies

```rust
use breaker::{BackoffStrategy, CircuitBreakerConfig};
use std::time::Duration;

let config = CircuitBreakerConfig::builder()
    .backoff(BackoffStrategy::ExponentialJitter {
        initial: Duration::from_millis(10),
        max: Duration::from_secs(1),
        factor: 2.0,
        jitter: 1.0, // full jitter: wait ~ U[0, computed]
    })
    .build();
```

Every trip (including `HalfOpen → Open` re-trips) increments an attempt
counter and computes the wait as `initial · factor^(attempts−1)`, capped at
`max`; the jittered variant scales it by `1 − jitter·rand()`. Deterministic
sequences are unit-tested via `BackoffStrategy::wait_for(attempt)`.

## Presets

| Preset       | Rate (window)     | Streak | Wait   | Half-Open |
|--------------|-------------------|--------|--------|-----------|
| `standard()` | 0.5 over 10       | 5      | 30 s   | 3         |
| `fast_fail()`| 1.0 over 5        | 1      | 10 s   | 1         |
| `lenient()`  | 0.5 over 20       | 10     | 60 s   | 5         |

## Tower Integration

Enable the `tower` feature for real middleware — `BreakerLayer` wraps any
`tower::Service`, including Axum routers:

```rust,ignore
use breaker::{CircuitBreaker, CircuitBreakerConfig, tower::BreakerLayer};
use axum::{Router, routing::get};

let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());

let app = Router::new()
    .route("/", get(handler))
    .layer(BreakerLayer::new(cb));
```

The service error is `CircuitBreakerError<S::Error>` — the inner error is
preserved typed, no `BoxError` conversion, no allocation on the allowed
path. `BreakerService::call` boxes one response future per request (the
standard Tower-middleware trade-off; see the module docs in
`src/tower.rs`). A runnable example:
`cargo run --example tower_middleware --features tower`.

## Failure classification

```rust
use breaker::CircuitBreakerConfig;

let config = CircuitBreakerConfig::builder()
    // Only connection errors trip the circuit:
    .failure_predicate(|e: &std::io::Error| {
        e.kind() == std::io::ErrorKind::ConnectionRefused
    })
    .build();
```

Predicate-false errors are returned to the caller verbatim (typed) and do
not count toward any threshold.

## Comparison with tower-resilience

|                    | breaker                       | tower-resilience             |
|--------------------|-------------------------------|------------------------------|
| Algorithm          | State machine + sliding window| State machine                |
| Metrics            | Built-in `CircuitMetrics`     | None                         |
| Presets            | `.standard() / .fast_fail()`  | Manual config only           |
| Backoff            | Fixed / Exp / Exp+jitter      | Fixed                        |
| Half-open permits  | Bounded concurrent probes     | Single probe                 |
| Tower layer        | Optional                      | Required                     |

## Out of scope (future work)

Composable policies (`OrElse` chaining of breakers/retries, composite
retry+breaker pipelines) are deliberately **not** in 2.0.0 — predicate-based
classification covers the main composition use case. Tracked as future
work; the config surface stays flat until then.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)
at your option.

## Concurrency testing

The state machine is model-checked with [loom](https://crates.io/crates/loom)
under `--cfg loom`: the `parking_lot::RwLock` is swapped for `loom::sync::RwLock`
(and the half-open permit counter for `loom`'s `AtomicUsize`) and
`tests/loom.rs` proves, across all bounded interleavings, that concurrent
failure recording never loses updates (exactly one trip at threshold) and that
`trip()` racing `record_success()` yields a serialized, consistent final state.

```sh
RUSTFLAGS="--cfg loom" cargo test --release --test loom
```

What loom does not cover: the async `call()` path's check-then-await-then-record
window — recording results as operations complete is the documented
total-ordering design, and tokio/loom cannot model the await point. The
half-open permit protocol (acquire before the await, release on completion
*or* cancellation) is enforced by a drop guard, so cancelled futures cannot
leak slots; the concurrent-probe bound is covered by integration tests.

## Security

Threat model: [THREAT-MODEL.md](THREAT-MODEL.md).

## Performance

Measured hot-path SLOs and allocation profile: [PERF-SLO.md](PERF-SLO.md). Benchmarks run in CI (non-gating regression visibility against the saved `ci` baseline).

| Hot path (criterion mean, 2026-09-11, pinned CPU, 6-core x86_64) | 2.0.0 | 1.0.0 (2026-09) | SLO |
|---|---|---|---|
| `call` allowed (closed circuit), single | **35.7 ns** | 37 ns | < 50 ns |
| `call` allowed, ×1000 batch | 35.3 ns/call | 39.0 ns/call | |
| `call` rejected (open circuit, short-circuit), single | 41.8 ns | 46 ns | < 60 ns, no user future polled |

The sliding-window record (one O(1) `VecDeque` push + running counter) and
the half-open permit check (skipped entirely while `Closed`) add no
measurable cost on the allowed path; making `call` generic over the error
type is monomorphized away. The rejected path is source-identical to 1.0.0.
The CI perf gate is the deterministic iai-callgrind instruction count
(`perf-gate` job); the gate re-baselines on the 2.0.0 main push.

Head-to-head numbers against failsafe (the leading dedicated circuit-breaker crate), with an honest feature comparison: [COMPARISON.md](COMPARISON.md).
