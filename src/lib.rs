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
//! The crate is still in early development, so there may be cases that aren't covered. Please open
//! an issue if you can reproduce a deadlock.
//!
//! ## Non-Guarantees
//!
//! This crate allows the following potentially undesired behavior:
//!
//! - [`CoopMutex::lock`] may return [`Retry`] when it could wait and acquire the lock without
//! a deadlock.
//! - [`CoopMutex::lock`] may wait arbitrarily long before returning [`Retry`].
//! - [`MutexGuard`] is not [`Send`], as we haven't fully evaluated the implications of sending
//! `MutexGuard`s.
//! - The current "Scheduler" for which thread has priority over locks is not fair.
//! - [`retry_loop`] will spin-lock the calling thread until it can make progress.

use std::sync::atomic::AtomicUsize;
use std::sync::{PoisonError, TryLockError};

#[cfg(feature = "loom")]
use loom::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard as StdMutexGuard},
    thread, thread_local,
};

#[cfg(not(feature = "loom"))]
use std::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard as StdMutexGuard},
    thread, thread_local,
};

static THREAD_ID: AtomicUsize = AtomicUsize::new(0);

thread_local!(
    static THIS_SCOPE: LockScope =
        LockScope::new(THREAD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
);

/// A deadlock-free version of [`Mutex`](std::sync::Mutex).
///
/// This is only deadlock-free if:
/// 1. All Mutexes that may deadlock are `CoopMutex`es
/// 2. When [`Retry`] is returned, the requesting thread drops all other [`MutexGuard`]s it is holding.
/// Easily accomplished with [`retry_loop`].
pub struct CoopMutex<T> {
    native: Mutex<T>,
    held_waiter: Mutex<(Option<usize>, Option<usize>)>,
    waiters: Condvar,
}

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
    pub fn lock(&self) -> Result<LockResult<MutexGuard<T>>, Retry> {
        THIS_SCOPE.with(|scope| scope.lock(self))
    }
}

struct LockScope {
    id: usize,
    lock_count: Arc<()>,
}

impl LockScope {
    fn lock<'m, T>(&self, mutex: &'m CoopMutex<T>) -> Result<LockResult<MutexGuard<'m, T>>, Retry> {
        self.lock_native(mutex).map(|result| match result {
            Ok(g) => Ok(self.guard(g, mutex)),
            Err(p) => Err(PoisonError::new(self.guard(p.into_inner(), mutex))),
        })
    }

    fn lock_native<'m, T>(
        &self,
        mutex: &'m CoopMutex<T>,
    ) -> Result<LockResult<StdMutexGuard<'m, T>>, Retry> {
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

                    // A more important thread is waiting, we should drop all our locks.
                    (_, Some(waiter)) if self.id > *waiter => return Err(Retry::default()),
                    _ if Arc::strong_count(&self.lock_count) == 1 => {
                        lock = mutex.waiters.wait(lock).unwrap();
                    }
                    // Already held by a more important thread, we should drop all our locks.
                    (Some(holder), _) if self.id > *holder => return Err(Retry::default()),

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

    fn guard<'m, T>(
        &self,
        native: StdMutexGuard<'m, T>,
        mutex: &'m CoopMutex<T>,
    ) -> MutexGuard<'m, T> {
        mutex.held_waiter.lock().unwrap().0 = Some(self.id);
        MutexGuard {
            native,
            mutex,
            _lock_count: Arc::clone(&self.lock_count),
            _not_send: core::marker::PhantomData,
        }
    }

    fn new(id: usize) -> LockScope {
        LockScope {
            id,
            lock_count: Arc::new(()),
        }
    }
}

/// A guard for a [`CoopMutex`]. Access the underlying data through [`Deref`](core::ops::Deref) and
/// [`DerefMut`](core::ops::DerefMut).
pub struct MutexGuard<'m, T> {
    native: StdMutexGuard<'m, T>,
    mutex: &'m CoopMutex<T>,
    _lock_count: Arc<()>,
    _not_send: core::marker::PhantomData<std::rc::Rc<()>>,
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
#[derive(Debug, PartialEq, Eq, Default)]
pub struct Retry {
    _disallow_matching: std::marker::PhantomData<()>,
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
/// use cooptex::*;
/// let a = CoopMutex::new(42);
/// let b = CoopMutex::new(43);
///
/// fn use_locks(a: &mut usize, b: &mut usize) -> Result<usize, ()> {
///   *a += 1;
///   *b += 1;
///   Ok(*a + *b)
/// }
///
/// let result = retry_loop(|| {
///   let mut a_lock = a.lock()?.unwrap();
///   let mut b_lock = b.lock()?.unwrap();
///   Ok(use_locks(&mut a_lock, &mut b_lock))
/// });
///
/// assert_eq!(result, Ok(87));
/// ```
pub fn retry_loop<T, F: FnMut() -> Result<T, Retry>>(mut f: F) -> T {
    loop {
        match f() {
            Ok(t) => return t,
            Err(Retry { .. }) => {
                thread::yield_now();
            }
        }
    }
}

#[cfg(all(test, not(feature = "loom")))]
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

            assert_eq!(s2.lock(&a).err(), Some(Retry::default()));

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

#[cfg(all(test, feature = "loom"))]
mod loom_tests {
    use super::*;

    use loom::{self, sync::Arc};

    #[test]
    #[ignore]
    // Ignored because of https://github.com/tokio-rs/loom/issues/173.
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
