/*!
Concurrent-expiry races on the sharded TTL stores (`ShardedTtlCache`,
`ShardedLruTtlCache`).

When an entry expires, the first `cache_get` to reach it removes it, fires
`on_evict` once, and counts a single eviction; concurrent readers racing the same
expired key must NOT double-fire the callback or double-count the eviction. The
lazy-expiry path takes a read lock, then upgrades to a write lock and re-checks
under the write lock, so only the thread that wins `remove_entry` observes the
removal (`src/stores/sharded/ttl.rs` and `sharded/lru_ttl.rs`).

These tests do NOT require a Redis server.

Covered:
- 1-shard cache, short TTL, N threads racing `cache_get` on the same expired key:
  `on_evict` fires exactly once and `cache_evictions` advances by exactly 1, and
  every reader observes `None`.
- A flip-stress that re-inserts a fresh value while readers race the expired one,
  exercising the write-upgrade recheck branch: the eviction counter and the
  `on_evict` callback stay in lockstep (never diverge) across many rounds.

All items are gated `#[cfg(feature = "time_stores")]`.
*/
#![cfg(feature = "time_stores")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use cached::{ConcurrentCacheBase, ConcurrentCached, ShardedLruTtlCache, ShardedTtlCache};

const RACERS: usize = 16;

#[test]
fn sharded_ttl_expiry_race_fires_on_evict_once() {
    let fired = Arc::new(AtomicU64::new(0));
    let fired2 = fired.clone();
    let cache = Arc::new(
        ShardedTtlCache::<u32, u32>::builder()
            .shards(1)
            .ttl(Duration::from_millis(30))
            .on_evict(move |_, _| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build 1-shard ShardedTtlCache"),
    );

    ConcurrentCached::cache_set(&*cache, 1, 100).unwrap();
    let before = ConcurrentCacheBase::cache_evictions(&*cache).unwrap();
    std::thread::sleep(Duration::from_millis(80));

    // Release all readers at once so they collide on the expired entry.
    let gate = Arc::new(Barrier::new(RACERS));
    let mut handles = Vec::new();
    for _ in 0..RACERS {
        let cache = cache.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            ConcurrentCached::cache_get(&*cache, &1).unwrap()
        }));
    }
    for h in handles {
        assert_eq!(h.join().unwrap(), None, "expired key must read as None");
    }

    assert_eq!(
        fired.load(Ordering::Relaxed),
        1,
        "on_evict must fire exactly once no matter how many readers race the expiry"
    );
    assert_eq!(
        ConcurrentCacheBase::cache_evictions(&*cache).unwrap(),
        before + 1,
        "exactly one eviction must be counted for the single expired entry"
    );
}

#[test]
fn sharded_lru_ttl_expiry_race_fires_on_evict_once() {
    let fired = Arc::new(AtomicU64::new(0));
    let fired2 = fired.clone();
    let cache = Arc::new(
        ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(64)
            .ttl(Duration::from_millis(30))
            .on_evict(move |_, _| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build 1-shard ShardedLruTtlCache"),
    );

    ConcurrentCached::cache_set(&*cache, 1, 100).unwrap();
    let before = ConcurrentCacheBase::cache_evictions(&*cache).unwrap();
    std::thread::sleep(Duration::from_millis(80));

    let gate = Arc::new(Barrier::new(RACERS));
    let mut handles = Vec::new();
    for _ in 0..RACERS {
        let cache = cache.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            ConcurrentCached::cache_get(&*cache, &1).unwrap()
        }));
    }
    for h in handles {
        assert_eq!(h.join().unwrap(), None, "expired key must read as None");
    }

    assert_eq!(
        fired.load(Ordering::Relaxed),
        1,
        "on_evict must fire exactly once no matter how many readers race the expiry"
    );
    assert_eq!(
        ConcurrentCacheBase::cache_evictions(&*cache).unwrap(),
        before + 1,
        "exactly one eviction must be counted for the single expired entry"
    );
}

// Flip-stress: a writer keeps re-inserting a fresh value under a short TTL while
// readers race the (possibly-expired) key. This exercises the write-upgrade
// recheck branch, where a reader that upgraded to the write lock finds a fresh
// value and returns a hit instead of evicting. The invariant that must always
// hold: the `on_evict` callback fires exactly as many times as the eviction
// counter advances -- the two are bumped together, so any divergence would mean
// a double-count or a missed callback in the race.
#[test]
fn sharded_ttl_flip_stress_evictions_and_callback_stay_in_lockstep() {
    let fired = Arc::new(AtomicU64::new(0));
    let fired2 = fired.clone();
    let cache = Arc::new(
        ShardedTtlCache::<u32, u32>::builder()
            .shards(1)
            .ttl(Duration::from_millis(2))
            .on_evict(move |_, _| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build 1-shard ShardedTtlCache"),
    );

    const ROUNDS: u32 = 200;
    let gate = Arc::new(Barrier::new(RACERS + 1));

    let mut handles = Vec::new();
    // Reader threads: hammer cache_get across the whole run.
    for _ in 0..RACERS {
        let cache = cache.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for _ in 0..ROUNDS {
                let _ = ConcurrentCached::cache_get(&*cache, &1).unwrap();
            }
        }));
    }
    // Writer thread: re-insert a fresh value each round, letting the short TTL
    // lapse in between so readers alternately hit and evict.
    {
        let cache = cache.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                ConcurrentCached::cache_set(&*cache, 1, r).unwrap();
                std::thread::sleep(Duration::from_millis(1));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // The counter and the callback are bumped together on every removal, so they
    // must be equal regardless of how the race interleaved.
    let evictions = ConcurrentCacheBase::cache_evictions(&*cache).unwrap();
    assert_eq!(
        fired.load(Ordering::Relaxed),
        evictions,
        "on_evict must fire exactly once per counted eviction across the race"
    );
}
