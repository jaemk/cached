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
- The same expiry race with `refresh_on_hit = true`, which takes a different code
  path entirely (an exclusive write lock and a single clock sample per call, with
  no read-lock/write-lock upgrade): `on_evict` still fires exactly once.
- A `refresh_on_hit = true` flip-stress across several shards with entries moving
  between live and expired under contention. Besides the callback/counter lockstep
  it asserts entry conservation: every entry ever inserted is either still stored,
  counted as an eviction, or was returned to a `cache_set` caller as a displaced
  live value -- so no removal can be lost or double-counted.

All items are gated `#[cfg(feature = "time_stores")]`.
*/
#![cfg(feature = "time_stores")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    let stop = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Barrier::new(RACERS + 1));

    // Reader threads: hammer cache_get continuously until the writer signals done.
    // Looping on the stop flag (rather than a fixed round count that finishes in
    // microseconds) keeps readers contending while each freshly-written entry ages
    // past the 2ms TTL, so they actually reach and evict expired entries.
    let mut readers = Vec::new();
    for _ in 0..RACERS {
        let cache = cache.clone();
        let gate = gate.clone();
        let stop = stop.clone();
        readers.push(std::thread::spawn(move || {
            gate.wait();
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let _ = ConcurrentCached::cache_get(&*cache, &1).unwrap();
                reads += 1;
            }
            reads
        }));
    }
    // Writer thread: re-insert a fresh value each round, sleeping LONGER than the
    // TTL between rounds so the entry expires before the next overwrite. That lets
    // the still-looping readers hit the expired entry and evict it, alternating the
    // hit-and-evict path and driving the write-upgrade recheck branch.
    let writer = {
        let cache = cache.clone();
        let gate = gate.clone();
        std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                ConcurrentCached::cache_set(&*cache, 1, r).unwrap();
                std::thread::sleep(Duration::from_millis(4));
            }
        })
    };
    writer.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    let mut reads = 0u64;
    for h in readers {
        reads += h.join().unwrap();
    }
    assert!(reads > 0, "the readers must have run");

    // Guard against a vacuous run: readers looping while entries age past the TTL
    // must actually reach expired entries and evict them, so the eviction counter
    // has to advance. Without this the lockstep check below could pass trivially as
    // 0 == 0 while never exercising the expiry/recheck branch it claims to cover.
    let evictions = ConcurrentCacheBase::cache_evictions(&*cache).unwrap();
    assert!(
        evictions > 0,
        "no entry ever expired -- the write-upgrade recheck branch was never exercised"
    );
    // The counter and the callback are bumped together on every removal, so they
    // must be equal regardless of how the race interleaved.
    assert_eq!(
        fired.load(Ordering::Relaxed),
        evictions,
        "on_evict must fire exactly once per counted eviction across the race"
    );
}

// `refresh_on_hit = true` sends `cache_get` down a completely different branch: it takes
// the shard's *write* lock up front, samples the clock once (and only when the key is
// present), and decides absent / expired / live-and-refreshed from that one sample. There
// is no read-lock-then-upgrade recheck on this path, so the "exactly one remover" property
// has to come from `remove_entry` under the single write lock instead. The two tests below
// stress that branch; the rest of this file only ever exercised the default (non-refresh)
// path.
#[test]
fn sharded_ttl_refresh_on_hit_expiry_race_fires_on_evict_once() {
    let fired = Arc::new(AtomicU64::new(0));
    let fired2 = fired.clone();
    let cache = Arc::new(
        ShardedTtlCache::<u32, u32>::builder()
            .shards(1)
            .ttl(Duration::from_millis(30))
            .refresh_on_hit(true)
            .on_evict(move |_, _| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build 1-shard refresh_on_hit ShardedTtlCache"),
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
        "refresh_on_hit must still evict a raced expired entry exactly once"
    );
    assert_eq!(
        ConcurrentCacheBase::cache_evictions(&*cache).unwrap(),
        before + 1,
        "exactly one eviction must be counted for the single expired entry"
    );
    assert_eq!(cache.len(), 0, "the expired entry must be physically gone");
}

#[test]
fn sharded_ttl_refresh_on_hit_flip_stress_conserves_entries_and_evictions() {
    const KEYS: u32 = 8;
    const ROUNDS: u32 = 150;
    const WRITERS: u32 = 2;

    let fired = Arc::new(AtomicU64::new(0));
    let fired2 = fired.clone();
    let cache = Arc::new(
        ShardedTtlCache::<u32, u32>::builder()
            .shards(4)
            .ttl(Duration::from_micros(200))
            .refresh_on_hit(true)
            .on_evict(move |_, _| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build 4-shard refresh_on_hit ShardedTtlCache"),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Barrier::new(RACERS + WRITERS as usize));

    // Readers: hammer every key. With a 200us TTL each key keeps flipping between live
    // (refreshed in place under the write lock) and expired (removed, counted, callback).
    let mut readers = Vec::new();
    for _ in 0..RACERS {
        let cache = cache.clone();
        let gate = gate.clone();
        let stop = stop.clone();
        readers.push(std::thread::spawn(move || {
            gate.wait();
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                for k in 0..KEYS {
                    if let Some(v) = ConcurrentCached::cache_get(&*cache, &k).unwrap() {
                        assert_eq!(
                            v % 10,
                            k,
                            "a read for key {k} returned a value written for key {}",
                            v % 10
                        );
                    }
                    reads += 1;
                }
            }
            reads
        }));
    }

    // Writers: re-insert every key each round, counting the displaced *live* values they
    // are handed back (those leave the map without being counted as evictions).
    let mut writers = Vec::new();
    for w in 0..WRITERS {
        let cache = cache.clone();
        let gate = gate.clone();
        writers.push(std::thread::spawn(move || {
            gate.wait();
            let mut live_displacements = 0u64;
            for r in 0..ROUNDS {
                for k in 0..KEYS {
                    let value = r * 100 + w * 10 + k;
                    if ConcurrentCached::cache_set(&*cache, k, value)
                        .unwrap()
                        .is_some()
                    {
                        live_displacements += 1;
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            live_displacements
        }));
    }

    let mut live_displacements = 0u64;
    for h in writers {
        live_displacements += h.join().expect("writer thread must not panic");
    }
    stop.store(true, Ordering::Relaxed);
    let mut reads = 0u64;
    for h in readers {
        reads += h.join().expect("reader thread must not panic");
    }
    assert!(reads > 0, "the readers must have run");

    let inserts = u64::from(WRITERS * ROUNDS * KEYS);
    let evictions = ConcurrentCacheBase::cache_evictions(&*cache).unwrap();
    // Guard against a vacuous run: with a 200us TTL, a 1ms pause between writer rounds and
    // readers refreshing hits in place, both kinds of displacement must actually occur.
    assert!(
        evictions > 0,
        "no entry ever expired -- the refresh/expiry branch was never exercised"
    );
    assert!(
        live_displacements > 0,
        "no live entry was ever displaced -- the live-refresh branch was never exercised"
    );
    assert_eq!(
        fired.load(Ordering::Relaxed),
        evictions,
        "on_evict must fire exactly once per counted eviction across the race"
    );
    let remaining = cache.len() as u64;
    assert!(
        remaining <= u64::from(KEYS),
        "at most one entry per key can be stored, found {remaining}"
    );
    // Every entry ever inserted left the map as a counted eviction, was handed back to a
    // `cache_set` caller as a displaced live value, or is still stored. A lost or
    // double-counted eviction breaks this equality.
    assert_eq!(
        inserts,
        remaining + evictions + live_displacements,
        "inserts={inserts} remaining={remaining} evictions={evictions} \
         live_displacements={live_displacements}"
    );

    // Final sweep: the counter advances by exactly the number of entries it removed, and
    // conservation still holds afterwards.
    let swept = cache.evict() as u64;
    let evictions_after = ConcurrentCacheBase::cache_evictions(&*cache).unwrap();
    assert_eq!(
        evictions_after,
        evictions + swept,
        "the sweep must count exactly what it removed"
    );
    assert_eq!(
        fired.load(Ordering::Relaxed),
        evictions_after,
        "the sweep must fire on_evict once per removed entry too"
    );
    assert_eq!(
        inserts,
        cache.len() as u64 + evictions_after + live_displacements,
        "conservation must survive the sweep"
    );
    for k in 0..KEYS {
        if let Some(v) = ConcurrentCached::cache_get(&*cache, &k).unwrap() {
            assert_eq!(v % 10, k, "surviving entries must be self-consistent");
        }
    }
}
