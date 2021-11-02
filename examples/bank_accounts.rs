use cooptex::{lock, lock_in_order::Unwrap};

use frunk::{hlist, hlist_pat};

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

fn main() {
    let num_accounts = 10;
    let num_threads = 100;

    let accounts = Arc::new(
        (0..num_accounts)
            .map(|_| Mutex::new(1000))
            .collect::<Vec<_>>(),
    );

    let started = Arc::new(AtomicUsize::new(0));

    (0..num_threads)
        .map(|_| {
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

                let hlist_pat![mut from, mut to] = lock(hlist![from, to]).unwrap();
                eprintln!("{:?} doing transfer", std::thread::current().id());
                *from -= 3;
                *to += 3;
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .for_each(|t| t.join().unwrap());

    eprintln!("All threads complete");
}
