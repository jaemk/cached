/*!
Certification: per-shard eviction counters and lazy-expiry removal under real concurrency.

Eviction counting on the expiry-aware sharded LRU stores is split across two families of
counters (`Shard::evictions` for expiry/removes/retain/clear, the inner `LruCache`'s counter
for capacity pressure) and summed on read. A split like that is exactly where a race loses or
double-counts an eviction, and where a lazily-expired entry can be removed twice.

The invariants pinned here hold for *any* interleaving, so they are checked with many threads
hammering the same shard and the same key:

- N concurrent readers of one expired key: exactly one removal, one `on_evict`, one eviction
  counted, and N misses;
- concurrent removes across shards: `metrics().evictions` equals the number of removes that
  actually returned an entry, and `on_evict` fires exactly once per removed key;
- concurrent inserts past capacity: `inserts == survivors + evictions` exactly, over all shards.

No Redis server required.
*/

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use cached::{
    ConcurrentCacheBase, ConcurrentCached, Expires, ShardedExpiringLruCache, ShardedLruCache,
};

const THREADS: usize = 16;

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

// --- one expired key, many readers -------------------------------------------------------

/// Every reader must observe the miss, but only the one that wins the shard lock may remove
/// the entry, fire `on_evict`, and count the eviction.
#[test]
fn expiring_lru_concurrent_reads_of_one_expired_key_evict_exactly_once() {
    // Repeated: the interesting interleavings (a second reader arriving between the first
    // reader's liveness decision and its removal) are rare, so one round is not enough.
    for _round in 0..80 {
        let evicted: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let evicted2 = Arc::clone(&evicted);
        let store = ShardedExpiringLruCache::<u32, Val>::builder()
            .shards(1)
            .per_shard_max_size(8)
            .on_evict(move |k: &u32, v: &Val| evicted2.lock().unwrap().push((*k, v.v)))
            .build()
            .expect("build ShardedExpiringLruCache");
        ConcurrentCached::cache_set(&store, 7, dead(70)).unwrap();
        let before = store.metrics().evictions.expect("evictions tracked");

        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                ConcurrentCached::cache_get(&store, &7).unwrap()
            }));
        }
        let results: Vec<Option<Val>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        assert!(
            results.iter().all(Option::is_none),
            "an expired entry must never be served"
        );
        assert_eq!(
            *evicted.lock().unwrap(),
            vec![(7, 70)],
            "on_evict must fire exactly once for the single removal"
        );
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked") - before,
            1,
            "exactly one eviction may be counted no matter how many readers raced"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_misses(&store),
            Some(THREADS as u64),
            "every racing read counts its own miss"
        );
        assert_eq!(ConcurrentCacheBase::cache_hits(&store), Some(0));
        assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(0));
    }
}

/// The same race with a live key: no removals, and every reader counts a hit.
#[test]
fn expiring_lru_concurrent_reads_of_one_live_key_count_every_hit() {
    let store = ShardedExpiringLruCache::<u32, Val>::builder()
        .shards(1)
        .per_shard_max_size(8)
        .on_evict(|_, _| panic!("a live read must never evict"))
        .build()
        .expect("build ShardedExpiringLruCache");
    ConcurrentCached::cache_set(&store, 7, live(70)).unwrap();

    let reads_per_thread = 200u64;
    let barrier = Arc::new(Barrier::new(THREADS));
    let mut handles = Vec::new();
    for _ in 0..THREADS {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..reads_per_thread {
                assert_eq!(
                    ConcurrentCached::cache_get(&store, &7).unwrap(),
                    Some(live(70))
                );
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        ConcurrentCacheBase::cache_hits(&store),
        Some(THREADS as u64 * reads_per_thread),
        "the per-shard hit counter must not lose an increment under contention"
    );
    assert_eq!(ConcurrentCacheBase::cache_misses(&store), Some(0));
    assert_eq!(store.metrics().evictions, Some(0));
}

#[cfg(feature = "time_stores")]
mod lru_ttl {
    use super::*;
    use cached::ShardedLruTtlCache;
    use std::time::Duration;

    /// TTL-expiry flavour of the same race. Every thread samples the clock after the TTL has
    /// already elapsed, so all of them must miss, and exactly one may evict.
    #[test]
    fn concurrent_reads_of_one_expired_key_evict_exactly_once() {
        for _round in 0..40 {
            let evicted: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
            let evicted2 = Arc::clone(&evicted);
            let store = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(Duration::from_millis(5))
                .on_evict(move |k: &u32, v: &u32| evicted2.lock().unwrap().push((*k, *v)))
                .build()
                .expect("build ShardedLruTtlCache");
            ConcurrentCached::cache_set(&store, 7, 70).unwrap();
            let before = store.metrics().evictions.expect("evictions tracked");
            // Every thread samples the clock only after this sleep, i.e. after the TTL.
            std::thread::sleep(Duration::from_millis(25));

            let barrier = Arc::new(Barrier::new(THREADS));
            let mut handles = Vec::new();
            for _ in 0..THREADS {
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    ConcurrentCached::cache_get(&store, &7).unwrap()
                }));
            }
            let results: Vec<Option<u32>> =
                handles.into_iter().map(|h| h.join().unwrap()).collect();

            assert!(
                results.iter().all(Option::is_none),
                "every reader sampled the clock after the TTL elapsed, so none may be served"
            );
            assert_eq!(*evicted.lock().unwrap(), vec![(7, 70)]);
            assert_eq!(
                store.metrics().evictions.expect("evictions tracked") - before,
                1,
                "exactly one eviction may be counted no matter how many readers raced"
            );
            assert_eq!(
                ConcurrentCacheBase::cache_misses(&store),
                Some(THREADS as u64)
            );
            assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(0));
        }
    }

    /// A racing `cache_get` (lazy expiry, counted on the shard's non-capacity counter) and
    /// `cache_set` (displaced-expired, counted on the same counter) over one expired key must
    /// between them produce exactly one eviction and one `on_evict` -- never two, never none.
    #[test]
    fn concurrent_expired_read_and_overwrite_evict_exactly_once() {
        for _round in 0..40 {
            let fires = Arc::new(AtomicU64::new(0));
            let fires2 = Arc::clone(&fires);
            let store = ShardedLruTtlCache::<u32, u32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(Duration::from_millis(10))
                .on_evict(move |_k: &u32, _v: &u32| {
                    fires2.fetch_add(1, Ordering::Relaxed);
                })
                .build()
                .expect("build ShardedLruTtlCache");
            ConcurrentCached::cache_set(&store, 1, 10).unwrap();
            let before = store.metrics().evictions.expect("evictions tracked");
            std::thread::sleep(Duration::from_millis(30));

            let barrier = Arc::new(Barrier::new(2));
            let reader = std::thread::spawn({
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                move || {
                    barrier.wait();
                    ConcurrentCached::cache_get(&store, &1).unwrap()
                }
            });
            let writer = std::thread::spawn({
                let store = store.clone();
                let barrier = Arc::clone(&barrier);
                move || {
                    barrier.wait();
                    ConcurrentCached::cache_set(&store, 1, 11).unwrap()
                }
            });
            // Whichever thread won the shard lock, the reader never sees the stale 10: it
            // either misses (it went first and evicted) or reads the freshly written 11.
            let read = reader.join().unwrap();
            assert!(
                read.is_none() || read == Some(11),
                "a reader must never be served the expired value; got {read:?}"
            );
            assert_eq!(
                writer.join().unwrap(),
                None,
                "an expired displaced value is filtered from the return"
            );

            assert_eq!(
                store.metrics().evictions.expect("evictions tracked") - before,
                1,
                "the expired entry must be accounted for exactly once across both racers"
            );
            assert_eq!(
                fires.load(Ordering::Relaxed),
                1,
                "on_evict must fire exactly once for the one expired entry"
            );
            // The write always wins the key back, whichever order the two ran in.
            assert_eq!(
                ConcurrentCacheBase::cache_size(&store).unwrap(),
                Some(1),
                "the written entry is stored regardless of the interleaving"
            );
        }
    }

    /// Removes spread over every shard: the summed per-shard counters must equal the number of
    /// removes that actually removed something, and every removed key must be reported once.
    #[test]
    fn concurrent_removes_across_shards_count_every_eviction_exactly_once() {
        let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(8)
            .per_shard_max_size(4096)
            .ttl(Duration::from_secs(3600))
            .on_evict(move |k: &u32, _v: &u32| seen2.lock().unwrap().push(*k))
            .build()
            .expect("build ShardedLruTtlCache");

        let per_thread = 500u32;
        for t in 0..THREADS as u32 {
            for i in 0..per_thread {
                ConcurrentCached::cache_set(&store, t * per_thread + i, i).unwrap();
            }
        }
        let total = THREADS as u32 * per_thread;
        assert_eq!(
            ConcurrentCacheBase::cache_size(&store).unwrap(),
            Some(total as usize),
            "no capacity eviction may happen in this fixture"
        );
        let before = store.metrics().evictions.expect("evictions tracked");

        // Every thread tries to remove EVERY key, so the same key is contended by all of them;
        // only one remove per key may report an entry.
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let mut removed = 0u64;
                for k in 0..total {
                    if ConcurrentCached::cache_remove(&store, &k)
                        .unwrap()
                        .is_some()
                    {
                        removed += 1;
                    }
                }
                removed
            }));
        }
        let removed: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();

        assert_eq!(
            removed, total as u64,
            "each key may be removed by exactly one thread"
        );
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked") - before,
            removed,
            "the summed per-shard counters must match the removals exactly"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_evictions(&store),
            store.metrics().evictions,
            "the trait method and metrics() must agree after concurrent mutation"
        );
        let mut fired = seen.lock().unwrap().clone();
        fired.sort_unstable();
        let expected: Vec<u32> = (0..total).collect();
        assert_eq!(fired, expected, "every key must fire on_evict exactly once");
        assert!(store.is_empty());
    }

    /// Concurrent inserts past capacity, all landing on one shard: every insert either survives
    /// or is counted as exactly one eviction. This is the accounting identity the per-shard
    /// counters must preserve under contention.
    #[test]
    fn concurrent_capacity_evictions_balance_against_survivors() {
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(32)
            .ttl(Duration::from_secs(3600))
            .build()
            .expect("build ShardedLruTtlCache");

        let per_thread = 500u32;
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();
        for t in 0..THREADS as u32 {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0..per_thread {
                    // Distinct keys per thread: no overwrites, so every insert adds an entry.
                    ConcurrentCached::cache_set(&store, t * per_thread + i, i).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let inserts = u64::from(per_thread) * THREADS as u64;
        let survivors = ConcurrentCacheBase::cache_size(&store).unwrap().unwrap() as u64;
        assert_eq!(survivors, 32, "the shard must be full at its cap");
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked"),
            inserts - survivors,
            "every insert not still stored must have been evicted exactly once"
        );
    }
}

/// The plain sharded LRU store keeps a single eviction family (the inner `LruCache` counter);
/// the same accounting identity must hold for it under concurrency, across many shards.
#[test]
fn sharded_lru_concurrent_capacity_evictions_balance_against_survivors() {
    let fires = Arc::new(AtomicU64::new(0));
    let fires2 = Arc::clone(&fires);
    let store = ShardedLruCache::<u32, u32>::builder()
        .shards(8)
        .per_shard_max_size(16)
        .on_evict(move |_k: &u32, _v: &u32| {
            fires2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .expect("build ShardedLruCache");

    let per_thread = 500u32;
    let barrier = Arc::new(Barrier::new(THREADS));
    let mut handles = Vec::new();
    for t in 0..THREADS as u32 {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for i in 0..per_thread {
                ConcurrentCached::cache_set(&store, t * per_thread + i, i).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let inserts = u64::from(per_thread) * THREADS as u64;
    let survivors = ConcurrentCacheBase::cache_size(&store).unwrap().unwrap() as u64;
    let evictions = store.metrics().evictions.expect("evictions tracked");
    assert_eq!(
        evictions,
        inserts - survivors,
        "every insert not still stored must have been evicted exactly once"
    );
    assert_eq!(
        fires.load(Ordering::Relaxed),
        evictions,
        "on_evict must fire exactly as often as an eviction is counted"
    );
}

/// `cache_clear_with_on_evict` racing concurrent inserts: the callback must fire exactly once
/// per entry it actually drained, and the eviction counter must move by the same amount. Any
/// entry inserted after its shard was drained simply survives.
#[test]
fn sharded_lru_clear_with_on_evict_races_inserts_without_double_firing() {
    for _round in 0..20 {
        let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let store = ShardedLruCache::<u32, u32>::builder()
            .shards(8)
            .per_shard_max_size(4096)
            .on_evict(move |k: &u32, _v: &u32| seen2.lock().unwrap().push(*k))
            .build()
            .expect("build ShardedLruCache");
        for i in 0..400u32 {
            ConcurrentCached::cache_set(&store, i, i).unwrap();
        }

        let go = Arc::new(AtomicBool::new(false));
        let inserter = std::thread::spawn({
            let store = store.clone();
            let go = Arc::clone(&go);
            move || {
                while !go.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                for i in 1000..1400u32 {
                    ConcurrentCached::cache_set(&store, i, i).unwrap();
                }
            }
        });
        go.store(true, Ordering::Release);
        store.cache_clear_with_on_evict();
        inserter.join().unwrap();

        let fired = seen.lock().unwrap().clone();
        let mut sorted = fired.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            fired.len(),
            "no entry may be handed to on_evict twice"
        );
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked"),
            fired.len() as u64,
            "the eviction counter must match the number of drained entries"
        );
        let survivors = ConcurrentCacheBase::cache_size(&store).unwrap().unwrap();
        assert_eq!(
            fired.len() + survivors,
            800,
            "every entry is either drained exactly once or still stored"
        );
    }
}
