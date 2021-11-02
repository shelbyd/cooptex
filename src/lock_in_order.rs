//! Support for the [`lock`] function.
//!
//! Only public to remove compiler warnings.

#![allow(missing_docs)]

use crate::sync::{LockResult, Mutex, MutexGuard};

use frunk::{HCons, HNil};

/// Lock a list of [`Mutex`]es in a consistent order regardless of input order.
///
/// Locking mutexes in a known order makes deadlocks impossible, as long as all locks are acquired
/// through the order-preserving method.
///
/// This is `O(locks^2)`.
///
/// ```
/// use std::sync::Mutex;
/// use cooptex::{lock, lock_in_order::Unwrap};
/// use frunk::{hlist, hlist_pat};
///
/// let a = Mutex::new(1);
/// let b = Mutex::new(2);
///
/// let hlist_pat!(a, b) = lock(hlist!(&a, &b)).unwrap();
/// assert_eq!(*a + *b, 3);
/// ```
pub fn lock<L: LockSequence>(l: L) -> L::Output {
    l.lock_in_order()
}

fn mutex_ptr<T>(t: &Mutex<T>) -> *const () {
    t as *const Mutex<T> as *const ()
}

pub enum MaybeLocked<'m, T> {
    Locked(LockResult<MutexGuard<'m, T>>),
    NotLocked(&'m Mutex<T>),
}

impl<'m, T> MaybeLocked<'m, T> {
    fn bound(&self) -> Bound {
        match self {
            MaybeLocked::Locked(_) => Bound::None,
            MaybeLocked::NotLocked(m) => Bound::Before(mutex_ptr(m)),
        }
    }

    fn lock(self) -> Self {
        MaybeLocked::Locked(match self {
            MaybeLocked::Locked(l) => l,
            MaybeLocked::NotLocked(m) => m.lock(),
        })
    }

    fn lock_before(&self, bound: &Bound) -> bool {
        match (self, bound) {
            (MaybeLocked::Locked(_), _) => false,
            (_, Bound::None) => true,
            (MaybeLocked::NotLocked(m), Bound::Before(ptr)) => mutex_ptr(m) < *ptr,
        }
    }
}

#[derive(Debug)]
pub enum Bound {
    None,
    Before(*const ()),
}

pub trait LockSequence {
    type Output;
    type Maybe: LockOrder<Locked = Self::Output>;

    fn as_maybe(self) -> Self::Maybe;

    fn lock_in_order(self) -> Self::Output
    where
        Self: Sized,
    {
        self.as_maybe().lock_in_order(Bound::None).as_locked()
    }
}

impl LockSequence for HNil {
    type Output = HNil;
    type Maybe = HNil;

    fn as_maybe(self) -> Self::Maybe {
        HNil
    }
}

impl<'m, H, Tail> LockSequence for HCons<&'m Mutex<H>, Tail>
where
    Tail: LockSequence,
{
    type Output = HCons<LockResult<MutexGuard<'m, H>>, Tail::Output>;
    type Maybe = HCons<MaybeLocked<'m, H>, Tail::Maybe>;

    fn as_maybe(self) -> Self::Maybe {
        let (h, tail) = self.pop();
        HCons {
            head: MaybeLocked::NotLocked(h),
            tail: tail.as_maybe(),
        }
    }
}

pub trait LockOrder {
    type Locked;

    fn lock_in_order(self, bound: Bound) -> Self;
    fn as_locked(self) -> Self::Locked;
}

impl LockOrder for HNil {
    type Locked = HNil;

    fn lock_in_order(self, _: Bound) -> Self {
        HNil
    }

    fn as_locked(self) -> Self::Locked {
        HNil
    }
}

impl<'m, H, Tail> LockOrder for HCons<MaybeLocked<'m, H>, Tail>
where
    Tail: LockOrder,
{
    type Locked = HCons<LockResult<MutexGuard<'m, H>>, Tail::Locked>;

    fn lock_in_order(self, bound: Bound) -> Self {
        let (h, tail) = self.pop();

        let (before, lock) = if h.lock_before(&bound) {
            (tail.lock_in_order(h.bound()), h.lock())
        } else {
            (tail, h)
        };

        HCons {
            head: lock,
            tail: before.lock_in_order(bound),
        }
    }

    fn as_locked(self) -> Self::Locked {
        let (h, tail) = self.pop();
        match h {
            MaybeLocked::Locked(l) => HCons {
                head: l,
                tail: tail.as_locked(),
            },
            MaybeLocked::NotLocked(_) => unreachable!(),
        }
    }
}

/// Unwrap a frunk::hlist of `Result`s.
///
/// Analogous to [`Result::unwrap`].
pub trait Unwrap {
    type Output;

    fn unwrap(self) -> Self::Output;
}

impl Unwrap for HNil {
    type Output = HNil;

    fn unwrap(self) -> Self::Output {
        HNil
    }
}

impl<R, E, Tail> Unwrap for HCons<Result<R, E>, Tail>
where
    Tail: Unwrap,
    E: core::fmt::Debug,
{
    type Output = HCons<R, Tail::Output>;

    fn unwrap(self) -> Self::Output {
        let (h, tail) = self.pop();
        HCons {
            head: h.unwrap(),
            tail: tail.unwrap(),
        }
    }
}

#[cfg(all(test, feature = "loom-tests"))]
mod loom_tests {
    use super::*;

    use frunk::{hlist, hlist_pat};
    use loom::{self, sync::Arc};

    #[test]
    fn loom_deadlock() {
        loom::model(|| {
            let a = Arc::new(Mutex::new(42));
            let b = Arc::new(Mutex::new(43));

            let t1 = {
                let a = a.clone();
                let b = b.clone();
                loom::thread::spawn(move || {
                    let hlist_pat![a, mut b] = lock(hlist![&*a, &*b]).unwrap();
                    *b += *a;
                })
            };

            let t2 = {
                let a = a.clone();
                let b = b.clone();
                loom::thread::spawn(move || {
                    let hlist_pat![b, mut a] = lock(hlist![&*b, &*a]).unwrap();
                    *a += *b;
                })
            };

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
