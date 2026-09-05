# Threat Model — breaker

Status: **v1.0** · One-page STRIDE over the public API surface
(`CircuitBreaker`, `record_success`/`record_failure`, `call`, `trip`/`reset`,
`CircuitBreakerConfig`, metrics).

The breaker is a self-DoS device by design: its "attack surface" is mostly
about state-machine races, not adversaries. Assets: (A1) fail-fast behavior
(open circuit actually rejects), (A2) state consistency under concurrent
callers, (A3) availability (breaker must not lock the service out forever).

| # | Threat | Category | Surface | Mitigation | Verifying test |
|---|--------|----------|---------|------------|----------------|
| T1 | Concurrent failure/success updates lost | Tampering | `record_success`/`record_failure` | Atomic state/counter updates | `tests/loom.rs::loom_concurrent_failures_no_lost_updates`, `loom_concurrent_trip_and_success_serialized` |
| T2 | Open circuit admits calls (fail-fast broken) | Spoofing | `call` | State check before invoking inner service | `tests/integration.rs::open_circuit_rejects_all_calls`, `call_returns_inner_error_value` |
| T3 | Half-open probe storm | DoS | half-open transition | `half_open_max_calls` serialization | `tests/loom.rs::loom_concurrent_trip_and_success_serialized`, `tests/integration.rs::half_open_failure_reopens_circuit` |
| T4 | Stuck open (permanent lockout) | DoS | wait duration + success threshold | Timed half-open transition; configurable `success_threshold` | `tests/integration.rs::full_lifecycle_closed_open_halfopen_closed`, `success_threshold_defaults_to_half_open_max` |
| T5 | Forced trip/reset bypasses invariants | Elevation | `trip`/`reset` | Explicit manual transitions, tested | `tests/integration.rs::trip_and_reset_forced_transitions` |

**OPEN RISKS**

- **OPEN-1 — no wall-clock injection.** Wait-duration transitions use the
  real clock; time-travel tests (deterministic transition timing) are not
  possible, so NTP jumps may shorten/lengthen the open window untested.
- **OPEN-2 — metrics are informational only.** `CircuitMetrics` exposes
  counts; no alerting hook verifies that a caller actually reacts to an
  open circuit.

**Out of scope:** upstream dependency health (the breaker reacts, never
probes); retry/backoff policy (caller-side).

**Residual risk:** per-instance state only — a fleet of instances trips
independently, so systemic failure is detected N times slower.
