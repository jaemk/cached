//! Integration tests for the additive parity surface added ahead of 3.0.0 final:
//! - inherent `peek` on the six sharded stores
//! - `new()` on the in-memory builders
//! - `CachedExt::capacity`/`evictions` and `ConcurrentCachedExt` metric aliases
//! - `ConcurrentCached::cache_try_get_or_set_with` (+ async counterpart)
//! - `retain` on `UnboundCache` / `TtlCache` / `ExpiringCache`
//! - `TtlSortedCache::capacity`

use cached::{ShardedLruCache, ShardedUnboundCache, UnboundCache};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "time_stores")]
use cached::time::Duration;

// ── sharded inherent peek ─────────────────────────────────────────────────────

#[test]
fn sharded_unbound_peek_returns_value_without_metrics() {
    let c: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::new();
    c.set(1, 10);
    let hits_before = c.metrics().hits;
    let misses_before = c.metrics().misses;
    assert_eq!(c.peek(&1), Some(10));
    assert_eq!(c.peek(&2), None);
    let m = c.metrics();
    assert_eq!(m.hits, hits_before, "peek must not count a hit");
    assert_eq!(m.misses, misses_before, "peek must not count a miss");
}

#[test]
fn sharded_lru_peek_does_not_promote_recency() {
    // shards = 1 so all keys share one LRU order.
    let c: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(2)
        .shards(1)
        .build()
        .unwrap();
    c.set(1, 10);
    c.set(2, 20);
    // peek(1) must NOT promote key 1; inserting key 3 then evicts key 1 (LRU).
    assert_eq!(c.peek(&1), Some(10));
    c.set(3, 30);
    assert_eq!(c.peek(&1), None, "peeked key must still be evicted first");
    assert_eq!(c.peek(&2), Some(20));
    assert_eq!(c.peek(&3), Some(30));
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_peek_respects_expiry() {
    use cached::{ShardedLruTtlCache, ShardedTtlCache};
    let c: ShardedTtlCache<u32, u32> = ShardedTtlCache::new(Duration::from_millis(20));
    c.set(1, 10);
    assert_eq!(c.peek(&1), Some(10));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(c.peek(&1), None, "expired entry must peek as absent");

    let c: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::new(8, Duration::from_millis(20));
    c.set(1, 10);
    assert_eq!(c.peek(&1), Some(10));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(c.peek(&1), None, "expired entry must peek as absent");
}

#[test]
fn sharded_expiring_peek_respects_expiry() {
    use cached::{Expires, ShardedExpiringCache, ShardedExpiringLruCache};

    #[derive(Clone, PartialEq, Debug)]
    struct Val {
        id: u32,
        expired: bool,
    }
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let c: ShardedExpiringCache<u32, Val> = ShardedExpiringCache::new();
    c.set(
        1,
        Val {
            id: 1,
            expired: false,
        },
    );
    c.set(
        2,
        Val {
            id: 2,
            expired: true,
        },
    );
    assert_eq!(c.peek(&1).map(|v| v.id), Some(1));
    assert_eq!(c.peek(&2), None, "expired value must peek as absent");

    let c: ShardedExpiringLruCache<u32, Val> = ShardedExpiringLruCache::new(8);
    c.set(
        1,
        Val {
            id: 1,
            expired: false,
        },
    );
    c.set(
        2,
        Val {
            id: 2,
            expired: true,
        },
    );
    assert_eq!(c.peek(&1).map(|v| v.id), Some(1));
    assert_eq!(c.peek(&2), None, "expired value must peek as absent");
}

// ── builder new() ─────────────────────────────────────────────────────────────

#[test]
fn builders_construct_via_new() {
    use cached::{
        LruCacheBuilder, ShardedLruCacheBuilder, ShardedUnboundCacheBuilder, UnboundCacheBuilder,
    };

    let _: UnboundCache<u32, u32> = UnboundCacheBuilder::new().build().unwrap();
    let _ = LruCacheBuilder::<u32, u32>::new()
        .max_size(4)
        .build()
        .unwrap();
    let _ = ShardedUnboundCacheBuilder::<u32, u32>::new()
        .build()
        .unwrap();
    let _ = ShardedLruCacheBuilder::<u32, u32>::new()
        .max_size(16)
        .build()
        .unwrap();
}

#[cfg(feature = "time_stores")]
#[test]
fn time_store_builders_construct_via_new() {
    use cached::{
        LruTtlCacheBuilder, ShardedLruTtlCacheBuilder, ShardedTtlCacheBuilder, TtlCacheBuilder,
        TtlSortedCacheBuilder,
    };

    let ttl = Duration::from_secs(60);
    let _ = TtlCacheBuilder::<u32, u32>::new().ttl(ttl).build().unwrap();
    let _ = LruTtlCacheBuilder::<u32, u32>::new()
        .max_size(4)
        .ttl(ttl)
        .build()
        .unwrap();
    let _ = TtlSortedCacheBuilder::<u32, u32>::new()
        .ttl(ttl)
        .build()
        .unwrap();
    let _ = ShardedTtlCacheBuilder::<u32, u32>::new()
        .ttl(ttl)
        .build()
        .unwrap();
    let _ = ShardedLruTtlCacheBuilder::<u32, u32>::new()
        .max_size(16)
        .ttl(ttl)
        .build()
        .unwrap();
}

#[test]
fn expiring_builders_construct_via_new() {
    use cached::{Expires, ExpiringCacheBuilder, ExpiringLruCacheBuilder};
    use cached::{ShardedExpiringCacheBuilder, ShardedExpiringLruCacheBuilder};

    #[derive(Clone)]
    struct Val;
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let _ = ExpiringCacheBuilder::<u32, Val>::new().build().unwrap();
    let _ = ExpiringLruCacheBuilder::<u32, Val>::new()
        .max_size(4)
        .build()
        .unwrap();
    let _ = ShardedExpiringCacheBuilder::<u32, Val>::new()
        .build()
        .unwrap();
    let _ = ShardedExpiringLruCacheBuilder::<u32, Val>::new()
        .max_size(16)
        .build()
        .unwrap();
}

// ── ext alias parity ──────────────────────────────────────────────────────────

#[test]
fn cached_ext_capacity_and_evictions_aliases() {
    use cached::{CachedExt, LruCache};

    let mut c: LruCache<u32, u32> = LruCache::new(2);
    // The inherent `capacity()` (returning plain usize) takes call-site priority;
    // the alias is reachable via the trait path.
    assert_eq!(c.capacity(), 2);
    assert_eq!(CachedExt::capacity(&c), Some(2));
    assert_eq!(c.evictions(), Some(0));
    c.set(1, 10);
    c.set(2, 20);
    c.set(3, 30); // evicts key 1
    assert_eq!(c.evictions(), Some(1));
}

#[test]
fn concurrent_ext_metric_aliases() {
    use cached::ConcurrentCachedExt;

    let c: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::new();
    c.set(1, 10);
    let _ = c.get(&1);
    let _ = c.get(&2);
    // The inherent len/is_empty take call-site priority; use the trait path.
    assert_eq!(ConcurrentCachedExt::len(&c).unwrap(), Some(1));
    assert_eq!(ConcurrentCachedExt::is_empty(&c).unwrap(), Some(false));
    assert_eq!(ConcurrentCachedExt::hits(&c), Some(1));
    assert_eq!(ConcurrentCachedExt::misses(&c), Some(1));
    // Unbound store: no eviction tracking and no capacity bound.
    assert_eq!(ConcurrentCachedExt::evictions(&c), None);
    assert_eq!(ConcurrentCachedExt::capacity(&c), None);
}

// ── concurrent cache_try_get_or_set_with ─────────────────────────────────────

#[test]
fn concurrent_try_get_or_set_with_ok_err_and_hit() {
    use cached::ConcurrentCached;

    let c: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::new();

    // Miss + Err: nothing stored, inner Err returned.
    let r: Result<u32, &str> = c.cache_try_get_or_set_with(1, || Err("nope")).unwrap();
    assert_eq!(r, Err("nope"));
    assert_eq!(c.get(&1), None, "failed init must not store");

    // Miss + Ok: stored and returned.
    let r: Result<u32, &str> = c.cache_try_get_or_set_with(1, || Ok(10)).unwrap();
    assert_eq!(r, Ok(10));
    assert_eq!(c.get(&1), Some(10));

    // Hit: closure must not run.
    let ran = Arc::new(AtomicUsize::new(0));
    let ran2 = ran.clone();
    let r: Result<u32, &str> = c
        .cache_try_get_or_set_with(1, move || {
            ran2.fetch_add(1, Ordering::Relaxed);
            Ok(99)
        })
        .unwrap();
    assert_eq!(r, Ok(10));
    assert_eq!(
        ran.load(Ordering::Relaxed),
        0,
        "hit must not run the closure"
    );
}

#[cfg(feature = "async")]
#[tokio::test]
async fn concurrent_async_try_get_or_set_with_ok_err_and_hit() {
    use cached::ConcurrentCachedAsync;

    let c: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::new();

    let r: Result<u32, &str> = c
        .async_cache_try_get_or_set_with(1, || async { Err("nope") })
        .await
        .unwrap();
    assert_eq!(r, Err("nope"));
    assert_eq!(c.get(&1), None, "failed init must not store");

    let r: Result<u32, &str> = c
        .async_cache_try_get_or_set_with(1, || async { Ok(10) })
        .await
        .unwrap();
    assert_eq!(r, Ok(10));

    let r: Result<u32, &str> = c
        .async_cache_try_get_or_set_with(1, || async { Ok(99) })
        .await
        .unwrap();
    assert_eq!(r, Ok(10), "hit returns the cached value");
}

// ── retain on the map-backed stores ──────────────────────────────────────────

#[test]
fn unbound_cache_retain_filters_on_predicate_and_fires_on_evict() {
    use cached::{Cached, CachedExt};

    let evicted = Arc::new(std::sync::Mutex::new(Vec::<(u32, u32)>::new()));
    let evicted2 = evicted.clone();
    let mut c: UnboundCache<u32, u32> = UnboundCache::builder()
        .on_evict(move |k: &u32, v: &u32| {
            evicted2.lock().unwrap().push((*k, *v));
        })
        .build()
        .unwrap();
    c.cache_set(1, 11);
    c.cache_set(2, 20);
    c.cache_set(3, 31);

    // No eviction dimension: a keep-everything predicate removes nothing.
    c.retain(|_k, _v| true);
    assert_eq!(c.cache_size(), 3);
    assert!(evicted.lock().unwrap().is_empty());

    // An entry survives exactly when `keep` returns true; removed entries fire on_evict.
    c.retain(|_k, v| v % 2 == 0);
    assert_eq!(c.cache_size(), 1);
    assert!(c.contains(&2));
    assert_eq!(c.cache_get(&1), None);
    assert_eq!(c.cache_get(&3), None);
    let mut fired = evicted.lock().unwrap().clone();
    fired.sort_unstable();
    assert_eq!(fired, vec![(1, 11), (3, 31)]);
}

#[cfg(feature = "time_stores")]
#[test]
fn ttl_cache_retain_removes_expired_regardless_of_predicate() {
    use cached::{CacheTtl, Cached, CachedExt, TtlCache};

    let fired = Arc::new(AtomicUsize::new(0));
    let fired2 = fired.clone();
    let mut c: TtlCache<u32, u32> = TtlCache::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |_k: &u32, _v: &u32| {
            fired2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();
    c.set(1, 10);
    std::thread::sleep(std::time::Duration::from_millis(50));
    // Switch to a long ttl for the live entries.
    c.set_ttl(Duration::from_secs(60));
    c.set(2, 20);
    c.set(3, 31);

    // keep-everything predicate: the expired entry must still be removed.
    c.retain(|_k, _v| true);
    assert_eq!(c.cache_size(), 2, "expired entry removed despite keep=true");
    assert_eq!(fired.load(Ordering::Relaxed), 1);

    // Predicate-based removal fires on_evict and counts evictions.
    let evictions_before = c.evictions().unwrap();
    c.retain(|_k, v| v % 2 == 0);
    assert_eq!(c.cache_size(), 1);
    assert!(c.contains(&2));
    assert_eq!(c.evictions(), Some(evictions_before + 1));
    assert_eq!(fired.load(Ordering::Relaxed), 2);
}

#[test]
fn expiring_cache_retain_removes_expired_regardless_of_predicate() {
    use cached::{Cached, CachedExt, Expires, ExpiringCache};

    #[derive(Clone)]
    struct Val {
        id: u32,
        expired: bool,
    }
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let mut c: ExpiringCache<u32, Val> = ExpiringCache::builder().build().unwrap();
    c.set(
        1,
        Val {
            id: 1,
            expired: true,
        },
    );
    c.set(
        2,
        Val {
            id: 2,
            expired: false,
        },
    );
    c.set(
        3,
        Val {
            id: 3,
            expired: false,
        },
    );

    let evictions_before = c.evictions().unwrap();
    // keep-everything predicate: the expired value must still be removed.
    c.retain(|_k, _v| true);
    assert_eq!(c.cache_size(), 2);
    // Predicate removal.
    c.retain(|_k, v| v.id != 3);
    assert_eq!(c.cache_size(), 1);
    assert!(c.contains(&2));
    assert_eq!(c.evictions(), Some(evictions_before + 2));
}

// ── TtlSortedCache::capacity ─────────────────────────────────────────────────

#[cfg(feature = "time_stores")]
#[test]
fn ttl_sorted_capacity_reflects_bound() {
    use cached::TtlSortedCache;

    let mut c: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    assert_eq!(c.capacity(), None, "no bound configured");
    c.set_max_size(5);
    assert_eq!(c.capacity(), Some(5));

    let bounded: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .max_size(3)
        .build()
        .unwrap();
    assert_eq!(bounded.capacity(), Some(3));
}
