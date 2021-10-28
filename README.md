# cooptex

cooptex provides deadlock-free Mutexes. The [`CoopMutex::lock`] method wraps the
[`std::sync::Mutex`] return value with a `Result` that will request the caller to drop other held
locks so another thread could make progress. This behavior is easily accomplished by using the
[`retry_loop`] function.

```rust
use cooptex::*;
let a = CoopMutex::new(42);
let b = CoopMutex::new(43);

retry_loop(|| {
  let a_lock = a.lock()?.unwrap();
  let b_lock = b.lock()?.unwrap();
  assert_eq!(*a_lock + *b_lock, 85);
  Ok(())
});
```

## Guarantees

This crate aims to guarantee that multiple threads cannot possibly deadlock by acquiring
locks.

The crate is still in early development, so there may be cases that aren't covered. Please open
an issue if you can reproduce a deadlock.

### Non-Guarantees

This crate allows the following potentially undesired behavior:

- [`CoopMutex::lock`] may return [`Retry`] when it could wait and acquire the lock without
a deadlock.
- [`CoopMutex::lock`] may wait arbitrarily long before returning [`Retry`].
- [`MutexGuard`] is not [`Send`], as we haven't fully evaluated the implications of sending
`MutexGuard`s.
- The current "Scheduler" for which thread has priority over locks is not fair.
- [`retry_loop`] will spin-lock the calling thread until it can make progress.
