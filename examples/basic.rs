/*
The basics: `#[cached(max_size = N)]` (LRU memoization) and `#[once(ttl_secs = N)]`
(a single cached value that expires), plus reading the generated cache static
through the `Cached` trait, and manual cache invalidation via `remove`.

Run:
    cargo run --example basic --features "time_stores,proc_macro"
*/

use cached::macros::cached;
use cached::macros::once;
use cached::time::{Duration, Instant};
use std::thread::sleep;

#[cached(max_size = 50)]
fn slow_fn(n: u32) -> String {
    if n == 0 {
        return "done".to_string();
    }
    sleep(Duration::new(1, 0));
    slow_fn(n - 1)
}

/// Remove a specific entry from the `slow_fn` cache.
/// `Cached` must be in scope to call `remove`.
fn invalidate_slow_fn(n: u32) {
    use cached::Cached;
    // `.0` accesses the inner lock of the (lock, key-buckets) tuple produced by
    // the default `sync_writes = "by_key"` mode.
    SLOW_FN.0.write().remove(&n);
}

#[once(ttl_secs = 1)]
fn once_slow_fn(n: u32) -> String {
    sleep(Duration::new(1, 0));
    format!("{n}")
}

pub fn main() {
    println!("[cached] Initial run...");
    let now = Instant::now();
    let _ = slow_fn(10);
    println!("[cached] Elapsed: {}\n", now.elapsed().as_secs());

    println!("[cached] Cached run...");
    let now = Instant::now();
    let _ = slow_fn(10);
    println!("[cached] Elapsed: {}\n", now.elapsed().as_secs());

    // Inspect the cache
    {
        use cached::Cached; // must be in scope to access cache

        println!("[cached] ** Cache info **");
        let cache = SLOW_FN.0.read();
        assert_eq!(cache.hits().unwrap(), 1);
        println!("[cached] hits=1 -> {:?}", cache.hits().unwrap() == 1);
        assert_eq!(cache.misses().unwrap(), 11);
        println!("[cached] misses=11 -> {:?}", cache.misses().unwrap() == 11);
        // make sure the cache-lock is dropped
    }

    // Invalidate the entry for n=10, then show the next call is a cache miss.
    println!("[cached] Invalidating entry for n=10...");
    invalidate_slow_fn(10);
    {
        use cached::Cached;
        let mut cache = SLOW_FN.0.write();
        // Entry for 10 is gone; the recursive sub-entries (0 through 9) are still present.
        let present = cache.get(&10).is_some();
        println!("[cached] cache contains n=10 after invalidation -> {present}");
        assert!(!present, "entry should have been removed");
    }

    println!("[once] Initial run...");
    let now = Instant::now();
    let _ = once_slow_fn(10);
    println!("[once] Elapsed: {}\n", now.elapsed().as_secs());

    println!("[once] Cached run...");
    let now = Instant::now();
    let _ = once_slow_fn(10);
    println!("[once] Elapsed: {}\n", now.elapsed().as_secs());

    println!("done!");
}
