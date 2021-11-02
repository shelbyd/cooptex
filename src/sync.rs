#[cfg(all(test, feature = "loom-tests"))]
pub use loom::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard},
    thread_local,
};

#[cfg(not(all(test, feature = "loom-tests")))]
pub use std::{
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard},
    thread_local,
};
