# breaker vs failsafe — head-to-head comparison

failsafe (crates.io: `failsafe`) is the most-used dedicated circuit-breaker
crate on crates.io (~16M downloads; futures-aware state machine with
pluggable policies). This page compares them honestly: measured numbers
first, then features.

Reproduce:

```sh
cargo bench --bench head_to_head
```

## Benchmark: allowed-path call and state check

Same operation on both sides, steady state (closed breaker, always-succeed
operation, no transitions during the run), criterion, tokio current runtime:

- `*_call_ok`: one `call(<always-succeed op>).await` through the breaker.
  Ours takes a closure returning a future; failsafe's futures API takes a
  future value directly. The constructed future and the recorded outcome are
  identical.
- `*_is_call_permitted` / `is_closed`: the cheapest decision each crate
  exposes, isolating state/lock overhead from future plumbing.

| benchmark                      | median time |
|--------------------------------|-------------|
| `breaker_call_ok`              | ~71 ns      |
| `failsafe_call_ok`             | ~133 ns     |
| `failsafe_is_call_permitted`   | ~18 ns      |
| `breaker_is_closed`            | ~19 ns      |

Hardware: Intel Core i5-9400F @ 2.90GHz (6 cores), Linux x86_64,
rustc 1.94.1, criterion 0.5, failsafe 1.3.0 (default features), breaker 1.0.0
with default features.

Reading the numbers honestly:

- On the allowed path breaker is ~1.9x faster (71 ns vs 133 ns): the hot
  path is a parking_lot read-lock state check plus a success record; we
  don't wrap the operation future in an extra state-machine combinator the
  way failsafe's `ResponseFuture` does.
- State checks are a wash (~18 vs ~19 ns) — both are a lock + enum compare.
- Both numbers are pure overhead: at ~71 ns, a breaker adds ~0.7 ms of
  overhead per million calls. Unless your service does tens of millions of
  calls per second per core, overhead should not decide this for you.

## Feature matrix

|                                   | breaker 2.0                       | failsafe 1.3                          |
|-----------------------------------|-----------------------------------|---------------------------------------|
| Async `call()`                    | Yes (closure -> future)           | Yes (future value, `futures-support`) |
| Sync `call()`                     | No (async only)                   | Yes (`FnMut -> Result`)               |
| State machine                     | closed/open/half-open             | closed/open/half-open                 |
| Failure accrual                   | Sliding window + failure-rate threshold **and** consecutive-failure streak (whichever first) | Consecutive failures, EWMA success-rate window, `OrElse` combinator |
| Open-duration strategy            | Fixed / exponential / exponential-with-jitter backoff (per-trip attempt counter) | Pluggable backoff (policy-driven)     |
| Error classification              | Yes (`failure_predicate` — non-failures pass through typed) | Yes (`FailurePredicate`)              |
| Typed errors                      | Yes (`CircuitBreakerError<E>` preserves the original error) | No (maps to its own error type)       |
| State-change hooks                | Yes (`on_state_change` callback)  | Yes (`Instrument` trait)              |
| Presets                           | Yes (`.standard()/.fast_fail()/.lenient()`) | No                          |
| Metrics snapshot                  | Yes (`CircuitMetrics` + window rate, `metrics` feature) | No                    |
| Tower layer                       | Optional built-in (`BreakerLayer`, real) | No                             |
| Half-open probe budget            | Yes — bounded concurrent probes with lock-free permits (`half_open_max_calls`, `success_threshold`) | Policy-internal        |
| Per-call timeout                  | Yes (`timeout` feature)           | No                                    |
| Model checking / verification     | Yes (loom: `tests/loom.rs`; Kani: `tests/kani.rs`) | No                  |
| Overhead measured here            | ~71 ns/call (1.0 measurement; 2.0 re-measured at parity, see PERF-SLO.md) | ~133 ns/call       |

## Positioning

failsafe is the better choice when you need a sync-callable breaker or an
`OrElse` policy-combinator engine — it remains the more configurable policy
engine. breaker now covers the rest of that gap itself: window-based
tripping, exponential/jitter backoff, error-classification predicates, and
per-call timeouts are all built in as of 2.0.0.

breaker is the better choice when you live in async/tokio (faster allowed
path, closure-based call), want batteries included — presets, metrics
snapshots, a real Tower layer, bounded half-open probing, backoff
strategies, typed error passthrough — or care about
model-checked concurrency (loom) and proof-checked state transitions
(Kani).

The honest headline: both are correct, cheap state machines; breaker wins
measured overhead and integration surface in async Rust, failsafe wins
policy flexibility and sync support.
