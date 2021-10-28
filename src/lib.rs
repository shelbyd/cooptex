use std::sync::atomic::AtomicUsize;
use std::sync::{PoisonError, TryLockError};

#[cfg(feature = "loom")]
pub(crate) use loom::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard as StdMutexGuard},
    thread, thread_local,
};

#[cfg(not(feature = "loom"))]
pub(crate) use std::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard as StdMutexGuard},
    thread, thread_local,
};

static THREAD_ID: AtomicUsize = AtomicUsize::new(0);

thread_local!(
    static THIS_SCOPE: LockScope =
        LockScope::new(THREAD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
);

pub struct CoopMutex<T> {
    native: Mutex<T>,
    held_waiter: Mutex<(Option<usize>, Option<usize>)>,
    main_waiter: Condvar,
}

impl<T> CoopMutex<T> {
    pub fn new(item: T) -> Self {
        CoopMutex {
            native: Mutex::new(item),
            held_waiter: Mutex::new((None, None)),
            main_waiter: Condvar::new(),
        }
    }

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
                    (_, Some(waiter)) if self.id > *waiter => return Err(Retry),
                    _ if Arc::strong_count(&self.lock_count) == 1 => {
                        lock = mutex.main_waiter.wait(lock).unwrap();
                    }
                    // Already held by a more important thread, we should drop all our locks.
                    (Some(holder), _) if self.id > *holder => return Err(Retry),

                    // We become the primary waiter.
                    (_, waiter) => {
                        *waiter = Some(self.id);
                        mutex.main_waiter.notify_all();
                        lock = mutex.main_waiter.wait(lock).unwrap();
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
        }
    }

    fn new(id: usize) -> LockScope {
        LockScope {
            id,
            lock_count: Arc::new(()),
        }
    }
}

pub struct MutexGuard<'m, T> {
    native: StdMutexGuard<'m, T>,
    mutex: &'m CoopMutex<T>,
    _lock_count: Arc<()>,
}

impl<'m, T> Drop for MutexGuard<'m, T> {
    fn drop(&mut self) {
        self.mutex.held_waiter.lock().unwrap().0 = None;
        self.mutex.main_waiter.notify_all();
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
#[derive(Debug, PartialEq, Eq)]
pub struct Retry;

pub fn retry_loop<T, F: FnMut() -> Result<T, Retry>>(mut f: F) -> T {
    loop {
        match f() {
            Ok(t) => return t,
            Err(Retry) => {
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

            assert_eq!(s2.lock(&a).err(), Some(Retry));

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
