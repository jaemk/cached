/*!
Certification: the sharded LRU-family stores against their single-owner siblings as an oracle,
plus the paths the single-lookup rewrite left thin.

The sharded stores are documented as sharded versions of the single-owner ones, so with one
shard they must answer an identical op script identically: same per-op return values, same
`on_evict` victim sequence (which is exactly the recency chain, observed from outside), and the
same hit / miss / eviction / entry counts. Any recency change from the single-lookup `cache_get`
or the callback-dependent write path shows up as a diff against that oracle.

Also covered here, from the public API only:

- overwrite recency: `cache_set` over an existing key promotes it to MRU everywhere, with or
  without an `on_evict` callback, so the sharded stores and the oracle agree;
- `refresh_on_hit`, including the runtime toggle, the expired-entry case, the interaction with
  `unset_ttl`, and the read paths that must NOT refresh;
- `copy_from` metric provenance (a fresh cache must not inherit the source's counters);
- `cache_clear_with_on_evict`'s `drain_all`: every key and value delivered exactly once.

No Redis server required.
*/

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cached::{
    Cached, ConcurrentCacheBase, ConcurrentCached, Expires, ExpiringLruCache, LruCache,
    ShardedExpiringLruCache, ShardedLruCache,
};

// --- oracle harness ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Op<V> {
    Set(u32, V),
    Get(u32),
    Remove(u32),
}

/// Everything an op script can observe from outside: the per-op return values, the `on_evict`
/// victims in firing order, and the final counters.
#[derive(Debug, PartialEq)]
struct Trace<V> {
    returns: Vec<Option<V>>,
    victims: Vec<(u32, V)>,
    hits: Option<u64>,
    misses: Option<u64>,
    evictions: Option<u64>,
    size: usize,
}

/// A script that exercises inserts, capacity eviction, live/absent reads, and explicit
/// removes -- but never overwrites an existing key (overwrite recency is pinned separately by
/// `overwrite_promotion_agrees_across_the_sharded_stores_and_the_single_owner_oracle`).
fn script<V: Clone>(v: impl Fn(u32) -> V) -> Vec<Op<V>> {
    vec![
        Op::Set(1, v(10)),
        Op::Set(2, v(20)),
        Op::Set(3, v(30)),
        Op::Set(4, v(40)),
        Op::Get(1),
        Op::Get(999),
        Op::Get(2),
        Op::Set(5, v(50)), // capacity eviction: key 3 is least-recently-used
        Op::Get(3),
        Op::Remove(4),
        Op::Set(6, v(60)),
        Op::Set(7, v(70)),
        Op::Get(1),
        Op::Get(5),
        Op::Get(6),
        Op::Get(7),
        Op::Set(8, v(80)),
        Op::Remove(999),
        Op::Get(8),
    ]
}

// --- ShardedLruCache vs LruCache ----------------------------------------------------------

#[test]
fn sharded_lru_answers_the_op_script_exactly_like_the_single_owner_lru() {
    let ops = script(|n| n);

    let single = {
        let victims: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let victims2 = Arc::clone(&victims);
        let mut c = LruCache::<u32, u32>::builder()
            .max_size(4)
            .on_evict(move |k: &u32, v: &u32| victims2.lock().unwrap().push((*k, *v)))
            .build()
            .expect("build LruCache");
        let returns = ops
            .iter()
            .map(|op| match op {
                Op::Set(k, v) => c.cache_set(*k, *v),
                Op::Get(k) => c.cache_get(k).copied(),
                Op::Remove(k) => c.cache_remove(k),
            })
            .collect();
        Trace {
            returns,
            victims: victims.lock().unwrap().clone(),
            hits: c.cache_hits(),
            misses: c.cache_misses(),
            evictions: c.cache_evictions(),
            size: c.cache_size(),
        }
    };

    let sharded = {
        let victims: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let victims2 = Arc::clone(&victims);
        let c = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(4)
            .on_evict(move |k: &u32, v: &u32| victims2.lock().unwrap().push((*k, *v)))
            .build()
            .expect("build ShardedLruCache");
        let returns = ops
            .iter()
            .map(|op| match op {
                Op::Set(k, v) => ConcurrentCached::cache_set(&c, *k, *v).unwrap(),
                Op::Get(k) => ConcurrentCached::cache_get(&c, k).unwrap(),
                Op::Remove(k) => ConcurrentCached::cache_remove(&c, k).unwrap(),
            })
            .collect();
        let m = c.metrics();
        Trace {
            returns,
            victims: victims.lock().unwrap().clone(),
            hits: m.hits,
            misses: m.misses,
            evictions: m.evictions,
            size: m.entry_count.expect("sharded stores report an entry count"),
        }
    };

    assert_eq!(
        sharded, single,
        "a one-shard ShardedLruCache must be observationally identical to LruCache"
    );
    assert!(
        !single.victims.is_empty(),
        "the script must actually evict something for the comparison to be meaningful"
    );
}

// --- ShardedExpiringLruCache vs ExpiringLruCache ------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Val {
    v: u32,
    dead: bool,
}

impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.dead
    }
}

fn live(v: u32) -> Val {
    Val { v, dead: false }
}

fn dead(v: u32) -> Val {
    Val { v, dead: true }
}

#[test]
fn sharded_expiring_lru_answers_the_op_script_exactly_like_the_single_owner_store() {
    // Value-driven expiry needs no clock, so expired entries can be scripted deterministically:
    // keys 6 and 8 are inserted already expired.
    let mut ops = script(live);
    for op in &mut ops {
        if let Op::Set(k, v) = op
            && (*k == 6 || *k == 8)
        {
            *v = dead(v.v);
        }
    }

    let single = {
        let victims: Arc<Mutex<Vec<(u32, Val)>>> = Arc::new(Mutex::new(Vec::new()));
        let victims2 = Arc::clone(&victims);
        let mut c = ExpiringLruCache::<u32, Val>::builder()
            .max_size(4)
            .on_evict(move |k: &u32, v: &Val| victims2.lock().unwrap().push((*k, v.clone())))
            .build()
            .expect("build ExpiringLruCache");
        let returns = ops
            .iter()
            .map(|op| match op {
                Op::Set(k, v) => c.cache_set(*k, v.clone()),
                Op::Get(k) => c.cache_get(k).cloned(),
                Op::Remove(k) => c.cache_remove(k),
            })
            .collect();
        Trace {
            returns,
            victims: victims.lock().unwrap().clone(),
            hits: c.cache_hits(),
            misses: c.cache_misses(),
            evictions: c.cache_evictions(),
            size: c.cache_size(),
        }
    };

    let sharded = {
        let victims: Arc<Mutex<Vec<(u32, Val)>>> = Arc::new(Mutex::new(Vec::new()));
        let victims2 = Arc::clone(&victims);
        let c = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .per_shard_max_size(4)
            .on_evict(move |k: &u32, v: &Val| victims2.lock().unwrap().push((*k, v.clone())))
            .build()
            .expect("build ShardedExpiringLruCache");
        let returns = ops
            .iter()
            .map(|op| match op {
                Op::Set(k, v) => ConcurrentCached::cache_set(&c, *k, v.clone()).unwrap(),
                Op::Get(k) => ConcurrentCached::cache_get(&c, k).unwrap(),
                Op::Remove(k) => ConcurrentCached::cache_remove(&c, k).unwrap(),
            })
            .collect();
        let m = c.metrics();
        Trace {
            returns,
            victims: victims.lock().unwrap().clone(),
            hits: m.hits,
            misses: m.misses,
            evictions: m.evictions,
            size: m.entry_count.expect("sharded stores report an entry count"),
        }
    };

    assert_eq!(
        sharded, single,
        "a one-shard ShardedExpiringLruCache must be observationally identical to \
         ExpiringLruCache, including the expired-on-read removals"
    );
    assert!(
        single
            .victims
            .iter()
            .any(|(_, v): &(u32, Val)| v.is_expired()),
        "the script must actually hit the expired-on-read path"
    );
}

/// Overwrite recency, stated in one place: `cache_set` over an EXISTING key promotes that key
/// to most-recently-used, in the single-owner stores and in the sharded ones, and whether or
/// not an `on_evict` callback is configured. Configuring a purely observational callback must
/// not change which entry a capacity eviction picks.
///
/// The scenario is the smallest one that makes the promotion externally visible: capacity 2,
/// insert 1 and 2, overwrite 1, then insert 3. Under promote-on-set the victim is key 2; the
/// pre-3.0 in-place behavior would evict key 1 instead. Every variant is asserted against the
/// same oracle outcome, so the contract cannot be "fixed" in one store and left in the others.
#[test]
fn overwrite_promotion_agrees_across_the_sharded_stores_and_the_single_owner_oracle() {
    /// What survives the scenario, observed from outside: the value for each of keys 1..=3.
    type Outcome = [Option<u32>; 3];

    /// Promote-on-set: key 1 was rewritten (so promoted) and survives with its new value;
    /// key 2 became the least-recently-used entry and is the capacity victim.
    const EXPECTED: Outcome = [Some(11), None, Some(30)];

    // --- oracle: single-owner LruCache ---
    let oracle: Outcome = {
        let mut c = LruCache::<u32, u32>::builder()
            .max_size(2)
            .build()
            .expect("build LruCache");
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(1, 11);
        c.cache_set(3, 30);
        [
            c.cache_get(&1).copied(),
            c.cache_get(&2).copied(),
            c.cache_get(&3).copied(),
        ]
    };
    assert_eq!(
        oracle, EXPECTED,
        "single-owner: overwriting key 1 promotes it, so key 2 is the eviction victim"
    );

    // --- oracle: single-owner ExpiringLruCache ---
    let expiring_oracle: Outcome = {
        let mut c = ExpiringLruCache::<u32, Val>::builder()
            .max_size(2)
            .build()
            .expect("build ExpiringLruCache");
        c.cache_set(1, live(10));
        c.cache_set(2, live(20));
        c.cache_set(1, live(11));
        c.cache_set(3, live(30));
        [
            c.cache_get(&1).map(|v| v.v),
            c.cache_get(&2).map(|v| v.v),
            c.cache_get(&3).map(|v| v.v),
        ]
    };
    assert_eq!(
        expiring_oracle, oracle,
        "ExpiringLruCache must agree with the LruCache oracle on overwrite recency"
    );

    // --- ShardedLruCache, with and without a callback ---
    for with_on_evict in [false, true] {
        let builder = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(2);
        let c = if with_on_evict {
            builder.on_evict(|_, _| {}).build()
        } else {
            builder.build()
        }
        .expect("build ShardedLruCache");
        ConcurrentCached::cache_set(&c, 1, 10).unwrap();
        ConcurrentCached::cache_set(&c, 2, 20).unwrap();
        ConcurrentCached::cache_set(&c, 1, 11).unwrap();
        ConcurrentCached::cache_set(&c, 3, 30).unwrap();
        let got: Outcome = [
            ConcurrentCached::cache_get(&c, &1).unwrap(),
            ConcurrentCached::cache_get(&c, &2).unwrap(),
            ConcurrentCached::cache_get(&c, &3).unwrap(),
        ];
        assert_eq!(
            got, oracle,
            "ShardedLruCache (on_evict={with_on_evict}) must match the single-owner oracle"
        );
    }

    // --- ShardedExpiringLruCache, with and without a callback (the two write branches) ---
    for with_on_evict in [false, true] {
        let builder = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .per_shard_max_size(2);
        let c = if with_on_evict {
            builder.on_evict(|_, _| {}).build()
        } else {
            builder.build()
        }
        .expect("build ShardedExpiringLruCache");
        ConcurrentCached::cache_set(&c, 1, live(10)).unwrap();
        ConcurrentCached::cache_set(&c, 2, live(20)).unwrap();
        ConcurrentCached::cache_set(&c, 1, live(11)).unwrap();
        ConcurrentCached::cache_set(&c, 3, live(30)).unwrap();
        let got: Outcome = [
            ConcurrentCached::cache_get(&c, &1).unwrap().map(|v| v.v),
            ConcurrentCached::cache_get(&c, &2).unwrap().map(|v| v.v),
            ConcurrentCached::cache_get(&c, &3).unwrap().map(|v| v.v),
        ];
        assert_eq!(
            got, oracle,
            "ShardedExpiringLruCache (on_evict={with_on_evict}) must match the single-owner \
             oracle: attaching an observational callback must not change eviction order"
        );
    }

    // --- LruTtlCache / ShardedLruTtlCache, same scenario under a TTL long enough not to fire ---
    #[cfg(feature = "time_stores")]
    {
        use cached::{LruTtlCache, ShardedLruTtlCache};
        let ttl = std::time::Duration::from_secs(3600);

        let timed_oracle: Outcome = {
            let mut c = LruTtlCache::<u32, u32>::builder()
                .max_size(2)
                .ttl(ttl)
                .build()
                .expect("build LruTtlCache");
            c.cache_set(1, 10);
            c.cache_set(2, 20);
            c.cache_set(1, 11);
            c.cache_set(3, 30);
            [
                c.cache_get(&1).copied(),
                c.cache_get(&2).copied(),
                c.cache_get(&3).copied(),
            ]
        };
        assert_eq!(
            timed_oracle, oracle,
            "LruTtlCache must agree with the LruCache oracle on overwrite recency"
        );

        for with_on_evict in [false, true] {
            let builder = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .per_shard_max_size(2)
                .ttl(ttl);
            let c = if with_on_evict {
                builder.on_evict(|_, _| {}).build()
            } else {
                builder.build()
            }
            .expect("build ShardedLruTtlCache");
            ConcurrentCached::cache_set(&c, 1, 10).unwrap();
            ConcurrentCached::cache_set(&c, 2, 20).unwrap();
            ConcurrentCached::cache_set(&c, 1, 11).unwrap();
            ConcurrentCached::cache_set(&c, 3, 30).unwrap();
            let got: Outcome = [
                ConcurrentCached::cache_get(&c, &1).unwrap(),
                ConcurrentCached::cache_get(&c, &2).unwrap(),
                ConcurrentCached::cache_get(&c, &3).unwrap(),
            ];
            assert_eq!(
                got, oracle,
                "ShardedLruTtlCache (on_evict={with_on_evict}) must match the single-owner \
                 oracle: attaching an observational callback must not change eviction order"
            );
        }
    }
}

// --- cache_clear_with_on_evict / drain_all ------------------------------------------------

#[test]
fn clear_with_on_evict_delivers_every_key_and_value_exactly_once_across_shards() {
    let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let store = ShardedLruCache::<u32, u32>::builder()
        // Per-shard capacity far above any plausible skew for 500 keys over 16 shards, so no
        // capacity eviction can perturb the exactly-once count.
        .shards(16)
        .per_shard_max_size(512)
        .on_evict(move |k: &u32, v: &u32| seen2.lock().unwrap().push((*k, *v)))
        .build()
        .expect("build ShardedLruCache");
    for i in 0..500u32 {
        ConcurrentCached::cache_set(&store, i, i * 7).unwrap();
    }
    // Read some entries back so the recency chains are not plain insertion order.
    for i in (0..500u32).step_by(3) {
        assert_eq!(
            ConcurrentCached::cache_get(&store, &i).unwrap(),
            Some(i * 7)
        );
    }
    let before = store.metrics().evictions.expect("evictions tracked");

    store.cache_clear_with_on_evict();

    let mut fired = seen.lock().unwrap().clone();
    assert_eq!(
        fired.len(),
        500,
        "one callback per entry, no more and no less"
    );
    fired.sort_unstable();
    let expected: Vec<(u32, u32)> = (0..500u32).map(|i| (i, i * 7)).collect();
    assert_eq!(
        fired, expected,
        "every stored key must be delivered exactly once, paired with its own value"
    );
    assert_eq!(
        store.metrics().evictions.expect("evictions tracked") - before,
        500
    );
    assert!(store.is_empty());
    assert_eq!(store.shard_sizes().iter().sum::<usize>(), 0);
}

/// `deep_clone` on the plain sharded LRU store: its eviction total lives entirely in the
/// per-shard inner `LruCache` counters (it has no separate non-capacity family), so the clone
/// must report the same total and then diverge independently. The sibling stores' per-shard
/// counters are covered by their own tests; this is the one that has neither.
#[test]
fn sharded_lru_deep_clone_carries_the_eviction_total_and_stays_independent() {
    let store = ShardedLruCache::<u32, u32>::builder()
        .shards(4)
        .per_shard_max_size(4)
        .build()
        .expect("build ShardedLruCache");
    for i in 0..100u32 {
        ConcurrentCached::cache_set(&store, i, i).unwrap();
    }
    assert_eq!(
        ConcurrentCached::cache_remove(&store, &99).unwrap(),
        Some(99)
    );
    let total = store.metrics().evictions.expect("evictions tracked");
    assert!(total > 0, "the fixture must produce evictions");
    let hits_before = store.metrics().hits;

    let cloned = store.deep_clone();
    assert_eq!(
        cloned.metrics().evictions,
        Some(total),
        "the clone must report the source's eviction total"
    );
    assert_eq!(
        ConcurrentCacheBase::cache_evictions(&cloned),
        Some(total),
        "the trait method must agree with metrics() on the clone"
    );
    assert_eq!(cloned.metrics().entry_count, store.metrics().entry_count);
    assert_eq!(cloned.metrics().hits, hits_before);

    // Independent from here on. `clear()` empties the clone without counting anything, so the
    // insert below cannot trigger a capacity eviction and the delta is exactly the one remove.
    cloned.clear();
    assert_eq!(
        cloned.metrics().evictions,
        Some(total),
        "plain clear() must not count evictions"
    );
    ConcurrentCached::cache_set(&cloned, 1000, 1000).unwrap();
    assert_eq!(
        ConcurrentCached::cache_remove(&cloned, &1000).unwrap(),
        Some(1000)
    );
    assert_eq!(cloned.metrics().evictions, Some(total + 1));
    assert_eq!(
        store.metrics().evictions,
        Some(total),
        "the source must be untouched by the clone"
    );
}

#[test]
fn clear_with_on_evict_on_an_empty_cache_fires_nothing_and_counts_nothing() {
    let fires = Arc::new(AtomicU64::new(0));
    let fires2 = Arc::clone(&fires);
    let store = ShardedLruCache::<u32, u32>::builder()
        .shards(4)
        .per_shard_max_size(16)
        .on_evict(move |_k: &u32, _v: &u32| {
            fires2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .expect("build ShardedLruCache");

    store.cache_clear_with_on_evict();
    assert_eq!(fires.load(Ordering::Relaxed), 0);
    assert_eq!(store.metrics().evictions, Some(0));

    // Draining a cache that only has entries in *some* shards must not count the empty ones.
    ConcurrentCached::cache_set(&store, 1, 10).unwrap();
    store.cache_clear_with_on_evict();
    assert_eq!(fires.load(Ordering::Relaxed), 1);
    assert_eq!(store.metrics().evictions, Some(1));

    // ... and a second drain of the now-empty cache adds nothing.
    store.cache_clear_with_on_evict();
    assert_eq!(fires.load(Ordering::Relaxed), 1);
    assert_eq!(store.metrics().evictions, Some(1));
}

/// `drain_all` takes the whole chain, expired entries included: an expired entry is still a
/// stored entry, so it is handed to `on_evict` and counted like any other.
#[test]
fn clear_with_on_evict_drains_expired_entries_too() {
    let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let store = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(1)
        .per_shard_max_size(16)
        .on_evict(move |k: &u32, _v: &Val| seen2.lock().unwrap().push(*k))
        .build()
        .expect("build ShardedExpiringLruCache");
    ConcurrentCached::cache_set(&store, 1, live(10)).unwrap();
    ConcurrentCached::cache_set(&store, 2, dead(20)).unwrap();
    ConcurrentCached::cache_set(&store, 3, live(30)).unwrap();

    store.cache_clear_with_on_evict();

    assert_eq!(
        *seen.lock().unwrap(),
        vec![3, 2, 1],
        "expired entries are drained in chain order alongside live ones"
    );
    assert_eq!(store.metrics().evictions, Some(3));
    assert!(store.is_empty());
}

// --- copy_from metric provenance ----------------------------------------------------------

#[test]
fn sharded_lru_copy_from_starts_from_clean_counters() {
    let source_fires = Arc::new(AtomicU64::new(0));
    let source_fires2 = Arc::clone(&source_fires);
    let source = ShardedLruCache::<u32, u32>::builder()
        .shards(1)
        .per_shard_max_size(8)
        .on_evict(move |_k: &u32, _v: &u32| {
            source_fires2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .expect("build ShardedLruCache");
    for i in 0..40u32 {
        ConcurrentCached::cache_set(&source, i, i).unwrap();
    }
    for i in 32..40u32 {
        assert_eq!(ConcurrentCached::cache_get(&source, &i).unwrap(), Some(i));
    }
    let source_evictions = source.metrics().evictions.expect("evictions tracked");
    assert!(source_evictions >= 32, "the source must carry a history");
    let source_fires_before = source_fires.load(Ordering::Relaxed);

    // Same capacity: nothing is dropped, so the new cache starts at zero on every counter.
    let same = ShardedLruCache::<u32, u32>::builder()
        .shards(1)
        .per_shard_max_size(8)
        .copy_from(&source)
        .expect("copy_from must succeed");
    let m = same.metrics();
    assert_eq!(
        m.evictions,
        Some(0),
        "a copy must not inherit the source's eviction history"
    );
    assert_eq!(m.hits, Some(0));
    assert_eq!(m.misses, Some(0));
    assert_eq!(m.entry_count, Some(8));

    // Smaller target: exactly the entries that did not fit are evicted, counted, and reported
    // to the NEW cache's callback -- never to the source's.
    let dropped: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let dropped2 = Arc::clone(&dropped);
    let small = ShardedLruCache::<u32, u32>::builder()
        .shards(1)
        .per_shard_max_size(3)
        .on_evict(move |k: &u32, _v: &u32| dropped2.lock().unwrap().push(*k))
        .copy_from(&source)
        .expect("copy_from must succeed");
    assert_eq!(
        small.metrics().entry_count,
        Some(3),
        "the copy is bounded by its own capacity"
    );
    assert_eq!(
        small.metrics().evictions,
        Some(5),
        "the 5 entries that did not fit must each count exactly one eviction"
    );
    assert_eq!(
        dropped.lock().unwrap().len(),
        5,
        "the new cache's callback receives the entries that did not fit"
    );
    assert_eq!(
        source_fires.load(Ordering::Relaxed),
        source_fires_before,
        "copy_from reads the source, so the source's callback must stay silent"
    );
    assert_eq!(
        source.metrics().evictions,
        Some(source_evictions),
        "the source's counters must be untouched by a copy"
    );

    // Copying preserves recency: the survivors are the most-recently-used entries.
    for i in 37..40u32 {
        assert!(
            ConcurrentCached::cache_contains(&small, &i).unwrap(),
            "the most-recently-used entries must survive the shrinking copy"
        );
    }
}

#[test]
fn sharded_expiring_lru_copy_from_starts_from_clean_counters() {
    let source = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(1)
        .per_shard_max_size(8)
        .build()
        .expect("build ShardedExpiringLruCache");
    for i in 0..40u32 {
        ConcurrentCached::cache_set(&source, i, live(i)).unwrap();
    }
    let source_evictions = source.metrics().evictions.expect("evictions tracked");
    assert!(source_evictions >= 32);

    let copy = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(1)
        .per_shard_max_size(8)
        .copy_from(&source)
        .expect("copy_from must succeed");
    assert_eq!(copy.metrics().evictions, Some(0));
    assert_eq!(copy.metrics().hits, Some(0));
    assert_eq!(copy.metrics().misses, Some(0));

    // An expired entry in the source is skipped by the copy, and skipping is not an eviction.
    ConcurrentCached::cache_set(&source, 100, dead(100)).unwrap();
    let copy2 = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(1)
        .per_shard_max_size(64)
        .copy_from(&source)
        .expect("copy_from must succeed");
    assert!(!ConcurrentCached::cache_contains(&copy2, &100).unwrap());
    assert_eq!(
        copy2.metrics().evictions,
        Some(0),
        "skipping an expired source entry is not an eviction on the copy"
    );
}

// --- refresh_on_hit -----------------------------------------------------------------------

#[cfg(feature = "time_stores")]
mod refresh_on_hit {
    use super::*;
    use cached::{
        CacheTtl, ConcurrentCachePeek, ConcurrentCacheRefreshOnHit, ConcurrentCacheTtl,
        LruTtlCache, ShardedLruTtlCache,
    };
    use std::time::Duration;

    /// One-shard parity with `LruTtlCache` over the same script, with expiry kept out of the
    /// picture (long TTL) so the comparison is purely about recency and counters.
    #[test]
    fn sharded_lru_ttl_answers_the_op_script_exactly_like_the_single_owner_store() {
        let ops = script(|n| n);

        let single = {
            let victims: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
            let victims2 = Arc::clone(&victims);
            let mut c = LruTtlCache::<u32, u32>::builder()
                .max_size(4)
                .ttl(Duration::from_secs(3600))
                .on_evict(move |k: &u32, v: &u32| victims2.lock().unwrap().push((*k, *v)))
                .build()
                .expect("build LruTtlCache");
            let returns = ops
                .iter()
                .map(|op| match op {
                    Op::Set(k, v) => c.cache_set(*k, *v),
                    Op::Get(k) => c.cache_get(k).copied(),
                    Op::Remove(k) => c.cache_remove(k),
                })
                .collect();
            Trace {
                returns,
                victims: victims.lock().unwrap().clone(),
                hits: c.cache_hits(),
                misses: c.cache_misses(),
                evictions: c.cache_evictions(),
                size: c.cache_size(),
            }
        };

        let sharded = {
            let victims: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
            let victims2 = Arc::clone(&victims);
            let c = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .per_shard_max_size(4)
                .ttl(Duration::from_secs(3600))
                .on_evict(move |k: &u32, v: &u32| victims2.lock().unwrap().push((*k, *v)))
                .build()
                .expect("build ShardedLruTtlCache");
            let returns = ops
                .iter()
                .map(|op| match op {
                    Op::Set(k, v) => ConcurrentCached::cache_set(&c, *k, *v).unwrap(),
                    Op::Get(k) => ConcurrentCached::cache_get(&c, k).unwrap(),
                    Op::Remove(k) => ConcurrentCached::cache_remove(&c, k).unwrap(),
                })
                .collect();
            let m = c.metrics();
            Trace {
                returns,
                victims: victims.lock().unwrap().clone(),
                hits: m.hits,
                misses: m.misses,
                evictions: m.evictions,
                size: m.entry_count.expect("sharded stores report an entry count"),
            }
        };

        assert_eq!(
            sharded, single,
            "a one-shard ShardedLruTtlCache must be observationally identical to LruTtlCache"
        );
    }

    /// Lazy TTL expiry must agree with the single-owner store too: same return, same eviction
    /// accounting, same callback.
    #[test]
    fn lazy_expiry_matches_the_single_owner_store() {
        let single_fires = Arc::new(AtomicU64::new(0));
        let single_fires2 = Arc::clone(&single_fires);
        let mut single = LruTtlCache::<u32, u32>::builder()
            .max_size(4)
            .ttl(Duration::from_millis(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                single_fires2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build LruTtlCache");
        let sharded_fires = Arc::new(AtomicU64::new(0));
        let sharded_fires2 = Arc::clone(&sharded_fires);
        let sharded = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(4)
            .ttl(Duration::from_millis(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                sharded_fires2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build ShardedLruTtlCache");

        single.cache_set(1, 10);
        ConcurrentCached::cache_set(&sharded, 1, 10).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        assert_eq!(single.cache_get(&1).copied(), None);
        assert_eq!(ConcurrentCached::cache_get(&sharded, &1).unwrap(), None);
        assert_eq!(single.cache_size(), 0, "the expired entry is removed");
        assert_eq!(
            ConcurrentCacheBase::cache_size(&sharded).unwrap(),
            Some(0),
            "the sharded store must remove it too"
        );
        assert_eq!(single_fires.load(Ordering::Relaxed), 1);
        assert_eq!(sharded_fires.load(Ordering::Relaxed), 1);
        assert_eq!(single.cache_evictions(), Some(1));
        assert_eq!(sharded.metrics().evictions, Some(1));
        assert_eq!(single.cache_misses(), Some(1));
        assert_eq!(ConcurrentCacheBase::cache_misses(&sharded), Some(1));
    }

    /// `set_refresh_on_hit` flips the behaviour at runtime, in both directions, and the getter
    /// reports the previous value.
    #[test]
    fn runtime_toggle_switches_the_read_path_both_ways() {
        // A comfortably large TTL (relative to the 150 ms poll interval below) so that only the
        // deliberate past-expiry sleeps decide expiry, never scheduling jitter in the CI runner.
        let ttl = Duration::from_millis(1000);
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(ttl)
            .build()
            .expect("build ShardedLruTtlCache");
        assert!(!store.refresh_on_hit(), "the builder default is off");

        // Off: repeated reads do not push the expiry out. Poll a few times inside the TTL
        // window (their results are irrelevant here), then deliberately sleep well past the
        // TTL before checking that the original expiry stood.
        ConcurrentCached::cache_set(&store, 1, 10).unwrap();
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(150));
            let _ = ConcurrentCached::cache_get(&store, &1).unwrap();
        }
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(
            ConcurrentCached::cache_get(&store, &1).unwrap(),
            None,
            "without refresh_on_hit the original expiry stands"
        );

        assert!(
            !store.set_refresh_on_hit(true),
            "the setter returns the previous value"
        );
        assert!(store.refresh_on_hit());

        // On: the same read pattern keeps the entry alive well past one TTL. Each hit renews
        // the deadline, so the gap the scheduler must blow through to falsify this is the full
        // 1000 ms TTL minus the 150 ms poll interval, not a shrinking cumulative margin.
        ConcurrentCached::cache_set(&store, 2, 20).unwrap();
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(
                ConcurrentCached::cache_get(&store, &2).unwrap(),
                Some(20),
                "each hit must renew the expiry"
            );
        }

        // And back off again: the entry now expires on the last refreshed deadline.
        assert!(store.set_refresh_on_hit(false));
        std::thread::sleep(Duration::from_millis(1300));
        assert_eq!(ConcurrentCached::cache_get(&store, &2).unwrap(), None);
    }

    /// A refreshing read must not resurrect an already-expired entry: the liveness predicate
    /// fails, so the entry is removed and counted rather than given a new lease.
    #[test]
    fn a_refreshing_read_never_resurrects_an_expired_entry() {
        let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(Duration::from_millis(60))
            .refresh_on_hit(true)
            .on_evict(move |k: &u32, v: &u32| seen2.lock().unwrap().push((*k, *v)))
            .build()
            .expect("build ShardedLruTtlCache");
        ConcurrentCached::cache_set(&store, 1, 10).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        assert_eq!(ConcurrentCached::cache_get(&store, &1).unwrap(), None);
        assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(0));
        assert_eq!(*seen.lock().unwrap(), vec![(1, 10)]);
        assert_eq!(store.metrics().evictions, Some(1));
        // Still gone on the next read, i.e. nothing was re-armed.
        assert_eq!(ConcurrentCached::cache_get(&store, &1).unwrap(), None);
        assert_eq!(store.metrics().evictions, Some(1));
    }

    /// The side-effect-free reads must stay side-effect-free under `refresh_on_hit`:
    /// `cache_peek` and `cache_contains` neither renew the expiry nor promote recency.
    #[test]
    fn peek_and_contains_do_not_refresh_the_expiry() {
        // A comfortably large TTL (relative to the 100 ms poll interval below) so that only the
        // deliberate past-expiry sleep at the end decides expiry, never scheduling jitter in the
        // CI runner: only the intentional final sleep should ever flip this to expired.
        let ttl = Duration::from_millis(1000);
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(ttl)
            .refresh_on_hit(true)
            .build()
            .expect("build ShardedLruTtlCache");
        ConcurrentCached::cache_set(&store, 1, 10).unwrap();
        // Three peeks well inside the entry's single 1000 ms lifetime; if any of them refreshed
        // the expiry the final read below would still be a hit.
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(store.peek(&1), Some(10), "peek still reads the live value");
            assert!(ConcurrentCached::cache_contains(&store, &1).unwrap());
            assert_eq!(
                ConcurrentCachePeek::cache_peek(&store, &1).unwrap(),
                Some(10)
            );
        }
        // Land the final read strictly inside the window (original 1000 ms deadline,
        // refreshed 1300 ms deadline): the last peek was at t~=300 ms, so sleeping 850 ms
        // more reads at t~=1150 ms. A correctly non-refreshing entry (deadline 1000 ms) is
        // expired; an entry that a buggy peek had refreshed (deadline 1300 ms) would still
        // be live, flipping the assertion below. 700 <= 850 < 1000 keeps this true under
        // scheduling jitter without false failures.
        std::thread::sleep(Duration::from_millis(850));
        assert_eq!(
            ConcurrentCached::cache_get(&store, &1).unwrap(),
            None,
            "peeking must not have renewed the entry's expiry"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_hits(&store),
            Some(0),
            "peek and contains record no hits"
        );
    }

    /// With expiry disabled at runtime (`unset_ttl`), a refreshing hit has no new deadline to
    /// install, so it keeps the entry's original one rather than clearing it: the entry still
    /// expires on schedule. The single-owner store is the oracle for this corner.
    #[test]
    fn refresh_after_unset_ttl_keeps_the_original_expiry() {
        let sharded = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(Duration::from_millis(150))
            .refresh_on_hit(true)
            .build()
            .expect("build ShardedLruTtlCache");
        let mut single = LruTtlCache::<u32, u32>::builder()
            .max_size(8)
            .ttl(Duration::from_millis(150))
            .refresh_on_hit(true)
            .build()
            .expect("build LruTtlCache");

        ConcurrentCached::cache_set(&sharded, 1, 10).unwrap();
        single.cache_set(1, 10);
        assert_eq!(
            sharded.unset_ttl(),
            Some(Duration::from_millis(150)),
            "unset_ttl returns the previous ttl"
        );
        assert_eq!(single.unset_ttl(), Some(Duration::from_millis(150)));

        // A refreshing hit while expiry is disabled leaves the stored deadline alone ...
        assert_eq!(ConcurrentCached::cache_get(&sharded, &1).unwrap(), Some(10));
        assert_eq!(single.cache_get(&1).copied(), Some(10));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            ConcurrentCached::cache_get(&sharded, &1).unwrap(),
            None,
            "the entry keeps the expiry it was written with"
        );
        assert_eq!(
            single.cache_get(&1).copied(),
            None,
            "the single-owner store agrees"
        );

        // ... while an entry written *after* expiry was disabled never expires.
        ConcurrentCached::cache_set(&sharded, 2, 20).unwrap();
        single.cache_set(2, 20);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(ConcurrentCached::cache_get(&sharded, &2).unwrap(), Some(20));
        assert_eq!(single.cache_get(&2).copied(), Some(20));
    }

    /// A refreshing hit must promote recency exactly like a non-refreshing one -- the two use
    /// different probes (`get_mut_if` vs `get_if`), so the promotion is asserted for both.
    #[test]
    fn a_refreshing_read_promotes_like_a_plain_read() {
        for refresh in [false, true] {
            let victims: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
            let victims2 = Arc::clone(&victims);
            let store = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .per_shard_max_size(3)
                .ttl(Duration::from_secs(3600))
                .refresh_on_hit(refresh)
                .on_evict(move |k: &u32, _v: &u32| victims2.lock().unwrap().push(*k))
                .build()
                .expect("build ShardedLruTtlCache");
            for i in 1..=3u32 {
                ConcurrentCached::cache_set(&store, i, i * 10).unwrap();
            }
            // Read in an order that fully reverses the chain.
            for i in 1..=3u32 {
                assert_eq!(
                    ConcurrentCached::cache_get(&store, &i).unwrap(),
                    Some(i * 10)
                );
            }
            // Now the chain is 3, 2, 1 (MRU -> LRU): the next three inserts evict 1, 2, 3.
            for i in 10..13u32 {
                ConcurrentCached::cache_set(&store, i, i).unwrap();
            }
            assert_eq!(
                *victims.lock().unwrap(),
                vec![1, 2, 3],
                "refresh_on_hit={refresh}: reads must promote in the order they happened"
            );
        }
    }
}
