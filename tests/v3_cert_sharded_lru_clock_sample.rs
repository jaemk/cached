/*!
Certification: the pre-lock clock sample on the sharded LRU-family stores.

`ShardedLruTtlCache::cache_get` / `cache_set` sample the clock **before acquiring the
shard write lock**, and every expiry decision (and the `expires_at` seeded on a write) is made
against that sample. The observable consequence is deliberate and documented: a caller that
queues behind the shard lock for longer than the TTL still judges the entry by the clock it
read before it queued.

These tests pin that deterministically instead of hoping for a race. `retain` is the only
public API that runs caller code while a shard's write lock is held, so its predicate is used
as a lock holder of a controlled duration; the reader/writer under test is released only after
the TTL has already elapsed in real time.

The contrast case is `ShardedExpiringLruCache`, which has no clock at all: liveness comes from
`Expires::is_expired()`, evaluated on the stored value *under* the lock, so the same setup gives
the opposite answer.

No Redis server required.
*/

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cached::{ConcurrentCacheBase, ConcurrentCached, Expires, ShardedExpiringLruCache};

/// TTL used by the LRU-TTL tests. Long enough that the sample the queued caller takes is
/// comfortably inside the entry's lifetime even on a badly loaded machine.
#[cfg(feature = "time_stores")]
const TTL: Duration = Duration::from_millis(500);
/// How long the lock holder keeps the shard's write lock. Must exceed `TTL` so the queued
/// caller is released only after the entry has expired in real time.
#[cfg(feature = "time_stores")]
const HOLD: Duration = Duration::from_millis(1200);

/// Spin until the lock-holder thread reports that it is inside `retain`'s predicate, i.e. the
/// shard write lock is held. No sleeps are used for ordering.
fn wait_for(flag: &AtomicBool) {
    while !flag.load(Ordering::Acquire) {
        std::hint::spin_loop();
        std::thread::yield_now();
    }
}

#[cfg(feature = "time_stores")]
mod lru_ttl {
    use super::*;
    use cached::ShardedLruTtlCache;

    /// A `cache_get` that samples the clock while the entry is live, then queues behind the
    /// shard lock past the entry's expiry, is served the value as a live hit. The very next
    /// `cache_get` (a fresh sample) sees the expiry and evicts.
    ///
    /// If the clock sample were moved back inside the critical section, the first read would
    /// return `None` and this test would fail.
    #[test]
    fn cache_get_decides_expiry_on_the_clock_sampled_before_the_shard_lock() {
        let evicted: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let evicted2 = Arc::clone(&evicted);
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(TTL)
            .on_evict(move |k: &u32, v: &u32| evicted2.lock().unwrap().push((*k, *v)))
            .build()
            .expect("build ShardedLruTtlCache");
        ConcurrentCached::cache_set(&store, 1, 10).unwrap();

        let holding = Arc::new(AtomicBool::new(false));
        let holding2 = Arc::clone(&holding);
        let holder = std::thread::spawn({
            let store = store.clone();
            move || {
                // `retain` runs `keep` under the shard write lock. Sampled at entry, key 1 is
                // still live, so it is kept, not swept.
                store.retain(move |_k, _v| {
                    holding2.store(true, Ordering::Release);
                    std::thread::sleep(HOLD);
                    true
                });
            }
        });
        wait_for(&holding);

        // Sampled now (well inside the TTL), served after `HOLD` (well past it).
        let queued = ConcurrentCached::cache_get(&store, &1).unwrap();
        holder.join().expect("lock holder must not panic");

        assert_eq!(
            queued,
            Some(10),
            "expiry must be judged against the clock read before the lock was acquired"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_hits(&store),
            Some(1),
            "the queued read counted a hit, not a miss"
        );
        assert!(
            evicted.lock().unwrap().is_empty(),
            "a read served as live must not evict"
        );

        // A fresh sample sees the expiry: miss, removal, one eviction, callback.
        let before = store.metrics().evictions.expect("evictions tracked");
        assert_eq!(ConcurrentCached::cache_get(&store, &1).unwrap(), None);
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked") - before,
            1
        );
        assert_eq!(*evicted.lock().unwrap(), vec![(1, 10)]);
        assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(0));
    }

    /// `cache_set` seeds `expires_at` from the same pre-lock sample, so a write that queues
    /// behind the shard lock for longer than the TTL lands already expired. The entry is
    /// really stored (the following read evicts it and fires `on_evict` with its value), it
    /// is just dead on arrival.
    ///
    /// If the clock sample were taken after the lock, the entry would be live and the read
    /// below would return `Some(20)`.
    #[test]
    fn cache_set_seeds_expiry_from_the_clock_sampled_before_the_shard_lock() {
        let evicted: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let evicted2 = Arc::clone(&evicted);
        let store = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(TTL)
            .on_evict(move |k: &u32, v: &u32| evicted2.lock().unwrap().push((*k, *v)))
            .build()
            .expect("build ShardedLruTtlCache");
        ConcurrentCached::cache_set(&store, 1, 10).unwrap();

        let holding = Arc::new(AtomicBool::new(false));
        let holding2 = Arc::clone(&holding);
        let holder = std::thread::spawn({
            let store = store.clone();
            move || {
                store.retain(move |_k, _v| {
                    holding2.store(true, Ordering::Release);
                    std::thread::sleep(HOLD);
                    true
                });
            }
        });
        wait_for(&holding);

        assert_eq!(
            ConcurrentCached::cache_set(&store, 2, 20).unwrap(),
            None,
            "fresh key: no displaced value"
        );
        holder.join().expect("lock holder must not panic");

        assert_eq!(
            ConcurrentCacheBase::cache_size(&store).unwrap(),
            Some(2),
            "the queued write must have stored the entry"
        );
        let before = store.metrics().evictions.expect("evictions tracked");
        assert_eq!(
            ConcurrentCached::cache_get(&store, &2).unwrap(),
            None,
            "the entry's expiry was seeded from the pre-lock sample, so it is already expired"
        );
        assert_eq!(
            store.metrics().evictions.expect("evictions tracked") - before,
            1,
            "the already-expired entry must be removed and counted exactly once"
        );
        assert_eq!(
            *evicted.lock().unwrap(),
            vec![(2, 20)],
            "on_evict must receive the stored key and the value that was written"
        );
    }
}

// --- contrast: the `Expires`-driven store has no clock sample to hoist -------------------

#[derive(Clone, Debug)]
struct Flagged {
    #[allow(dead_code)]
    v: u32,
    dead: Arc<AtomicBool>,
}

impl Expires for Flagged {
    fn is_expired(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }
}

/// `ShardedExpiringLruCache` asks the *stored value* whether it is expired, inside the
/// critical section. A value that becomes expired while a reader is queued behind the shard
/// lock is therefore seen as expired by that reader -- the opposite of the LRU-TTL store's
/// pre-lock sample, and the reason no clock hoist is possible here.
#[test]
fn expiring_lru_evaluates_is_expired_under_the_lock_not_before() {
    let dead = Arc::new(AtomicBool::new(false));
    let evicted: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let evicted2 = Arc::clone(&evicted);
    let store = ShardedExpiringLruCache::<u32, Flagged>::builder()
        .shards(1)
        .per_shard_max_size(8)
        .on_evict(move |k: &u32, _v: &Flagged| evicted2.lock().unwrap().push(*k))
        .build()
        .expect("build ShardedExpiringLruCache");
    ConcurrentCached::cache_set(
        &store,
        1,
        Flagged {
            v: 10,
            dead: Arc::clone(&dead),
        },
    )
    .unwrap();

    let holding = Arc::new(AtomicBool::new(false));
    let holding2 = Arc::clone(&holding);
    let holder = std::thread::spawn({
        let store = store.clone();
        move || {
            // The predicate sees a live value (`dead` is still false) and keeps it.
            store.retain(move |_k, _v| {
                holding2.store(true, Ordering::Release);
                std::thread::sleep(Duration::from_millis(300));
                true
            });
        }
    });
    wait_for(&holding);

    // Flip the value to expired while the reader is (about to be) queued.
    dead.store(true, Ordering::Release);
    let queued = ConcurrentCached::cache_get(&store, &1).unwrap();
    holder.join().expect("lock holder must not panic");

    assert!(
        queued.is_none(),
        "liveness is read from the value under the lock, so the flip is observed"
    );
    assert_eq!(ConcurrentCacheBase::cache_size(&store).unwrap(), Some(0));
    assert_eq!(*evicted.lock().unwrap(), vec![1]);
    assert_eq!(ConcurrentCacheBase::cache_misses(&store), Some(1));
}
