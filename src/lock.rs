//! Internal lock shim.
//!
//! Production uses `parking_lot::RwLock` (guards returned directly). Under
//! `cfg(loom)` — see `tests/loom.rs` — the lock is swapped for loom's, whose
//! `read`/`write` return `Result` like `std`. This wrapper normalizes both to
//! the parking_lot shape so the state machine call sites stay unchanged.

#[cfg(all(not(loom), not(kani)))]
pub(crate) use parking_lot::RwLock;

#[cfg(loom)]
pub(crate) use loom_lock::RwLock;

// The half-open probe-permit counter (src/lib.rs) is shared across threads
// and mutated without the state lock, so it must be a modelable atomic:
// std's AtomicUsize under loom would introduce unmodeled nondeterminism.
// Both flavors expose the same load/store/compare_exchange/fetch_sub API
// with their own `Ordering` type, imported from the same branch below.
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(loom))]
pub(crate) use std::sync::atomic::{AtomicUsize, Ordering};

// Harness-only module (loom model checking): `expect` documents the
// poisoning invariant the model is proving; never compiled for release.
#[cfg(loom)]
#[allow(clippy::expect_used)]
mod loom_lock {
    use loom::sync::{RwLock as LoomRwLock, RwLockReadGuard, RwLockWriteGuard};

    pub(crate) struct RwLock<T>(LoomRwLock<T>);

    impl<T> RwLock<T> {
        pub(crate) fn new(value: T) -> Self {
            Self(LoomRwLock::new(value))
        }

        pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
            self.0.read().expect("rwlock read poisoned")
        }

        pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T> {
            self.0.write().expect("rwlock write poisoned")
        }
    }
}

// Under cfg(kani) — see src/kani.rs — the lock is swapped for std's: Kani
// models std sync primitives natively, while parking_lot's futex/spin
// internals explode symbolic execution (observed: >15 GB RSS, no SAT
// result even on a 3-step harness). Same guard-shape normalization as the
// loom swap above; never active together with cfg(loom).
#[cfg(kani)]
pub(crate) use kani_lock::RwLock;

// Harness-only module (Kani symbolic execution): `expect` documents the
// poisoning invariant; never compiled for release.
#[cfg(kani)]
#[allow(clippy::expect_used)]
mod kani_lock {
    use std::sync::{RwLock as StdRwLock, RwLockReadGuard, RwLockWriteGuard};

    pub(crate) struct RwLock<T>(StdRwLock<T>);

    impl<T> RwLock<T> {
        pub(crate) fn new(value: T) -> Self {
            Self(StdRwLock::new(value))
        }

        pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
            self.0.read().expect("rwlock read poisoned")
        }

        pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T> {
            self.0.write().expect("rwlock write poisoned")
        }
    }
}
