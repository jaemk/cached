//! Pin the capacity semantics of the LRU-bounded sharded stores built with
//! `.shards(4).max_size(8)`.
//!
//! The key behavior asserted here:
//!
//! 1. `capacity()` is NOT `max_size`. It equals `shards * per_shard_cap`, where
//!    `per_shard_cap = max(max_size.div_ceil(shards), 16)`. With `shards=4` and
//!    `max_size=8`, that is `max(2, 16) = 16` per shard, so `total = 64`.
//!
//! 2. The store can hold **more than `max_size` entries** because eviction is
//!    enforced per-shard at the per-shard cap, not at the total `max_size`. With
//!    `max_size=8` and `shards=4`, inserting 9 entries (max_size+1) must not
//!    evict anything (total capacity is 64).
//!
//! Stores covered: `ShardedLruCache`, `ShardedLruTtlCache` (feature-gated).

use cached::{ConcurrentCached, ShardedLruCache};

// ── ShardedLruCache ──────────────────────────────────────────────────────────

/// `builder().shards(4).max_size(8)` must yield `capacity() == 64`.
///
/// Per-shard cap: `max(8.div_ceil(4), 16) = max(2, 16) = 16`.
/// Total: `4 shards * 16 = 64`.
#[test]
fn sharded_lru_builder_shards4_max8_capacity_is_64() {
    let cache = ShardedLruCache::<u32, u32>::builder()
        .shards(4)
        .max_size(8)
        .build()
        .expect("build must succeed");

    assert_eq!(
        cache.capacity(),
        64,
        "capacity must be shards * per_shard_cap = 4 * 16 = 64"
    );
    assert_eq!(
        cache.shards(),
        4,
        "shard count must be exactly 4 (4 is already a power of two)"
    );
}

/// With `shards(4).max_size(8)` the effective capacity is 64, so inserting
/// more than `max_size` (8) entries must not evict anything.
#[test]
fn sharded_lru_builder_holds_more_than_max_size_entries() {
    let max_size: u32 = 8;
    let cache = ShardedLruCache::<u32, u32>::builder()
        .shards(4)
        .max_size(max_size as usize)
        .build()
        .expect("build must succeed");

    // Insert max_size + 1 entries.
    let insert_count: u32 = max_size + 1;
    for i in 0..insert_count {
        ConcurrentCached::cache_set(&cache, i, i * 10).expect("insert must succeed");
    }

    // Every entry must still be present -- no eviction should have occurred
    // because the total capacity (64) is far larger than max_size (8).
    assert_eq!(
        cache.len(),
        insert_count as usize,
        "all inserted entries must be present: max_size is per-shard, not a global cap"
    );

    for i in 0..insert_count {
        assert_eq!(
            ConcurrentCached::cache_get(&cache, &i).expect("cache_get must succeed"),
            Some(i * 10),
            "entry {} must be retrievable",
            i
        );
    }
}

// ── ShardedLruTtlCache ───────────────────────────────────────────────────────

#[cfg(feature = "time_stores")]
mod lru_ttl {
    use cached::time::Duration;
    use cached::{ConcurrentCached, ShardedLruTtlCache};

    /// `builder().shards(4).max_size(8).ttl(...)` must yield `capacity() == 64`.
    ///
    /// Same per-shard minimum floor as `ShardedLruCache`: 16 per shard -> 64 total.
    #[test]
    fn sharded_lru_ttl_builder_shards4_max8_capacity_is_64() {
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(4)
            .max_size(8)
            .ttl(Duration::from_secs(3600))
            .build()
            .expect("build must succeed");

        assert_eq!(
            cache.capacity(),
            64,
            "capacity must be shards * per_shard_cap = 4 * 16 = 64"
        );
        assert_eq!(cache.shards(), 4, "shard count must be exactly 4");
    }

    /// With `shards(4).max_size(8)` the effective capacity is 64, so inserting
    /// more than `max_size` (8) entries must not evict anything (TTL is 1 hour).
    #[test]
    fn sharded_lru_ttl_builder_holds_more_than_max_size_entries() {
        let max_size: u32 = 8;
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(4)
            .max_size(max_size as usize)
            .ttl(Duration::from_secs(3600))
            .build()
            .expect("build must succeed");

        let insert_count: u32 = max_size + 1;
        for i in 0..insert_count {
            ConcurrentCached::cache_set(&cache, i, i * 10).expect("insert must succeed");
        }

        assert_eq!(
            cache.len(),
            insert_count as usize,
            "all inserted entries must be present: max_size is per-shard, not a global cap"
        );

        for i in 0..insert_count {
            assert_eq!(
                ConcurrentCached::cache_get(&cache, &i).expect("cache_get must succeed"),
                Some(i * 10),
                "entry {} must be retrievable",
                i
            );
        }
    }
}

// ── per_shard_initial_capacity ───────────────────────────────────────────────
//
// The capacity hint has no observable getter (it is a preallocation hint, like
// the single-owner builders' `initial_capacity`), so these tests pin the API:
// the setter exists on the three unbounded sharded builders, survives the
// type-changing `.hasher()` call, and the built cache functions normally.

mod per_shard_initial_capacity {
    use cached::{ConcurrentCached, DefaultShardHasher, ShardedUnboundCache};

    #[test]
    fn unbound_builder_accepts_hint_and_threads_through_hasher() {
        let cache: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::builder()
            .shards(4)
            .per_shard_initial_capacity(128)
            .hasher(DefaultShardHasher::new())
            .build()
            .expect("build must succeed");
        for i in 0..100u32 {
            ConcurrentCached::cache_set(&cache, i, i).expect("insert must succeed");
        }
        assert_eq!(cache.len(), 100);
    }

    #[cfg(feature = "time_stores")]
    #[test]
    fn ttl_builder_accepts_hint() {
        use cached::ShardedTtlCache;
        use cached::time::Duration;

        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(3600))
            .shards(4)
            .per_shard_initial_capacity(128)
            .build()
            .expect("build must succeed");
        for i in 0..100u32 {
            ConcurrentCached::cache_set(&cache, i, i).expect("insert must succeed");
        }
        assert_eq!(cache.len(), 100);
    }

    #[test]
    fn expiring_builder_accepts_hint() {
        use cached::{Expires, ShardedExpiringCache};

        #[derive(Clone)]
        struct V(#[allow(dead_code)] u32);
        impl Expires for V {
            fn is_expired(&self) -> bool {
                false
            }
        }

        let cache: ShardedExpiringCache<u32, V> = ShardedExpiringCache::builder()
            .shards(4)
            .per_shard_initial_capacity(128)
            .build()
            .expect("build must succeed");
        for i in 0..100u32 {
            ConcurrentCached::cache_set(&cache, i, V(i)).expect("insert must succeed");
        }
        assert_eq!(cache.len(), 100);
    }
}
