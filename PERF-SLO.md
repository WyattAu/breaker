# Performance SLOs: breaker
mean** (P99 is not directly measured; treat the mean as the regression
  (measured 46 ns)**; no user future is polled.
- **Allowed path: ~0 allocations per call**; 4 allocations observed across
- `iai_hot_path/call_allowed`; steady-state closed-circuit `call`
- `iai_hot_path/call_rejected`; open-circuit short-circuit `call`.
machine, no passwordless sudo; CI-only until then). Without valgrind,
`cargo bench` + `iai-callgrind-runner` in PATH; there is no
## Addendum (2026-09-11): 2.0.0 re-measurement: no allowed-path regression
the state match; **skipped entirely while `Closed`**), and `call`'s
Re-measured (criterion, same machine, **CPU-pinned with `taskset`**; the
Verdict: **the allowed path did not regress**; 2.0.0 measures at or below
hot path anymore; that allocation moved out of the error path entirely).