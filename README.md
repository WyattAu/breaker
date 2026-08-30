# breaker

Async circuit breaker for Rust — state machine with configurable thresholds,
metrics, and optional [Tower](https://docs.rs/tower) layer integration.

[![CI](https://github.com/WyattAu/breaker/actions/workflows/ci.yml/badge.svg)](https://github.com/WyattAu/breaker/actions)
[![crates.io](https://img.shields.io/crates/v/breaker)](https://crates.io/crates/breaker)
[![license](https://img.shields.io/crates/l/breaker)](LICENSE-MIT)

## Features

- Three-state machine: **Closed → Open → HalfOpen → Closed**
- Configurable failure threshold, sliding window, wait duration
- Builder with `.standard()`, `.fast_fail()`, `.lenient()` presets
- Per-call metrics: failure rate, state, transition count
- Optional Tower `Layer` for Axum / Tonic integration (`tower` feature)

## State Machine

```text
┌────────┐  failure threshold  ┌──────┐  wait duration  ┌──────────┐
│ Closed │ ──────────────────> │ Open │ ─────────────> │ HalfOpen │
└────────┘                     └──────┘                 └──────────┘
     ^                          failure                     │  │
     └──────────────────────────────────────────────────────┘  │
                         success ──────────────────────────────┘
```

## Quick Start

```rust
use breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError};

#[tokio::main]
async fn main() -> Result<(), CircuitBreakerError> {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());

    cb.call(|| async {
        reqwest::get("https://api.example.com/data")
            .await?
            .error_for_status()?
            .json()
            .await
    })
    .await?;

    let m = cb.metrics();
    println!("state: {:?}, failures: {}", m.state, m.total_failures);

    Ok(())
}
```

## Presets

| Preset       | Threshold | Wait   | Half-Open |
|--------------|-----------|--------|-----------|
| `standard()` | 5         | 30 s   | 3         |
| `fast_fail()`| 1         | 10 s   | 1         |
| `lenient()`  | 10        | 60 s   | 5         |

## Tower Integration

Enable the `tower` feature for Axum middleware:

```rust,ignore
use breaker::{CircuitBreaker, CircuitBreakerConfig};
use axum::{Router, routing::get};

let cb = CircuitBreaker::new(CircuitBreakerConfig::standard());

let app = Router::new()
    .route("/", get(handler))
    .layer(CircuitBreakerLayer::new(cb));
```

## Comparison with tower-resilience

|                    | breaker                       | tower-resilience             |
|--------------------|-------------------------------|------------------------------|
| Algorithm          | State machine + sliding window| State machine                |
| Metrics            | Built-in `CircuitMetrics`     | None                         |
| Presets            | `.standard() / .fast_fail()`  | Manual config only           |
| Tower layer        | Optional                      | Required                     |

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)
at your option.
