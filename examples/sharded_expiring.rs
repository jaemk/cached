/*
In-memory concurrent expiring memoization with zero boilerplate.

When using the `#[concurrent_cached]` procedural macro, the `expires = true` attribute
selects a sharded in-memory expiring store:
- `expires = true`                 → ShardedExpiringCache     (unbounded, expiring)
- `expires = true, max_size = N`    → ShardedExpiringLruCache  (LRU-bounded, expiring)

Expired entries are checked on lookup and evicted on access or during explicit sweeps.
These stores wrap an `Arc` (Send + Sync) and are fully concurrent.

Run:
    cargo run --example sharded_expiring --features "proc_macro"
*/

use cached::macros::concurrent_cached;
use cached::{Expires, ShardedExpiringCache, ShardedExpiringLruCache};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

// Values stored in expiring caches must implement the `Expires` trait.
#[derive(Clone, Debug)]
struct Session {
    user_id: u32,
    expired: Arc<AtomicBool>,
}

impl Expires for Session {
    fn is_expired(&self) -> bool {
        self.expired.load(Ordering::Relaxed)
    }
}

// Unbounded expiring sharded cache
#[concurrent_cached(expires = true, key = "u32", convert = r#"{ user_id }"#)]
fn get_session(user_id: u32, expired_flag: Arc<AtomicBool>) -> Session {
    Session {
        user_id,
        expired: expired_flag,
    }
}

// LRU-bounded expiring sharded cache
#[concurrent_cached(
    expires = true,
    max_size = 128,
    key = "u32",
    convert = r#"{ user_id }"#
)]
fn get_session_bounded(user_id: u32, expired_flag: Arc<AtomicBool>) -> Session {
    Session {
        user_id,
        expired: expired_flag,
    }
}

fn main() {
    println!("--- Sharded Expiring Cache Example ---");

    // The AtomicBool models `is_expired`: false = live, true = expired.
    let expiry = Arc::new(AtomicBool::new(false)); // starts live
    let already_expired = Arc::new(AtomicBool::new(true)); // starts expired

    // 1. Unbounded expiring cache lookup
    let s1 = get_session(1, expiry.clone());
    let s2 = get_session(1, expiry.clone());
    println!("Call 1 (live session): {:?}", s1);
    println!("Call 2 (live session cached): {:?}", s2);

    // Mark the cached entry as expired; next call will re-execute the function.
    expiry.store(true, Ordering::Relaxed);
    let s3 = get_session(1, expiry.clone());
    println!(
        "Call 3 (after flag changed to expired -> recalculated): {:?}",
        s3
    );

    // 2. Bounded expiring LRU cache
    let bounded_flag = Arc::new(AtomicBool::new(false));
    let b1 = get_session_bounded(10, bounded_flag.clone());
    let b2 = get_session_bounded(10, bounded_flag.clone());
    println!("LRU call 1 (live): {:?}", b1);
    println!("LRU call 2 (live): {:?}", b2);

    // 3. Multi-threaded concurrent usage
    let live_shared = Arc::new(AtomicBool::new(false));
    let handles: Vec<_> = (0..8)
        .map(|id| {
            let flag = live_shared.clone();
            thread::spawn(move || {
                for i in 0..50 {
                    let _ = get_session(i % 5, flag.clone());
                }
                println!("Thread {id} finished lookups.");
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    // 4. Direct Manual construction and usage (without macro).
    // The inherent `set`/`get` methods return unwrapped values directly.
    println!("\n--- Manual Store Construction ---");
    let cache: ShardedExpiringCache<u32, Session> =
        ShardedExpiringCache::builder().build().unwrap();
    let s_manual = Session {
        user_id: 100,
        expired: already_expired.clone(), // starts expired
    };
    cache.set(100, s_manual);

    let val = cache.get(&100);
    assert!(
        val.is_none(),
        "Expired manual entry should be filtered out on get"
    );
    println!(
        "Manual ShardedExpiringCache lookup for expired entry: {:?}",
        val
    );

    let lru: ShardedExpiringLruCache<u32, Session> = ShardedExpiringLruCache::builder()
        .max_size(64)
        .shards(4)
        .build()
        .expect("valid config");

    let live_manual = Session {
        user_id: 200,
        expired: Arc::new(AtomicBool::new(false)),
    };
    println!("Caching session for user_id {}", live_manual.user_id);
    lru.set(200, live_manual);
    let val_lru = lru.get(&200);
    assert!(val_lru.is_some(), "Live manual entry should be present");
    println!(
        "Manual ShardedExpiringLruCache lookup for live entry: {:?}",
        val_lru
    );

    println!("Example complete.");
}
