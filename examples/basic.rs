#[macro_use] extern crate cached;
// `cached!` macro requires the `lazy_static!` macro
#[macro_use] extern crate lazy_static;

use std::time::{Instant, Duration};
use std::thread::sleep;

use cached::SizedCache;


cached! {
    SLOW_FN: SizedCache<(u32), String> = SizedCache::with_capacity(50);
    fn slow_fn(n: u32) -> String = {
        if n == 0 { return "done".to_string(); }
        sleep(Duration::new(1, 0));
        slow_fn(n-1)
    }
}


pub fn main() {
    println!("Initial run...");
    let now = Instant::now();
    let _ = slow_fn(10);
    println!("Elapsed: {}\n", now.elapsed().as_secs());

    println!("Cached run...");
    let now = Instant::now();
    let _ = slow_fn(10);
    println!("Elapsed: {}\n", now.elapsed().as_secs());

    // Inspect the cache
    {
        use cached::Cached;  // must be in scope to access cache

        println!(" ** Cache info **");
        let cache = SLOW_FN.lock().unwrap();
        println!("hits=1 -> {:?}", cache.cache_hits().unwrap() == 1);
        println!("misses=11 -> {:?}", cache.cache_misses().unwrap() == 11);
        // make sure the cache-lock is dropped
    }
}

