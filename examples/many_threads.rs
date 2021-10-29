use cooptex::{retry_loop, CoopMutex};

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn main() {
    let num_items = 10;
    let num_threads = 100;

    let items = (0..num_items)
        .map(|_| Arc::new(CoopMutex::new(0)))
        .collect::<Vec<_>>();

    let started = Arc::new(AtomicUsize::new(0));

    (0..num_threads)
        .map(|i| {
            let mut items = items.clone();
            let started = started.clone();
            std::thread::spawn(move || {
                use rand::prelude::SliceRandom;
                items.shuffle(&mut rand::thread_rng());

                started.fetch_add(1, Ordering::SeqCst);
                while started.load(Ordering::SeqCst) < num_threads {
                    std::thread::yield_now();
                }

                retry_loop(|| {
                    let locks = items
                        .iter()
                        .map(|mutex| mutex.lock())
                        .collect::<Result<Vec<_>, _>>()?;
                    eprintln!("Thread {:>3} acquired locks", i);
                    for lock in locks {
                        *lock.unwrap() += 1;
                    }
                    Ok(())
                });
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .for_each(|t| t.join().unwrap());

    for item in items {
        assert_eq!(*item.lock().unwrap().unwrap(), num_threads);
    }

    eprintln!("All threads complete");
}
