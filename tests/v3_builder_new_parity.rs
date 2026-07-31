//! Equivalence tests for `Builder::new()` on the 13 in-memory/sharded builders
//! (see CHANGELOG [Unreleased] Added: "Builder::new() on the in-memory and
//! sharded builders (all 13)").
//!
//! `tests/v3_additive_parity.rs::builders_construct_via_new` already proves bare
//! construction works for a subset of these builders. This file goes further:
//! for each of the 13 builders it configures a store via `XxxBuilder::new()`
//! and an equivalently-configured store via `Xxx::builder()`, then asserts the
//! two agree on observable configuration (capacity/cache_capacity, ttl where
//! applicable, shards() for sharded stores) and on basic set/get behavior.
//!
//! Sharded LRU-family stores built with `max_size` and no explicit `.shards(n)`
//! have a default shard count that is being changed (capacity-derived) by a
//! concurrent shard of work; to stay correct regardless of that change, every
//! sharded assertion below sets `.shards(n)` explicitly rather than relying on
//! (or hardcoding) the default.

#[cfg(feature = "time_stores")]
use cached::time::Duration;

// ── in-memory (non-sharded) ────────────────────────────────────────────────

#[test]
fn unbound_cache_new_matches_builder() {
    use cached::{Cached, UnboundCache, UnboundCacheBuilder};

    let mut a: UnboundCache<u32, u32> = UnboundCacheBuilder::new()
        .initial_capacity(4)
        .build()
        .unwrap();
    let mut b: UnboundCache<u32, u32> =
        UnboundCache::builder().initial_capacity(4).build().unwrap();

    assert_eq!(a.cache_capacity(), b.cache_capacity());

    a.cache_set(1, 10);
    b.cache_set(1, 10);
    assert_eq!(a.cache_get(&1), b.cache_get(&1));
    assert_eq!(a.cache_size(), b.cache_size());
}

#[test]
fn lru_cache_new_matches_builder() {
    use cached::{Cached, LruCache, LruCacheBuilder};

    let mut a: LruCache<u32, u32> = LruCacheBuilder::new().max_size(2).build().unwrap();
    let mut b: LruCache<u32, u32> = LruCache::builder().max_size(2).build().unwrap();

    assert_eq!(a.capacity(), b.capacity());
    assert_eq!(a.capacity(), 2);

    a.cache_set(1, 10);
    a.cache_set(2, 20);
    a.cache_set(3, 30); // evicts key 1 (LRU, max_size = 2)
    b.cache_set(1, 10);
    b.cache_set(2, 20);
    b.cache_set(3, 30);

    assert_eq!(a.cache_get(&1), b.cache_get(&1));
    assert_eq!(a.cache_get(&1), None);
    assert_eq!(a.cache_get(&2), b.cache_get(&2));
    assert_eq!(a.cache_get(&3), b.cache_get(&3));
    assert_eq!(a.cache_size(), b.cache_size());
}

#[cfg(feature = "time_stores")]
#[test]
fn ttl_cache_new_matches_builder() {
    use cached::{CacheTtl, Cached, TtlCache, TtlCacheBuilder};

    let ttl = Duration::from_secs(60);
    let mut a: TtlCache<u32, u32> = TtlCacheBuilder::new()
        .ttl(ttl)
        .refresh_on_hit(true)
        .build()
        .unwrap();
    let mut b: TtlCache<u32, u32> = TtlCache::builder()
        .ttl(ttl)
        .refresh_on_hit(true)
        .build()
        .unwrap();

    assert_eq!(CacheTtl::ttl(&a), CacheTtl::ttl(&b));
    assert_eq!(CacheTtl::ttl(&a), Some(ttl));
    assert_eq!(CacheTtl::refresh_on_hit(&a), CacheTtl::refresh_on_hit(&b));
    assert!(
        CacheTtl::refresh_on_hit(&a),
        "refresh_on_hit(true) set via new() must be wired through to the store"
    );

    a.cache_set(1, 10);
    b.cache_set(1, 10);
    assert_eq!(a.cache_get(&1), b.cache_get(&1));
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_cache_new_matches_builder() {
    use cached::{CacheTtl, Cached, LruTtlCache, LruTtlCacheBuilder};

    let ttl = Duration::from_secs(60);
    let mut a: LruTtlCache<u32, u32> = LruTtlCacheBuilder::new()
        .max_size(2)
        .ttl(ttl)
        .refresh_on_hit(true)
        .build()
        .unwrap();
    let mut b: LruTtlCache<u32, u32> = LruTtlCache::builder()
        .max_size(2)
        .ttl(ttl)
        .refresh_on_hit(true)
        .build()
        .unwrap();

    assert_eq!(a.capacity(), b.capacity());
    assert_eq!(a.capacity(), 2);
    assert_eq!(CacheTtl::ttl(&a), CacheTtl::ttl(&b));
    assert_eq!(CacheTtl::ttl(&a), Some(ttl));
    assert_eq!(CacheTtl::refresh_on_hit(&a), CacheTtl::refresh_on_hit(&b));
    assert!(
        CacheTtl::refresh_on_hit(&a),
        "refresh_on_hit(true) set via new() must be wired through to the store"
    );

    a.cache_set(1, 10);
    a.cache_set(2, 20);
    a.cache_set(3, 30); // evicts key 1 (LRU, max_size = 2)
    b.cache_set(1, 10);
    b.cache_set(2, 20);
    b.cache_set(3, 30);

    assert_eq!(a.cache_get(&1), b.cache_get(&1));
    assert_eq!(a.cache_get(&1), None);
    assert_eq!(a.cache_get(&2), b.cache_get(&2));
    assert_eq!(a.cache_get(&3), b.cache_get(&3));
}

#[cfg(feature = "time_stores")]
#[test]
fn ttl_sorted_cache_new_matches_builder() {
    use cached::{CacheTtl, Cached, TtlSortedCache, TtlSortedCacheBuilder};

    let ttl = Duration::from_secs(60);
    let mut a: TtlSortedCache<u32, u32> = TtlSortedCacheBuilder::new()
        .ttl(ttl)
        .max_size(2)
        .build()
        .unwrap();
    let mut b: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(ttl)
        .max_size(2)
        .build()
        .unwrap();

    assert_eq!(a.capacity(), b.capacity());
    assert_eq!(a.capacity(), Some(2));
    assert_eq!(CacheTtl::ttl(&a), CacheTtl::ttl(&b));
    assert_eq!(CacheTtl::ttl(&a), Some(ttl));

    a.cache_set(1, 10);
    b.cache_set(1, 10);
    assert_eq!(a.cache_get(&1), b.cache_get(&1));
}

#[cfg(feature = "time_stores")]
#[test]
fn ttl_sorted_cache_new_initial_capacity_matches_builder() {
    use cached::{Cached, TtlSortedCache, TtlSortedCacheBuilder};

    // `initial_capacity` is a preallocation hint with no public getter (its only
    // externally-observable effect is via `cache_reset`, which shrinks the
    // backing map back to this hint rather than to zero). We cannot compare
    // the raw allocated capacity from outside the crate, but we can prove the
    // option is accepted and wired through identically on both construction
    // paths across a set/reset/refill cycle.
    let ttl = Duration::from_secs(60);
    let mut a: TtlSortedCache<u32, u32> = TtlSortedCacheBuilder::new()
        .ttl(ttl)
        .initial_capacity(64)
        .build()
        .unwrap();
    let mut b: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(ttl)
        .initial_capacity(64)
        .build()
        .unwrap();

    for i in 0..32u32 {
        a.cache_set(i, i * 10);
        b.cache_set(i, i * 10);
    }
    assert_eq!(a.cache_size(), b.cache_size());

    a.cache_reset();
    b.cache_reset();
    assert_eq!(a.cache_size(), b.cache_size());
    assert_eq!(a.cache_size(), 0);

    a.cache_set(1, 100);
    b.cache_set(1, 100);
    assert_eq!(a.cache_get(&1), b.cache_get(&1));
}

// ── Expiring stores (not time_stores-gated) ─────────────────────────────────

#[test]
fn expiring_cache_new_matches_builder() {
    use cached::{Cached, Expires, ExpiringCache, ExpiringCacheBuilder};

    #[derive(Clone, PartialEq, Debug)]
    struct Val(u32);
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let mut a: ExpiringCache<u32, Val> = ExpiringCacheBuilder::new()
        .initial_capacity(4)
        .build()
        .unwrap();
    let mut b: ExpiringCache<u32, Val> = ExpiringCache::builder()
        .initial_capacity(4)
        .build()
        .unwrap();

    assert_eq!(a.cache_capacity(), b.cache_capacity());

    a.cache_set(1, Val(10));
    b.cache_set(1, Val(10));
    assert_eq!(a.cache_get(&1), b.cache_get(&1));
}

#[test]
fn expiring_lru_cache_new_matches_builder() {
    use cached::{Cached, Expires, ExpiringLruCache, ExpiringLruCacheBuilder};

    #[derive(Clone, PartialEq, Debug)]
    struct Val(u32);
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let mut a: ExpiringLruCache<u32, Val> =
        ExpiringLruCacheBuilder::new().max_size(2).build().unwrap();
    let mut b: ExpiringLruCache<u32, Val> =
        ExpiringLruCache::builder().max_size(2).build().unwrap();

    assert_eq!(a.capacity(), b.capacity());
    assert_eq!(a.capacity(), 2);

    a.cache_set(1, Val(10));
    a.cache_set(2, Val(20));
    a.cache_set(3, Val(30)); // evicts key 1 (LRU, max_size = 2)
    b.cache_set(1, Val(10));
    b.cache_set(2, Val(20));
    b.cache_set(3, Val(30));

    assert_eq!(a.cache_get(&1), b.cache_get(&1));
    assert_eq!(a.cache_get(&1), None);
    assert_eq!(a.cache_get(&2), b.cache_get(&2));
    assert_eq!(a.cache_get(&3), b.cache_get(&3));
}

// ── sharded ──────────────────────────────────────────────────────────────────

#[test]
fn sharded_unbound_cache_new_matches_builder() {
    use cached::{ShardedUnboundCache, ShardedUnboundCacheBuilder};

    let a: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCacheBuilder::new().shards(4).build().unwrap();
    let b: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCache::builder().shards(4).build().unwrap();

    assert_eq!(a.shards(), b.shards());
    assert_eq!(a.shards(), 4);

    a.set(1, 10);
    b.set(1, 10);
    assert_eq!(a.get(&1), b.get(&1));
}

#[test]
fn sharded_lru_cache_new_matches_builder() {
    use cached::{ShardedLruCache, ShardedLruCacheBuilder};

    let a: ShardedLruCache<u32, u32> = ShardedLruCacheBuilder::new()
        .max_size(2)
        .shards(1)
        .build()
        .unwrap();
    let b: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(2)
        .shards(1)
        .build()
        .unwrap();

    assert_eq!(a.shards(), b.shards());
    assert_eq!(a.shards(), 1);
    assert_eq!(a.capacity(), b.capacity());

    a.set(1, 10);
    a.set(2, 20);
    a.set(3, 30); // evicts key 1 (LRU, shards = 1, max_size = 2)
    b.set(1, 10);
    b.set(2, 20);
    b.set(3, 30);

    assert_eq!(a.get(&1), b.get(&1));
    assert_eq!(a.get(&1), None);
    assert_eq!(a.get(&2), b.get(&2));
    assert_eq!(a.get(&3), b.get(&3));
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_cache_new_matches_builder() {
    use cached::{ConcurrentCacheTtl, ShardedTtlCache, ShardedTtlCacheBuilder};

    let ttl = Duration::from_secs(60);
    let a: ShardedTtlCache<u32, u32> = ShardedTtlCacheBuilder::new()
        .ttl(ttl)
        .refresh_on_hit(true)
        .shards(4)
        .build()
        .unwrap();
    let b: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(ttl)
        .refresh_on_hit(true)
        .shards(4)
        .build()
        .unwrap();

    assert_eq!(a.shards(), b.shards());
    assert_eq!(a.shards(), 4);
    assert_eq!(ConcurrentCacheTtl::ttl(&a), ConcurrentCacheTtl::ttl(&b));
    assert_eq!(ConcurrentCacheTtl::ttl(&a), Some(ttl));
    assert_eq!(
        ConcurrentCacheTtl::refresh_on_hit(&a),
        ConcurrentCacheTtl::refresh_on_hit(&b)
    );
    assert!(
        ConcurrentCacheTtl::refresh_on_hit(&a),
        "refresh_on_hit(true) set via new() must be wired through to the store"
    );

    a.set(1, 10);
    b.set(1, 10);
    assert_eq!(a.get(&1), b.get(&1));
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_lru_ttl_cache_new_matches_builder() {
    use cached::{ConcurrentCacheTtl, ShardedLruTtlCache, ShardedLruTtlCacheBuilder};

    let ttl = Duration::from_secs(60);
    let a: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCacheBuilder::new()
        .max_size(2)
        .ttl(ttl)
        .refresh_on_hit(true)
        .shards(1)
        .build()
        .unwrap();
    let b: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .max_size(2)
        .ttl(ttl)
        .refresh_on_hit(true)
        .shards(1)
        .build()
        .unwrap();

    assert_eq!(a.shards(), b.shards());
    assert_eq!(a.shards(), 1);
    assert_eq!(a.capacity(), b.capacity());
    assert_eq!(ConcurrentCacheTtl::ttl(&a), ConcurrentCacheTtl::ttl(&b));
    assert_eq!(ConcurrentCacheTtl::ttl(&a), Some(ttl));
    assert_eq!(
        ConcurrentCacheTtl::refresh_on_hit(&a),
        ConcurrentCacheTtl::refresh_on_hit(&b)
    );
    assert!(
        ConcurrentCacheTtl::refresh_on_hit(&a),
        "refresh_on_hit(true) set via new() must be wired through to the store"
    );

    a.set(1, 10);
    a.set(2, 20);
    a.set(3, 30); // evicts key 1 (LRU, shards = 1, max_size = 2)
    b.set(1, 10);
    b.set(2, 20);
    b.set(3, 30);

    assert_eq!(a.get(&1), b.get(&1));
    assert_eq!(a.get(&1), None);
    assert_eq!(a.get(&2), b.get(&2));
    assert_eq!(a.get(&3), b.get(&3));
}

#[test]
fn sharded_expiring_cache_new_matches_builder() {
    use cached::{Expires, ShardedExpiringCache, ShardedExpiringCacheBuilder};

    #[derive(Clone, PartialEq, Debug)]
    struct Val(u32);
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let a: ShardedExpiringCache<u32, Val> = ShardedExpiringCacheBuilder::new()
        .shards(4)
        .build()
        .unwrap();
    let b: ShardedExpiringCache<u32, Val> =
        ShardedExpiringCache::builder().shards(4).build().unwrap();

    assert_eq!(a.shards(), b.shards());
    assert_eq!(a.shards(), 4);

    a.set(1, Val(10));
    b.set(1, Val(10));
    assert_eq!(a.get(&1), b.get(&1));
}

#[test]
fn sharded_expiring_lru_cache_new_matches_builder() {
    use cached::{Expires, ShardedExpiringLruCache, ShardedExpiringLruCacheBuilder};

    #[derive(Clone, PartialEq, Debug)]
    struct Val(u32);
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let a: ShardedExpiringLruCache<u32, Val> = ShardedExpiringLruCacheBuilder::new()
        .max_size(2)
        .shards(1)
        .build()
        .unwrap();
    let b: ShardedExpiringLruCache<u32, Val> = ShardedExpiringLruCache::builder()
        .max_size(2)
        .shards(1)
        .build()
        .unwrap();

    assert_eq!(a.shards(), b.shards());
    assert_eq!(a.shards(), 1);
    assert_eq!(a.capacity(), b.capacity());

    a.set(1, Val(10));
    a.set(2, Val(20));
    a.set(3, Val(30)); // evicts key 1 (LRU, shards = 1, max_size = 2)
    b.set(1, Val(10));
    b.set(2, Val(20));
    b.set(3, Val(30));

    assert_eq!(a.get(&1), b.get(&1));
    assert_eq!(a.get(&1), None);
    assert_eq!(a.get(&2), b.get(&2));
    assert_eq!(a.get(&3), b.get(&3));
}

// ── on_evict / hasher wiring ─────────────────────────────────────────────────
//
// `on_evict` and `hasher` are configurable on every one of the 13 builders but
// aren't exercised by the equivalence tests above (whose stores are built
// without either). A closure or a `BuildHasher`/`ShardHasher` instance can't be
// compared for equality, so these are checked by wiring *effect* instead:
// - `on_evict`: fire an eviction on both construction paths and assert the
//   recorded (k, v) callback events are identical.
// - `hasher` (sharded only): a custom `ShardHasher`'s effect on shard
//   placement *is* externally observable via `shard_sizes()`, so a
//   deterministic router proves the swapped hasher actually took effect on
//   both paths, not just that the type parameter compiled. For the
//   non-sharded (`BuildHasher`) case, hash choice has no externally-observable
//   effect on a correct `HashMap` beyond performance, so we only prove the
//   `.hasher()` call chains onto a `new()`-built builder and yields a working
//   store, matching the pattern in every `hasher()` doc example.

#[test]
fn lru_cache_new_on_evict_matches_builder() {
    use cached::{Cached, LruCache, LruCacheBuilder};
    use std::sync::{Arc, Mutex};

    let events_a = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
    let events_a2 = events_a.clone();
    let mut a: LruCache<u32, u32> = LruCacheBuilder::new()
        .max_size(2)
        .on_evict(move |k: &u32, v: &u32| events_a2.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();

    let events_b = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
    let events_b2 = events_b.clone();
    let mut b: LruCache<u32, u32> = LruCache::builder()
        .max_size(2)
        .on_evict(move |k: &u32, v: &u32| events_b2.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();

    a.cache_set(1, 10);
    a.cache_set(2, 20);
    a.cache_set(3, 30); // evicts key 1 (LRU, max_size = 2), firing on_evict
    b.cache_set(1, 10);
    b.cache_set(2, 20);
    b.cache_set(3, 30);

    let events_a = events_a.lock().unwrap().clone();
    let events_b = events_b.lock().unwrap().clone();
    assert_eq!(
        events_a, events_b,
        "on_evict wired via new() must fire identically to on_evict wired via builder()"
    );
    assert_eq!(events_a, vec![(1, 10)]);
}

#[test]
fn sharded_lru_cache_new_on_evict_matches_builder() {
    use cached::{ShardedLruCache, ShardedLruCacheBuilder};
    use std::sync::{Arc, Mutex};

    // shards = 1 so all keys share one LRU order and eviction is deterministic.
    let events_a = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
    let events_a2 = events_a.clone();
    let a: ShardedLruCache<u32, u32> = ShardedLruCacheBuilder::new()
        .max_size(2)
        .shards(1)
        .on_evict(move |k: &u32, v: &u32| events_a2.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();

    let events_b = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
    let events_b2 = events_b.clone();
    let b: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(2)
        .shards(1)
        .on_evict(move |k: &u32, v: &u32| events_b2.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();

    a.set(1, 10);
    a.set(2, 20);
    a.set(3, 30); // evicts key 1, firing on_evict
    b.set(1, 10);
    b.set(2, 20);
    b.set(3, 30);

    let events_a = events_a.lock().unwrap().clone();
    let events_b = events_b.lock().unwrap().clone();
    assert_eq!(
        events_a, events_b,
        "on_evict wired via new() must fire identically to on_evict wired via builder()"
    );
    assert_eq!(events_a, vec![(1, 10)]);
}

#[test]
fn unbound_cache_new_hasher_matches_builder() {
    use cached::{Cached, UnboundCache, UnboundCacheBuilder};
    use std::collections::hash_map::RandomState;

    // `.hasher()` changes the builder's type parameter; this proves it's
    // reachable and functional when chained onto a `new()`-built builder,
    // exactly as it is when chained onto a `builder()`-built one.
    let mut a: UnboundCache<u32, u32, RandomState> = UnboundCacheBuilder::<u32, u32>::new()
        .hasher(RandomState::new())
        .build()
        .unwrap();
    let mut b: UnboundCache<u32, u32, RandomState> = UnboundCache::<u32, u32>::builder()
        .hasher(RandomState::new())
        .build()
        .unwrap();

    a.cache_set(1, 10);
    b.cache_set(1, 10);
    assert_eq!(a.cache_get(&1), b.cache_get(&1));
    assert_eq!(a.cache_get(&1), Some(&10));
}

/// Deterministic shard router for `u32` keys: shard selection reads the upper 32
/// bits of the hash (`(hash >> 32) & mask`), so left-shifting the key by 32
/// makes `shard == key & mask` -- i.e. `key % shard_count` for a power-of-two
/// shard count. Lets the test below pin keys to specific shards instead of
/// relying on (and being unable to observe) `DefaultShardHasher`'s distribution.
#[derive(Clone)]
struct KeyIsShardHasher;

impl cached::ShardHasher<u32> for KeyIsShardHasher {
    fn shard_hash(&self, key: &u32) -> u64 {
        (u64::from(*key)) << 32
    }
}

#[test]
fn sharded_unbound_cache_new_hasher_matches_builder() {
    use cached::{ShardedUnboundCache, ShardedUnboundCacheBase, ShardedUnboundCacheBuilder};

    let a: ShardedUnboundCacheBase<u32, u32, KeyIsShardHasher> = ShardedUnboundCacheBuilder::new()
        .shards(4)
        .hasher(KeyIsShardHasher)
        .build()
        .unwrap();
    let b: ShardedUnboundCacheBase<u32, u32, KeyIsShardHasher> = ShardedUnboundCache::builder()
        .shards(4)
        .hasher(KeyIsShardHasher)
        .build()
        .unwrap();

    for k in 0..16u32 {
        a.set(k, k * 10);
        b.set(k, k * 10);
    }

    // `key % 4` sends exactly 4 of the 16 keys to each of the 4 shards; a
    // mismatch here (or with `DefaultShardHasher`'s distribution) would prove
    // the custom hasher set via `new()` didn't actually take effect.
    assert_eq!(a.shard_sizes(), b.shard_sizes());
    assert_eq!(a.shard_sizes(), vec![4, 4, 4, 4]);

    for k in 0..16u32 {
        assert_eq!(a.get(&k), b.get(&k));
    }
}
