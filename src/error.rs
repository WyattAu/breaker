/// Errors returned by [`CircuitBreaker::call`](crate::CircuitBreaker::call).
#[derive(Debug, thiserror::Error)]
pub enum CircuitBreakerError {
    /// The circuit is open and rejecting requests.
    #[error("circuit breaker is open")]
    CircuitOpen,

    /// The wrapped operation failed (mapped from the inner error).
    #[error("circuit breaker: operation timed out or failed")]
    Timeout,

    /// The inner operation returned an error.
    #[error("circuit breaker: inner error: {0}")]
    Inner(String),
}
