//! Integration tests for runtime capacity resizing of the sharded LRU-bounded stores.
//!
//! Covers `ShardedLruCache`, `ShardedLruTtlCache`, and `ShardedExpiringLruCache`.
//! Each behavioral path is exercised: grow, shrink (with eviction), zero panic, zero
//! error, min-per-shard clamp, per_shard_max_size migration, and single-shard exact
//! capacity.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cached::{ConcurrentCacheBase, ConcurrentCached, SetMaxSizeError};

// ---------------------------------------------------------------------------
// Helper: a type that impls Expires for ShardedExpiringLruCache tests
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct E {
    v: u32,
}

impl cached::Expires for E {
    fn is_expired(&self) -> bool {
        false // never expires from the value side; capacity drives eviction
    }
}

/// A value whose expiry is controlled per-instance, for the expiring_lru
/// "shrink evicts LRU-order regardless of expiry" contract test.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ExpVal {
    v: u32,
    expired: bool,
}

impl cached::Expires for ExpVal {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

// ---------------------------------------------------------------------------
// ShardedLruCache
// ---------------------------------------------------------------------------

mod lru {
    use super::*;
    use cached::ShardedLruCacheBase;

    #[test]
    fn grow_keeps_entries_returns_previous_total() {
        // shards(1): per_shard_cap = max_size (no floor), so capacity == max_size exactly.
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();

        let prev = c.set_max_size(8);
        assert_eq!(prev, Some(4), "must return previous total capacity");
        assert_eq!(c.capacity(), 8, "capacity() must reflect new size");

        // Existing entries must survive.
        assert_eq!(ConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(ConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
    }

    #[test]
    fn shrink_evicts_lru_order_fires_on_evict_and_counts_evictions() {
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted2 = evicted.clone();

        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .on_evict(move |_, _| {
                evicted2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        // Insert 4 entries, then promote 1 and 2 to MRU.
        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();
        ConcurrentCached::cache_set(&c, 3, 30).unwrap();
        ConcurrentCached::cache_set(&c, 4, 40).unwrap();
        ConcurrentCached::cache_get(&c, &1).unwrap();
        ConcurrentCached::cache_get(&c, &2).unwrap();

        let evictions_before = c.metrics().evictions.unwrap();
        let prev = c.set_max_size(2);
        assert_eq!(prev, Some(4), "must return previous capacity");
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.len(), 2);

        // Two LRU entries (3 and 4) must have been evicted.
        assert_eq!(
            c.metrics().evictions.unwrap() - evictions_before,
            2,
            "eviction counter must increment for each evicted entry"
        );
        assert_eq!(
            evicted.load(Ordering::Relaxed),
            2,
            "on_evict must fire for each evicted entry"
        );

        // MRU survivors must still be present.
        assert_eq!(ConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(ConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
        assert!(ConcurrentCached::cache_get(&c, &3).unwrap().is_none());
        assert!(ConcurrentCached::cache_get(&c, &4).unwrap().is_none());
    }

    #[test]
    #[should_panic(expected = "max_size must be greater than zero")]
    fn zero_set_max_size_panics() {
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        let _ = c.set_max_size(0);
    }

    #[test]
    fn zero_try_set_max_size_returns_error() {
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        assert_eq!(
            c.try_set_max_size(0),
            Err(SetMaxSizeError::ZeroSize),
            "try_set_max_size(0) must return ZeroSize error"
        );
        // Valid call still works after the failed attempt.
        let r = c.try_set_max_size(8);
        assert_eq!(r, Ok(Some(4)));
        assert_eq!(c.capacity(), 8);
    }

    #[test]
    fn min_per_shard_clamp_applied_and_capacity_reflects_clamped_total() {
        // 4 shards, total = 4 → per_shard = max(4.div_ceil(4)=1, 16) = 16 → total = 64.
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(4)
            .max_size(1024)
            .build()
            .unwrap();
        assert_eq!(c.capacity(), 1024);

        // Resize to a tiny total that triggers the 16-per-shard floor.
        let prev = c.set_max_size(4);
        assert_eq!(prev, Some(1024), "must return previous capacity");
        // 4 shards * 16 = 64
        assert_eq!(
            c.capacity(),
            64,
            "capacity() must report the clamped total (4 shards * 16 = 64)"
        );
        // Exercise the clamped caps under load: 200 distinct keys over 4 shards
        // guarantees (by pigeonhole) at least one shard sees >= 50 inserts, so
        // the 16-entry per-shard floor must be reached and enforced.
        for i in 0..200u32 {
            ConcurrentCached::cache_set(&c, i, i).unwrap();
        }
        let sizes = c.shard_sizes();
        for sz in &sizes {
            assert!(
                *sz <= 16,
                "each shard must not hold more entries than its clamped cap; got {sz}"
            );
        }
        assert_eq!(
            sizes.iter().max().copied(),
            Some(16),
            "at least one shard must fill to its clamped 16-entry cap"
        );
        assert!(
            c.len() <= 64,
            "total entries must respect the clamped total capacity"
        );
    }

    #[test]
    fn resize_cache_built_with_per_shard_max_size_switches_to_total_policy() {
        // Built with per_shard_max_size = 10, 4 shards → total = 40.
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(4)
            .per_shard_max_size(10)
            .build()
            .unwrap();
        assert_eq!(c.capacity(), 40, "initial capacity from per_shard_max_size");

        // After resize, total-based policy applies.
        let prev = c.set_max_size(100);
        assert_eq!(prev, Some(40), "must return previous total");
        // 4 shards, total=100 → per_shard = 100.div_ceil(4) = 25 → total = 100.
        assert_eq!(c.capacity(), 100);
    }

    #[test]
    fn single_shard_exact_capacity_no_clamp() {
        // shards=1: no minimum clamp, capacity == max_size exactly.
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(3)
            .build()
            .unwrap();
        // Fill to capacity.
        ConcurrentCached::cache_set(&c, 1, 1).unwrap();
        ConcurrentCached::cache_set(&c, 2, 2).unwrap();
        ConcurrentCached::cache_set(&c, 3, 3).unwrap();
        assert_eq!(c.len(), 3);

        let prev = c.set_max_size(1);
        assert_eq!(prev, Some(3));
        assert_eq!(c.capacity(), 1, "no clamp for single shard");
        assert_eq!(c.len(), 1, "must evict down to new capacity");
    }

    #[test]
    fn same_size_resize_is_noop_keeps_entries_and_returns_previous() {
        // Resizing to the identical size must not evict anything: every entry
        // survives, no on_evict fires, and the returned prev equals the current cap.
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted2 = evicted.clone();
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .on_evict(move |_, _| {
                evicted2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..4u32 {
            ConcurrentCached::cache_set(&c, i, i * 10).unwrap();
        }
        let ev_before = c.metrics().evictions.unwrap();

        let prev = c.set_max_size(4);
        assert_eq!(
            prev,
            Some(4),
            "same-size resize returns the unchanged total"
        );
        assert_eq!(c.capacity(), 4);
        assert_eq!(c.len(), 4, "no entry evicted on a same-size resize");
        assert_eq!(
            c.metrics().evictions.unwrap(),
            ev_before,
            "same-size resize must not count any eviction"
        );
        assert_eq!(
            evicted.load(Ordering::Relaxed),
            0,
            "same-size resize must not fire on_evict"
        );
        for i in 0..4u32 {
            assert_eq!(ConcurrentCached::cache_get(&c, &i).unwrap(), Some(i * 10));
        }
    }

    #[test]
    fn repeated_resizes_grow_shrink_grow_track_capacity_and_survival() {
        // Capacity and entry survival must be correct at each step of a
        // grow -> shrink -> grow sequence. Grow never resurrects evicted entries.
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        // Grow: capacity moves up, existing entries untouched.
        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();
        assert_eq!(c.set_max_size(8), Some(4));
        assert_eq!(c.capacity(), 8);
        // Fill up to the grown cap; MRU order will be 1..=8 (8 most recent).
        for i in 3..=8u32 {
            ConcurrentCached::cache_set(&c, i, i * 10).unwrap();
        }
        assert_eq!(c.len(), 8);

        // Shrink to 2: only the two MRU keys (7, 8) survive.
        assert_eq!(c.set_max_size(2), Some(8));
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.len(), 2);
        assert_eq!(ConcurrentCached::cache_get(&c, &8).unwrap(), Some(80));
        assert_eq!(ConcurrentCached::cache_get(&c, &7).unwrap(), Some(70));
        assert!(ConcurrentCached::cache_get(&c, &1).unwrap().is_none());

        // Grow again to 6: capacity moves up but evicted keys are NOT resurrected.
        assert_eq!(c.set_max_size(6), Some(2));
        assert_eq!(c.capacity(), 6);
        assert_eq!(c.len(), 2, "grow must not resurrect evicted entries");
        assert!(ConcurrentCached::cache_get(&c, &6).unwrap().is_none());
    }

    #[test]
    fn grow_past_entry_count_evicts_nothing() {
        // Growing when the cache holds fewer entries than even the old cap must
        // touch nothing: no eviction, no on_evict, all entries survive.
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted2 = evicted.clone();
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .on_evict(move |_, _| {
                evicted2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();
        let ev_before = c.metrics().evictions.unwrap();
        c.set_max_size(1024);
        assert_eq!(c.len(), 2);
        assert_eq!(c.metrics().evictions.unwrap(), ev_before);
        assert_eq!(evicted.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn trait_cache_capacity_reflects_resize() {
        // The ConcurrentCacheBase::cache_capacity trait method (not just the
        // inherent capacity()) must report the post-resize total.
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        assert_eq!(ConcurrentCacheBase::cache_capacity(&c), Some(4));
        c.set_max_size(32);
        assert_eq!(
            ConcurrentCacheBase::cache_capacity(&c),
            Some(32),
            "trait cache_capacity() must reflect the resize, not just inherent capacity()"
        );
        assert_eq!(
            c.capacity(),
            ConcurrentCacheBase::cache_capacity(&c).unwrap()
        );
    }

    #[test]
    fn shrink_across_multiple_shards_aggregates_eviction_metrics() {
        // With many shards, shrink evictions spread across shards must all show up
        // in the aggregated metrics().evictions and total on_evict count. Built with
        // a generous per-shard cap of 32 (total 256), then resized down. Note that
        // set_max_size applies the 16-per-shard floor for multi-shard caches, so we
        // shrink to a total whose per-shard quotient is still >= 16 to guarantee a
        // real trim: max_size = 8*16 = 128 -> per_shard = 128.div_ceil(8) = 16, so
        // every shard goes from cap 32 down to cap 16.
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted2 = evicted.clone();
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(8)
            .per_shard_max_size(32)
            .on_evict(move |_, _| {
                evicted2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        assert_eq!(c.capacity(), 256);
        // Insert enough distinct keys that several shards exceed the post-shrink
        // per-shard cap of 16. 1024 keys over 8 shards averages 128 per shard, well
        // above 16, so the shrink is guaranteed to evict.
        for i in 0..1024u32 {
            ConcurrentCached::cache_set(&c, i, i).unwrap();
        }
        let filled = c.len();
        let ev_before = c.metrics().evictions.unwrap();
        // Snapshot on_evict too: fill-time capacity evictions already fired the
        // callback, so only the delta over the shrink is under test here.
        let cb_before = evicted.load(Ordering::Relaxed);

        // Shrink: per-shard cap 32 -> 16, total 256 -> 128.
        c.set_max_size(128);
        assert_eq!(c.capacity(), 128);
        let after = c.len();
        assert!(after < filled, "the shrink must actually drop entries");
        let shrink_evictions = c.metrics().evictions.unwrap() - ev_before;
        assert_eq!(
            shrink_evictions,
            (filled - after) as u64,
            "aggregated metrics().evictions must account for every entry dropped by the shrink across all shards"
        );
        assert_eq!(
            evicted.load(Ordering::Relaxed) - cb_before,
            shrink_evictions,
            "on_evict must fire exactly once per evicted entry across all shards"
        );
    }

    #[test]
    fn grow_single_shard_built_with_per_shard_max_size() {
        // A shards=1 cache built via per_shard_max_size must grow correctly:
        // capacity becomes the new max_size exactly (no floor at shards=1) and
        // entries survive.
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(4)
            .build()
            .unwrap();
        assert_eq!(c.capacity(), 4);
        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();
        assert_eq!(
            c.set_max_size(100),
            Some(4),
            "returns prior per-shard total"
        );
        assert_eq!(c.capacity(), 100, "single shard: exact grow, no clamp");
        assert_eq!(ConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(ConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
    }

    #[test]
    fn concurrent_ops_during_resize_no_deadlock_and_capacity_consistent() {
        // Stress: while worker threads hammer get/set, another thread flips the
        // capacity between two sizes. The resize takes per-shard write locks one
        // at a time, so this must never deadlock or panic, and once the resizer
        // finishes the observed capacity must be one of the two target totals.
        use std::thread;
        let c = ShardedLruCacheBase::<u32, u32>::builder()
            .shards(8)
            .max_size(1024)
            .build()
            .unwrap();

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut handles = Vec::new();
        for t in 0..4 {
            let c = c.clone();
            let stop = stop.clone();
            handles.push(thread::spawn(move || {
                let mut i = t;
                while !stop.load(Ordering::Relaxed) {
                    let _ = ConcurrentCached::cache_set(&c, i, i);
                    let _ = ConcurrentCached::cache_get(&c, &(i.wrapping_sub(1)));
                    i = i.wrapping_add(4);
                }
            }));
        }
        // Resizer: alternate between two totals. 512 -> 16-floor? no: 512/8=64 exact;
        // 2048/8=256 exact. Both are floor-free so capacity() lands on one of them.
        let resizer = {
            let c = c.clone();
            thread::spawn(move || {
                for r in 0..500 {
                    let target = if r % 2 == 0 { 512 } else { 2048 };
                    let prev = c.set_max_size(target);
                    assert!(prev.is_some(), "set_max_size always returns Some(prev)");
                }
            })
        };
        resizer.join().expect("resizer thread must not panic");
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        // After the resizer completed, the last write was set_max_size(2048)
        // (r = 499 is odd), so capacity settles at exactly 2048.
        assert_eq!(
            c.capacity(),
            2048,
            "capacity must be eventually consistent with the final resize"
        );
    }
}

// ---------------------------------------------------------------------------
// ShardedLruTtlCache
// ---------------------------------------------------------------------------

#[cfg(feature = "time_stores")]
mod lru_ttl {
    use super::*;
    use cached::ShardedLruTtlCacheBase;
    use cached::time::Duration;

    #[test]
    fn grow_keeps_entries_returns_previous_total() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();

        let prev = c.set_max_size(8);
        assert_eq!(prev, Some(4));
        assert_eq!(c.capacity(), 8);
        assert_eq!(ConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(ConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
    }

    #[test]
    fn shrink_evicts_lru_order_fires_on_evict_and_counts_evictions() {
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted2 = evicted.clone();

        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_, _| {
                evicted2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();
        ConcurrentCached::cache_set(&c, 3, 30).unwrap();
        ConcurrentCached::cache_set(&c, 4, 40).unwrap();
        // Promote 1 and 2 to MRU.
        ConcurrentCached::cache_get(&c, &1).unwrap();
        ConcurrentCached::cache_get(&c, &2).unwrap();

        let evictions_before = c.metrics().evictions.unwrap();
        let prev = c.set_max_size(2);
        assert_eq!(prev, Some(4));
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.len(), 2);
        assert_eq!(
            c.metrics().evictions.unwrap() - evictions_before,
            2,
            "eviction counter must reflect shrink evictions"
        );
        assert_eq!(
            evicted.load(Ordering::Relaxed),
            2,
            "on_evict must fire for each evicted entry"
        );
        // Survivors.
        assert_eq!(ConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
        assert_eq!(ConcurrentCached::cache_get(&c, &2).unwrap(), Some(20));
        assert!(ConcurrentCached::cache_get(&c, &3).unwrap().is_none());
        assert!(ConcurrentCached::cache_get(&c, &4).unwrap().is_none());
    }

    #[test]
    #[should_panic(expected = "max_size must be greater than zero")]
    fn zero_set_max_size_panics() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let _ = c.set_max_size(0);
    }

    #[test]
    fn zero_try_set_max_size_returns_error() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.try_set_max_size(0), Err(SetMaxSizeError::ZeroSize));
        let r = c.try_set_max_size(8);
        assert_eq!(r, Ok(Some(4)));
        assert_eq!(c.capacity(), 8);
    }

    #[test]
    fn min_per_shard_clamp_applied_and_capacity_reflects_clamped_total() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(4)
            .max_size(1024)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        let prev = c.set_max_size(4);
        assert_eq!(prev, Some(1024));
        // 4 shards * 16 = 64
        assert_eq!(c.capacity(), 64);
    }

    #[test]
    fn resize_cache_built_with_per_shard_max_size_switches_to_total_policy() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(4)
            .per_shard_max_size(10)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(c.capacity(), 40);
        let prev = c.set_max_size(100);
        assert_eq!(prev, Some(40));
        assert_eq!(c.capacity(), 100);
    }

    #[test]
    fn single_shard_exact_capacity_no_clamp() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&c, 1, 1).unwrap();
        ConcurrentCached::cache_set(&c, 2, 2).unwrap();
        ConcurrentCached::cache_set(&c, 3, 3).unwrap();
        assert_eq!(c.len(), 3);
        let prev = c.set_max_size(1);
        assert_eq!(prev, Some(3));
        assert_eq!(c.capacity(), 1);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn no_evict_typestate_shrink_evicts_and_counts_without_callback() {
        // Built via the NoEvict typestate (no .on_evict). Shrink must still evict
        // in LRU order and count those evictions in metrics (the LRU capacity
        // counter fires whether or not a callback is attached).
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        for i in 0..4u32 {
            ConcurrentCached::cache_set(&c, i, i * 10).unwrap();
        }
        // Promote 0 and 1 so 2 and 3 are the LRU victims.
        ConcurrentCached::cache_get(&c, &0).unwrap();
        ConcurrentCached::cache_get(&c, &1).unwrap();
        let before = c.metrics().evictions.unwrap();
        c.set_max_size(2);
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.len(), 2);
        assert_eq!(
            c.metrics().evictions.unwrap() - before,
            2,
            "NoEvict cache must still count shrink evictions in metrics"
        );
        assert_eq!(ConcurrentCached::cache_get(&c, &0).unwrap(), Some(0));
        assert_eq!(ConcurrentCached::cache_get(&c, &1).unwrap(), Some(10));
    }

    #[test]
    fn has_evict_typestate_shrink_fires_callback() {
        // Built via the HasEvict typestate (.on_evict called). The typestate
        // transition must still produce a cache whose set_max_size shrink fires
        // the callback for each evicted entry.
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted2 = evicted.clone();
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_, _| {
                evicted2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..4u32 {
            ConcurrentCached::cache_set(&c, i, i).unwrap();
        }
        c.set_max_size(1);
        assert_eq!(c.capacity(), 1);
        assert_eq!(
            evicted.load(Ordering::Relaxed),
            3,
            "HasEvict cache must fire on_evict for each shrink eviction"
        );
    }

    #[test]
    fn trait_cache_capacity_reflects_resize() {
        let c = ShardedLruTtlCacheBase::<u32, u32>::builder()
            .shards(1)
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(ConcurrentCacheBase::cache_capacity(&c), Some(4));
        c.set_max_size(32);
        assert_eq!(ConcurrentCacheBase::cache_capacity(&c), Some(32));
    }
}

// ---------------------------------------------------------------------------
// ShardedExpiringLruCache
// ---------------------------------------------------------------------------

mod expiring_lru {
    use super::*;
    use cached::ShardedExpiringLruCacheBase;

    #[test]
    fn grow_keeps_entries_returns_previous_total() {
        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&c, 1, E { v: 10 }).unwrap();
        ConcurrentCached::cache_set(&c, 2, E { v: 20 }).unwrap();

        let prev = c.set_max_size(8);
        assert_eq!(prev, Some(4));
        assert_eq!(c.capacity(), 8);
        assert!(ConcurrentCached::cache_get(&c, &1).unwrap().is_some());
        assert!(ConcurrentCached::cache_get(&c, &2).unwrap().is_some());
    }

    #[test]
    fn shrink_evicts_lru_order_fires_on_evict_and_counts_evictions() {
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted2 = evicted.clone();

        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(1)
            .max_size(4)
            .on_evict(move |_, _| {
                evicted2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        ConcurrentCached::cache_set(&c, 1, E { v: 10 }).unwrap();
        ConcurrentCached::cache_set(&c, 2, E { v: 20 }).unwrap();
        ConcurrentCached::cache_set(&c, 3, E { v: 30 }).unwrap();
        ConcurrentCached::cache_set(&c, 4, E { v: 40 }).unwrap();
        // Promote 1 and 2 to MRU.
        ConcurrentCached::cache_get(&c, &1).unwrap();
        ConcurrentCached::cache_get(&c, &2).unwrap();

        let evictions_before = c.metrics().evictions.unwrap();
        let prev = c.set_max_size(2);
        assert_eq!(prev, Some(4));
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.len(), 2);
        assert_eq!(
            c.metrics().evictions.unwrap() - evictions_before,
            2,
            "eviction counter must reflect shrink evictions"
        );
        assert_eq!(
            evicted.load(Ordering::Relaxed),
            2,
            "on_evict must fire for each evicted entry"
        );
        // Survivors.
        assert!(ConcurrentCached::cache_get(&c, &1).unwrap().is_some());
        assert!(ConcurrentCached::cache_get(&c, &2).unwrap().is_some());
        assert!(ConcurrentCached::cache_get(&c, &3).unwrap().is_none());
        assert!(ConcurrentCached::cache_get(&c, &4).unwrap().is_none());
    }

    #[test]
    #[should_panic(expected = "max_size must be greater than zero")]
    fn zero_set_max_size_panics() {
        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        let _ = c.set_max_size(0);
    }

    #[test]
    fn zero_try_set_max_size_returns_error() {
        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        assert_eq!(c.try_set_max_size(0), Err(SetMaxSizeError::ZeroSize));
        let r = c.try_set_max_size(8);
        assert_eq!(r, Ok(Some(4)));
        assert_eq!(c.capacity(), 8);
    }

    #[test]
    fn min_per_shard_clamp_applied_and_capacity_reflects_clamped_total() {
        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(4)
            .max_size(1024)
            .build()
            .unwrap();
        let prev = c.set_max_size(4);
        assert_eq!(prev, Some(1024));
        // 4 shards * 16 = 64
        assert_eq!(c.capacity(), 64);
    }

    #[test]
    fn resize_cache_built_with_per_shard_max_size_switches_to_total_policy() {
        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(4)
            .per_shard_max_size(10)
            .build()
            .unwrap();
        assert_eq!(c.capacity(), 40);
        let prev = c.set_max_size(100);
        assert_eq!(prev, Some(40));
        assert_eq!(c.capacity(), 100);
    }

    #[test]
    fn single_shard_exact_capacity_no_clamp() {
        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(1)
            .max_size(3)
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&c, 1, E { v: 1 }).unwrap();
        ConcurrentCached::cache_set(&c, 2, E { v: 2 }).unwrap();
        ConcurrentCached::cache_set(&c, 3, E { v: 3 }).unwrap();
        assert_eq!(c.len(), 3);
        let prev = c.set_max_size(1);
        assert_eq!(prev, Some(3));
        assert_eq!(c.capacity(), 1);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn trait_cache_capacity_reflects_resize() {
        let c = ShardedExpiringLruCacheBase::<u32, E>::builder()
            .shards(1)
            .max_size(4)
            .build()
            .unwrap();
        assert_eq!(ConcurrentCacheBase::cache_capacity(&c), Some(4));
        c.set_max_size(32);
        assert_eq!(ConcurrentCacheBase::cache_capacity(&c), Some(32));
    }

    #[test]
    fn shrink_evicts_by_lru_order_not_expiry_and_fires_on_evict_for_expired() {
        // CONTRACT: a capacity shrink on the sharded expiring-LRU store evicts the
        // least-recently-used entries *regardless of expiry status*. It is a pure
        // LRU capacity trim — it does NOT preferentially drop expired entries, and
        // on_evict fires for every dropped entry including expired ones. (The only
        // expiry-aware removal path is `evict()`, not `set_max_size`.)
        //
        // Layout below (single shard, cap 4), inserted oldest-first:
        //   key 1 EXPIRED, key 2 live, key 3 EXPIRED, key 4 live
        // LRU order after inserts (no gets): MRU = 4,3,2,1 = LRU.
        // Shrink to cap 2 evicts the two LRU keys 1 and 2 (one expired, one live),
        // leaving keys 3 (expired) and 4 (live). on_evict must fire twice.
        let evicted_keys = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let ek = evicted_keys.clone();
        let c = ShardedExpiringLruCacheBase::<u32, ExpVal>::builder()
            .shards(1)
            .max_size(4)
            .on_evict(move |k: &u32, _v: &ExpVal| {
                ek.lock().unwrap().push(*k);
            })
            .build()
            .unwrap();
        ConcurrentCached::cache_set(
            &c,
            1,
            ExpVal {
                v: 1,
                expired: true,
            },
        )
        .unwrap();
        ConcurrentCached::cache_set(
            &c,
            2,
            ExpVal {
                v: 2,
                expired: false,
            },
        )
        .unwrap();
        ConcurrentCached::cache_set(
            &c,
            3,
            ExpVal {
                v: 3,
                expired: true,
            },
        )
        .unwrap();
        ConcurrentCached::cache_set(
            &c,
            4,
            ExpVal {
                v: 4,
                expired: false,
            },
        )
        .unwrap();
        assert_eq!(c.len(), 4, "no lazy expiry sweep happened during inserts");

        let ev_before = c.metrics().evictions.unwrap();
        c.set_max_size(2);
        assert_eq!(c.capacity(), 2);

        let mut fired = evicted_keys.lock().unwrap().clone();
        fired.sort_unstable();
        assert_eq!(
            fired,
            vec![1, 2],
            "shrink must evict the two LRU keys (1,2) by recency, NOT the two expired keys (1,3)"
        );
        assert_eq!(
            c.metrics().evictions.unwrap() - ev_before,
            2,
            "both LRU-trim evictions (expired and live alike) must be counted"
        );
        // Survivors: key 3 (expired but LRU-recent) and key 4 (live). A cache_get on
        // key 3 is a miss because it is expired, but it was NOT dropped by the shrink.
        assert!(
            ConcurrentCached::cache_get(&c, &4).unwrap().is_some(),
            "live MRU key 4 survives"
        );
        assert_eq!(
            c.len(),
            2,
            "expired key 3 survives the capacity shrink (it is LRU-recent); only evict() would sweep it"
        );
    }
}

// ---------------------------------------------------------------------------
// Shared helper: verify per_shard_cap_from_total policy is consistent with
// what the builder produces. This pins the helper against the builder.
// ---------------------------------------------------------------------------

#[test]
fn per_shard_cap_matches_builder_for_various_sizes() {
    // For each (shards, max_size) pair, the cache built with max_size and the
    // cache after set_max_size(same) must report the same capacity().
    let cases: &[(usize, usize)] = &[
        (1, 1),
        (1, 64),
        (4, 8),   // triggers 16-per-shard floor
        (4, 100), // no floor: 25 per shard
        (8, 64),  // 8 per shard -> floor kicks in: 16 each -> 128
    ];

    for &(shards, max_size) in cases {
        let built = cached::ShardedLruCacheBase::<u32, u32>::builder()
            .shards(shards)
            .max_size(max_size)
            .build()
            .unwrap();
        let built_cap = built.capacity();

        // Build a larger cache and resize to max_size.
        let resized = cached::ShardedLruCacheBase::<u32, u32>::builder()
            .shards(shards)
            .max_size(max_size * 10 + 100)
            .build()
            .unwrap();
        resized.set_max_size(max_size);
        assert_eq!(
            resized.capacity(),
            built_cap,
            "set_max_size({max_size}) on {shards}-shard cache must yield same capacity as builder"
        );
    }
}
