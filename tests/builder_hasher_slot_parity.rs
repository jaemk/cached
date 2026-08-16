//! Every cache builder in the crate takes its hasher in the **third** generic slot.
//!
//! `LruTtlCacheBuilder` and `ShardedLruTtlCacheBuilder` also carry an eviction typestate
//! marker (`NoEvict` / `HasEvict`). That marker used to sit in slot 3 with the hasher pushed
//! to slot 4, so the natural spelling `ShardedLruTtlCacheBuilder<K, V, MyHasher>` silently
//! bound `MyHasher` to the typestate slot and produced a confusing mismatch at `.build()`
//! rather than an error at the annotation. The typestate now trails the hasher.
//!
//! These are compile-time assertions: if a builder's parameter order regresses, this file
//! fails to compile.

use std::collections::hash_map::RandomState;

use cached::stores::DefaultShardHasher;

// --- Non-sharded builders: <K, V, S> ------------------------------------------------------

type PinUnbound = cached::UnboundCacheBuilder<u32, u32, RandomState>;
type PinLru = cached::LruCacheBuilder<u32, u32, RandomState>;
type PinExpiring = cached::ExpiringCacheBuilder<u32, u32, RandomState>;
type PinExpiringLru = cached::ExpiringLruCacheBuilder<u32, u32, RandomState>;
#[cfg(feature = "time_stores")]
type PinTtl = cached::TtlCacheBuilder<u32, u32, RandomState>;
#[cfg(feature = "time_stores")]
type PinTtlSorted = cached::TtlSortedCacheBuilder<u32, u32, RandomState>;
#[cfg(feature = "time_stores")]
type PinLruTtl = cached::LruTtlCacheBuilder<u32, u32, RandomState>;

// --- Sharded builders: <K, V, H> ----------------------------------------------------------

type PinShardedUnbound = cached::ShardedUnboundCacheBuilder<u32, u32, DefaultShardHasher>;
type PinShardedLru = cached::ShardedLruCacheBuilder<u32, u32, DefaultShardHasher>;
type PinShardedExpiring = cached::ShardedExpiringCacheBuilder<u32, u32, DefaultShardHasher>;
type PinShardedExpiringLru = cached::ShardedExpiringLruCacheBuilder<u32, u32, DefaultShardHasher>;
#[cfg(feature = "time_stores")]
type PinShardedTtl = cached::ShardedTtlCacheBuilder<u32, u32, DefaultShardHasher>;
#[cfg(feature = "time_stores")]
type PinShardedLruTtl = cached::ShardedLruTtlCacheBuilder<u32, u32, DefaultShardHasher>;

/// Naming the hasher positionally must resolve to the hasher slot on all 13 builders.
#[test]
fn hasher_is_the_third_generic_parameter_on_every_builder() {
    fn accepts<T>() {}

    accepts::<PinUnbound>();
    accepts::<PinLru>();
    accepts::<PinExpiring>();
    accepts::<PinExpiringLru>();
    accepts::<PinShardedUnbound>();
    accepts::<PinShardedLru>();
    accepts::<PinShardedExpiring>();
    accepts::<PinShardedExpiringLru>();

    #[cfg(feature = "time_stores")]
    {
        accepts::<PinTtl>();
        accepts::<PinTtlSorted>();
        accepts::<PinLruTtl>();
        accepts::<PinShardedTtl>();
        accepts::<PinShardedLruTtl>();
    }
}

/// The two typestate-carrying builders must round-trip a custom hasher through `.hasher(..)`
/// and `.on_evict(..)` in either order, with the hasher staying in slot 3 throughout.
#[cfg(feature = "time_stores")]
#[test]
fn typestate_builders_keep_the_hasher_in_slot_three_across_on_evict() {
    use cached::{
        Cached, ConcurrentCached, HasEvict, LruTtlCacheBuilder, ShardedLruTtlCacheBuilder,
    };
    use std::time::Duration;

    // Non-sharded: hasher first, then on_evict.
    let builder: LruTtlCacheBuilder<u32, u32, RandomState, HasEvict> = LruTtlCacheBuilder::new()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .hasher(RandomState::new())
        .on_evict(|_, _| {});
    let mut cache = builder.build().unwrap();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));

    // Non-sharded: on_evict first, then hasher -- the typestate must survive the hasher swap.
    let builder: LruTtlCacheBuilder<u32, u32, RandomState, HasEvict> = LruTtlCacheBuilder::new()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .on_evict(|_, _| {})
        .hasher(RandomState::new());
    let mut cache = builder.build().unwrap();
    cache.cache_set(2, 20);
    assert_eq!(cache.cache_get(&2), Some(&20));

    // Sharded: hasher first, then on_evict.
    let builder: ShardedLruTtlCacheBuilder<u32, u32, DefaultShardHasher, HasEvict> =
        ShardedLruTtlCacheBuilder::new()
            .shards(2)
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .hasher(DefaultShardHasher::default())
            .on_evict(|_, _| {});
    let cache = builder.build().unwrap();
    cache.cache_set(3, 30).unwrap();
    assert_eq!(cache.cache_get(&3).unwrap(), Some(30));

    // Sharded: on_evict first, then hasher.
    let builder: ShardedLruTtlCacheBuilder<u32, u32, DefaultShardHasher, HasEvict> =
        ShardedLruTtlCacheBuilder::new()
            .shards(2)
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .on_evict(|_, _| {})
            .hasher(DefaultShardHasher::default());
    let cache = builder.build().unwrap();
    cache.cache_set(4, 40).unwrap();
    assert_eq!(cache.cache_get(&4).unwrap(), Some(40));
}

/// A function returning a pre-configured builder can name the hasher without naming the
/// eviction typestate. Before the reorder this signature bound `DefaultShardHasher` to the
/// typestate slot and failed at `.build()` with a mismatched-types note.
#[cfg(feature = "time_stores")]
#[test]
fn a_preconfigured_builder_can_name_only_the_hasher() {
    use cached::{ConcurrentCached, ShardedLruTtlCacheBuilder};
    use std::time::Duration;

    fn preconfigured() -> ShardedLruTtlCacheBuilder<u64, u64, DefaultShardHasher> {
        ShardedLruTtlCacheBuilder::new()
            .shards(2)
            .max_size(16)
            .ttl(Duration::from_secs(30))
            .hasher(DefaultShardHasher::default())
    }

    let cache = preconfigured().build().unwrap();
    cache.cache_set(1, 100).unwrap();
    assert_eq!(cache.cache_get(&1).unwrap(), Some(100));
}
