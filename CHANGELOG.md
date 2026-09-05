# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

## [0.3.0] - 2026-09-03

### Changed

- Performance: `Cow`/`Arc` for `CircuitBreakerError` and breaker name to
  reduce clones on the hot path.

### Testing

- Loom model-checking of state-machine concurrency
  (`RUSTFLAGS="--cfg loom" cargo test --release --test loom`): concurrent
  failures trip the circuit exactly once with no lost updates; `trip()`
  vs `record_success()` races stay serialized.

## [0.2.0] - 2026-09-02

### Added

- `metrics` feature flag for circuit-breaker observability: gauge for
  circuit state and histogram for failure count.

## [0.1.0] - 2026-09-01

### Added

- Three-state circuit breaker: Closed → Open → HalfOpen → Closed, with
  configurable failure threshold, sliding window, and wait duration.
- `record_success` / `record_failure`, `is_open` / `is_closed`,
  `on_state_change` hook, and `success_threshold` for half-open probes.
- Builder with `.standard()`, `.fast_fail()`, `.lenient()` presets.
- Per-call metrics: failure rate, state, transition count.
- Optional Tower `Layer` for Axum / Tonic integration (`tower` feature).
