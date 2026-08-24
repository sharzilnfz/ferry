//! Poison-tolerant mutex locking (T-04).
//!
//! The state behind these mutexes (route table, path observations, stream
//! slots) tolerates recovery after a panic in another thread, so a poisoned
//! mutex must not cascade into a daemon-wide crash.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock `m`, recovering the guard even if the mutex was poisoned.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
