// Tests exercise failure paths directly; unwrap/expect, slicing, and
// panicking asserts are the test signal here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Tower integration tests for [`BreakerLayer`] / [`BreakerService`]
//! (feature: `tower`).

#![cfg(feature = "tower")]

use breaker::tower::{BreakerLayer, BreakerService};
use breaker::{BackoffStrategy, CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError};
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tower::{Layer, Service, ServiceBuilder, ServiceExt, service_fn};

fn test_breaker() -> CircuitBreaker {
    CircuitBreaker::new(
        CircuitBreakerConfig::builder()
            .consecutive_failures(2)
            .failure_rate_threshold(0.0)
            .backoff(BackoffStrategy::Fixed(Duration::from_secs(60)))
            .build(),
    )
}

#[tokio::test]
async fn layer_passes_requests_through_when_closed() {
    let mut svc: BreakerService<_> =
        BreakerLayer::new(test_breaker()).layer(service_fn(|req: &'static str| async move {
            Ok::<_, Infallible>(format!("pong: {req}"))
        }));

    let svc = svc.ready().await.unwrap();
    let res = svc.call("hello").await.unwrap();
    assert_eq!(res, "pong: hello");
}

#[tokio::test]
async fn layer_preserves_inner_error_typed() {
    #[derive(Debug, PartialEq)]
    struct DbError {
        code: i32,
    }

    let mut svc: BreakerService<_> =
        BreakerLayer::new(test_breaker()).layer(service_fn(|_req: ()| async move {
            Err::<String, _>(DbError { code: 42 })
        }));

    let svc = svc.ready().await.unwrap();
    match svc.call(()).await {
        Err(CircuitBreakerError::Failure(e)) => assert_eq!(e, DbError { code: 42 }),
        other => panic!("expected typed Failure, got {other:?}"),
    }
}

#[tokio::test]
async fn open_circuit_short_circuits_with_circuit_open() {
    let breaker = test_breaker();
    let calls = Arc::new(AtomicUsize::new(0));

    let mut svc: BreakerService<_> = BreakerLayer::new(breaker.clone()).layer(service_fn({
        let calls = calls.clone();
        move |_req: ()| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>("boom")
            }
        }
    }));

    // Two failures trip the breaker (threshold 2).
    let ready = svc.ready().await.unwrap();
    let _ = ready.call(()).await;
    let ready = svc.ready().await.unwrap();
    let _ = ready.call(()).await;
    assert!(breaker.is_open());

    // Third call: short-circuited before the inner service runs.
    let ready = svc.ready().await.unwrap();
    let res = ready.call(()).await;
    assert!(matches!(res, Err(CircuitBreakerError::CircuitOpen)));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "inner service not polled");
}

#[tokio::test]
async fn works_in_service_builder_stack() {
    let mut svc = ServiceBuilder::new()
        .layer(BreakerLayer::new(test_breaker()))
        .service(service_fn(|req: u32| async move {
            Ok::<_, Infallible>(req * 2)
        }));

    let svc = svc.ready().await.unwrap();
    assert_eq!(svc.call(21).await.unwrap(), 42);
}

#[tokio::test]
async fn layer_is_clonable_and_shares_the_breaker() {
    let breaker = test_breaker();
    let layer = BreakerLayer::new(breaker.clone());

    let mut a: BreakerService<_> = layer
        .clone()
        .layer(service_fn(|_req: ()| async move { Err::<(), _>("fail") }));
    let mut b: BreakerService<_> =
        layer.layer(service_fn(
            |req: ()| async move { Ok::<_, Infallible>(req) },
        ));

    // Fail through `a` twice → breaker trips; `b` must see the same circuit.
    let ready = a.ready().await.unwrap();
    let _ = ready.call(()).await;
    let ready = a.ready().await.unwrap();
    let _ = ready.call(()).await;
    assert!(breaker.is_open());

    let ready = b.ready().await.unwrap();
    let res = ready.call(()).await;
    assert!(matches!(res, Err(CircuitBreakerError::CircuitOpen)));
}

#[tokio::test]
async fn concurrent_requests_respect_probe_permits() {
    let config = CircuitBreakerConfig::builder()
        .consecutive_failures(1)
        .failure_rate_threshold(0.0)
        .half_open_max_calls(1)
        .success_threshold(100)
        .backoff(BackoffStrategy::Fixed(Duration::from_millis(10)))
        .build();
    let breaker = CircuitBreaker::new(config);
    breaker.record_failure(); // → Open
    tokio::time::sleep(Duration::from_millis(50)).await;

    let layer = BreakerLayer::new(breaker);
    let make_svc = || {
        layer.clone().layer(service_fn(|_req: ()| async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<(), Infallible>(())
        }))
    };

    // Request 1 goes first and holds the single probe permit mid-flight;
    // request 2 starts while it is guaranteed still in progress: exactly
    // one probe admitted.
    let mut s1 = make_svc();
    let f1 = tokio::spawn(async move {
        let s = s1.ready().await.unwrap();
        s.call(()).await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;

    let mut s2 = make_svc();
    let f2 = tokio::spawn(async move {
        let s = s2.ready().await.unwrap();
        s.call(()).await
    });

    let r1 = f1.await.unwrap();
    let r2 = f2.await.unwrap();
    assert!(r1.is_ok(), "first request admitted as the probe");
    assert!(
        matches!(r2, Err(CircuitBreakerError::Rejected)),
        "second request must be rejected: {r2:?}"
    );
}
