//! Coverage closing gaps the implementor of the `ShardedXBase` -> `ShardedX<K, V, H =
//! DefaultShardHasher>` collapse flagged (or an outside review turned up) after landing
//! `tests/v3_sharded_hasher_type_param.rs`. That file pins the type-parameter shape; this
//! file pins behavior the mechanical rename could have silently dropped or narrowed:
//!
//! 1. `ShardedLruTtlCacheBuilder`'s `HasEvict` typestate `build`/`copy_from` overloads, combined
//!    with a custom hasher, on the collapsed type (only the `NoEvict` path was covered before).
//! 2. `copy_from<H2>` actually re-hashes through the *target* cache's hasher `H`, not the
//!    source's `H2` -- proven by using a source hasher that collapses every key onto shard 0
//!    and a target hasher that spreads evenly, on all six sharded stores.
//! 3. `ShardedUnboundCache`/`ShardedExpiringCache`'s `impl<K, V> Default for ShardedX<K, V>`
//!    is reachable through an explicit `ShardedX<K, V, DefaultShardHasher>` annotation, and the
//!    resulting cache actually works (not just type-checks).
//! 4. `deep_clone`, `metrics()`, and `shard_sizes()` are reachable -- and behave -- on a
//!    custom-hasher instantiation of every one of the six sharded stores, not just the
//!    default-hasher one exercised by the store's own inline unit tests.

#[cfg(feature = "time_stores")]
use std::time::Duration;

use cached::{
    ConcurrentCachedExt, DefaultShardHasher, ShardHasher, ShardedExpiringCache,
    ShardedExpiringLruCache, ShardedLruCache, ShardedUnboundCache,
};
#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

/// Deterministic shard router for `u32` keys: shard selection reads the upper 32 bits of the
/// hash (`(hash >> 32) & mask`), so left-shifting the key by 32 makes `shard == key % shards`
/// for a power-of-two shard count.
#[derive(Clone)]
struct EvenSpreadHasher;

impl ShardHasher<u32> for EvenSpreadHasher {
    fn shard_hash(&self, key: &u32) -> u64 {
        u64::from(*key) << 32
    }
}

/// A pathological hasher whose upper 32 bits are always zero, so every key routes to shard 0
/// regardless of shard count. Used as the *source* side of cross-hasher `copy_from` tests: if
/// `copy_from` accidentally preserved the source's routing (or its raw per-shard layout)
/// instead of re-hashing through the target's `H`, the target would inherit this lopsided
/// distribution instead of `EvenSpreadHasher`'s even one.
#[derive(Clone)]
struct AllShardZeroHasher;

impl ShardHasher<u32> for AllShardZeroHasher {
    fn shard_hash(&self, _key: &u32) -> u64 {
        0
    }
}

/// A never-expiring value for the per-value-expiry stores.
#[derive(Clone, Debug, PartialEq)]
struct Val(u32);

impl cached::Expires for Val {
    fn is_expired(&self) -> bool {
        false
    }
}

/// With 4 shards and keys `0..16`, `EvenSpreadHasher` routing puts exactly 4 keys in each shard.
const EVEN_SPREAD: [usize; 4] = [4, 4, 4, 4];

// ---------------------------------------------------------------------------------------------
// 1. `ShardedLruTtlCacheBuilder`'s `HasEvict` typestate path combined with a custom hasher.
// ---------------------------------------------------------------------------------------------

/// `.on_evict(..)` flips the builder's typestate to `HasEvict`, which resolves to a *different*
/// `build()` impl (`ShardedLruTtlCacheBuilder<K, V, H, HasEvict>`) than the untouched-typestate
/// `NoEvict` path the rest of this crate's own tests exercise on a collapsed-type annotation.
/// Chaining `.hasher(..)` either before or after `.on_evict(..)` must resolve to the same
/// `ShardedLruTtlCache<K, V, H>` and the callback must actually fire, keyed correctly through
/// the custom hasher's shard routing (not just compile).
#[cfg(feature = "time_stores")]
#[test]
fn sharded_lru_ttl_has_evict_typestate_combines_with_custom_hasher() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // `.hasher(..)` first, `.on_evict(..)` second: the routing check needs no callback.
    let cache: ShardedLruTtlCache<u32, u32, EvenSpreadHasher> = ShardedLruTtlCache::builder()
        .shards(4)
        .max_size(64)
        .ttl(Duration::from_secs(600))
        .hasher(EvenSpreadHasher)
        .on_evict(|_, _| {})
        .build()
        .expect("HasEvict build with a custom hasher must succeed");

    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(
        cache.shard_sizes(),
        EVEN_SPREAD,
        "the hasher passed before .on_evict(..) must still drive shard routing"
    );

    // Force an LRU-capacity eviction on a single shard so on_evict fires deterministically.
    let evicted: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::default();
    let evicted_cb = Arc::clone(&evicted);
    let small: ShardedLruTtlCache<u32, u32, EvenSpreadHasher> = ShardedLruTtlCache::builder()
        .shards(1)
        .max_size(2)
        .ttl(Duration::from_secs(600))
        .hasher(EvenSpreadHasher)
        .on_evict(move |k, v| evicted_cb.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();
    small.set(1, 100);
    small.set(2, 200);
    small.set(3, 300); // evicts key 1 (LRU)
    assert_eq!(
        evicted.lock().unwrap().as_slice(),
        &[(1, 100)],
        "on_evict must fire for the capacity-evicted entry on the custom-hasher instantiation"
    );

    // `.on_evict(..)` first, `.hasher(..)` second -- the reverse chaining order must also
    // compile and produce the same collapsed type.
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = Arc::clone(&count);
    let reordered: ShardedLruTtlCache<u32, u32, EvenSpreadHasher> = ShardedLruTtlCache::builder()
        .shards(1)
        .max_size(1)
        .ttl(Duration::from_secs(600))
        .on_evict(move |_, _| {
            count2.fetch_add(1, Ordering::Relaxed);
        })
        .hasher(EvenSpreadHasher)
        .build()
        .unwrap();
    reordered.set(1, 1);
    reordered.set(2, 2); // evicts key 1
    assert_eq!(count.load(Ordering::Relaxed), 1);
}

// ---------------------------------------------------------------------------------------------
// 2. `copy_from<H2>` re-hashes through the target's `H`, not the source's `H2`.
// ---------------------------------------------------------------------------------------------

#[test]
fn sharded_unbound_copy_from_rehashes_through_target_hasher() {
    let source: ShardedUnboundCache<u32, u32, AllShardZeroHasher> = ShardedUnboundCache::builder()
        .shards(4)
        .hasher(AllShardZeroHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        source.set(k, k * 10);
    }
    assert_eq!(
        source.shard_sizes(),
        [16, 0, 0, 0],
        "sanity: AllShardZeroHasher must pile every key onto shard 0"
    );

    let target: ShardedUnboundCache<u32, u32, EvenSpreadHasher> = ShardedUnboundCache::builder()
        .shards(4)
        .hasher(EvenSpreadHasher)
        .copy_from(&source)
        .expect("copy_from across distinct concrete hashers must succeed");

    assert_eq!(
        target.shard_sizes(),
        EVEN_SPREAD,
        "copy_from must re-hash every entry through the TARGET's hasher, not preserve the \
         source's (all-shard-0) layout"
    );
    // `EvenSpreadHasher` / `AllShardZeroHasher` implement `ShardHasher<u32>` only, so the
    // inherent owned-key lookup (`&K`) works on stores built with them (design 0055); exercise
    // it alongside `ConcurrentCachedExt::get`'s trait path.
    for k in 0..16u32 {
        assert_eq!(target.get(&k), Some(k * 10));
        assert_eq!(ConcurrentCachedExt::get(&target, &k).unwrap(), Some(k * 10));
    }
}

#[test]
fn sharded_lru_copy_from_rehashes_through_target_hasher() {
    let source: ShardedLruCache<u32, u32, AllShardZeroHasher> = ShardedLruCache::builder()
        .shards(4)
        .max_size(64)
        .hasher(AllShardZeroHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        source.set(k, k * 10);
    }

    let target: ShardedLruCache<u32, u32, EvenSpreadHasher> = ShardedLruCache::builder()
        .shards(4)
        .max_size(64)
        .hasher(EvenSpreadHasher)
        .copy_from(&source)
        .expect("copy_from across distinct concrete hashers must succeed");

    assert_eq!(target.shard_sizes(), EVEN_SPREAD);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_copy_from_rehashes_through_target_hasher`.
    for k in 0..16u32 {
        assert_eq!(target.get(&k), Some(k * 10));
        assert_eq!(ConcurrentCachedExt::get(&target, &k).unwrap(), Some(k * 10));
    }
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_copy_from_rehashes_through_target_hasher() {
    let source: ShardedTtlCache<u32, u32, AllShardZeroHasher> = ShardedTtlCache::builder()
        .shards(4)
        .ttl(Duration::from_secs(600))
        .hasher(AllShardZeroHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        source.set(k, k * 10);
    }

    let target: ShardedTtlCache<u32, u32, EvenSpreadHasher> = ShardedTtlCache::builder()
        .shards(4)
        .ttl(Duration::from_secs(600))
        .hasher(EvenSpreadHasher)
        .copy_from(&source)
        .expect("copy_from across distinct concrete hashers must succeed");

    assert_eq!(target.shard_sizes(), EVEN_SPREAD);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_copy_from_rehashes_through_target_hasher`.
    for k in 0..16u32 {
        assert_eq!(target.get(&k), Some(k * 10));
        assert_eq!(ConcurrentCachedExt::get(&target, &k).unwrap(), Some(k * 10));
    }
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_lru_ttl_copy_from_rehashes_through_target_hasher() {
    let source: ShardedLruTtlCache<u32, u32, AllShardZeroHasher> = ShardedLruTtlCache::builder()
        .shards(4)
        .max_size(64)
        .ttl(Duration::from_secs(600))
        .hasher(AllShardZeroHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        source.set(k, k * 10);
    }

    let target: ShardedLruTtlCache<u32, u32, EvenSpreadHasher> = ShardedLruTtlCache::builder()
        .shards(4)
        .max_size(64)
        .ttl(Duration::from_secs(600))
        .hasher(EvenSpreadHasher)
        .copy_from(&source)
        .expect("copy_from across distinct concrete hashers must succeed");

    assert_eq!(target.shard_sizes(), EVEN_SPREAD);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_copy_from_rehashes_through_target_hasher`.
    for k in 0..16u32 {
        assert_eq!(target.get(&k), Some(k * 10));
        assert_eq!(ConcurrentCachedExt::get(&target, &k).unwrap(), Some(k * 10));
    }
}

#[test]
fn sharded_expiring_copy_from_rehashes_through_target_hasher() {
    let source: ShardedExpiringCache<u32, Val, AllShardZeroHasher> =
        ShardedExpiringCache::builder()
            .shards(4)
            .hasher(AllShardZeroHasher)
            .build()
            .unwrap();
    for k in 0..16u32 {
        source.set(k, Val(k * 10));
    }

    let target: ShardedExpiringCache<u32, Val, EvenSpreadHasher> = ShardedExpiringCache::builder()
        .shards(4)
        .hasher(EvenSpreadHasher)
        .copy_from(&source)
        .expect("copy_from across distinct concrete hashers must succeed");

    assert_eq!(target.shard_sizes(), EVEN_SPREAD);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_copy_from_rehashes_through_target_hasher`.
    for k in 0..16u32 {
        assert_eq!(target.get(&k), Some(Val(k * 10)));
        assert_eq!(
            ConcurrentCachedExt::get(&target, &k).unwrap(),
            Some(Val(k * 10))
        );
    }
}

#[test]
fn sharded_expiring_lru_copy_from_rehashes_through_target_hasher() {
    let source: ShardedExpiringLruCache<u32, Val, AllShardZeroHasher> =
        ShardedExpiringLruCache::builder()
            .shards(4)
            .max_size(64)
            .hasher(AllShardZeroHasher)
            .build()
            .unwrap();
    for k in 0..16u32 {
        source.set(k, Val(k * 10));
    }

    let target: ShardedExpiringLruCache<u32, Val, EvenSpreadHasher> =
        ShardedExpiringLruCache::builder()
            .shards(4)
            .max_size(64)
            .hasher(EvenSpreadHasher)
            .copy_from(&source)
            .expect("copy_from across distinct concrete hashers must succeed");

    assert_eq!(target.shard_sizes(), EVEN_SPREAD);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_copy_from_rehashes_through_target_hasher`.
    for k in 0..16u32 {
        assert_eq!(target.get(&k), Some(Val(k * 10)));
        assert_eq!(
            ConcurrentCachedExt::get(&target, &k).unwrap(),
            Some(Val(k * 10))
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 3. `impl<K, V> Default for ShardedX<K, V>` reachable through an explicit
//    `ShardedX<K, V, DefaultShardHasher>` annotation, and it produces a working cache.
// ---------------------------------------------------------------------------------------------

#[test]
fn sharded_unbound_cache_default_resolves_through_explicit_hasher_annotation() {
    let cache: ShardedUnboundCache<u32, u32, DefaultShardHasher> = Default::default();
    assert_eq!(cache.len(), 0);
    cache.set(1, 10);
    assert_eq!(cache.get(&1), Some(10));

    // The two-argument spelling must be the exact same `Default` impl.
    let two_arg: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::default();
    two_arg.set(2, 20);
    assert_eq!(two_arg.get(&2), Some(20));
}

#[test]
fn sharded_expiring_cache_default_resolves_through_explicit_hasher_annotation() {
    let cache: ShardedExpiringCache<u32, Val, DefaultShardHasher> = Default::default();
    assert_eq!(cache.len(), 0);
    cache.set(1, Val(10));
    assert_eq!(cache.get(&1), Some(Val(10)));

    let two_arg: ShardedExpiringCache<u32, Val> = ShardedExpiringCache::default();
    two_arg.set(2, Val(20));
    assert_eq!(two_arg.get(&2), Some(Val(20)));
}

// ---------------------------------------------------------------------------------------------
// 4. `deep_clone`, `metrics()`, and `shard_sizes()` reachable and behaviorally correct on a
//    custom-hasher instantiation of every one of the six sharded stores.
// ---------------------------------------------------------------------------------------------

#[test]
fn sharded_unbound_cache_deep_clone_metrics_shard_sizes_on_custom_hasher() {
    let cache: ShardedUnboundCache<u32, u32, EvenSpreadHasher> = ShardedUnboundCache::builder()
        .shards(4)
        .hasher(EvenSpreadHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.metrics().entry_count, Some(16));

    let clone = cache.deep_clone();
    cache.set(0, 999);
    // `EvenSpreadHasher` implements `ShardHasher<u32>` only, so the inherent owned-key lookup
    // (`&K`) works on stores built with it (design 0055); exercise it alongside
    // `ConcurrentCachedExt::get`'s trait path.
    assert_eq!(
        clone.get(&0),
        Some(0),
        "deep_clone must be an independent snapshot on a custom-hasher instantiation"
    );
    assert_eq!(
        ConcurrentCachedExt::get(&clone, &0).unwrap(),
        Some(0),
        "deep_clone must be an independent snapshot on a custom-hasher instantiation"
    );
    assert_eq!(clone.shard_sizes(), EVEN_SPREAD);
}

#[test]
fn sharded_lru_deep_clone_metrics_shard_sizes_on_custom_hasher() {
    let cache: ShardedLruCache<u32, u32, EvenSpreadHasher> = ShardedLruCache::builder()
        .shards(4)
        .max_size(64)
        .hasher(EvenSpreadHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.metrics().entry_count, Some(16));
    assert_eq!(cache.metrics().capacity, Some(64));

    let clone = cache.deep_clone();
    cache.set(0, 999);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_cache_deep_clone_metrics_shard_sizes_on_custom_hasher`.
    assert_eq!(clone.get(&0), Some(0));
    assert_eq!(ConcurrentCachedExt::get(&clone, &0).unwrap(), Some(0));
    assert_eq!(clone.shard_sizes(), EVEN_SPREAD);
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_deep_clone_metrics_shard_sizes_on_custom_hasher() {
    let cache: ShardedTtlCache<u32, u32, EvenSpreadHasher> = ShardedTtlCache::builder()
        .shards(4)
        .ttl(Duration::from_secs(600))
        .hasher(EvenSpreadHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.metrics().entry_count, Some(16));

    let clone = cache.deep_clone();
    cache.set(0, 999);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_cache_deep_clone_metrics_shard_sizes_on_custom_hasher`.
    assert_eq!(clone.get(&0), Some(0));
    assert_eq!(ConcurrentCachedExt::get(&clone, &0).unwrap(), Some(0));
    assert_eq!(clone.shard_sizes(), EVEN_SPREAD);
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_lru_ttl_deep_clone_metrics_shard_sizes_on_custom_hasher() {
    let cache: ShardedLruTtlCache<u32, u32, EvenSpreadHasher> = ShardedLruTtlCache::builder()
        .shards(4)
        .max_size(64)
        .ttl(Duration::from_secs(600))
        .hasher(EvenSpreadHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.metrics().entry_count, Some(16));
    assert_eq!(cache.metrics().capacity, Some(64));

    let clone = cache.deep_clone();
    cache.set(0, 999);
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_cache_deep_clone_metrics_shard_sizes_on_custom_hasher`.
    assert_eq!(clone.get(&0), Some(0));
    assert_eq!(ConcurrentCachedExt::get(&clone, &0).unwrap(), Some(0));
    assert_eq!(clone.shard_sizes(), EVEN_SPREAD);
}

#[test]
fn sharded_expiring_cache_deep_clone_metrics_shard_sizes_on_custom_hasher() {
    let cache: ShardedExpiringCache<u32, Val, EvenSpreadHasher> = ShardedExpiringCache::builder()
        .shards(4)
        .hasher(EvenSpreadHasher)
        .build()
        .unwrap();
    for k in 0..16u32 {
        cache.set(k, Val(k * 10));
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.metrics().entry_count, Some(16));

    let clone = cache.deep_clone();
    cache.set(0, Val(999));
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_cache_deep_clone_metrics_shard_sizes_on_custom_hasher`.
    assert_eq!(clone.get(&0), Some(Val(0)));
    assert_eq!(ConcurrentCachedExt::get(&clone, &0).unwrap(), Some(Val(0)));
    assert_eq!(clone.shard_sizes(), EVEN_SPREAD);
}

#[test]
fn sharded_expiring_lru_cache_deep_clone_metrics_shard_sizes_on_custom_hasher() {
    let cache: ShardedExpiringLruCache<u32, Val, EvenSpreadHasher> =
        ShardedExpiringLruCache::builder()
            .shards(4)
            .max_size(64)
            .hasher(EvenSpreadHasher)
            .build()
            .unwrap();
    for k in 0..16u32 {
        cache.set(k, Val(k * 10));
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.metrics().entry_count, Some(16));
    assert_eq!(cache.metrics().capacity, Some(64));

    let clone = cache.deep_clone();
    cache.set(0, Val(999));
    // Inherent owned-key lookup alongside the trait path, for the reason spelled out in
    // `sharded_unbound_cache_deep_clone_metrics_shard_sizes_on_custom_hasher`.
    assert_eq!(clone.get(&0), Some(Val(0)));
    assert_eq!(ConcurrentCachedExt::get(&clone, &0).unwrap(), Some(Val(0)));
    assert_eq!(clone.shard_sizes(), EVEN_SPREAD);
}
