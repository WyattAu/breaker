/// Errors returned by [`CircuitBreaker::call`](crate::CircuitBreaker::call).
///
/// Generic over the operation's error type `E`: the original error value is
/// preserved verbatim in [`CircuitBreakerError::Failure`] — 2.0.0 no longer
/// stringifies user errors (the 1.x `Inner(Cow<'static, str>)` erasure is
/// gone). `E` no longer needs to implement `Display` (or anything at all
/// beyond `'static`).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CircuitBreakerError<E> {
    /// The circuit is `Open` (and its backoff wait has not yet elapsed) —
    /// the call was rejected before the operation ran.
    #[error("circuit breaker is open")]
    CircuitOpen,

    /// The circuit is `HalfOpen` and all probe permits are in use — the
    /// call was rejected by half-open admission control (stampede
    /// protection). This does **not** count as a failure.
    #[error("circuit breaker: half-open probe capacity exhausted")]
    Rejected,

    /// The wrapped operation returned an error. Whether the breaker counts
    /// it toward tripping is decided by the configured failure predicate
    /// (all errors count when no predicate is configured); the original
    /// error value is preserved either way.
    #[error(transparent)]
    Failure(#[from] E),

    /// The operation exceeded the configured call timeout (see
    /// [`CircuitBreakerConfig`](crate::CircuitBreakerConfig)) and was
    /// recorded as a failure. Only available with the `timeout` feature.
    #[cfg(feature = "timeout")]
    #[error("circuit breaker: operation timed out")]
    Timeout,
}
