/*
In-memory concurrent memoization with zero boilerplate.

`#[concurrent_cached]` defaults to a sharded in-memory store — no Redis,
no disk, no `map_error`, no `ty`/`create`. The right variant is selected
automatically based on `max_size` and `ttl` attributes:

  (no attrs)              → ShardedCache        (unbounded, no TTL)
  max_size = N            → ShardedLruCache     (LRU, no TTL)
  ttl = T                 → ShardedTtlCache     (unbounded, with TTL)
  max_size = N, ttl = T   → ShardedLruTtlCache  (LRU, with TTL)

For per-value expiry (`expires = true`), see `examples/sharded_expiring.rs`.

All four are fully concurrent: multiple threads can share the same cache and
call get/set concurrently without any external locking.

Return types:
  - Plain `T`: the return value is always cached.
  - `Option<T>`: only `Some` is cached; `None` is returned without being stored
    (use `cache_none = true` to also cache `None`).
  - `Result<T, E>`: only `Ok` values are cached; `Err` is returned without
    being stored, so the function will be retried on the next call.

Run:
    cargo run --example sharded --features "time_stores,proc_macro"
*/

use cached::macros::concurrent_cached;
use cached::{ConcurrentCached, ShardedCache, ShardedLruCache};
use std::thread;

// Bare default: ShardedCache (unbounded, no TTL)
#[concurrent_cached]
fn compute(x: u64) -> u64 {
    x * x
}

// LRU: ShardedLruCache (max_size = 128 requested; actual capacity is ≥ 128 because each shard
// gets ceiling(max_size/shards) slots with a minimum of 16 — so max_size=128 with 8 shards is
// exactly 128, but max_size=10 with 8 shards would yield 128 slots (8 × 16 minimum).
// See the `max_size` attribute docs for details.)
#[concurrent_cached(max_size = 128)]
fn compute_lru(x: u64) -> u64 {
    x * x
}

// TTL: ShardedTtlCache (expires after 60 s)
#[concurrent_cached(ttl = 60)]
fn compute_ttl(x: u64) -> u64 {
    x * x
}

// LRU + TTL: ShardedLruTtlCache
#[concurrent_cached(max_size = 64, ttl = 30)]
fn compute_lru_ttl(x: u64) -> u64 {
    x * x
}

// Explicit shard count (overrides the default of cpu-count × 4, rounded up)
#[concurrent_cached(shards = 32)]
fn compute_shards(x: u64) -> u64 {
    x * x
}

// Only cache successful lookups — Err is returned but not stored, so the
// function is retried on the next call.
#[concurrent_cached]
fn load_record(id: u64) -> Result<String, std::io::Error> {
    Ok(format!("record_{id}"))
}

// Cache Option — only Some values are stored; None is returned without being
// cached, so find_record(0) will re-execute on every call.
#[concurrent_cached]
fn find_record(id: u64) -> Option<String> {
    if id == 0 {
        None
    } else {
        Some(format!("record_{id}"))
    }
}

fn main() {
    // Basic memoization check
    let v1 = compute(7);
    let v2 = compute(7);
    assert_eq!(v1, v2);
    assert_eq!(v1, 49);
    println!("compute(7) = {v1} (both calls agree)");

    // Result: only Ok is cached
    let r1 = load_record(42);
    let r2 = load_record(42);
    assert_eq!(r1.as_deref().expect("infallible"), "record_42");
    assert_eq!(r2.as_deref().expect("infallible"), "record_42");
    println!("load_record(42) = {:?} (cached)", r1);

    // Option: None is NOT cached by default; the function re-executes each time
    assert_eq!(find_record(0), None);
    assert_eq!(find_record(0), None); // re-executes, not a cache hit
    assert_eq!(find_record(1), Some("record_1".to_string()));
    println!(
        "find_record(0) = None (not cached), find_record(1) = {:?}",
        find_record(1)
    );

    // Exercise the other cached variants to confirm they work
    let v = compute_lru(7);
    assert_eq!(v, 49);
    println!("compute_lru(7) = {v}");
    let v = compute_ttl(7);
    assert_eq!(v, 49);
    println!("compute_ttl(7) = {v}");
    let v = compute_lru_ttl(7);
    assert_eq!(v, 49);
    println!("compute_lru_ttl(7) = {v}");
    let v = compute_shards(7);
    assert_eq!(v, 49);
    println!("compute_shards(7) = {v}");

    // Demonstrate that concurrent callers share the same cache
    let handles: Vec<_> = (0..8)
        .map(|_| {
            thread::spawn(|| {
                for i in 0u64..100 {
                    let _ = compute(i % 10);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread panicked");
    }

    // Inspect the cache directly. The `cache_get`/`cache_set`/... methods come from the
    // `ConcurrentCached` trait (it must be in scope). The async trait's operations are
    // `async_`-prefixed: `COMPUTE.async_cache_get(&7).await`.
    {
        let val = COMPUTE.cache_get(&7).expect("infallible");
        assert_eq!(val, Some(49));
        println!("cache_get(7) = {val:?}");
    }

    // Build a ShardedCache manually and use it without a macro
    let cache: ShardedCache<u32, String> = ShardedCache::builder().build().unwrap();
    cache.cache_set(1, "hello".to_string()).expect("infallible");
    cache.cache_set(2, "world".to_string()).expect("infallible");
    assert_eq!(
        cache.cache_get(&1).expect("infallible").as_deref(),
        Some("hello")
    );
    println!(
        "manual ShardedCache: {:?}",
        cache.cache_get(&1).expect("infallible")
    );

    // ShardedLruCache with explicit shard count
    let lru: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(256)
        .shards(8)
        .build()
        .expect("valid config");
    for i in 0..256u32 {
        lru.cache_set(i, i * 2).expect("infallible");
    }
    println!("ShardedLruCache len = {}", lru.len());
    println!("ShardedLruCache shard_sizes = {:?}", lru.shard_sizes());

    println!("\ndone!");
}
