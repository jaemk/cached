//! The six sharded stores expose one generic type each, `ShardedX<K, V, H = DefaultShardHasher>`,
//! mirroring `std::collections::HashMap<K, V, S = RandomState>`. There is no separate
//! `ShardedXBase` struct and no `ShardedX` alias over it.
//!
//! Two properties are pinned here:
//!
//! 1. The public name accepts the hasher as its third type argument, so a
//!    `ShardedX<K, V, MyHasher>` annotation names the type produced by
//!    `ShardedX::builder().hasher(MyHasher)`. Under the old alias-plus-`*Base` shape this file
//!    would not compile: `ShardedX<K, V>` took exactly two parameters.
//! 2. Omitting the third argument keeps `DefaultShardHasher`, so `ShardedX<K, V>` still names the
//!    common case and interoperates with `ShardedX::new(..)` / `ShardedX::builder()`.
//!
//! Routing is asserted, not just the type check: `KeyIsShardHasher` places key `k` in shard
//! `k % shards`, so a hasher that failed to take effect would show a different `shard_sizes()`.

#[cfg(feature = "time_stores")]
use std::time::Duration;

use cached::{
    ConcurrentCachedExt, DefaultShardHasher, ShardHasher, ShardedExpiringCache,
    ShardedExpiringLruCache, ShardedLruCache, ShardedUnboundCache,
};
#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

/// Deterministic shard router for `u32` keys: shard selection reads the upper 32 bits of the
/// hash (`(hash >> 32) & mask`), so left-shifting the key by 32 makes `shard == key & mask`,
/// i.e. `key % shard_count` for a power-of-two shard count.
#[derive(Clone)]
struct KeyIsShardHasher;

impl ShardHasher<u32> for KeyIsShardHasher {
    fn shard_hash(&self, key: &u32) -> u64 {
        u64::from(*key) << 32
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

/// With 4 shards and keys `0..16`, `key % 4` routing puts exactly 4 keys in each shard.
const EVEN_SPREAD: [usize; 4] = [4, 4, 4, 4];

#[test]
fn sharded_unbound_cache_names_its_hasher_as_a_type_argument() {
    let cache: ShardedUnboundCache<u32, u32, KeyIsShardHasher> = ShardedUnboundCache::builder()
        .shards(4)
        .hasher(KeyIsShardHasher)
        .build()
        .expect("build must succeed");

    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(
        cache.shard_sizes(),
        EVEN_SPREAD,
        "the hasher passed to `.hasher()` must drive shard routing"
    );
    // `KeyIsShardHasher` implements `ShardHasher<u32>` only (design 0055), so both the inherent
    // owned-key `get(&K)` and the `ConcurrentCachedExt::get` trait path resolve on this store;
    // assert through both to exercise `shard_of_borrowed` at `Q = K` as well as the trait path.
    assert_eq!(cache.get(&7), Some(70));
    assert_eq!(ConcurrentCachedExt::get(&cache, &7).unwrap(), Some(70));
}

#[test]
fn sharded_lru_cache_names_its_hasher_as_a_type_argument() {
    let cache: ShardedLruCache<u32, u32, KeyIsShardHasher> = ShardedLruCache::builder()
        .shards(4)
        .max_size(64)
        .hasher(KeyIsShardHasher)
        .build()
        .expect("build must succeed");

    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.get(&7), Some(70));
    assert_eq!(ConcurrentCachedExt::get(&cache, &7).unwrap(), Some(70));
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_cache_names_its_hasher_as_a_type_argument() {
    let cache: ShardedTtlCache<u32, u32, KeyIsShardHasher> = ShardedTtlCache::builder()
        .shards(4)
        .ttl(Duration::from_secs(600))
        .hasher(KeyIsShardHasher)
        .build()
        .expect("build must succeed");

    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.get(&7), Some(70));
    assert_eq!(ConcurrentCachedExt::get(&cache, &7).unwrap(), Some(70));
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_lru_ttl_cache_names_its_hasher_as_a_type_argument() {
    let cache: ShardedLruTtlCache<u32, u32, KeyIsShardHasher> = ShardedLruTtlCache::builder()
        .shards(4)
        .max_size(64)
        .ttl(Duration::from_secs(600))
        .hasher(KeyIsShardHasher)
        .build()
        .expect("build must succeed");

    for k in 0..16u32 {
        cache.set(k, k * 10);
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.get(&7), Some(70));
    assert_eq!(ConcurrentCachedExt::get(&cache, &7).unwrap(), Some(70));
}

#[test]
fn sharded_expiring_cache_names_its_hasher_as_a_type_argument() {
    let cache: ShardedExpiringCache<u32, Val, KeyIsShardHasher> = ShardedExpiringCache::builder()
        .shards(4)
        .hasher(KeyIsShardHasher)
        .build()
        .expect("build must succeed");

    for k in 0..16u32 {
        cache.set(k, Val(k * 10));
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.get(&7), Some(Val(70)));
    assert_eq!(ConcurrentCachedExt::get(&cache, &7).unwrap(), Some(Val(70)));
}

#[test]
fn sharded_expiring_lru_cache_names_its_hasher_as_a_type_argument() {
    let cache: ShardedExpiringLruCache<u32, Val, KeyIsShardHasher> =
        ShardedExpiringLruCache::builder()
            .shards(4)
            .max_size(64)
            .hasher(KeyIsShardHasher)
            .build()
            .expect("build must succeed");

    for k in 0..16u32 {
        cache.set(k, Val(k * 10));
    }
    assert_eq!(cache.shard_sizes(), EVEN_SPREAD);
    assert_eq!(cache.get(&7), Some(Val(70)));
    assert_eq!(ConcurrentCachedExt::get(&cache, &7).unwrap(), Some(Val(70)));
}

/// The hasher parameter defaults, so the two-argument spelling still names the default-hasher
/// store and unifies with what `new()` / `builder()` return.
#[test]
fn omitting_the_hasher_argument_yields_the_default_shard_hasher() {
    let from_new: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::new();
    let spelled_out: ShardedUnboundCache<u32, u32, DefaultShardHasher> = from_new.clone();
    spelled_out.set(1, 10);
    assert_eq!(
        from_new.get(&1),
        Some(10),
        "the two spellings must name the same type (an Arc-share clone is visible through both)"
    );

    let bounded: ShardedLruCache<u32, u32> = ShardedLruCache::new(64);
    let _: ShardedLruCache<u32, u32, DefaultShardHasher> = bounded;

    let unbounded_expiring: ShardedExpiringCache<u32, Val> = ShardedExpiringCache::new();
    let _: ShardedExpiringCache<u32, Val, DefaultShardHasher> = unbounded_expiring;
}

/// A default-hasher store and a custom-hasher store of the same `K`/`V` are distinct types, so
/// generic code can be written over the hasher parameter without erasing it.
#[test]
fn the_hasher_parameter_is_visible_to_generic_code() {
    fn total_len<H: ShardHasher<u32>>(cache: &ShardedUnboundCache<u32, u32, H>) -> usize {
        cache.len()
    }

    let default_hashed = ShardedUnboundCache::<u32, u32>::new();
    let custom_hashed: ShardedUnboundCache<u32, u32, KeyIsShardHasher> =
        ShardedUnboundCache::builder()
            .shards(4)
            .hasher(KeyIsShardHasher)
            .build()
            .expect("build must succeed");

    for k in 0..8u32 {
        default_hashed.set(k, k);
        custom_hashed.set(k, k);
    }
    assert_eq!(total_len(&default_hashed), 8);
    assert_eq!(total_len(&custom_hashed), 8);
}
