# Requirements — breaker

Numbered, testable requirements. Every requirement maps to at least one named
test; every security-relevant test cites at least one requirement. Doc
comments on the implementing public item carry `REQ-BRK-NNN` tags.

## Functional

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-BRK-001 | A new `CircuitBreaker` starts in the `Closed` state | MUST |
| REQ-BRK-002 | `call` executes the wrapped async operation and returns its `Ok` value unchanged when the circuit is closed | MUST |
| REQ-BRK-003 | `call` returns the inner error wrapped as `CircuitBreakerError::Inner` (message preserved) when the operation fails and the circuit stays closed | MUST |
| REQ-BRK-004 | When consecutive failures reach `failure_rate_threshold`, the circuit transitions `Closed → Open` | MUST |
| REQ-BRK-005 | While `Open`, `call` rejects immediately with `CircuitBreakerError::CircuitOpen` without invoking the operation | MUST |
| REQ-BRK-006 | After `wait_duration` elapses, the circuit transitions `Open → HalfOpen` and admits probe calls | MUST |
| REQ-BRK-007 | After `success_threshold` successes in `HalfOpen`, the circuit transitions `HalfOpen → Closed` | MUST |
| REQ-BRK-008 | A failure in `HalfOpen` transitions the circuit back to `Open` | MUST |
| REQ-BRK-009 | A success recorded in `Closed` resets the consecutive-failure count (failures before + after a success do not trip) | MUST |
| REQ-BRK-010 | `record_success` / `record_failure` manually drive the same state machine as `call` (including `success_threshold` semantics) | MUST |
| REQ-BRK-011 | `trip()` forces the state to `Open`; `reset()` forces it to `Closed` | MUST |
| REQ-BRK-012 | `metrics()` returns a snapshot with correct `total_successes`, `total_failures`, `failure_rate` (0.0 initially), transition count, and current state | MUST |
| REQ-BRK-013 | `state()`, `is_open()`, `is_closed()`, `is_half_open()` reflect the actual state machine state at all times | MUST |
| REQ-BRK-014 | `CircuitBreakerConfig::standard` / `fast_fail` / `lenient` presets produce their documented threshold, window, wait, and half-open values | MUST |
| REQ-BRK-015 | `CircuitBreakerConfig::builder()` sets custom `failure_rate_threshold`, `sliding_window_size`, `wait_duration`, `half_open_max_calls`, and `success_threshold` | MUST |
| REQ-BRK-016 | `success_threshold` defaults to `half_open_max_calls` when not explicitly set | MUST |
| REQ-BRK-017 | The builder sets a breaker `name` (`"default"` when unset) retrievable via `name()` | MUST |
| REQ-BRK-018 | The `on_state_change` callback fires exactly once per transition with the correct `(prev, next)` pair, and never fires for non-transitions (successes while `Closed`) | SHOULD |
| REQ-BRK-019 | `CircuitBreakerError` variants render stable `Display` messages (`CircuitOpen`, `Timeout`, `Inner`) | SHOULD |
| REQ-BRK-020 | `CircuitBreaker` is `Clone`; clones share one state machine | MUST |

## Security

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-BRK-100 | The crate contains no `unsafe` code (`#![forbid(unsafe_code)]`); thread safety is achieved with standard library primitives | MUST |
| REQ-BRK-101 | `call` never panics on operation failure — inner errors are stringified into `CircuitBreakerError::Inner`, and arbitrary error values cannot crash the caller | MUST |
| REQ-BRK-102 | An open circuit fails fast: it must not invoke the user operation, preventing cascading load against a failing dependency | MUST |
| REQ-BRK-103 | State reads during `call` hold the lock only for the check/record window; user futures are awaited outside the lock so a hung operation cannot deadlock other callers | MUST |

## Robustness

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-BRK-200 | Concurrent `record_failure` calls from multiple tasks never lose updates — exactly `n` failures are counted for `n` racing callers (model-checked) | MUST |
| REQ-BRK-201 | Concurrent trip and success operations are serialized; the state machine never observes a torn transition | MUST |
| REQ-BRK-202 | For arbitrary operation sequences, invariants hold: state ∈ {Closed, Open, HalfOpen}; `Open` requires threshold reached or forced trip; failure rate stays within [0.0, 1.0] | MUST |
| REQ-BRK-203 | The half-open path maintains its invariants under arbitrary interleaving (bounded probes, no direct `HalfOpen → Closed` without `success_threshold` successes) | SHOULD |
| REQ-BRK-204 | Metrics stay consistent across the full lifecycle `Closed → Open → HalfOpen → Closed` (totals monotonically increase, transitions counted) | MUST |
| REQ-BRK-205 | Under the `metrics` feature, the failure-count histogram records the real consecutive-failure count (mutation-killed: no constant/stub values) | SHOULD |

## Traceability Matrix

| Requirement | Test (fn, file) | Property class |
|-------------|-----------------|----------------|
| REQ-BRK-001 | `starts_in_closed_state` (`src/lib.rs`), `starts_closed` (`tests/proptest.rs`) | unit/property |
| REQ-BRK-002 | `async_call_success` (`src/lib.rs`), `call_returns_inner_error_value` (`tests/integration.rs`) | unit |
| REQ-BRK-003 | `async_call_failure_propagates` (`src/lib.rs`) | unit |
| REQ-BRK-004 | `closed_to_open_after_failures` (`src/lib.rs`), `record_failure_threshold_opens` (`tests/proptest.rs`) | unit/property |
| REQ-BRK-005 | `open_rejects_requests` (`src/lib.rs`), `open_circuit_rejects_all_calls` (`tests/integration.rs`) | unit |
| REQ-BRK-006 | `open_to_half_open_after_wait` (`src/lib.rs`), `full_lifecycle_closed_open_halfopen_closed` (`tests/integration.rs`) | unit |
| REQ-BRK-007 | `record_success_manual` (`src/lib.rs`) | unit |
| REQ-BRK-008 | `half_open_to_open_on_failure` (`src/lib.rs`), `half_open_failure_reopens_circuit` (`tests/integration.rs`) | unit |
| REQ-BRK-009 | `success_resets_failure_count_in_closed` (`src/lib.rs`), `success_resets_failure_count` (`tests/proptest.rs`) | unit/property |
| REQ-BRK-010 | `manual_record_success_and_failure` (`tests/integration.rs`), `record_failure_manual` (`src/lib.rs`) | unit |
| REQ-BRK-011 | `trip_and_reset` (`src/lib.rs`), `trip_and_reset_forced_transitions` (`tests/integration.rs`), `trip_makes_open`, `reset_after_trip_makes_closed` (`tests/proptest.rs`) | unit/property |
| REQ-BRK-012 | `metrics_records_successes_and_failures` (`src/lib.rs`), `metrics_tracks_successes_and_failures`, `metrics_initial_state_zeroes`, `metrics_transitions_count` (`tests/integration.rs`) | unit |
| REQ-BRK-013 | `is_open_is_closed_is_half_open`, `state_method` (`src/lib.rs`) | unit |
| REQ-BRK-014 | `config_standard_preset` (`src/lib.rs`), `config_standard_preset_values`, `config_fast_fail_preset_values`, `config_lenient_preset_values` (`tests/integration.rs`) | unit |
| REQ-BRK-015 | `config_builder_custom` (`src/lib.rs`), `builder_config_fields` (`tests/integration.rs`), `success_threshold_config` (`src/lib.rs`) | unit |
| REQ-BRK-016 | `success_threshold_defaults_to_half_open_max_calls` (`src/lib.rs`), `success_threshold_defaults_to_half_open_max` (`tests/integration.rs`) | unit |
| REQ-BRK-017 | `name_returns_configured_name`, `name_default`, `builder_creates_named_breaker` (`src/lib.rs`), `builder_sets_name`, `builder_default_name` (`tests/integration.rs`) | unit |
| REQ-BRK-018 | `on_state_change_callback`, `on_state_change_records_transition`, `record_success_in_closed_does_not_transition` (`src/lib.rs`), `builder_with_state_change_callback` (`tests/integration.rs`) | unit |
| REQ-BRK-019 | `error_display_messages` (`src/lib.rs`), `error_display_all_variants` (`tests/integration.rs`) | unit |
| REQ-BRK-020 | `Clone` derive on `CircuitBreaker` (`src/lib.rs`); shared-state exercised by loom concurrency tests (`tests/loom.rs`) | unit/model |
| REQ-BRK-100 | `#![forbid(unsafe_code)]` (`src/lib.rs`); `src/lock.rs` std-only lock abstraction verified by `loom_concurrent_failures_no_lost_updates` (`tests/loom.rs`) | model |
| REQ-BRK-101 | `async_call_failure_propagates`, `call_returns_inner_error_value` (`src/lib.rs`, `tests/integration.rs`) | unit |
| REQ-BRK-102 | `open_rejects_requests` (`src/lib.rs`), `open_circuit_rejects_all_calls` (`tests/integration.rs`) | unit |
| REQ-BRK-103 | `src/lock.rs` design; verified by `loom_concurrent_trip_and_success_serialized` (`tests/loom.rs`) | model |
| REQ-BRK-200 | `loom_concurrent_failures_no_lost_updates` (`tests/loom.rs`) | model |
| REQ-BRK-201 | `loom_concurrent_trip_and_success_serialized` (`tests/loom.rs`) | model |
| REQ-BRK-202 | `kani_breaker_invariants_under_arbitrary_sequences` (`tests/kani.rs`), `failure_rate_bounded` (`tests/proptest.rs`) | model/property |
| REQ-BRK-203 | `kani_breaker_half_open_path_invariants` (`tests/kani.rs`) | model |
| REQ-BRK-204 | `full_lifecycle_closed_open_halfopen_closed` (`tests/integration.rs`) | integration |
| REQ-BRK-205 | `failure_count_histogram_records_consecutive_failures` (`src/lib.rs`, `metrics` feature) | mutation |
