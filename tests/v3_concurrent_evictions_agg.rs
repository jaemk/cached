/*!
Tests for aggregated metrics on sharded concurrent stores via the `ConcurrentCacheBase`
trait: `cache_hits`, `cache_misses`, `cache_capacity`, and `cache_evictions`.

These tests do NOT require a Redis server.

Covered:
- A sharded LRU store with small per-shard capacity is driven to eviction; the
  aggregated `cache_evictions` via the TRAIT METHOD (not an inherent method) returns
  `Some(n)` with n > 0.
- `cache_capacity` equals the sum of per-shard capacities (total logical capacity).
- `cache_hits` and `cache_misses` are consistent with the operations performed.
*/

use cached::{ConcurrentCacheBase, ConcurrentCached, ShardedLruCache};

/// Drive enough inserts to guarantee at least one eviction across the shards.
/// With 2 shards each holding 4 entries (total capacity 8), inserting 20 distinct
/// keys will overflow every shard and produce evictions.
#[test]
fn sharded_lru_cache_evictions_aggregated_via_trait() {
    // Use `per_shard_max_size` to bypass the 16-per-shard minimum floor, which
    // would otherwise force each shard to hold at least 16 entries and require
    // many more inserts to trigger an eviction.
    let store = ShardedLruCache::<String, u64>::builder()
        .shards(2)
        .per_shard_max_size(4)
        .build()
        .expect("build ShardedLruCache(2 shards, 4 per-shard)");

    // Insert enough distinct keys to overflow both shards.
    for i in 0u64..20 {
        ConcurrentCached::cache_set(&store, i.to_string(), i).unwrap();
    }

    // Aggregated evictions via the TRAIT method must be Some(n) with n > 0.
    let evictions = ConcurrentCacheBase::cache_evictions(&store);
    assert!(
        evictions.is_some(),
        "cache_evictions must return Some(_) for a bounded sharded LRU"
    );
    assert!(
        evictions.unwrap() > 0,
        "at least one eviction must have occurred; got evictions={evictions:?}"
    );
}

/// `cache_capacity` via the trait must equal the effective total capacity
/// (sum of per-shard capacities, reflecting the per-shard floor if applicable).
#[test]
fn sharded_lru_cache_capacity_aggregated_via_trait() {
    let shards = 2usize;
    let per_shard = 4usize;

    let store = ShardedLruCache::<String, u64>::builder()
        .shards(shards)
        .per_shard_max_size(per_shard)
        .build()
        .expect("build ShardedLruCache");

    let capacity = ConcurrentCacheBase::cache_capacity(&store);
    assert_eq!(
        capacity,
        Some(shards * per_shard),
        "cache_capacity must equal shards * per_shard_max_size = {}",
        shards * per_shard
    );
}

/// `cache_hits` and `cache_misses` via the trait move correctly with gets.
#[test]
fn sharded_lru_cache_hits_and_misses_via_trait() {
    let store = ShardedLruCache::<String, u64>::builder()
        .shards(2)
        .per_shard_max_size(8)
        .build()
        .expect("build ShardedLruCache");

    // Baseline: empty cache, no hits or misses yet.
    let hits_before = ConcurrentCacheBase::cache_hits(&store).unwrap_or(0);
    let misses_before = ConcurrentCacheBase::cache_misses(&store).unwrap_or(0);

    // Insert a key.
    ConcurrentCached::cache_set(&store, "present".to_string(), 42u64).unwrap();

    // Miss on an absent key.
    let got_miss = ConcurrentCached::cache_get(&store, &"absent".to_string()).unwrap();
    assert_eq!(got_miss, None, "absent key must return None");

    // Hit on the present key.
    let got_hit = ConcurrentCached::cache_get(&store, &"present".to_string()).unwrap();
    assert_eq!(got_hit, Some(42), "present key must return Some(42)");

    let hits_after = ConcurrentCacheBase::cache_hits(&store).unwrap_or(0);
    let misses_after = ConcurrentCacheBase::cache_misses(&store).unwrap_or(0);

    assert_eq!(
        hits_after,
        hits_before + 1,
        "one hit must have been recorded"
    );
    assert_eq!(
        misses_after,
        misses_before + 1,
        "one miss must have been recorded"
    );
}

/// All three metrics coexist correctly after a mixed workload (inserts + gets).
#[test]
fn sharded_lru_cache_aggregated_metrics_consistent() {
    let store = ShardedLruCache::<u32, u32>::builder()
        .shards(4)
        .per_shard_max_size(2) // very small per-shard cap to force evictions
        .build()
        .expect("build ShardedLruCache(4 shards, 2 per-shard)");

    // Insert 40 distinct keys to saturate all shards and drive evictions.
    for i in 0u32..40 {
        ConcurrentCached::cache_set(&store, i, i * 10).unwrap();
    }

    // Read a few keys — some will hit, some will miss (evicted).
    let mut hits = 0u64;
    let mut misses = 0u64;
    for i in 0u32..40 {
        match ConcurrentCached::cache_get(&store, &i).unwrap() {
            Some(_) => hits += 1,
            None => misses += 1,
        }
    }

    let agg_hits = ConcurrentCacheBase::cache_hits(&store).unwrap_or(0);
    let agg_misses = ConcurrentCacheBase::cache_misses(&store).unwrap_or(0);
    let agg_evictions = ConcurrentCacheBase::cache_evictions(&store).unwrap_or(0);
    let agg_capacity = ConcurrentCacheBase::cache_capacity(&store);

    // Aggregated hits/misses must match what we counted.
    assert_eq!(
        agg_hits, hits,
        "aggregated hits must equal measured hit count"
    );
    assert_eq!(
        agg_misses, misses,
        "aggregated misses must equal measured miss count"
    );

    // Capacity must be reported and consistent (4 shards * 2 per-shard = 8).
    assert_eq!(
        agg_capacity,
        Some(4 * 2),
        "capacity must be 4 shards * 2 per-shard = 8"
    );

    // At least some evictions must have occurred (40 inserts into 8 slots).
    assert!(
        agg_evictions > 0,
        "at least one eviction must have occurred with 40 inserts into 8 slots; got {agg_evictions}"
    );
}
