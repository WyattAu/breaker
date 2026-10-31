# Threat Model: breaker
| T3 | Half-open probe storm | DoS | half-open transition | Bounded probe permits: lock-free CAS admission capped at `half_open_max_calls`, drop-guard release (cancellation-safe); **real as of 2.0.0** (the 1.0.0 config field existed but was not enforced; unlimited concurrent probes were admitted) | `tests/integration.rs::half_open_permits_bound_concurrent_probes`, `half_open_rejects_when_no_probe_capacity`, `cancelled_probe_releases_permit`, `tests/tower.rs::concurrent_requests_respect_probe_permits` |
- **OPEN-1; no wall-clock injection.** Wait-duration transitions use the
- **OPEN-2; metrics are informational only.** `CircuitMetrics` exposes
**Residual risk:** per-instance state only; a fleet of instances trips