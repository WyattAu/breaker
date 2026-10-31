Async circuit breaker for Rust; sliding-window failure-rate tripping,
| `metrics` | - | Emit `circuit_breaker_*` counters/histograms via the [`metrics`](https://docs.rs/metrics) facade. |
| `tower` | - | Real Tower middleware: `BreakerLayer`/`BreakerService` over any `tower::Service` (deps: `tower` + `util`, `tower-layer`). Axum/Tonic-compatible. |
| `timeout` | - | Optional per-call timeout via `tokio::time::timeout`; timed-out calls count as failures. This is the crate's only runtime dependency (`tokio`, time-only). |
  - *sliding-window failure rate*; a fixed-capacity ring buffer of the
  probes test the protected service during recovery; the rest are rejected
  value; no stringification; `E` needs no `Display`
the last 10 calls"*; set `minimum_calls(1)` for the aggressive
`CircuitBreakerError::Rejected` immediately; they never touch the
Enable the `tower` feature for real middleware; `BreakerLayer` wraps any
The service error is `CircuitBreakerError<S::Error>`; the inner error is
retry+breaker pipelines) are deliberately **not** in 2.0.0; predicate-based
window; recording results as operations complete is the documented