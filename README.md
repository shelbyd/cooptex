![Maintenance](https://img.shields.io/badge/maintenance-experimental-blue.svg)

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

The crate also provides a lower-overhead function [`lock`] which acquires a set of
[`std::sync::Mutex`]es in a consistent order, to guarantee no deadlocks. Use that function if
you can acquire all necessary locks at once.

If you conditionally acquire locks, [`CoopMutex`] and [`retry_loop`] are likely necessary.

## CoopMutex Guarantees

This crate aims to guarantee that multiple threads cannot possibly deadlock by acquiring
locks.

This crate also will prefer threads that have lived the "longest" without completing work.
Meaning, when [`retry_loop`] successfully completes, it will move that thread to the end of the
queue for acquiring future locks. This provides an approximately fair scheduler.

The crate is still in early development, so there may be cases that aren't covered. Please open
an issue if you can reproduce a deadlock.

### Non-Guarantees

This crate explicitly allows the following potentially undesired behavior:

- [`CoopMutex::lock`] may return [`Retry`] when it could wait and acquire the lock without
a deadlock.
- [`CoopMutex::lock`] may wait arbitrarily long before returning [`Retry`].

### Incomplete

- We have not fully analyzed the behavior during panics. There is no `unsafe` code, so we could
only possibly deadlock.

License: MIT
