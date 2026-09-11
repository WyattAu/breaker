# Requirements — breaker

Numbered, testable requirements. Every requirement maps to at least one named
test; every security-relevant test cites at least one requirement. Doc
comments on the implementing public item carry `REQ-BRK-NNN` tags.

## Functional

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-BRK-001 | A new `CircuitBreaker` starts in the `Closed` state | MUST |
| REQ-BRK-002 | `call` executes the wrapped async operation and returns its `Ok` value unchanged when the circuit is closed | MUST |
| REQ-BRK-003 | `call` returns the original operation error, preserved typed as `CircuitBreakerError::Failure(E)`, when the operation fails and the circuit stays closed | MUST |
| REQ-BRK-004 | When consecutive failures reach `consecutive_failures`, **or** the sliding-window failure fraction reaches `failure_rate_threshold` (evaluated per `minimum_calls`), the circuit transitions `Closed → Open` — whichever fires first | MUST |
| REQ-BRK-005 | While `Open`, `call` rejects immediately with `CircuitBreakerError::CircuitOpen` without invoking the operation | MUST |
| REQ-BRK-006 | After the configured backoff wait elapses, the circuit transitions `Open → HalfOpen` and admits probe calls | MUST |
| REQ-BRK-007 | After `success_threshold` successes in `HalfOpen`, the circuit transitions `HalfOpen → Closed` | MUST |
| REQ-BRK-008 | A failure in `HalfOpen` transitions the circuit back to `Open` | MUST |
| REQ-BRK-009 | A success recorded in `Closed` resets the consecutive-failure count (failures before + after a success do not trip) | MUST |
| REQ-BRK-010 | `record_success` / `record_failure` manually drive the same state machine as `call` (including `success_threshold` semantics) | MUST |
| REQ-BRK-011 | `trip()` forces the state to `Open`; `reset()` forces it to `Closed` | MUST |
| REQ-BRK-012 | `metrics()` returns a snapshot with correct `total_successes`, `total_failures`, `failure_rate` (0.0 initially), transition count, and current state | MUST |
| REQ-BRK-013 | `state()`, `is_open()`, `is_closed()`, `is_half_open()` reflect the actual state machine state at all times | MUST |
| REQ-BRK-014 | `CircuitBreakerConfig::standard` / `fast_fail` / `lenient` presets produce their documented threshold, window, wait, and half-open values | MUST |
| REQ-BRK-015 | `CircuitBreakerConfig::builder()` sets custom `consecutive_failures`, `failure_rate_threshold`, `sliding_window_size`, `minimum_calls`, `backoff`, `half_open_max_calls`, and `success_threshold` | MUST |
| REQ-BRK-016 | `success_threshold` defaults to `half_open_max_calls` when not explicitly set | MUST |
| REQ-BRK-017 | The builder sets a breaker `name` (`"default"` when unset) retrievable via `name()` | MUST |
| REQ-BRK-018 | The `on_state_change` callback fires exactly once per transition with the correct `(prev, next)` pair, and never fires for non-transitions (successes while `Closed`) | SHOULD |
| REQ-BRK-019 | `CircuitBreakerError` variants render stable `Display` messages (`CircuitOpen`, `Rejected`, `Timeout`; `Failure` delegates to the inner error) | SHOULD |
| REQ-BRK-020 | `CircuitBreaker` is `Clone`; clones share one state machine | MUST |
| REQ-BRK-021 | In `HalfOpen`, at most `half_open_max_calls` concurrent probes are admitted; excess calls are rejected with `CircuitBreakerError::Rejected` and do not count as failures; permits are released on probe completion **or** future cancellation | MUST |
| REQ-BRK-022 | The sliding window holds at most `sliding_window_size` outcomes (oldest evicted), resets to empty on `HalfOpen → Closed`, and its rate stays within [0.0, 1.0] under any outcome sequence | MUST |
| REQ-BRK-023 | Every trip recomputes the Open wait from `BackoffStrategy` with a monotonically increasing attempt counter; recovering to `Closed` resets the counter; `wait_for` is deterministic for `Fixed`/`Exponential` and bounded by the unjittered value for `ExponentialJitter` | MUST |
| REQ-BRK-024 | With a `failure_predicate`, predicate-false errors pass through typed, do not count as failures, and do not affect the state machine; predicate-true errors count; without a predicate every error counts | MUST |
| REQ-BRK-025 | Under the `timeout` feature, an operation exceeding `call_timeout` is aborted, recorded as a failure, and surfaces as `CircuitBreakerError::Timeout` | MUST |
| REQ-BRK-026 | Under the `tower` feature, `BreakerLayer` wraps any `tower::Service`; the service error is `CircuitBreakerError<S::Error>` with the inner error preserved typed; clones share the breaker | MUST |

## Security

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-BRK-100 | The crate contains no `unsafe` code (`#![forbid(unsafe_code)]`); thread safety is achieved with standard library primitives | MUST |
| REQ-BRK-101 | `call` never panics on operation failure — inner errors are preserved typed inside `CircuitBreakerError::Failure(E)`, and arbitrary error values cannot crash the caller | MUST |
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
| REQ-BRK-002 | `async_call_success` (`src/lib.rs`), `call_returns_original_typed_error` (`tests/integration.rs`) | unit |
| REQ-BRK-003 | `async_call_failure_preserves_typed_error` (`src/lib.rs`) | unit |
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
| REQ-BRK-101 | `async_call_failure_preserves_typed_error` (`src/lib.rs`), `call_returns_original_typed_error` (`tests/integration.rs`), `non_display_error_type_passes_through` (`src/lib.rs`) | unit |
| REQ-BRK-102 | `open_rejects_requests` (`src/lib.rs`), `open_circuit_rejects_all_calls` (`tests/integration.rs`) | unit |
| REQ-BRK-103 | `src/lock.rs` design; verified by `loom_concurrent_trip_and_success_serialized` (`tests/loom.rs`) | model |
| REQ-BRK-200 | `loom_concurrent_failures_no_lost_updates` (`tests/loom.rs`) | model |
| REQ-BRK-201 | `loom_concurrent_trip_and_success_serialized` (`tests/loom.rs`) | model |
| REQ-BRK-202 | `kani_breaker_invariants_under_arbitrary_sequences` (`tests/kani.rs`), `failure_rate_bounded` (`tests/proptest.rs`) | model/property |
| REQ-BRK-203 | `kani_breaker_half_open_path_invariants` (`tests/kani.rs`) | model |
| REQ-BRK-204 | `full_lifecycle_closed_open_halfopen_closed` (`tests/integration.rs`) | integration |
| REQ-BRK-205 | `failure_count_histogram_records_consecutive_failures` (`src/lib.rs`, `metrics` feature) | mutation |
| REQ-BRK-021 | `half_open_permits_bound_concurrent_probes`, `half_open_permits_released_after_failure`, `half_open_rejects_when_no_probe_capacity`, `cancelled_probe_releases_permit` (`src/lib.rs`, `tests/integration.rs`), `concurrent_requests_respect_probe_permits` (`tests/tower.rs`) | unit |
| REQ-BRK-022 | `sliding_window_rate_trips_interleaved_failures`, `sliding_window_resets_after_recovery`, `window_rate_is_evaluated_when_failures_are_recorded`, `early_warning_mode_evaluates_partially_filled_window` (`src/lib.rs`, `tests/integration.rs`), `window_rate_stays_bounded`, `window_filled_never_exceeds_capacity` (`tests/proptest.rs`), state-machine window tests (`src/state.rs`) | unit/property |
| REQ-BRK-023 | `backoff_*` tests (`src/config.rs`), `exponential_backoff_extends_each_open_episode`, `jittered_backoff_still_opens_and_half_opens` (`tests/integration.rs`), `open_attempts_grow_and_backoff_applies` (`src/state.rs`) | unit |
| REQ-BRK-024 | `predicate_false_errors_do_not_trip` (`src/lib.rs`), `predicate_classifies_errors` (`tests/integration.rs`), predicate tests (`src/config.rs`) | unit |
| REQ-BRK-025 | `timeout_fires_and_counts_as_failure`, `timeout_disabled_passes_slow_calls` (`src/lib.rs`, `timeout` feature) | unit |
| REQ-BRK-026 | `tests/tower.rs` (layer passthrough, typed inner error, open-circuit short-circuit, `ServiceBuilder` stack, shared breaker across clones) and the `tower` module doctest | unit/doctest |
