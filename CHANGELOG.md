# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

## [2.0.0] - 2026-09-11

Integrity release: every feature advertised on the 1.0.0 tin now exists,
and the API gaps the 1.0.0 audit found are closed. **Breaking changes** are
listed under each item; there is no compat layer (clean break, ~0 external
users).

### Added

- **Real Tower integration** behind the (now real) `tower` feature:
  `BreakerLayer` + `BreakerService` wrap any `tower::Service` in the
  breaker. Typed error mapping (`CircuitBreakerError<S::Error>` — no
  `BoxError`, no allocation on the allowed path); one boxed future per
  request (documented middleware trade-off). Axum-ready; runnable example
  (`examples/tower_middleware.rs`) and a doctest. 1.0.0 shipped an *empty*
  `tower = []` stub with README/CHANGELOG claims — that is now true.
- **Sliding-window failure-rate tripping**: `sliding_window_size` is wired
  into a fixed-capacity ring buffer of recent outcomes (alloc-free after
  construction, O(1) recording behind the existing state lock). The circuit
  trips on whichever fires first: the window rate
  (`failures / min(size, filled) >= failure_rate_threshold`, evaluated once
  `filled >= minimum_calls`) or the consecutive-failure streak. New config:
  `minimum_calls` (defaults to the window size; `1` = aggressive
  early-warning mode). `CircuitMetrics` gains `window_failure_rate`.
- **Half-open probe permits**: `half_open_max_calls` is enforced — at most
  that many concurrent probes are admitted in `HalfOpen`; excess calls are
  rejected immediately with the new `CircuitBreakerError::Rejected` variant
  (not counted as failures). Lock-free CAS acquisition, drop-guard release
  (cancellation-safe). Previously half-open admitted *unlimited*
  concurrent probes (stampede risk on recovery).
- **Backoff strategies**: `BackoffStrategy::Fixed | Exponential |
  ExponentialJitter` (full jitter = uniform `[0, computed]`). Every trip
  increments an attempt counter; the Open wait is recomputed per trip.
  Non-cryptographic thread-local xorshift jitter source — no new deps.
- **Failure predicate**: `failure_predicate` on the config builder — only
  predicate-true errors count as breaker failures; others pass through
  with the original error and never touch the state machine.
- **Typed errors**: `CircuitBreaker::call` is generic over the operation
  error `E`; the original error value is preserved in
  `CircuitBreakerError::Failure(E)` with no `Display`/`String` conversion.
- **`timeout` feature**: optional per-call timeout
  (`tokio::time::timeout`); timed-out calls count as failures and surface
  as `CircuitBreakerError::Timeout` (the variant existed in 1.0.0 but was
  never constructed).

### Changed (breaking)

- `failure_rate_threshold: u32` (consecutive-failure count) →
  `failure_rate_threshold: f32` (window failure fraction, 0.0–1.0). The
  old meaning moved to the new `consecutive_failures: u32` field. Presets
  preserve 1.0.0 behavior: `standard()` = 5-failure streak / 5-in-last-10
  rate; `fast_fail()` = trip on first failure; `lenient()` = 10.
- `CircuitBreakerError` is generic: `CircuitBreakerError<E>` with variants
  `CircuitOpen`, `Rejected`, `Failure(E)`, `Timeout` (`timeout` feature).
  The 1.0.0 `Inner(Cow<'static, str>)` erasure is gone; match on
  `Failure(e)` instead of `Inner(msg)`.
- `wait_duration` config field is replaced by `backoff: BackoffStrategy`;
  `.wait_duration(d)` remains as a convenience builder method equal to
  `.backoff(BackoffStrategy::Fixed(d))`.
- `CircuitBreaker::call` requires `E: 'static` (the predicate's type
  erasure); `E` no longer needs `Display`.
- `CircuitMetrics` gains `window_failure_rate: f32`.
- `tokio` is now an optional dependency (enabled by the `timeout` feature
  only); the core crate has no mandatory async-runtime dependency.
- The `tower` feature pulls real dependencies (`tower` 0.5 + `util`,
  `tower-layer` 0.3) instead of being an empty stub.

### Migration notes (1.0.0 → 2.0.0)

- `.failure_rate_threshold(3)` → `.consecutive_failures(3)` (streak rule)
  or `.failure_rate_threshold(0.3)` (window fraction).
- `CircuitBreakerError::Inner(msg)` → `CircuitBreakerError::Failure(e)` —
  and `e` is your original typed error, not a `String`.
- `.wait_duration(Duration::from_secs(30))` still works (maps to
  `Fixed(30 s)`); for growth between trips use
  `.backoff(BackoffStrategy::Exponential { .. })`.
- Dead config is live: `sliding_window_size` and `half_open_max_calls`
  now do exactly what their names say.
- `cargo semver-checks` will fail against the 1.0.0 baseline — expected
  and intentional for a major release.

## [1.0.0] - 2026-09-05

### Added

- API declared stable; semver contract enforced via cargo-semver-checks CI gate.
- Three-state circuit breaker (Closed → Open → HalfOpen) with failure
  threshold, sliding window, wait duration, and half-open success threshold.
- Builder with `.standard()` / `.fast_fail()` / `.lenient()` presets,
  per-call metrics, and state-change hook.
- `metrics` feature flag; optional Tower layer for Axum/Tonic integration.
- Loom model-checking of concurrency and Kani proofs of state-machine
  invariants.

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
