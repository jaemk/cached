//! A `std::hash::BuildHasher` is a `ShardHasher`.
//!
//! `ShardHasher<K>` used to be unrelated to `std::hash::BuildHasher`, so a hasher value that the
//! single-owner builders accept (`LruCacheBuilder::hasher`, `UnboundCacheBuilder::hasher`, ...)
//! was rejected by the sharded builders with a "trait bound `RandomState: ShardHasher<K>` is not
//! satisfied" error. A blanket impl over every `BuildHasher + Clone + Send + Sync + 'static`
//! closes that gap in both directions.
//!
//! What is pinned here:
//!
//! 1. `std::hash::RandomState` and `ahash::RandomState` satisfy `ShardHasher<K>` and are accepted
//!    by `ShardedLruCache::builder().hasher(..)` (a compile-level property: these tests fail to
//!    build without the blanket impl).
//! 2. `DefaultShardHasher` travels the same road in reverse: it implements `BuildHasher`, so it
//!    is a valid hash builder for a non-sharded store and for a plain `HashMap`.
//! 3. Routing through the blanket impl actually spreads keys. Shard selection reads the **upper**
//!    32 bits of the hash, so a hasher that only varied the low half would pile every key onto
//!    shard 0; `shard_sizes()` shows the real spread.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};

use cached::{Cached, DefaultShardHasher, ShardHasher, ShardedLruCache, UnboundCache};

/// Shard count and key count for the distribution checks. 4096 keys over 16 shards is a mean of
/// 256 per shard with a standard deviation of about 15.5, so the `[100, 600]` window asserted
/// below is roughly ten standard deviations wide on either side: it cannot flake on a
/// well-distributed hasher, but a hasher whose upper 32 bits are constant collapses to
/// `[4096, 0, 0, ...]` (clamped by the per-shard cap) and fails immediately.
const SHARDS: usize = 16;
const KEYS: u64 = 4096;
const MIN_PER_SHARD: usize = 100;
const MAX_PER_SHARD: usize = 600;

/// Accepts anything the sharded builders accept. Instantiating it is the compile-level
/// assertion that the blanket impl exists.
fn assert_is_shard_hasher<K: ?Sized, H: ShardHasher<K>>(_hasher: H) {}

/// Build a 16-shard LRU over the given hasher, fill it, and report the per-shard occupancy.
/// The capacity is deliberately four times the key count so that eviction cannot mask an
/// uneven distribution.
fn shard_occupancy<H: ShardHasher<u64>>(hasher: H) -> Vec<usize> {
    let cache = ShardedLruCache::<u64, u64>::builder()
        .shards(SHARDS)
        .max_size(SHARDS * 1024)
        .hasher(hasher)
        .build()
        .expect("build must succeed");

    for k in 0..KEYS {
        cache.set(k, k);
    }

    assert_eq!(
        cache.len(),
        KEYS as usize,
        "capacity is 4x the key count, so nothing should have been evicted"
    );
    cache.shard_sizes()
}

fn assert_well_spread(sizes: &[usize], label: &str) {
    assert_eq!(sizes.len(), SHARDS, "{label}: unexpected shard count");
    assert_eq!(
        sizes.iter().sum::<usize>(),
        KEYS as usize,
        "{label}: shard sizes must account for every key"
    );
    for (shard, &count) in sizes.iter().enumerate() {
        assert!(
            (MIN_PER_SHARD..=MAX_PER_SHARD).contains(&count),
            "{label}: shard {shard} holds {count} of {KEYS} keys, outside \
             [{MIN_PER_SHARD}, {MAX_PER_SHARD}] -- keys are not spread across shards"
        );
    }
}

#[test]
fn std_random_state_satisfies_shard_hasher() {
    assert_is_shard_hasher::<u64, _>(RandomState::new());
    assert_is_shard_hasher::<String, _>(RandomState::new());
    assert_is_shard_hasher::<str, _>(RandomState::new());
    assert_is_shard_hasher::<[u8], _>(RandomState::new());
}

#[test]
fn sharded_builder_accepts_std_random_state() {
    let cache = ShardedLruCache::<u64, u64>::builder()
        .max_size(1024)
        .hasher(RandomState::new())
        .build()
        .expect("build must succeed");

    assert_eq!(cache.set(1, 10), None);
    assert_eq!(cache.get(&1), Some(10));
}

#[test]
fn std_random_state_spreads_keys_across_shards() {
    assert_well_spread(&shard_occupancy(RandomState::new()), "std RandomState");
}

#[cfg(feature = "ahash")]
#[test]
fn ahash_random_state_satisfies_shard_hasher() {
    assert_is_shard_hasher::<u64, _>(ahash::RandomState::new());
    assert_is_shard_hasher::<String, _>(ahash::RandomState::new());
}

#[cfg(feature = "ahash")]
#[test]
fn sharded_builder_accepts_ahash_random_state() {
    let cache = ShardedLruCache::<u64, u64>::builder()
        .max_size(1024)
        .hasher(ahash::RandomState::new())
        .build()
        .expect("build must succeed");

    assert_eq!(cache.set(1, 10), None);
    assert_eq!(cache.get(&1), Some(10));
}

#[cfg(feature = "ahash")]
#[test]
fn ahash_random_state_spreads_keys_across_shards() {
    assert_well_spread(
        &shard_occupancy(ahash::RandomState::new()),
        "ahash RandomState",
    );
}

/// The upper half of the hash is what shard selection consumes, so check it directly rather
/// than only through occupancy: over 4096 keys a `BuildHasher` reached through the blanket impl
/// must produce many distinct high halves.
#[test]
fn shard_hash_varies_in_the_upper_32_bits() {
    fn distinct_high_halves<H: ShardHasher<u64>>(hasher: &H) -> usize {
        (0..KEYS)
            .map(|k| (hasher.shard_hash(&k) >> 32) as u32)
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    let std_state = RandomState::new();
    assert!(
        distinct_high_halves(&std_state) > KEYS as usize / 2,
        "std RandomState left the upper 32 bits nearly constant"
    );

    #[cfg(feature = "ahash")]
    {
        let ahash_state = ahash::RandomState::new();
        assert!(
            distinct_high_halves(&ahash_state) > KEYS as usize / 2,
            "ahash RandomState left the upper 32 bits nearly constant"
        );
    }
}

/// The same hasher value serves both cache families: one `RandomState` clone feeds a sharded
/// store's shard router and a single-owner store's map. This is the interop that the missing
/// blanket impl used to block.
#[test]
fn one_hasher_value_serves_both_cache_families() {
    let hasher = RandomState::new();

    let sharded = ShardedLruCache::<u64, u64>::builder()
        .max_size(1024)
        .hasher(hasher.clone())
        .build()
        .expect("sharded build must succeed");
    let unsharded = UnboundCache::<u64, u64>::builder()
        .hasher(hasher)
        .build()
        .expect("single-owner build must succeed");

    sharded.set(7, 70);
    assert_eq!(sharded.get(&7), Some(70));
    assert_eq!(unsharded.cache_size(), 0);
}

/// `DefaultShardHasher` reaches `ShardHasher` through `BuildHasher`, which makes it a hash
/// builder in its own right: usable by the single-owner builders and by `HashMap`.
#[test]
fn default_shard_hasher_is_usable_as_a_build_hasher() {
    let mut map: HashMap<u64, u64, DefaultShardHasher> =
        HashMap::with_hasher(DefaultShardHasher::new());
    map.insert(1, 10);
    assert_eq!(map.get(&1), Some(&10));

    let cache = UnboundCache::<u64, u64>::builder()
        .hasher(DefaultShardHasher::new())
        .build()
        .expect("build must succeed");
    assert_eq!(cache.cache_size(), 0);

    // `shard_hash` is the blanket impl, not a second, separate impl. It builds a `Hasher`, feeds
    // the key to it and finishes it, which is exactly what the provided `BuildHasher::hash_one`
    // body does; `DefaultShardHasher` does not override `hash_one`, so the two agree here.
    // Routing does not go through `hash_one`: `hash_one` may dispatch on its static type
    // argument, which is what would let an owned key and a borrowed one reach different shards.
    let hasher = DefaultShardHasher::new();
    assert_eq!(hasher.shard_hash(&99u64), hasher.hash_one(99u64));
}
