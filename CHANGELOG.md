Changelog](https://keepachangelog.com/); versions follow [semver](https://semver.org).
- `tests/config_matrix.rs`; per-knob behavior matrix for every 2.0.0
  breaker. Typed error mapping (`CircuitBreakerError<S::Error>`; no
  `tower = []` stub with README/CHANGELOG claims; that is now true.
- **Half-open probe permits**: `half_open_max_calls` is enforced; at most
  Non-cryptographic thread-local xorshift jitter source; no new deps.
- **Failure predicate**: `failure_predicate` on the config builder; only
- `CircuitBreakerError::Inner(msg)` → `CircuitBreakerError::Failure(e)` -
- `cargo semver-checks` will fail against the 1.0.0 baseline; expected