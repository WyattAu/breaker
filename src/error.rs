/// Errors returned by [`CircuitBreaker::call`](crate::CircuitBreaker::call).
#[derive(Debug, thiserror::Error)]
pub enum CircuitBreakerError {
    /// The circuit is open and rejecting requests.
    #[error("circuit breaker is open")]
    CircuitOpen,

    /// The wrapped operation failed (mapped from the inner error).
    #[error("circuit breaker: operation timed out or failed")]
    Timeout,

    /// The inner operation returned an error. `Cow` avoids an allocation
    /// when the error is a static string (e.g. `"boom"` in tests).
    #[error("circuit breaker: inner error: {0}")]
    Inner(std::borrow::Cow<'static, str>),
}
