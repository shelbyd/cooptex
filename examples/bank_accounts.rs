use cooptex::{retry_loop, CoopMutex};

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn main() {
    let num_accounts = 10;
    let num_threads = 100;

    let accounts = Arc::new(
        (0..num_accounts)
            .map(|_| CoopMutex::new(1000))
            .collect::<Vec<_>>(),
    );

    let started = Arc::new(AtomicUsize::new(0));

    (0..num_threads)
        .map(|i| {
            let accounts = accounts.clone();
            let started = started.clone();
            std::thread::spawn(move || {
                started.fetch_add(1, Ordering::SeqCst);
                while started.load(Ordering::SeqCst) < num_threads {
                    std::thread::yield_now();
                }

                use rand::prelude::SliceRandom;
                let mut accounts = accounts.choose_multiple(&mut rand::thread_rng(), 2);
                let from = accounts.next().unwrap();
                let to = accounts.next().unwrap();

                retry_loop(|| {
                    let mut from = from.lock()?.unwrap();
                    let mut to = to.lock()?.unwrap();
                    eprintln!("Thread {:>3} doing transfer", i);
                    *from -= 3;
                    *to += 3;
                    Ok(())
                });
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .for_each(|t| t.join().unwrap());

    eprintln!("All threads complete");
}
