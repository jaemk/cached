/*!
Public-API contracts for the sharded LRU-family read/write fast paths.

These pin, from outside the crate, the behavior that the internal single-lookup
`cache_get` rewrite, the per-shard eviction counters, and the `drain_all`-based
`cache_clear_with_on_evict` must preserve:

- a live read returns the value, counts a hit, and **promotes** the entry (so the next
  capacity eviction picks a different victim);
- an absent read counts a miss and removes nothing;
- an expired read counts a miss, removes the entry, fires `on_evict`, and counts exactly
  one eviction;
- `metrics().evictions` aggregates capacity and non-capacity evictions across *many*
  shards without double counting, and survives `deep_clone`;
- `cache_clear_with_on_evict` fires most-recently-used first;
- overwriting an entry through the `on_evict` path keeps it most-recently-used.

No Redis server required.
*/

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cached::{
    ConcurrentCacheBase, ConcurrentCacheEvict, ConcurrentCached, Expires, ShardedExpiringLruCache,
    ShardedLruCache,
};

#[derive(Clone, Debug, PartialEq)]
struct Val {
    v: u32,
    expired: bool,
}

impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

fn live(v: u32) -> Val {
    Val { v, expired: false }
}

fn dead(v: u32) -> Val {
    Val { v, expired: true }
}

// --- ShardedExpiringLruCache ------------------------------------------------------------

#[test]
fn expiring_lru_read_path_hit_miss_expired_and_promotion() {
    let evicted: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let evicted2 = Arc::clone(&evicted);
    let store = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(1)
        .per_shard_max_size(2)
        .on_evict(move |k: &u32, v: &Val| evicted2.lock().unwrap().push((*k, v.v)))
        .build()
        .expect("build ShardedExpiringLruCache");

    ConcurrentCached::cache_set(&store, 1, live(10)).unwrap();
    ConcurrentCached::cache_set(&store, 2, live(20)).unwrap();

    // Live read: value, one hit, nothing removed.
    assert_eq!(
        ConcurrentCached::cache_get(&store, &1).unwrap(),
        Some(live(10))
    );
    assert_eq!(ConcurrentCacheBase::cache_hits(&store), Some(1));
    assert_eq!(ConcurrentCacheBase::cache_misses(&store), Some(0));
    assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(2));

    // Absent read: one miss, nothing removed, no callback.
    assert_eq!(ConcurrentCached::cache_get(&store, &404).unwrap(), None);
    assert_eq!(ConcurrentCacheBase::cache_misses(&store), Some(1));
    assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(2));
    assert!(evicted.lock().unwrap().is_empty());

    // The live read promoted key 1, so the capacity eviction must claim key 2.
    ConcurrentCached::cache_set(&store, 3, live(30)).unwrap();
    assert!(ConcurrentCached::cache_contains(&store, &1).unwrap());
    assert!(!ConcurrentCached::cache_contains(&store, &2).unwrap());
    assert_eq!(*evicted.lock().unwrap(), vec![(2, 20)]);

    // Expired read: miss, removed, callback, exactly one more eviction. (The insert below
    // is at capacity and evicts on its own, so the baseline is taken after it.)
    ConcurrentCached::cache_set(&store, 4, dead(40)).unwrap();
    let before = store.metrics().evictions.expect("evictions tracked");
    assert_eq!(ConcurrentCached::cache_get(&store, &4).unwrap(), None);
    assert!(!ConcurrentCached::cache_contains(&store, &4).unwrap());
    assert_eq!(
        store.metrics().evictions.expect("evictions tracked") - before,
        1,
        "an expired read must count exactly one eviction"
    );
    assert_eq!(evicted.lock().unwrap().last(), Some(&(4, 40)));

    // Reading the now-absent key again is a plain miss with no further eviction.
    let after_expiry = store.metrics().evictions.expect("evictions tracked");
    assert_eq!(ConcurrentCached::cache_get(&store, &4).unwrap(), None);
    assert_eq!(
        store.metrics().evictions.expect("evictions tracked"),
        after_expiry
    );
}

#[test]
fn expiring_lru_evictions_aggregate_across_shards_and_survive_deep_clone() {
    let store = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(8)
        .per_shard_max_size(4)
        .build()
        .expect("build ShardedExpiringLruCache");

    // Capacity evictions: 200 keys spread over 8 shards of 4 entries each.
    for i in 0..200u32 {
        ConcurrentCached::cache_set(&store, i, live(i)).unwrap();
    }
    let capacity_evictions = store.metrics().evictions.expect("evictions tracked");
    assert_eq!(
        capacity_evictions,
        200 - ConcurrentCacheBase::cache_size(&store).unwrap().unwrap() as u64,
        "every insert beyond what is still stored must have been evicted exactly once"
    );

    // Non-capacity evictions from more than one shard: a sweep of expired entries.
    ConcurrentCached::cache_clear(&store).unwrap();
    for i in 1000..1016u32 {
        ConcurrentCached::cache_set(&store, i, dead(i)).unwrap();
    }
    let stored = ConcurrentCacheBase::cache_size(&store).unwrap().unwrap();
    assert!(
        stored >= 8,
        "the expired entries must be spread over several shards; got {stored}"
    );
    let before_sweep = store.metrics().evictions.expect("evictions tracked");
    let swept = ConcurrentCacheEvict::evict(&store);
    assert_eq!(swept, stored, "every stored entry is expired");
    assert_eq!(
        store.metrics().evictions.expect("evictions tracked") - before_sweep,
        swept as u64,
        "the sweep must add exactly one eviction per removed entry"
    );
    assert_eq!(
        ConcurrentCacheBase::cache_evictions(&store),
        store.metrics().evictions,
        "the trait method and metrics() must agree"
    );

    // deep_clone carries the eviction total across (the counters are per-shard now).
    let total = store.metrics().evictions.expect("evictions tracked");
    let cloned = store.deep_clone();
    assert_eq!(cloned.metrics().evictions, Some(total));
    assert_eq!(ConcurrentCacheBase::cache_evictions(&cloned), Some(total));

    // ... and the clone is independent.
    ConcurrentCached::cache_set(&cloned, 5000, dead(1)).unwrap();
    assert_eq!(ConcurrentCached::cache_get(&cloned, &5000).unwrap(), None);
    assert_eq!(cloned.metrics().evictions, Some(total + 1));
    assert_eq!(store.metrics().evictions, Some(total));

    // Resetting metrics zeroes every family of counters.
    ConcurrentCached::cache_reset_metrics(&store).unwrap();
    assert_eq!(store.metrics().evictions, Some(0));
    assert_eq!(ConcurrentCacheBase::cache_evictions(&store), Some(0));
}

#[test]
fn expiring_lru_overwrite_with_on_evict_keeps_entry_most_recently_used() {
    let store = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(1)
        .per_shard_max_size(2)
        .on_evict(|_, _| {})
        .build()
        .expect("build ShardedExpiringLruCache");
    ConcurrentCached::cache_set(&store, 1, live(10)).unwrap();
    ConcurrentCached::cache_set(&store, 2, live(20)).unwrap();

    // Overwrite the least-recently-used entry; it must become most-recently-used.
    assert_eq!(
        ConcurrentCached::cache_set(&store, 1, live(11)).unwrap(),
        Some(live(10))
    );
    ConcurrentCached::cache_set(&store, 3, live(30)).unwrap();
    assert_eq!(
        ConcurrentCached::cache_get(&store, &1).unwrap(),
        Some(live(11)),
        "the overwritten entry must have been promoted and so must survive"
    );
    assert!(!ConcurrentCached::cache_contains(&store, &2).unwrap());
}

// --- ShardedLruCache --------------------------------------------------------------------

#[test]
fn sharded_lru_clear_with_on_evict_fires_most_recently_used_first() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let store = ShardedLruCache::<String, u64>::builder()
        .shards(1)
        .per_shard_max_size(16)
        .on_evict(move |k: &String, _v: &u64| seen2.lock().unwrap().push(k.clone()))
        .build()
        .expect("build ShardedLruCache");

    for i in 0..5u64 {
        ConcurrentCached::cache_set(&store, i.to_string(), i).unwrap();
    }
    // Re-read "1" so the chain is not merely reverse insertion order.
    assert_eq!(
        ConcurrentCached::cache_get(&store, &"1".to_string()).unwrap(),
        Some(1)
    );
    let before = store.metrics().evictions.expect("evictions tracked");

    store.cache_clear_with_on_evict();

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "1".to_string(),
            "4".to_string(),
            "3".to_string(),
            "2".to_string(),
            "0".to_string(),
        ],
        "on_evict must fire most-recently-used first"
    );
    assert!(store.is_empty());
    assert_eq!(
        store.metrics().evictions.expect("evictions tracked") - before,
        5,
        "every cleared entry must be counted once"
    );

    // The cleared store keeps working.
    ConcurrentCached::cache_set(&store, "x".to_string(), 42).unwrap();
    assert_eq!(
        ConcurrentCached::cache_get(&store, &"x".to_string()).unwrap(),
        Some(42)
    );
}

#[test]
fn sharded_lru_clear_with_on_evict_clears_every_shard() {
    let count = Arc::new(AtomicU64::new(0));
    let count2 = Arc::clone(&count);
    let store = ShardedLruCache::<String, u64>::builder()
        .shards(8)
        .per_shard_max_size(64)
        .on_evict(move |_k: &String, _v: &u64| {
            count2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .expect("build ShardedLruCache");
    for i in 0..200u64 {
        ConcurrentCached::cache_set(&store, i.to_string(), i).unwrap();
    }
    assert_eq!(
        ConcurrentCacheBase::cache_size(&store).unwrap(),
        Some(200),
        "no capacity eviction should have happened yet"
    );
    store.cache_clear_with_on_evict();
    assert_eq!(count.load(Ordering::Relaxed), 200);
    assert!(store.is_empty());
}

// --- ShardedLruTtlCache -----------------------------------------------------------------

#[cfg(feature = "time_stores")]
mod lru_ttl {
    use super::*;
    use cached::ShardedLruTtlCache;
    use std::time::Duration;

    #[test]
    fn read_path_hit_miss_expired_and_promotion() {
        let evicted: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let evicted2 = Arc::clone(&evicted);
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(2)
            .ttl(Duration::from_millis(40))
            .on_evict(move |k: &u32, v: &u32| evicted2.lock().unwrap().push((*k, *v)))
            .build()
            .expect("build ShardedLruTtlCache");

        ConcurrentCached::cache_set(&store, 1, 10).unwrap();
        ConcurrentCached::cache_set(&store, 2, 20).unwrap();

        // Live read: value, one hit, nothing removed, and the entry is promoted.
        assert_eq!(ConcurrentCached::cache_get(&store, &1).unwrap(), Some(10));
        assert_eq!(ConcurrentCacheBase::cache_hits(&store), Some(1));
        assert_eq!(ConcurrentCached::cache_get(&store, &404).unwrap(), None);
        assert_eq!(ConcurrentCacheBase::cache_misses(&store), Some(1));
        assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(2));

        ConcurrentCached::cache_set(&store, 3, 30).unwrap();
        assert!(ConcurrentCached::cache_contains(&store, &1).unwrap());
        assert!(!ConcurrentCached::cache_contains(&store, &2).unwrap());
        assert_eq!(*evicted.lock().unwrap(), vec![(2, 20)]);

        // Expired read: miss, removed, callback, exactly one more eviction.
        let before = store.metrics().evictions.expect("evictions tracked");
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(ConcurrentCached::cache_get(&store, &1).unwrap(), None);
        assert!(!ConcurrentCached::cache_contains(&store, &1).unwrap());
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked") - before,
            1,
            "an expired read must count exactly one eviction"
        );
        assert_eq!(evicted.lock().unwrap().last(), Some(&(1, 10)));
    }

    #[test]
    fn evictions_aggregate_across_shards_and_survive_deep_clone() {
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(8)
            .per_shard_max_size(4)
            .ttl(Duration::from_millis(40))
            .build()
            .expect("build ShardedLruTtlCache");

        for i in 0..200u32 {
            ConcurrentCached::cache_set(&store, i, i).unwrap();
        }
        let stored = ConcurrentCacheBase::cache_size(&store).unwrap().unwrap() as u64;
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked"),
            200 - stored,
            "every insert beyond what is still stored must have been evicted exactly once"
        );

        // Non-capacity evictions from many shards at once: a TTL sweep.
        std::thread::sleep(Duration::from_millis(120));
        let before_sweep = store.metrics().evictions.expect("evictions tracked");
        let swept = ConcurrentCacheEvict::evict(&store) as u64;
        assert_eq!(swept, stored, "every stored entry is expired by now");
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked") - before_sweep,
            swept,
            "the sweep must add exactly one eviction per removed entry"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_evictions(&store),
            store.metrics().evictions,
            "the trait method and metrics() must agree"
        );

        let total = store.metrics().evictions.expect("evictions tracked");
        let cloned = store.deep_clone();
        assert_eq!(cloned.metrics().evictions, Some(total));
        assert_eq!(ConcurrentCacheBase::cache_evictions(&cloned), Some(total));

        // The clone is independent.
        ConcurrentCached::cache_set(&cloned, 5000, 1).unwrap();
        assert_eq!(
            ConcurrentCached::cache_remove(&cloned, &5000).unwrap(),
            Some(1)
        );
        assert_eq!(cloned.metrics().evictions, Some(total + 1));
        assert_eq!(store.metrics().evictions, Some(total));

        ConcurrentCached::cache_reset_metrics(&store).unwrap();
        assert_eq!(store.metrics().evictions, Some(0));
        assert_eq!(ConcurrentCacheBase::cache_evictions(&store), Some(0));
    }

    #[test]
    fn overwrite_with_on_evict_keeps_entry_most_recently_used() {
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(2)
            .ttl(Duration::from_secs(3600))
            .on_evict(|_, _| {})
            .build()
            .expect("build ShardedLruTtlCache");
        ConcurrentCached::cache_set(&store, 1, 10).unwrap();
        ConcurrentCached::cache_set(&store, 2, 20).unwrap();

        assert_eq!(
            ConcurrentCached::cache_set(&store, 1, 11).unwrap(),
            Some(10)
        );
        ConcurrentCached::cache_set(&store, 3, 30).unwrap();
        assert_eq!(
            ConcurrentCached::cache_get(&store, &1).unwrap(),
            Some(11),
            "the overwritten entry must have been promoted and so must survive"
        );
        assert!(!ConcurrentCached::cache_contains(&store, &2).unwrap());
    }

    #[test]
    fn clear_with_on_evict_fires_most_recently_used_first() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let store = ShardedLruTtlCache::<String, u64>::builder()
            .shards(1)
            .per_shard_max_size(16)
            .ttl(Duration::from_secs(3600))
            .on_evict(move |k: &String, _v: &u64| seen2.lock().unwrap().push(k.clone()))
            .build()
            .expect("build ShardedLruTtlCache");
        for i in 0..5u64 {
            ConcurrentCached::cache_set(&store, i.to_string(), i).unwrap();
        }
        assert_eq!(
            ConcurrentCached::cache_get(&store, &"1".to_string()).unwrap(),
            Some(1)
        );
        let before = store.metrics().evictions.expect("evictions tracked");

        store.cache_clear_with_on_evict();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "1".to_string(),
                "4".to_string(),
                "3".to_string(),
                "2".to_string(),
                "0".to_string(),
            ],
            "on_evict must fire most-recently-used first"
        );
        assert!(store.is_empty());
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked") - before,
            5
        );
    }
}
