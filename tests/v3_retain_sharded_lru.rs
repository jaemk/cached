//! Consumer-shaped coverage for `retain` on the three LRU-backed sharded stores:
//! `ShardedLruCache`, `ShardedExpiringLruCache`, and (behind `time_stores`)
//! `ShardedLruTtlCache`. Exercised only through the crate's public API, as an external
//! downstream consumer would use it.
//!
//! Sharded `retain` locks one shard at a time (not atomic across shards), so beyond plain
//! predicate filtering this file also certifies: `on_evict` fires exactly once per removed
//! entry with the correct `(k, v)`; the eviction counter delta matches the removed count;
//! the two expiry-aware stores drop expired entries even under a keep-everything predicate;
//! cross-shard behavior with multiple non-empty shards; and that a `retain` that changes
//! nothing about survivor recency still leaves the correct LRU victim for a later capacity
//! eviction.

use cached::{ConcurrentCachedExt, Expires, ShardHasher, ShardedExpiringLruCache, ShardedLruCache};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq)]
struct Token {
    v: u32,
    expired: bool,
}

impl Expires for Token {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

fn live(v: u32) -> Token {
    Token { v, expired: false }
}

fn expired(v: u32) -> Token {
    Token { v, expired: true }
}

/// Deterministic shard router for `u32` keys: shard selection reads the upper 32 bits of
/// the hash (`(hash >> 32) & mask`), so left-shifting the key by 32 makes `shard == key &
/// mask` — i.e. `key % shard_count` for a power-of-two shard count. Lets tests pin specific
/// keys to specific shards instead of relying on `DefaultShardHasher`'s distribution.
#[derive(Clone)]
struct KeyIsShardHasher;

impl ShardHasher<u32> for KeyIsShardHasher {
    fn shard_hash(&self, key: &u32) -> u64 {
        (*key as u64) << 32
    }
}

// ---------------------------------------------------------------------------------------
// ShardedLruCache
// ---------------------------------------------------------------------------------------

mod sharded_lru_cache {
    use super::*;

    #[allow(clippy::type_complexity)]
    fn events_cache(
        shards: usize,
        max_size: usize,
    ) -> (ShardedLruCache<u32, u32>, Arc<Mutex<Vec<(u32, u32)>>>) {
        let events = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
        let events2 = events.clone();
        let cache = ShardedLruCache::<u32, u32>::builder()
            .shards(shards)
            .max_size(max_size)
            .on_evict(move |k: &u32, v: &u32| {
                events2.lock().unwrap().push((*k, *v));
            })
            .build()
            .unwrap();
        (cache, events)
    }

    /// `retain` returns the number of entries removed (`usize`), not `()`.
    #[test]
    fn removes_entries_failing_the_predicate() {
        let (cache, events) = events_cache(1, 64);
        for k in 0u32..6 {
            cache.set(k, k * 10);
        }

        let before = cache.metrics().evictions.unwrap();
        let removed: usize = cache.retain(|k, _v| k % 2 == 0);
        assert_eq!(removed, 3, "retain must return the removed count");

        let mut survivors: Vec<u32> = (0..6u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![0, 2, 4]);
        assert_eq!(cache.len(), 3);

        // on_evict fired exactly once per removed entry, with the correct (k, v).
        let mut fired = events.lock().unwrap().clone();
        fired.sort_unstable();
        assert_eq!(fired, vec![(1, 10), (3, 30), (5, 50)]);

        // The eviction counter delta matches the removed count.
        assert_eq!(cache.metrics().evictions.unwrap() - before, 3);
    }

    #[test]
    fn predicate_receives_key_and_value() {
        let (cache, _events) = events_cache(1, 64);
        cache.set(1, 10);
        cache.set(2, 200);
        cache.set(3, 30);

        let mut seen: Vec<(u32, u32)> = Vec::new();
        cache.retain(|k, v| {
            seen.push((*k, *v));
            *v >= 100
        });
        seen.sort_unstable();
        assert_eq!(seen, vec![(1, 10), (2, 200), (3, 30)]);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&2));
    }

    #[test]
    fn keeping_everything_is_a_no_op() {
        let (cache, events) = events_cache(1, 64);
        for k in 0u32..10 {
            cache.set(k, k);
        }
        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| true);
        assert_eq!(cache.len(), 10);
        assert_eq!(cache.metrics().evictions.unwrap(), before);
        assert!(events.lock().unwrap().is_empty());
    }

    /// Multiple shards must each be filtered: build with 4 shards, insert enough keys that
    /// at least 2 shards are non-empty (asserted as a precondition), then retain across
    /// all of them.
    #[test]
    fn cross_shard_retain_filters_every_shard() {
        let (cache, events) = events_cache(4, 256);
        for k in 0u32..64 {
            cache.set(k, k);
        }

        let nonempty_shards = cache.shard_sizes().iter().filter(|&&n| n > 0).count();
        assert!(
            nonempty_shards >= 2,
            "test precondition: need >= 2 non-empty shards, got {nonempty_shards} \
             (shard_sizes = {:?})",
            cache.shard_sizes()
        );

        let before = cache.metrics().evictions.unwrap();
        cache.retain(|k, _| k % 2 == 0);

        let mut survivors: Vec<u32> = (0..64u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        let expected: Vec<u32> = (0..64u32).filter(|k| k % 2 == 0).collect();
        assert_eq!(survivors, expected);
        assert_eq!(cache.len(), 32);

        let removed_count = 32u64;
        assert_eq!(cache.metrics().evictions.unwrap() - before, removed_count);
        let mut fired: Vec<u32> = events.lock().unwrap().iter().map(|(k, _)| *k).collect();
        fired.sort_unstable();
        let expected_removed: Vec<u32> = (0..64u32).filter(|k| k % 2 != 0).collect();
        assert_eq!(fired, expected_removed);
    }

    /// A `retain` that keeps every entry must not disturb recency: a subsequent capacity
    /// eviction should still identify the correct (least-recently-used) victim.
    #[test]
    fn retain_does_not_disturb_recency_for_later_capacity_eviction() {
        let (cache, events) = events_cache(1, 3);
        cache.set(1, 100);
        cache.set(2, 200);
        cache.set(3, 300);
        // Promote 1 to most-recently-used: MRU order becomes 1, 3, 2 (2 is now LRU).
        assert_eq!(cache.get(&1), Some(100));

        // Keep-everything retain must not touch recency order.
        cache.retain(|_, _| true);
        assert!(events.lock().unwrap().is_empty());

        // Inserting a 4th entry forces a capacity eviction of the LRU victim: key 2.
        cache.set(4, 400);
        let fired = events.lock().unwrap().clone();
        assert_eq!(
            fired,
            vec![(2, 200)],
            "capacity eviction after retain must still evict the correct LRU victim"
        );
    }

    /// Multi-shard variant of `retain_does_not_disturb_recency_for_later_capacity_eviction`:
    /// pins keys to specific shards with a deterministic hasher so recency preservation can
    /// be verified independently, per shard, after a cross-shard retain pass.
    #[test]
    fn retain_preserves_recency_independently_across_shards() {
        let events = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
        let events2 = events.clone();
        let cache = ShardedLruCache::<u32, u32>::builder()
            .shards(2)
            .hasher(KeyIsShardHasher)
            .per_shard_max_size(3)
            .on_evict(move |k: &u32, v: &u32| {
                events2.lock().unwrap().push((*k, *v));
            })
            .build()
            .unwrap();

        // Shard 0 (even keys): insert 0, 2, 4 then promote 0 -> LRU victim becomes 2.
        cache.set(0, 100);
        cache.set(2, 200);
        cache.set(4, 400);
        // `KeyIsShardHasher` implements `ShardHasher` by hand, not `BuildHasher`, so it is not
        // `BorrowedKeyRouting` and the inherent borrowed-key `get` does not exist on stores built
        // with it. `ConcurrentCachedExt::get` takes `&K`, promotes recency exactly like the
        // inherent form, and stays available. Do not swap in a `BuildHasher` to get the inherent
        // call back: pinning keys to known shards is the point of these tests.
        assert_eq!(ConcurrentCachedExt::get(&cache, &0).unwrap(), Some(100));

        // Shard 1 (odd keys): insert 1, 3, 5 then promote 1 -> LRU victim becomes 3.
        cache.set(1, 1000);
        cache.set(3, 3000);
        cache.set(5, 5000);
        assert_eq!(ConcurrentCachedExt::get(&cache, &1).unwrap(), Some(1000));

        // No-op retain across both shards must not disturb either shard's recency order.
        cache.retain(|_, _| true);
        assert!(events.lock().unwrap().is_empty());

        // Overflow each shard by one: each must evict its own predicted victim, not the
        // other shard's, and not the just-promoted key.
        cache.set(6, 600); // shard 0 overflow -> evicts 2
        cache.set(7, 7000); // shard 1 overflow -> evicts 3

        assert_eq!(
            events.lock().unwrap().clone(),
            vec![(2, 200), (3, 3000)],
            "post-retain capacity eviction must pick the correct per-shard LRU victim"
        );
    }

    /// `retain` on a cache with no entries must not fire `on_evict` or move the eviction
    /// counter, even with a predicate that would reject everything.
    #[test]
    fn retain_on_empty_cache_is_a_full_noop() {
        let (cache, events) = events_cache(1, 64);
        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| false);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.metrics().evictions.unwrap(), before);
        assert!(events.lock().unwrap().is_empty());
    }

    /// A predicate that rejects everything must empty the shard entirely: every entry
    /// fires `on_evict` and counts as an eviction.
    #[test]
    fn retain_removing_every_entry_empties_the_shard() {
        let (cache, events) = events_cache(1, 64);
        for k in 0u32..8 {
            cache.set(k, k);
        }
        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| false);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
        assert_eq!(cache.metrics().evictions.unwrap() - before, 8);
        assert_eq!(events.lock().unwrap().len(), 8);
    }

    /// The predicate must observe the CURRENT (most recently set) value for a key that was
    /// overwritten before `retain` runs, not a stale first-inserted value.
    #[test]
    fn predicate_sees_current_value_for_overwritten_key() {
        let (cache, _events) = events_cache(1, 64);
        cache.set(1, 100);
        cache.set(1, 999); // overwrite before retain
        let mut seen: Option<u32> = None;
        cache.retain(|k, v| {
            if *k == 1 {
                seen = Some(*v);
            }
            true
        });
        assert_eq!(
            seen,
            Some(999),
            "predicate must see the post-overwrite value, not the original insert"
        );
    }

    /// `on_evict` fires only after the affected shard's write lock has been released
    /// (documented contract). A callback that calls back into the same cache — even for a
    /// single-shard cache, where any lock re-acquisition would hit the same lock — must not
    /// deadlock.
    #[test]
    fn on_evict_may_safely_call_back_into_the_cache_without_deadlock() {
        let handle: Arc<OnceLock<ShardedLruCache<u32, u32>>> = Arc::new(OnceLock::new());
        let handle2 = handle.clone();
        let reentrant_calls = Arc::new(Mutex::new(0u32));
        let reentrant_calls2 = reentrant_calls.clone();
        let cache = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .on_evict(move |k, _v| {
                if let Some(c) = handle2.get() {
                    // Reads on the same cache from inside on_evict: this must complete
                    // rather than block forever if the shard lock were still held.
                    let _ = c.len();
                    let _ = c.get(k);
                    *reentrant_calls2.lock().unwrap() += 1;
                }
            })
            .build()
            .unwrap();
        handle
            .set(cache.clone())
            .unwrap_or_else(|_| panic!("handle set exactly once"));

        for i in 0u32..6 {
            cache.set(i, i * 10);
        }
        cache.retain(|k, _| k % 2 == 0);

        assert_eq!(cache.len(), 3);
        assert_eq!(
            *reentrant_calls.lock().unwrap(),
            3,
            "on_evict must have run its reentrant len()/get() call for every removed entry \
             without deadlocking"
        );
    }

    /// A predicate that panics must unwind cleanly out of `retain` without poisoning the
    /// shard lock (the sharded stores use `parking_lot`, which does not poison on panic) —
    /// the cache must remain fully usable afterward.
    #[test]
    fn predicate_panic_does_not_poison_the_shard_lock() {
        let (cache, _events) = events_cache(1, 64);
        for k in 0u32..4 {
            cache.set(k, k);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.retain(|k, _| {
                if *k == 2 {
                    panic!("predicate boom");
                }
                true
            });
        }));
        assert!(
            result.is_err(),
            "predicate panic must propagate out of retain"
        );

        // The cache must still be fully usable: no poisoned lock left behind.
        cache.set(100, 100);
        assert_eq!(cache.get(&100), Some(100));
    }
}

// ---------------------------------------------------------------------------------------
// ShardedExpiringLruCache
// ---------------------------------------------------------------------------------------

mod sharded_expiring_lru_cache {
    use super::*;

    #[allow(clippy::type_complexity)]
    fn events_cache(
        shards: usize,
        max_size: usize,
    ) -> (
        ShardedExpiringLruCache<u32, Token>,
        Arc<Mutex<Vec<(u32, Token)>>>,
    ) {
        let events = Arc::new(Mutex::new(Vec::<(u32, Token)>::new()));
        let events2 = events.clone();
        let cache = ShardedExpiringLruCache::<u32, Token>::builder()
            .shards(shards)
            .max_size(max_size)
            .on_evict(move |k: &u32, v: &Token| {
                events2.lock().unwrap().push((*k, v.clone()));
            })
            .build()
            .unwrap();
        (cache, events)
    }

    #[test]
    fn removes_entries_failing_the_predicate() {
        let (cache, events) = events_cache(1, 64);
        for k in 0u32..6 {
            cache.set(k, live(k * 10));
        }

        let before = cache.metrics().evictions.unwrap();
        let removed: usize = cache.retain(|k, _v| k % 2 == 0);
        assert_eq!(removed, 3, "retain must return the removed count");

        let mut survivors: Vec<u32> = (0..6u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![0, 2, 4]);
        assert_eq!(cache.len(), 3);

        let mut fired: Vec<(u32, u32)> = events
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, v.v))
            .collect();
        fired.sort_unstable();
        assert_eq!(fired, vec![(1, 10), (3, 30), (5, 50)]);
        assert_eq!(cache.metrics().evictions.unwrap() - before, 3);
    }

    /// Expired entries are removed regardless of the predicate — even a keep-everything
    /// predicate must not save an already-expired entry.
    #[test]
    fn expired_entries_removed_under_keep_everything_predicate() {
        let (cache, events) = events_cache(1, 64);
        cache.set(1, live(10));
        cache.set(2, expired(20));
        cache.set(3, live(30));
        cache.set(4, expired(40));

        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| true);

        let mut survivors: Vec<u32> = (1..=4u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![1, 3]);
        assert_eq!(cache.len(), 2);

        let mut fired: Vec<u32> = events.lock().unwrap().iter().map(|(k, _)| *k).collect();
        fired.sort_unstable();
        assert_eq!(fired, vec![2, 4]);
        assert_eq!(cache.metrics().evictions.unwrap() - before, 2);
    }

    /// Callback path (an `on_evict` is configured): the returned count must fold together
    /// entries removed for having already expired AND entries the predicate itself rejected,
    /// in the same call. Neither cohort alone (3 predicate-rejected, 2 expired) equals the
    /// total (5), so this certifies `retain` isn't just returning one of the two categories.
    #[test]
    fn retain_callback_return_count_includes_both_expired_and_predicate_rejected() {
        let (cache, events) = events_cache(1, 64);
        cache.set(0, live(0));
        cache.set(1, live(10));
        cache.set(2, expired(20));
        cache.set(3, live(30));
        cache.set(4, expired(40));
        cache.set(5, live(50));

        let before = cache.metrics().evictions.unwrap();
        // Keep only even keys: rejects the odd LIVE keys (1, 3, 5) via the predicate; the
        // already-expired keys (2, 4) are removed regardless of what the predicate would
        // have said about them (both are even, so the predicate itself would keep them).
        let removed: usize = cache.retain(|k, _v| k % 2 == 0);

        assert_eq!(
            removed, 5,
            "retain's return must equal expired + predicate-rejected, not just one or the other"
        );
        assert_eq!(cache.len(), 1, "only key 0 (even, live) survives");
        assert!(cache.contains(&0));
        assert_eq!(cache.metrics().evictions.unwrap() - before, 5);
        assert_eq!(
            events.lock().unwrap().len(),
            5,
            "on_evict fires exactly once per removal"
        );
    }

    /// Same mixed scenario as above but with no `on_evict` configured.
    #[test]
    fn retain_no_callback_return_count_includes_both_expired_and_predicate_rejected() {
        let cache = ShardedExpiringLruCache::<u32, Token>::builder()
            .shards(1)
            .max_size(64)
            .build()
            .unwrap();
        cache.set(0, live(0));
        cache.set(1, live(10));
        cache.set(2, expired(20));
        cache.set(3, live(30));
        cache.set(4, expired(40));
        cache.set(5, live(50));

        let removed: usize = cache.retain(|k, _v| k % 2 == 0);

        assert_eq!(
            removed, 5,
            "retain's return must equal expired + predicate-rejected, not just one or the other"
        );
        assert_eq!(cache.len(), 1, "only key 0 (even, live) survives");
        assert!(cache.contains(&0));
    }

    #[test]
    fn cross_shard_retain_filters_every_shard() {
        let (cache, events) = events_cache(4, 256);
        for k in 0u32..64 {
            cache.set(k, live(k));
        }

        let nonempty_shards = cache.shard_sizes().iter().filter(|&&n| n > 0).count();
        assert!(
            nonempty_shards >= 2,
            "test precondition: need >= 2 non-empty shards, got {nonempty_shards} \
             (shard_sizes = {:?})",
            cache.shard_sizes()
        );

        let before = cache.metrics().evictions.unwrap();
        cache.retain(|k, _| k % 2 == 0);

        let mut survivors: Vec<u32> = (0..64u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        let expected: Vec<u32> = (0..64u32).filter(|k| k % 2 == 0).collect();
        assert_eq!(survivors, expected);
        assert_eq!(cache.metrics().evictions.unwrap() - before, 32);

        let mut fired: Vec<u32> = events.lock().unwrap().iter().map(|(k, _)| *k).collect();
        fired.sort_unstable();
        let expected_removed: Vec<u32> = (0..64u32).filter(|k| k % 2 != 0).collect();
        assert_eq!(fired, expected_removed);
    }

    #[test]
    fn retain_does_not_disturb_recency_for_later_capacity_eviction() {
        let (cache, events) = events_cache(1, 3);
        cache.set(1, live(100));
        cache.set(2, live(200));
        cache.set(3, live(300));
        assert_eq!(cache.get(&1).map(|v| v.v), Some(100));

        cache.retain(|_, _| true);
        assert!(events.lock().unwrap().is_empty());

        cache.set(4, live(400));
        let fired: Vec<(u32, u32)> = events
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, v.v))
            .collect();
        assert_eq!(
            fired,
            vec![(2, 200)],
            "capacity eviction after retain must still evict the correct LRU victim"
        );
    }

    /// `retain(|_, _| true)` and `evict()` both remove exactly the expired entries and no
    /// others — they must agree on which entries are expired given identical input.
    #[test]
    fn retain_and_evict_agree_on_which_entries_are_expired() {
        let build = || {
            ShardedExpiringLruCache::<u32, Token>::builder()
                .shards(1)
                .max_size(64)
                .build()
                .unwrap()
        };
        let evict_cache = build();
        let retain_cache = build();
        for i in 0u32..10 {
            let v = if i % 3 == 0 { expired(i) } else { live(i) };
            evict_cache.set(i, v.clone());
            retain_cache.set(i, v);
        }

        let n_evicted = evict_cache.evict();
        retain_cache.retain(|_, _| true);

        let mut evict_survivors: Vec<u32> =
            (0..10u32).filter(|k| evict_cache.contains(k)).collect();
        let mut retain_survivors: Vec<u32> =
            (0..10u32).filter(|k| retain_cache.contains(k)).collect();
        evict_survivors.sort_unstable();
        retain_survivors.sort_unstable();

        let expected: Vec<u32> = (0..10u32).filter(|k| k % 3 != 0).collect();
        assert_eq!(n_evicted, 4, "expired keys 0, 3, 6, 9 must be evicted");
        assert_eq!(evict_survivors, expected);
        assert_eq!(
            retain_survivors, evict_survivors,
            "retain and evict must agree on exactly which entries have expired"
        );
    }

    #[test]
    fn retain_preserves_recency_independently_across_shards() {
        let events = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
        let events2 = events.clone();
        let cache = ShardedExpiringLruCache::<u32, Token>::builder()
            .shards(2)
            .hasher(KeyIsShardHasher)
            .per_shard_max_size(3)
            .on_evict(move |k: &u32, v: &Token| {
                events2.lock().unwrap().push((*k, v.v));
            })
            .build()
            .unwrap();

        cache.set(0, live(100));
        cache.set(2, live(200));
        cache.set(4, live(400));
        // Trait form: `KeyIsShardHasher` is not a `BuildHasher`, so borrowed-key inherent
        // lookups do not exist here. See the note in
        // `sharded_lru_cache::retain_preserves_recency_independently_across_shards`.
        assert_eq!(
            ConcurrentCachedExt::get(&cache, &0).unwrap().map(|v| v.v),
            Some(100)
        );

        cache.set(1, live(1000));
        cache.set(3, live(3000));
        cache.set(5, live(5000));
        assert_eq!(
            ConcurrentCachedExt::get(&cache, &1).unwrap().map(|v| v.v),
            Some(1000)
        );

        cache.retain(|_, _| true);
        assert!(events.lock().unwrap().is_empty());

        cache.set(6, live(600));
        cache.set(7, live(7000));

        assert_eq!(
            events.lock().unwrap().clone(),
            vec![(2, 200), (3, 3000)],
            "post-retain capacity eviction must pick the correct per-shard LRU victim"
        );
    }

    #[test]
    fn retain_on_empty_cache_is_a_full_noop() {
        let (cache, events) = events_cache(1, 64);
        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| false);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.metrics().evictions.unwrap(), before);
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn retain_removing_every_entry_empties_the_shard() {
        let (cache, events) = events_cache(1, 64);
        for k in 0u32..8 {
            cache.set(k, live(k));
        }
        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| false);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
        assert_eq!(cache.metrics().evictions.unwrap() - before, 8);
        assert_eq!(events.lock().unwrap().len(), 8);
    }

    #[test]
    fn predicate_sees_current_value_for_overwritten_key() {
        let (cache, _events) = events_cache(1, 64);
        cache.set(1, live(100));
        cache.set(1, live(999));
        let mut seen: Option<u32> = None;
        cache.retain(|k, v| {
            if *k == 1 {
                seen = Some(v.v);
            }
            true
        });
        assert_eq!(seen, Some(999));
    }

    #[test]
    fn on_evict_may_safely_call_back_into_the_cache_without_deadlock() {
        let handle: Arc<OnceLock<ShardedExpiringLruCache<u32, Token>>> = Arc::new(OnceLock::new());
        let handle2 = handle.clone();
        let reentrant_calls = Arc::new(Mutex::new(0u32));
        let reentrant_calls2 = reentrant_calls.clone();
        let cache = ShardedExpiringLruCache::<u32, Token>::builder()
            .shards(1)
            .max_size(64)
            .on_evict(move |k, _v| {
                if let Some(c) = handle2.get() {
                    let _ = c.len();
                    let _ = c.get(k);
                    *reentrant_calls2.lock().unwrap() += 1;
                }
            })
            .build()
            .unwrap();
        handle
            .set(cache.clone())
            .unwrap_or_else(|_| panic!("handle set exactly once"));

        for i in 0u32..6 {
            cache.set(i, live(i * 10));
        }
        cache.retain(|k, _| k % 2 == 0);

        assert_eq!(cache.len(), 3);
        assert_eq!(*reentrant_calls.lock().unwrap(), 3);
    }

    #[test]
    fn predicate_panic_does_not_poison_the_shard_lock() {
        let (cache, _events) = events_cache(1, 64);
        for k in 0u32..4 {
            cache.set(k, live(k));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.retain(|k, _| {
                if *k == 2 {
                    panic!("predicate boom");
                }
                true
            });
        }));
        assert!(result.is_err());

        cache.set(100, live(100));
        assert_eq!(cache.get(&100).map(|v| v.v), Some(100));
    }
}

// ---------------------------------------------------------------------------------------
// ShardedLruTtlCache (time_stores)
// ---------------------------------------------------------------------------------------

#[cfg(feature = "time_stores")]
mod sharded_lru_ttl_cache {
    use super::*;
    use cached::ShardedLruTtlCache;
    use cached::time::Duration;

    #[allow(clippy::type_complexity)]
    fn events_cache(
        shards: usize,
        max_size: usize,
        ttl: Duration,
    ) -> (ShardedLruTtlCache<u32, u32>, Arc<Mutex<Vec<(u32, u32)>>>) {
        let events = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
        let events2 = events.clone();
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(shards)
            .max_size(max_size)
            .ttl(ttl)
            .on_evict(move |k: &u32, v: &u32| {
                events2.lock().unwrap().push((*k, *v));
            })
            .build()
            .unwrap();
        (cache, events)
    }

    #[test]
    fn removes_entries_failing_the_predicate() {
        let (cache, events) = events_cache(1, 64, Duration::from_secs(60));
        for k in 0u32..6 {
            cache.set(k, k * 10);
        }

        let before = cache.metrics().evictions.unwrap();
        let removed: usize = cache.retain(|k, _v| k % 2 == 0);
        assert_eq!(removed, 3, "retain must return the removed count");

        let mut survivors: Vec<u32> = (0..6u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![0, 2, 4]);
        assert_eq!(cache.len(), 3);

        let mut fired = events.lock().unwrap().clone();
        fired.sort_unstable();
        assert_eq!(fired, vec![(1, 10), (3, 30), (5, 50)]);
        assert_eq!(cache.metrics().evictions.unwrap() - before, 3);
    }

    /// Expired entries are removed regardless of the predicate — even a keep-everything
    /// predicate must not save an already-expired entry.
    #[test]
    fn expired_entries_removed_under_keep_everything_predicate() {
        let (cache, events) = events_cache(1, 64, Duration::from_millis(30));
        cache.set(1, 10);
        cache.set(2, 20);
        std::thread::sleep(std::time::Duration::from_millis(80));
        cache.set(3, 30); // fresh TTL, still live
        cache.set(4, 40); // fresh TTL, still live

        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| true);

        let mut survivors: Vec<u32> = (1..=4u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![3, 4]);

        let mut fired: Vec<u32> = events.lock().unwrap().iter().map(|(k, _)| *k).collect();
        fired.sort_unstable();
        assert_eq!(fired, vec![1, 2]);
        assert_eq!(cache.metrics().evictions.unwrap() - before, 2);
    }

    /// Callback path (an `on_evict` is configured): the returned count must fold together
    /// entries removed for having already expired AND entries the predicate itself rejected,
    /// in the same call. Keys 1, 2 expire regardless of the predicate; keys 3, 5 are live but
    /// predicate-rejected. Neither cohort alone (2 expired, 2 predicate-rejected) equals the
    /// total (4) by coincidence of overlap -- they're disjoint key ranges here, so the total
    /// genuinely requires both.
    #[test]
    fn retain_callback_return_count_includes_both_expired_and_predicate_rejected() {
        let (cache, events) = events_cache(1, 64, Duration::from_millis(30));
        // Keys 1, 2 will expire.
        cache.set(1, 10);
        cache.set(2, 20);
        std::thread::sleep(std::time::Duration::from_millis(80));
        // Fresh keys with a live TTL; the predicate rejects the odd ones (3, 5).
        cache.set(3, 30);
        cache.set(4, 40);
        cache.set(5, 50);
        cache.set(6, 60);

        let before = cache.metrics().evictions.unwrap();
        let removed: usize = cache.retain(|k, _v| k % 2 == 0);

        assert_eq!(
            removed, 4,
            "retain's return must equal expired + predicate-rejected, not just one or the other"
        );
        let mut survivors: Vec<u32> = (1..=6u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![4, 6]);
        assert_eq!(cache.metrics().evictions.unwrap() - before, 4);
        assert_eq!(
            events.lock().unwrap().len(),
            4,
            "on_evict fires exactly once per removal"
        );
    }

    /// Same mixed scenario as above but with no `on_evict` configured.
    #[test]
    fn retain_no_callback_return_count_includes_both_expired_and_predicate_rejected() {
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_millis(30))
            .build()
            .unwrap();
        cache.set(1, 10);
        cache.set(2, 20);
        std::thread::sleep(std::time::Duration::from_millis(80));
        cache.set(3, 30);
        cache.set(4, 40);
        cache.set(5, 50);
        cache.set(6, 60);

        let removed: usize = cache.retain(|k, _v| k % 2 == 0);

        assert_eq!(
            removed, 4,
            "retain's return must equal expired + predicate-rejected, not just one or the other"
        );
        let mut survivors: Vec<u32> = (1..=6u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![4, 6]);
    }

    #[test]
    fn cross_shard_retain_filters_every_shard() {
        let (cache, events) = events_cache(4, 256, Duration::from_secs(60));
        for k in 0u32..64 {
            cache.set(k, k);
        }

        let nonempty_shards = cache.shard_sizes().iter().filter(|&&n| n > 0).count();
        assert!(
            nonempty_shards >= 2,
            "test precondition: need >= 2 non-empty shards, got {nonempty_shards} \
             (shard_sizes = {:?})",
            cache.shard_sizes()
        );

        let before = cache.metrics().evictions.unwrap();
        cache.retain(|k, _| k % 2 == 0);

        let mut survivors: Vec<u32> = (0..64u32).filter(|k| cache.contains(k)).collect();
        survivors.sort_unstable();
        let expected: Vec<u32> = (0..64u32).filter(|k| k % 2 == 0).collect();
        assert_eq!(survivors, expected);
        assert_eq!(cache.metrics().evictions.unwrap() - before, 32);

        let mut fired: Vec<u32> = events.lock().unwrap().iter().map(|(k, _)| *k).collect();
        fired.sort_unstable();
        let expected_removed: Vec<u32> = (0..64u32).filter(|k| k % 2 != 0).collect();
        assert_eq!(fired, expected_removed);
    }

    #[test]
    fn retain_does_not_disturb_recency_for_later_capacity_eviction() {
        let (cache, events) = events_cache(1, 3, Duration::from_secs(60));
        cache.set(1, 100);
        cache.set(2, 200);
        cache.set(3, 300);
        assert_eq!(cache.get(&1), Some(100));

        cache.retain(|_, _| true);
        assert!(events.lock().unwrap().is_empty());

        cache.set(4, 400);
        let fired = events.lock().unwrap().clone();
        assert_eq!(
            fired,
            vec![(2, 200)],
            "capacity eviction after retain must still evict the correct LRU victim"
        );
    }

    /// An entry whose TTL has not elapsed must never be dropped by a keep-everything
    /// `retain`, no matter how the predicate answers. Deterministic (1 hour TTL, no sleep):
    /// unlike the sibling `expired_entries_removed_under_keep_everything_predicate` test,
    /// this isolates the "not yet expired" half of the `now >= expires_at` boundary without
    /// relying on a real-time margin.
    #[test]
    fn not_yet_expired_entry_is_never_removed_by_keep_everything_retain() {
        let (cache, events) = events_cache(1, 64, Duration::from_secs(3600));
        for k in 0u32..5 {
            cache.set(k, k);
        }
        cache.retain(|_, _| true);
        assert_eq!(cache.len(), 5);
        assert!(events.lock().unwrap().is_empty());
    }

    /// `retain(|_, _| true)` and `evict()` must agree on exactly which entries have expired
    /// given an identical elapsed-TTL scenario — both implement the same `now >=
    /// expires_at` convention.
    #[test]
    fn retain_and_evict_agree_on_which_entries_expire() {
        let build = || {
            ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .max_size(64)
                .ttl(Duration::from_millis(30))
                .build()
                .unwrap()
        };
        let evict_cache = build();
        let retain_cache = build();
        for i in 0u32..5 {
            evict_cache.set(i, i);
            retain_cache.set(i, i);
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Fresh entries inserted just before the check: still live on both caches.
        for i in 5u32..10 {
            evict_cache.set(i, i);
            retain_cache.set(i, i);
        }

        let n_evicted = evict_cache.evict();
        retain_cache.retain(|_, _| true);

        let mut evict_survivors: Vec<u32> =
            (0..10u32).filter(|k| evict_cache.contains(k)).collect();
        let mut retain_survivors: Vec<u32> =
            (0..10u32).filter(|k| retain_cache.contains(k)).collect();
        evict_survivors.sort_unstable();
        retain_survivors.sort_unstable();

        assert_eq!(n_evicted, 5, "expired keys 0..5 must be evicted");
        assert_eq!(evict_survivors, vec![5, 6, 7, 8, 9]);
        assert_eq!(
            retain_survivors, evict_survivors,
            "retain and evict must agree on exactly which entries have expired"
        );
    }

    #[test]
    fn retain_preserves_recency_independently_across_shards() {
        let events = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
        let events2 = events.clone();
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(2)
            .hasher(KeyIsShardHasher)
            .per_shard_max_size(3)
            .ttl(Duration::from_secs(60))
            .on_evict(move |k: &u32, v: &u32| {
                events2.lock().unwrap().push((*k, *v));
            })
            .build()
            .unwrap();

        cache.set(0, 100);
        cache.set(2, 200);
        cache.set(4, 400);
        // Trait form: `KeyIsShardHasher` is not a `BuildHasher`, so borrowed-key inherent
        // lookups do not exist here. See the note in
        // `sharded_lru_cache::retain_preserves_recency_independently_across_shards`.
        assert_eq!(ConcurrentCachedExt::get(&cache, &0).unwrap(), Some(100));

        cache.set(1, 1000);
        cache.set(3, 3000);
        cache.set(5, 5000);
        assert_eq!(ConcurrentCachedExt::get(&cache, &1).unwrap(), Some(1000));

        cache.retain(|_, _| true);
        assert!(events.lock().unwrap().is_empty());

        cache.set(6, 600);
        cache.set(7, 7000);

        assert_eq!(
            events.lock().unwrap().clone(),
            vec![(2, 200), (3, 3000)],
            "post-retain capacity eviction must pick the correct per-shard LRU victim"
        );
    }

    #[test]
    fn retain_on_empty_cache_is_a_full_noop() {
        let (cache, events) = events_cache(1, 64, Duration::from_secs(60));
        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| false);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.metrics().evictions.unwrap(), before);
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn retain_removing_every_entry_empties_the_shard() {
        let (cache, events) = events_cache(1, 64, Duration::from_secs(60));
        for k in 0u32..8 {
            cache.set(k, k);
        }
        let before = cache.metrics().evictions.unwrap();
        cache.retain(|_, _| false);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
        assert_eq!(cache.metrics().evictions.unwrap() - before, 8);
        assert_eq!(events.lock().unwrap().len(), 8);
    }

    #[test]
    fn predicate_sees_current_value_for_overwritten_key() {
        let (cache, _events) = events_cache(1, 64, Duration::from_secs(60));
        cache.set(1, 100);
        cache.set(1, 999);
        let mut seen: Option<u32> = None;
        cache.retain(|k, v| {
            if *k == 1 {
                seen = Some(*v);
            }
            true
        });
        assert_eq!(seen, Some(999));
    }

    #[test]
    fn on_evict_may_safely_call_back_into_the_cache_without_deadlock() {
        let handle: Arc<OnceLock<ShardedLruTtlCache<u32, u32>>> = Arc::new(OnceLock::new());
        let handle2 = handle.clone();
        let reentrant_calls = Arc::new(Mutex::new(0u32));
        let reentrant_calls2 = reentrant_calls.clone();
        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .on_evict(move |k, _v| {
                if let Some(c) = handle2.get() {
                    let _ = c.len();
                    let _ = c.get(k);
                    *reentrant_calls2.lock().unwrap() += 1;
                }
            })
            .build()
            .unwrap();
        handle
            .set(cache.clone())
            .unwrap_or_else(|_| panic!("handle set exactly once"));

        for i in 0u32..6 {
            cache.set(i, i * 10);
        }
        cache.retain(|k, _| k % 2 == 0);

        assert_eq!(cache.len(), 3);
        assert_eq!(*reentrant_calls.lock().unwrap(), 3);
    }

    #[test]
    fn predicate_panic_does_not_poison_the_shard_lock() {
        let (cache, _events) = events_cache(1, 64, Duration::from_secs(60));
        for k in 0u32..4 {
            cache.set(k, k);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.retain(|k, _| {
                if *k == 2 {
                    panic!("predicate boom");
                }
                true
            });
        }));
        assert!(result.is_err());

        cache.set(100, 100);
        assert_eq!(cache.get(&100), Some(100));
    }
}

// ── ConcurrentCachePeekAsync on the LRU-family stores (specs/traits-concurrent.md CTRAIT-5,
// specs/design/0040-peek-is-an-in-memory-concept.md) ────────────────────────────────────────
//
// `tests/v3_concurrent_peek_async.rs` certifies the trait definition itself (no default body,
// the `async_peek` alias, the prelude re-export, `Send`) against a local store. This module
// certifies the concrete delegation on `ShardedLruCache`, `ShardedLruTtlCache`, and
// `ShardedExpiringLruCache`: `async_cache_peek` must agree with the sync `cache_peek` / inherent
// `peek` (in particular: no LRU recency promotion), with `Self::Error = Infallible`, and must
// not move the hit/miss metrics.
#[cfg(feature = "async_core")]
mod concurrent_peek_async {
    use super::*;
    use cached::{ConcurrentCacheBase, ConcurrentCachePeek, ConcurrentCachePeekAsync};

    #[tokio::test]
    async fn sharded_lru_async_peek_matches_sync_peek_and_does_not_promote() {
        let cache = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(2)
            .build()
            .unwrap();
        cache.set(1, 10);
        cache.set(2, 20);

        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &1)
                .await
                .unwrap(),
            Some(10)
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &1)
                .await
                .unwrap(),
            ConcurrentCachePeek::cache_peek(&cache, &1).unwrap()
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &99)
                .await
                .unwrap(),
            None,
            "missing key peeks as None"
        );

        // A capacity eviction must still pick key 1 as the LRU victim: repeatedly peeking
        // (sync or async) key 1 must not have promoted it over key 2.
        cache.set(3, 30);
        assert_eq!(cache.get(&1), None, "peek must not have promoted key 1");
        assert_eq!(cache.get(&2), Some(20));

        assert_eq!(ConcurrentCacheBase::cache_hits(&cache), Some(1));
        assert_eq!(ConcurrentCacheBase::cache_misses(&cache), Some(1));
    }

    #[cfg(feature = "time_stores")]
    #[tokio::test]
    async fn sharded_lru_ttl_async_peek_matches_sync_peek_and_skips_expired() {
        use cached::ShardedLruTtlCache;
        use cached::time::Duration;

        let cache = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_millis(30))
            .build()
            .unwrap();
        cache.set(1, 10);
        std::thread::sleep(std::time::Duration::from_millis(80));

        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &1)
                .await
                .unwrap(),
            None,
            "an expired entry peeks as None, not a stale value"
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &1)
                .await
                .unwrap(),
            ConcurrentCachePeek::cache_peek(&cache, &1).unwrap()
        );
        assert_eq!(
            cache.len(),
            1,
            "peeking (sync or async) must not lazily remove the expired entry"
        );
        assert_eq!(ConcurrentCacheBase::cache_hits(&cache), Some(0));
        assert_eq!(ConcurrentCacheBase::cache_misses(&cache), Some(0));
    }

    #[tokio::test]
    async fn sharded_expiring_lru_async_peek_matches_sync_peek_and_skips_expired() {
        let cache = ShardedExpiringLruCache::<u32, Token>::builder()
            .shards(1)
            .max_size(64)
            .build()
            .unwrap();
        cache.set(1, live(10));
        cache.set(2, expired(20));

        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &1)
                .await
                .unwrap(),
            Some(live(10))
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &2)
                .await
                .unwrap(),
            None,
            "an already-expired value peeks as None"
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&cache, &1)
                .await
                .unwrap(),
            ConcurrentCachePeek::cache_peek(&cache, &1).unwrap()
        );
        assert_eq!(
            cache.len(),
            2,
            "peeking (sync or async) must not lazily remove the expired entry"
        );
        assert_eq!(ConcurrentCacheBase::cache_hits(&cache), Some(0));
        assert_eq!(ConcurrentCacheBase::cache_misses(&cache), Some(0));
    }
}
