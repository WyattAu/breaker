# Performance SLOs — breaker

Measured with criterion (`cargo bench --bench call_overhead`), 2026-09.
Hardware: Intel(R) Core(TM) i5-9400F CPU @ 2.90GHz, 6 cores, Linux x86_64.
Criterion reports mean/median/stddev, not percentiles; **P50 column = criterion
mean** (P99 is not directly measured — treat the mean as the regression
baseline; the CI bench job compares against the saved `ci` baseline).

## Measured (mean per operation)

| Benchmark | P50 (mean) | Notes |
|---|---|---|
| `call` allowed path, single call | **37 ns** | closed circuit, always-succeed op |
| `call` allowed path, ×100 batch | 36.5 ns/call | |
| `call` allowed path, ×1000 batch | 39.0 ns/call | steady-state |
| `call` rejected path (open), single | 46 ns | read-lock short-circuit |
| `call` rejected path (open), ×1000 | 50.3 ns/call | |

## SLO statements

- `CircuitBreaker::call` adds **< 50 ns overhead P50 (measured 37 ns) on the
  allowed path** on an idle closed circuit (measured 2026-09, 6-core x86_64).
- The rejected path (open circuit) short-circuits in **< 60 ns P50
  (measured 46 ns)** — no user future is polled.

## Allocation profile

Verified empirically with a temporary counting `GlobalAlloc` (test removed
after measurement; numbers retained):

- **Allowed path: ~0 allocations per call** — 4 allocations observed across
  10,000 calls (0.0004/call, background noise from the async runtime, none
  attributable to `call`).
- **Rejected path: exactly 0 allocations per call** across 10,000 calls.

Mechanism: the state check takes a parking_lot read lock; success/failure
recording takes a write lock; no `String`/`Vec` is constructed on either
steady-state path (error `to_string()` allocation occurs only on the user
operation's `Err` branch, not on circuit transitions).

## Regression policy

- Baselines are saved on main in CI by the shared bench job
  ([rust-kit.yml](https://github.com/WyattAu/engineering-standards/blob/main/.github/workflows/rust-kit.yml),
  `cargo bench -- --save-baseline ci`), non-gating (regression visibility).
- Local baseline: `cargo bench --bench call_overhead -- --save-baseline main`,
  compare with `cargo bench --bench call_overhead -- --baseline main`.
- Alert threshold: >2× mean regression on `call_allowed/call_ok_1`.
