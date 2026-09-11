//! Axum-ready Tower middleware: wrap any `tower::Service` (or Axum router)
//! in a circuit breaker with [`BreakerLayer`].
//!
//! Run with:
//!
//! ```sh
//! cargo run --example tower_middleware --features tower
//! ```
//!
//! The runnable part below uses `tower::service_fn` so the example's
//! dependency footprint stays light; the identical layer drops into Axum:
//!
//! ```ignore
//! use axum::{routing::get, Router};
//!
//! let app = Router::new()
//!     .route("/", get(handler))
//!     .layer(BreakerLayer::new(breaker));
//! ```

// Example code: unwrap on runtime construction is the demo signal, not a
// production path (clippy lints apply to examples under --all-targets).
#![allow(clippy::unwrap_used)]

#[cfg(feature = "tower")]
fn main() -> Result<(), tower::BoxError> {
    use breaker::tower::BreakerLayer;
    use breaker::{CircuitBreaker, CircuitBreakerConfig};
    use std::time::Duration;
    use tower::{Service, ServiceBuilder, ServiceExt, service_fn};

    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        // Exponential backoff with full jitter: each re-trip waits
        // 10ms * 2^n (capped at 1s), uniformly jittered across [0, wait]
        // so many instances don't retry in lockstep.
        let config = CircuitBreakerConfig::builder()
            .consecutive_failures(3)
            .failure_rate_threshold(0.5)
            .sliding_window_size(10)
            .backoff(breaker::BackoffStrategy::ExponentialJitter {
                initial: Duration::from_millis(10),
                max: Duration::from_secs(1),
                factor: 2.0,
                jitter: 1.0,
            })
            .half_open_max_calls(2)
            .build();
        let breaker = CircuitBreaker::builder(config)
            .name("downstream-api")
            .build();

        // Stand-in for an HTTP client / DB pool / gRPC channel.
        let mut svc = ServiceBuilder::new()
            .layer(BreakerLayer::new(breaker))
            .service(service_fn(|req: String| async move {
                if req == "fail" {
                    Err("downstream error")
                } else {
                    Ok(format!("200 OK: {req}"))
                }
            }));

        for req in ["ping", "ping", "fail", "fail", "fail", "ping"] {
            // Tower contract: poll readiness before each call.
            let ready = match svc.ready().await {
                Ok(svc) => svc,
                Err(e) => {
                    println!("{req} -> not ready: {e}");
                    continue;
                }
            };
            match ready.call(req.to_string()).await {
                Ok(res) => println!("{req} -> {res}"),
                Err(breaker::CircuitBreakerError::CircuitOpen) => {
                    println!("{req} -> 503 circuit open (backing off)")
                }
                Err(breaker::CircuitBreakerError::Rejected) => {
                    println!("{req} -> 503 half-open probe capacity exhausted")
                }
                Err(breaker::CircuitBreakerError::Failure(e)) => {
                    println!("{req} -> 502 downstream error: {e}")
                }
                Err(other) => println!("{req} -> 500: {other}"),
            }
        }

        Ok(())
    })
}

#[cfg(not(feature = "tower"))]
fn main() {
    eprintln!("This example requires the `tower` feature:");
    eprintln!("  cargo run --example tower_middleware --features tower");
}
