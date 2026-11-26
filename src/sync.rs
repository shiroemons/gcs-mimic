//! Acquires standard-library locks with an explicit poison policy.

use crate::diagnostics::{Event, LogLevel, emit};
use std::{
    panic::Location,
    sync::{LockResult, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

/// Use when the caller fails closed or must repair protected state before clearing poison.
#[expect(
    clippy::disallowed_methods,
    reason = "raw acquisition for callers that handle poison"
)]
pub(crate) fn lock_checked<T: ?Sized>(lock: &Mutex<T>) -> LockResult<MutexGuard<'_, T>> {
    lock.lock()
}

#[expect(
    clippy::disallowed_methods,
    reason = "raw read for callers that handle poison"
)]
pub(crate) fn read_checked<T: ?Sized>(lock: &RwLock<T>) -> LockResult<RwLockReadGuard<'_, T>> {
    lock.read()
}

#[expect(
    clippy::disallowed_methods,
    reason = "raw write for callers that handle poison"
)]
pub(crate) fn write_checked<T: ?Sized>(lock: &RwLock<T>) -> LockResult<RwLockWriteGuard<'_, T>> {
    lock.write()
}

/// Releases owned resources even after a panic while keeping poison visible to health checks.
pub(crate) fn lock_for_cleanup<T: ?Sized>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock_checked(lock).unwrap_or_else(PoisonError::into_inner)
}

/// Recovers a mutex whose contents remain usable after an unwind.
#[track_caller]
pub(crate) fn lock<T: ?Sized>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    match lock_checked(lock) {
        Ok(guard) => guard,
        Err(error) => {
            lock.clear_poison();
            report_recovery();
            error.into_inner()
        }
    }
}

#[track_caller]
pub(crate) fn read<T: ?Sized>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    match read_checked(lock) {
        Ok(guard) => guard,
        Err(error) => {
            // Clear and report under an exclusive guard so concurrent readers report once.
            drop(error.into_inner());
            RwLockWriteGuard::downgrade(write(lock))
        }
    }
}

#[track_caller]
pub(crate) fn write<T: ?Sized>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    match write_checked(lock) {
        Ok(guard) => guard,
        Err(error) => {
            lock.clear_poison();
            report_recovery();
            error.into_inner()
        }
    }
}

/// Called once after protected state is usable again, while still holding exclusive access.
#[track_caller]
pub(crate) fn report_recovery() {
    #[derive(serde::Serialize)]
    struct Recovered {
        file: &'static str,
        line: u32,
    }
    let location = Location::caller();
    emit(
        LogLevel::Warn,
        Event::LockPoisonRecovered,
        Recovered {
            file: location.file(),
            line: location.line(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::catch_unwind;

    #[test]
    fn mutex_cleanup_preserves_poison_until_recovery() {
        let value = Mutex::new(0);
        assert!(
            catch_unwind(|| {
                let mut guard = lock(&value);
                *guard = 1;
                panic!("poison mutex");
            })
            .is_err()
        );
        assert_eq!(*lock_for_cleanup(&value), 1);
        assert!(lock_checked(&value).is_err());
        assert_eq!(*lock(&value), 1);
        assert!(lock_checked(&value).is_ok());
    }

    #[test]
    fn rwlock_read_and_write_recover_exclusive_poison() {
        let value = RwLock::new(0);
        for expected in [1, 2] {
            assert!(
                catch_unwind(|| {
                    let mut guard = write(&value);
                    *guard = expected;
                    panic!("poison write lock");
                })
                .is_err()
            );
            assert!(read_checked(&value).is_err());
            if expected == 1 {
                assert_eq!(*read(&value), expected);
            } else {
                assert_eq!(*write(&value), expected);
            }
            assert!(!value.is_poisoned());
        }
    }
}
