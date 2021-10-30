#![warn(missing_docs)]

//! cooptex provides deadlock-free Mutexes. The [`CoopMutex::lock`] method wraps the
//! [`std::sync::Mutex`] return value with a `Result` that will request the caller to drop other held
//! locks so another thread could make progress. This behavior is easily accomplished by using the
//! [`retry_loop`] function.
//!
//! ```
//! use cooptex::*;
//! let a = CoopMutex::new(42);
//! let b = CoopMutex::new(43);
//!
//! retry_loop(|| {
//!   let a_lock = a.lock()?.unwrap();
//!   let b_lock = b.lock()?.unwrap();
//!   assert_eq!(*a_lock + *b_lock, 85);
//!   Ok(())
//! });
//! ```
//!
//! # Guarantees
//!
//! This crate aims to guarantee that multiple threads cannot possibly deadlock by acquiring
//! locks.
//!
//! This crate also will prefer threads that have lived the "longest" without completing work.
//! Meaning, when [`retry_loop`] successfully completes, it will move that thread to the end of the
//! queue for acquiring future locks. This provides an approximately fair scheduler.
//!
//! The crate is still in early development, so there may be cases that aren't covered. Please open
//! an issue if you can reproduce a deadlock.
//!
//! ## Non-Guarantees
//!
//! This crate explicitly allows the following potentially undesired behavior:
//!
//! - [`CoopMutex::lock`] may return [`Retry`] when it could wait and acquire the lock without
//! a deadlock.
//! - [`CoopMutex::lock`] may wait arbitrarily long before returning [`Retry`].
//!
//! ## Incomplete
//!
//! - We have not fully analyzed the behavior during panics. There is no `unsafe` code, so we could
//! only possibly deadlock.

use std::cell::RefCell;
use std::sync::atomic::AtomicUsize;
use std::sync::{PoisonError, TryLockError};

#[cfg(feature = "loom-tests")]
use loom::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard as StdMutexGuard},
    thread_local,
};

#[cfg(not(feature = "loom-tests"))]
use std::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard as StdMutexGuard},
    thread_local,
};

static THREAD_ID: AtomicUsize = AtomicUsize::new(0);

thread_local!(
    static THIS_SCOPE: RefCell<LockScope> = RefCell::new(LockScope::new(
        THREAD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
);

/// A deadlock-free version of [`Mutex`](std::sync::Mutex).
///
/// This is only deadlock-free if:
/// 1. All Mutexes that may deadlock are `CoopMutex`es
/// 2. When [`Retry`] is returned, the requesting thread drops all other [`MutexGuard`]s it is holding.
/// Easily accomplished with [`retry_loop`].
#[derive(Default)]
pub struct CoopMutex<T> {
    native: Mutex<T>,
    held_waiter: Mutex<HeldWaiter>,
    waiters: Condvar,
}

type HeldWaiter = (Option<usize>, Option<usize>);

impl<T> CoopMutex<T> {
    /// Create a new `CoopMutex` holding the provided `item`.
    pub fn new(item: T) -> Self {
        CoopMutex {
            native: Mutex::new(item),
            held_waiter: Mutex::new((None, None)),
            waiters: Condvar::new(),
        }
    }

    /// Acquire a mutex or return `Err(Retry)`, indicating that the current thread should drop all
    /// its currently held locks and try to acquire them again. Use [`retry_loop`] to automatically
    /// use the correct behavior.
    ///
    /// # Panics
    ///
    /// Panics when a thread attempts to acquire a lock it is already holding.
    pub fn lock(&self) -> Result<LockResult<MutexGuard<T>>, Retry> {
        THIS_SCOPE.with(|scope| scope.borrow().lock(self))
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// See [`std::sync::Mutex`] for more details of implications.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.native.get_mut()
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for CoopMutex<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        self.native.fmt(f)
    }
}

impl<T> From<T> for CoopMutex<T> {
    fn from(t: T) -> Self {
        CoopMutex::new(t)
    }
}

struct LockScope {
    id: usize,
    // TODO(shelbyd): This could be Rc<()> instead of Arc. Only internal tests would need to
    // change.
    lock_count: Arc<()>,
}

impl LockScope {
    fn lock<'m, T>(
        &self,
        mutex: &'m CoopMutex<T>,
    ) -> Result<LockResult<MutexGuard<'m, T>>, Retry<'m>> {
        self.lock_native(mutex).map(|result| match result {
            Ok(g) => Ok(self.guard(g, mutex)),
            Err(p) => Err(PoisonError::new(self.guard(p.into_inner(), mutex))),
        })
    }

    fn lock_native<'m, T>(
        &self,
        mutex: &'m CoopMutex<T>,
    ) -> Result<LockResult<StdMutexGuard<'m, T>>, Retry<'m>> {
        loop {
            match mutex.native.try_lock() {
                Ok(g) => return Ok(Ok(g)),
                Err(TryLockError::Poisoned(p)) => return Ok(Err(p)),
                Err(TryLockError::WouldBlock) => {}
            }

            let mut lock = mutex.held_waiter.lock().unwrap();
            loop {
                match &mut *lock {
                    // No one is holding the lock, retry the try_lock above.
                    (None, _) => break,
                    (Some(holder), _) if self.id == *holder => {
                        unreachable!("Already holding this lock")
                    }
                    (Some(holder), Some(waiter)) if holder == waiter => {
                        unreachable!("Held and waited by same thread")
                    }

                    // A more important thread is waiting, we should drop all our locks.
                    (_, Some(waiter)) if self.id > *waiter => return Err(self.retry(mutex)),
                    _ if Arc::strong_count(&self.lock_count) == 1 => {
                        lock = mutex.waiters.wait(lock).unwrap();
                    }
                    // Already held by a more important thread, we should drop all our locks.
                    (Some(holder), _) if self.id > *holder => return Err(self.retry(mutex)),

                    // We become the primary waiter.
                    (_, waiter) => {
                        *waiter = Some(self.id);
                        mutex.waiters.notify_all();
                        lock = mutex.waiters.wait(lock).unwrap();
                    }
                }
            }
        }
    }

    fn retry<'m, T>(&self, mutex: &'m CoopMutex<T>) -> Retry<'m> {
        Retry {
            waiters: &mutex.waiters,
            mutex: &mutex.held_waiter,
        }
    }

    fn guard<'m, T>(
        &self,
        native: StdMutexGuard<'m, T>,
        mutex: &'m CoopMutex<T>,
    ) -> MutexGuard<'m, T> {
        let mut held_waiter = mutex.held_waiter.lock().unwrap();
        held_waiter.0 = Some(self.id);
        held_waiter.1 = None;

        MutexGuard {
            native,
            mutex,
            _lock_count: Arc::clone(&self.lock_count),
        }
    }

    fn new(id: usize) -> LockScope {
        LockScope {
            id,
            lock_count: Arc::new(()),
        }
    }

    fn update_id_for_fairness(&mut self) {
        if Arc::strong_count(&self.lock_count) == 1 {
            self.id = THREAD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// A guard for a [`CoopMutex`]. Access the underlying data through [`Deref`](core::ops::Deref) and
/// [`DerefMut`](core::ops::DerefMut).
pub struct MutexGuard<'m, T> {
    native: StdMutexGuard<'m, T>,
    mutex: &'m CoopMutex<T>,
    _lock_count: Arc<()>,
}

impl<'m, T> Drop for MutexGuard<'m, T> {
    fn drop(&mut self) {
        self.mutex.held_waiter.lock().unwrap().0 = None;
        self.mutex.waiters.notify_all();
    }
}

impl<'m, T> core::ops::Deref for MutexGuard<'m, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.native
    }
}

impl<'m, T> core::ops::DerefMut for MutexGuard<'m, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.native
    }
}

// TODO(shelbyd): Should be enum with From implementations.
// Blocked on https://doc.rust-lang.org/std/ops/trait.Try.html being nightly-only.
/// Marker struct indicating that a thread requesting a [`CoopMutex`] should drop all its currently
/// held [`MutexGuard`]s and attempt to reacquire them.
///
/// Use [`retry_loop`] to get the correct behavior.
#[derive(Debug)]
pub struct Retry<'m> {
    mutex: &'m Mutex<HeldWaiter>,
    waiters: &'m Condvar,
}

impl<'m> Retry<'m> {
    fn wait(self) {
        let mut lock = self.mutex.lock().unwrap();
        while let Some(_) = lock.0 {
            lock = self.waiters.wait(lock).unwrap();
        }
    }
}

/// Helper function for implementing the behavior of dropping held [`MutexGuard`]s when a
/// [`CoopMutex::lock`] call returns [`Retry`].
///
/// You should use the early return operator `?` to raise any [`Retry`] errors. While the
/// [`std::ops::Try`] trait is unstable, we can't allow using the early return operator for
/// returning as normal user code would. We recommend having one function that acquires all
/// relevant locks, and another that uses them.
///
/// ```
/// fn use_locks(a: &mut usize, b: &mut usize) -> Result<usize, ()> {
///   *a += 1;
///   *b += 1;
///   Ok(*a + *b)
/// }
///
/// use cooptex::*;
/// let a = CoopMutex::new(42);
/// let b = CoopMutex::new(43);
///
/// let result = retry_loop(|| {
///   let mut a_lock = a.lock()?.unwrap();
///   let mut b_lock = b.lock()?.unwrap();
///   Ok(use_locks(&mut a_lock, &mut b_lock))
/// });
///
/// assert_eq!(result, Ok(87));
/// ```
pub fn retry_loop<'m, T, F: FnMut() -> Result<T, Retry<'m>>>(mut f: F) -> T {
    loop {
        match f() {
            Ok(t) => {
                THIS_SCOPE.with(|s| s.borrow_mut().update_id_for_fairness());
                return t;
            }
            Err(retry) => retry.wait(),
        }
    }
}

#[cfg(all(test, not(feature = "loom-tests")))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn second_thread_retries() {
        let a = CoopMutex::new(42);
        let b = CoopMutex::new(43);

        let s1 = LockScope::new(0);
        let s2 = LockScope::new(1);

        crossbeam::thread::scope(|s| {
            let x1 = s1.lock(&a).unwrap();
            let x2 = s2.lock(&b).unwrap();

            s.spawn(|_| {
                let _ = s1.lock(&b).unwrap();
            });

            assert!(s2.lock(&a).is_err());

            drop((x1, x2));
        })
        .unwrap();
    }

    #[test]
    fn first_thread_blocks() {
        let mutex = CoopMutex::new(42);

        let s1 = LockScope::new(0);
        let s2 = LockScope::new(1);

        crossbeam::thread::scope(|s| {
            let lock = s2.lock(&mutex).unwrap();

            s.spawn(|_| {
                assert_eq!(*s1.lock(&mutex).unwrap().unwrap(), 42);
            });

            std::thread::sleep(Duration::from_millis(100));
            drop(lock);
        })
        .unwrap();
    }

    #[test]
    fn second_waits_if_not_holding_other_locks() {
        let mutex = CoopMutex::new(42);

        let s1 = LockScope::new(0);
        let s2 = LockScope::new(1);

        crossbeam::thread::scope(|s| {
            s.spawn(|_| {
                let lock = s1.lock(&mutex);
                std::thread::sleep(Duration::from_millis(100));
                drop(lock);
            });

            std::thread::sleep(Duration::from_millis(10));
            assert_eq!(*s2.lock(&mutex).unwrap().unwrap(), 42);
        })
        .unwrap();
    }
}

#[cfg(all(test, feature = "loom-tests"))]
mod loom_tests {
    use super::*;

    use loom::{self, sync::Arc};

    #[test]
    #[ignore]
    // Ignored because our "spin-lock" overrides the maximum number of paths for loom.
    fn loom_deadlock() {
        loom::model(|| {
            let a = Arc::new(CoopMutex::new(42));
            let b = Arc::new(CoopMutex::new(43));

            let t1 = {
                let a = a.clone();
                let b = b.clone();
                loom::thread::spawn(move || {
                    retry_loop(|| {
                        let a = a.lock()?.unwrap();
                        let mut b = b.lock()?.unwrap();
                        *b += *a;
                        Ok(())
                    });
                })
            };

            let t2 = {
                let a = a.clone();
                let b = b.clone();
                loom::thread::spawn(move || {
                    retry_loop(|| {
                        let b = b.lock()?.unwrap();
                        let mut a = a.lock()?.unwrap();
                        *a += *b;
                        Ok(())
                    });
                })
            };

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
