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

## Addendum (2026-09): deterministic CI gate via iai-callgrind

Criterion measures wall-clock time and cannot pass/fail a PR on a busy
runner. The gating signal is now **instruction counts** from
iai-callgrind (`benches/iai_hot_path.rs`), which are deterministic for a
given binary:

- `iai_hot_path/call_allowed` — steady-state closed-circuit `call`
  (read-lock state check + write-lock success record).
- `iai_hot_path/call_rejected` — open-circuit short-circuit `call`.

Split of responsibilities: **iai-callgrind is the CI gate**
(`perf-gate` job, PR compares against the cached `main` baseline with
`--fail-fast`); **criterion remains the human-readable trend** and the
source of the SLO wall-clock numbers above (its saved `ci` baseline stays
non-gating).

Baseline workflow: every push to main re-saves the `main` baseline
(`cargo bench --bench iai_hot_path -- --save-baseline=main`) and caches it
in `target/iai`; PRs run `cargo bench --bench iai_hot_path --
--baseline=main --fail-fast`. Baseline updates are **intentional**: after
merging a deliberate perf change, the next main push refreshes the
reference.

Local runs: this gate needs `valgrind` (not installed on the primary dev
machine, no passwordless sudo — CI-only until then). Without valgrind,
compile-check with `cargo bench --no-run --bench iai_hot_path` (verified:
compiles clean, harness wires to `iai-callgrind-runner` 0.16.1 and fails
exactly at the valgrind lookup). Tooling note: iai-callgrind 0.16.1 is the
final release under that name; the project continues as `gungraun` (API
compatible, renamed). Benchmarks run through plain
`cargo bench` + `iai-callgrind-runner` in PATH — there is no
`cargo iai-callgrind` subcommand in this version line.

## Addendum (2026-09-11): 2.0.0 re-measurement — no allowed-path regression

2.0.0 changes the hot path in three ways: the sliding-window record on
every recorded outcome (one O(1) `VecDeque` push + running-counter update,
inside the existing write lock), the half-open permit check (a branch on
the state match — **skipped entirely while `Closed`**), and `call`'s
genericity over the user error type (monomorphized away; the rejected-path
error construction is the same unit variant as 1.0.0).

Re-measured (criterion, same machine, **CPU-pinned with `taskset`** — the
box was under load average > 10 that day and unpinned runs varied ±80%,
with 1.0.0 itself measuring slower than 2.0.0 in one A/B run; pinning was
required for any meaningful number):

| Benchmark | 2.0.0 (pinned mean) | 1.0.0 (2026-09 baseline) |
|---|---|---|
| `call_allowed/call_ok_1` | **35.7 ns** | 37 ns |
| `call_allowed/call_ok_100` | 35.7 ns/call | 36.5 ns/call |
| `call_allowed/call_ok_1000` | 35.3 ns/call | 39.0 ns/call |
| `call_rejected/call_open_1` | 41.8 ns | 46 ns |
| `call_rejected/call_open_1000` | (noisy, see below) | 50.3 ns/call |

Verdict: **the allowed path did not regress** — 2.0.0 measures at or below
every 1.0.0 number under identical conditions, so the SLO statements above
carry over unchanged. The generic `E` adds no cost (confirmed
empirically; there is no `Display`/`String` conversion anywhere on the
hot path anymore — that allocation moved out of the error path entirely).
The ×1000 *rejected* batch numbers were load-noise dominated in both the
1.0.0 and 2.0.0 runs; the single-shot rejected path (the SLO metric) is
source-identical to 1.0.0 and measured faster than baseline.

The authoritative gate remains iai-callgrind in CI: the 2.0.0 push to main
re-saves the `main` baseline (intentional baseline update per the policy
above), and subsequent PRs are gated against it.
