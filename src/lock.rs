//! Internal lock shim.
//!
//! Production uses `parking_lot::RwLock` (guards returned directly). Under
//! `cfg(loom)` — see `tests/loom.rs` — the lock is swapped for loom's, whose
//! `read`/`write` return `Result` like `std`. This wrapper normalizes both to
//! the parking_lot shape so the state machine call sites stay unchanged.

#[cfg(not(loom))]
pub(crate) use parking_lot::RwLock;

#[cfg(loom)]
pub(crate) use loom_lock::RwLock;

#[cfg(loom)]
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
