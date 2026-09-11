//! Tower integration: [`BreakerLayer`] wraps any [`::tower::Service`] in a
//! [`CircuitBreaker`].
//!
//! # Design notes
//!
//! - **Typed, allocation-free error mapping.** The service error is
//!   [`CircuitBreakerError<S::Error>`] directly — no `BoxError`, no string
//!   formatting, no allocation on the allowed (closed-circuit) path. The
//!   original inner error is preserved verbatim inside
//!   [`CircuitBreakerError::Failure`].
//! - **One boxed future per request.** `BreakerService::call` boxes its
//!   response future (`Pin<Box<dyn Future + Send>>`), which is the standard
//!   trade-off across the Tower middleware ecosystem (it keeps `Service::
//!   Future` nameable and `'static`, so the layer composes in real
//!   `ServiceBuilder` stacks). The allocation is one small box on an async
//!   request path that already allocates a response; the core
//!   [`CircuitBreaker::call`](crate::CircuitBreaker::call) API remains
//!   allocation-free for hot paths that need it.
//!
//! # Example
//!
//! ```
//! use breaker::{CircuitBreaker, CircuitBreakerConfig, tower::BreakerLayer};
//! use ::tower::{Service, ServiceBuilder, ServiceExt, service_fn};
//!
//! # fn main() -> Result<(), ::tower::BoxError> {
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let breaker = CircuitBreaker::new(CircuitBreakerConfig::standard());
//!
//! let mut svc = ServiceBuilder::new()
//!     .layer(BreakerLayer::new(breaker))
//!     .service(service_fn(|req: String| async move {
//!         Ok::<_, std::convert::Infallible>(format!("hello {req}"))
//!     }));
//!
//! // `ready()` is required by the Tower contract before each `call`.
//! let svc = svc.ready().await?;
//! let response = svc.call("world".to_string()).await?;
//! assert_eq!(response, "hello world");
//! # Ok::<_, ::tower::BoxError>(())
//! # })?;
//! # Ok(())
//! # }
//! ```
//!
//! # Axum
//!
//! `BreakerLayer` implements [`tower_layer::Layer`], so it drops straight
//! into an Axum router:
//!
//! ```ignore
//! use axum::{routing::get, Router};
//! use breaker::{CircuitBreaker, CircuitBreakerConfig, tower::BreakerLayer};
//!
//! let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());
//! let app = Router::new()
//!     .route("/", get(handler))
//!     .layer(BreakerLayer::new(cb));
//! ```
//!
//! Note that Axum handlers return `Result<Response, E>`, which is exactly
//! the shape [`CircuitBreaker::call`](crate::CircuitBreaker::call) wraps;
//! the breaker error surfaces as the handler error (mapped through
//! [`CircuitBreakerError`]).

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use ::tower::Service;
use ::tower_layer::Layer;

use crate::{CircuitBreaker, CircuitBreakerError};

/// A [`Layer`] that installs a
/// [`CircuitBreaker`] in front of a service.
///
/// All clones of the produced [`BreakerService`] share the breaker: every
/// request through the layer counts toward the same circuit.
#[derive(Debug, Clone)]
pub struct BreakerLayer {
    breaker: CircuitBreaker,
}

impl BreakerLayer {
    /// Wrap requests to the wrapped service in the given breaker.
    pub fn new(breaker: CircuitBreaker) -> Self {
        Self { breaker }
    }
}

impl<S> Layer<S> for BreakerLayer {
    type Service = BreakerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BreakerService {
            breaker: self.breaker.clone(),
            inner,
        }
    }
}

/// The service produced by [`BreakerLayer`]: delegates to the inner service
/// through [`CircuitBreaker::call`].
#[derive(Debug, Clone)]
pub struct BreakerService<S> {
    breaker: CircuitBreaker,
    inner: S,
}

impl<S, Request> Service<Request> for BreakerService<S>
where
    S: Service<Request>,
    // `Send + 'static` for the boxed response future; `Send` also makes the
    // typed `CircuitBreakerError<S::Error>` `Send` so it can cross threads.
    S::Error: Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = CircuitBreakerError<S::Error>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready(cx)
            .map_err(CircuitBreakerError::Failure)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let breaker = self.breaker.clone();
        let fut = self.inner.call(req);
        Box::pin(async move { breaker.call(move || fut).await })
    }
}
