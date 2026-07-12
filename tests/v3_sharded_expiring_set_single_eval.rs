//! `cache_set` on the sharded expiring stores must evaluate the displaced value's
//! `is_expired()` exactly once, under the shard write lock, and use that single result
//! for all three outcomes it drives: the eviction counter, the `on_evict` callback, and
//! whether the displaced value is filtered from the return.
//!
//! With two evaluations, a value crossing the expiry threshold between them fires
//! `on_evict` without counting an eviction (or vice versa). The `FlipVal` type below makes
//! that transition deterministic: its `is_expired()` reports live on the first call and
//! expired on every call after, so a double-evaluating implementation visibly disagrees
//! with itself.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cached::prelude::*;
use cached::{ShardedExpiringCache, ShardedExpiringLruCache};

/// Reports live on the first `is_expired()` call, expired afterwards, and counts calls.
#[derive(Clone)]
struct FlipVal {
    calls: Arc<AtomicUsize>,
}

impl FlipVal {
    fn new() -> Self {
        FlipVal {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Expires for FlipVal {
    fn is_expired(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) >= 1
    }
}

#[test]
fn sharded_expiring_lru_set_evaluates_displaced_expiry_once() {
    let evict_calls = Arc::new(AtomicUsize::new(0));
    let evict_calls_cb = Arc::clone(&evict_calls);
    let cache: ShardedExpiringLruCache<u32, FlipVal> = ShardedExpiringLruCache::builder()
        .max_size(16)
        .shards(1)
        .on_evict(move |_k, _v| {
            evict_calls_cb.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .unwrap();

    let first = FlipVal::new();
    let first_calls = Arc::clone(&first.calls);
    cache.set(1, first);
    assert_eq!(first_calls.load(Ordering::SeqCst), 0);

    // Displace the first value. Its single under-lock evaluation reports live, so it must
    // be returned to the caller with no eviction counted and no callback fired.
    let displaced = cache.set(1, FlipVal::new());
    assert_eq!(
        first_calls.load(Ordering::SeqCst),
        1,
        "displaced value's is_expired() must be evaluated exactly once"
    );
    assert!(
        displaced.is_some(),
        "a live displaced value is returned to the caller"
    );
    assert_eq!(evict_calls.load(Ordering::SeqCst), 0);
    assert_eq!(cache.metrics().evictions, Some(0));
}

#[test]
fn sharded_expiring_set_evaluates_displaced_expiry_once() {
    let evict_calls = Arc::new(AtomicUsize::new(0));
    let evict_calls_cb = Arc::clone(&evict_calls);
    let cache: ShardedExpiringCache<u32, FlipVal> = ShardedExpiringCache::builder()
        .shards(1)
        .on_evict(move |_k, _v| {
            evict_calls_cb.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .unwrap();

    let first = FlipVal::new();
    let first_calls = Arc::clone(&first.calls);
    cache.set(1, first);
    assert_eq!(first_calls.load(Ordering::SeqCst), 0);

    let displaced = cache.set(1, FlipVal::new());
    assert_eq!(
        first_calls.load(Ordering::SeqCst),
        1,
        "displaced value's is_expired() must be evaluated exactly once"
    );
    assert!(
        displaced.is_some(),
        "a live displaced value is returned to the caller"
    );
    assert_eq!(evict_calls.load(Ordering::SeqCst), 0);
    assert_eq!(cache.metrics().evictions, Some(0));
}
