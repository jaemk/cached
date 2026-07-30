//! Outside-in certification of the `LruTtlCache` (non-sharded) performance rework:
//! single-clock-sample threading (`entry_live_at`), `drain_all`-backed clearing,
//! hash reuse via `pop_raw_with_hash`, and the pre-sized eager collectors.
//!
//! Every test here pins an **observable** contract from outside the store: no
//! private state is touched, so the tests remain valid if the internals are
//! rewritten again.
//!
//! The instruments used deliberately go beyond the happy path:
//!
//! * A **counting** `BuildHasher` makes "the hash is reused, not recomputed" and
//!   "clearing performs zero hashing" directly observable: the number of
//!   `build_hasher()` calls per public operation is asserted exactly.
//! * A **colliding** `BuildHasher` (every key hashes to `0`) forces every entry
//!   into one bucket chain, so any removal path that trusts the hash instead of
//!   the `Eq` probe removes the wrong entry and is caught.
//! * A **slow-cloning** key/value type stretches an eager collector pass past an
//!   entry's expiry, which distinguishes "one clock sample for the whole pass"
//!   from "a clock read per entry".
//! * A **lazily consumed** `iter()` held across an expiry pins the deliberately
//!   *opposite* convention for the lazy iterator.
#![cfg(feature = "time_stores")]

use std::collections::hash_map::DefaultHasher;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cached::time::Duration;
use cached::{CacheTtl, Cached, CachedExt, CachedIter, CachedPeek, CloneCached, LruTtlCache};

// ── timing constants ─────────────────────────────────────────────────────────
//
// The gaps are deliberately generous: every assertion needs only "the sleep is
// longer than the ttl" (or the reverse), so a descheduled test thread widens the
// margin instead of flipping the result.

/// TTL short enough that `PAST_TTL` reliably outlives it.
const TTL: Duration = Duration::from_millis(150);
/// Sleep that puts an entry inserted under [`TTL`] comfortably past its expiry.
const PAST_TTL: std::time::Duration = std::time::Duration::from_millis(400);
/// TTL used by the eager-collector snapshot tests: must be much longer than the
/// microseconds between the last insert and the collector call, and much shorter
/// than [`SLOW_CLONE`].
const SNAPSHOT_TTL: Duration = Duration::from_millis(500);
/// Time a "slow" `Clone` burns, stretching one eager collector pass well past
/// [`SNAPSHOT_TTL`].
const SLOW_CLONE: std::time::Duration = std::time::Duration::from_millis(900);

// ── instruments ──────────────────────────────────────────────────────────────

/// A `BuildHasher` that hashes every key to `0`, so all entries land in one
/// bucket chain and only the `K: Eq` probe can tell them apart.
#[derive(Clone, Default)]
struct CollideBuildHasher;

struct CollideHasher;

impl Hasher for CollideHasher {
    fn write(&mut self, _bytes: &[u8]) {}
    fn finish(&self) -> u64 {
        0
    }
}

impl BuildHasher for CollideBuildHasher {
    type Hasher = CollideHasher;
    fn build_hasher(&self) -> Self::Hasher {
        CollideHasher
    }
}

/// A deterministic `BuildHasher` that counts how many hashers it has handed out.
/// One `build_hasher()` call == one key hashed.
#[derive(Clone)]
struct CountingBuildHasher {
    built: Arc<AtomicUsize>,
}

impl CountingBuildHasher {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let built = Arc::new(AtomicUsize::new(0));
        (
            Self {
                built: built.clone(),
            },
            built,
        )
    }
}

impl BuildHasher for CountingBuildHasher {
    type Hasher = DefaultHasher;
    fn build_hasher(&self) -> Self::Hasher {
        self.built.fetch_add(1, Ordering::Relaxed);
        // `DefaultHasher::new()` is deterministic within a process, so the hash a
        // probe computes is the same one a later lookup for the same key computes.
        DefaultHasher::new()
    }
}

/// A deterministic FNV-1a `BuildHasher`: a real custom hasher (not the crate
/// default, not a degenerate one) for the end-to-end custom-hasher pass.
#[derive(Clone, Default)]
struct FnvBuildHasher;

struct FnvHasher(u64);

impl Hasher for FnvHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

impl BuildHasher for FnvBuildHasher {
    type Hasher = FnvHasher;
    fn build_hasher(&self) -> Self::Hasher {
        FnvHasher(0xcbf2_9ce4_8422_2325)
    }
}

/// Records every `on_evict` firing as `(key, value)` in order.
type Log<K, V> = Arc<Mutex<Vec<(K, V)>>>;

fn log<K, V>() -> Log<K, V> {
    Arc::new(Mutex::new(Vec::new()))
}

fn drain<K: Clone, V: Clone>(l: &Log<K, V>) -> Vec<(K, V)> {
    l.lock().expect("on_evict log poisoned").clone()
}

/// A value whose `Clone` optionally sleeps, used to stretch an eager collector pass.
#[derive(Debug, PartialEq, Eq)]
struct SlowClone {
    id: u32,
    slow: bool,
}

impl Clone for SlowClone {
    fn clone(&self) -> Self {
        if self.slow {
            std::thread::sleep(SLOW_CLONE);
        }
        Self {
            id: self.id,
            slow: self.slow,
        }
    }
}

/// A key whose `Clone` optionally sleeps (`key_order` clones keys, not values).
#[derive(Debug, PartialEq, Eq, Hash)]
struct SlowKey {
    id: u32,
    slow: bool,
}

impl Clone for SlowKey {
    fn clone(&self) -> Self {
        if self.slow {
            std::thread::sleep(SLOW_CLONE);
        }
        Self {
            id: self.id,
            slow: self.slow,
        }
    }
}

// ── builders ─────────────────────────────────────────────────────────────────

fn collide_cache(
    max_size: usize,
    ttl: Duration,
    l: Log<u32, u32>,
) -> LruTtlCache<u32, u32, CollideBuildHasher> {
    LruTtlCache::<u32, u32>::builder()
        .max_size(max_size)
        .ttl(ttl)
        .hasher(CollideBuildHasher)
        .on_evict(move |k: &u32, v: &u32| l.lock().expect("on_evict log poisoned").push((*k, *v)))
        .build()
        .expect("build LruTtlCache with a colliding hasher")
}

fn logging_cache(max_size: usize, ttl: Duration, l: Log<u32, u32>) -> LruTtlCache<u32, u32> {
    LruTtlCache::<u32, u32>::builder()
        .max_size(max_size)
        .ttl(ttl)
        .on_evict(move |k: &u32, v: &u32| l.lock().expect("on_evict log poisoned").push((*k, *v)))
        .build()
        .expect("build LruTtlCache")
}

// =============================================================================
// CHANGE 3 -- `pop_raw_with_hash`: the probe's hash is reused, not recomputed.
// =============================================================================

// The perf claim itself, made observable: a `cache_get` that finds an EXPIRED
// entry hashes the key exactly ONCE. Reverting the lazy sweep to `pop_raw`
// (which re-hashes) makes this 2.
#[test]
fn cache_get_lazy_sweep_hashes_the_key_exactly_once() {
    let (hasher, built) = CountingBuildHasher::new();
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(TTL)
        .hasher(hasher)
        .build()
        .expect("build LruTtlCache with a counting hasher");
    c.cache_set(1, 10);
    std::thread::sleep(PAST_TTL);

    built.store(0, Ordering::Relaxed);
    assert_eq!(c.cache_get(&1), None, "the entry has expired");
    assert_eq!(
        built.load(Ordering::Relaxed),
        1,
        "the expired-entry sweep must reuse the probe's hash: exactly one hash per call"
    );
    assert_eq!(c.cache_size(), 0, "the expired entry must be gone");

    // The plain paths hash once too, so the assertion above is a real bound and
    // not an accident of the miss path.
    built.store(0, Ordering::Relaxed);
    assert_eq!(c.cache_get(&1), None, "absent key");
    assert_eq!(
        built.load(Ordering::Relaxed),
        1,
        "an absent-key miss must hash exactly once"
    );

    c.cache_set(2, 20);
    built.store(0, Ordering::Relaxed);
    assert_eq!(c.cache_get(&2), Some(&20));
    assert_eq!(
        built.load(Ordering::Relaxed),
        1,
        "a live hit must hash exactly once"
    );
}

#[test]
fn cache_get_mut_lazy_sweep_hashes_the_key_exactly_once() {
    let (hasher, built) = CountingBuildHasher::new();
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(TTL)
        .hasher(hasher)
        .build()
        .expect("build LruTtlCache with a counting hasher");
    c.cache_set(1, 10);
    std::thread::sleep(PAST_TTL);

    built.store(0, Ordering::Relaxed);
    assert_eq!(c.cache_get_mut(&1), None);
    assert_eq!(
        built.load(Ordering::Relaxed),
        1,
        "cache_get_mut's expired-entry sweep must reuse the probe's hash"
    );
    assert_eq!(c.cache_size(), 0);
}

/// Build a 4-entry colliding cache in which exactly `doomed` has a short ttl
/// (inserted at its natural position in the insertion order, so the doomed entry
/// occupies every possible position in the shared bucket chain across a sweep of
/// `doomed` values). Returns the cache and its `on_evict` log; the caller sleeps
/// past [`TTL`] before probing.
fn colliding_cache_with_one_doomed_key(
    doomed: u32,
) -> (LruTtlCache<u32, u32, CollideBuildHasher>, Log<u32, u32>) {
    const LONG: Duration = Duration::from_secs(60);
    let l = log();
    let mut c = collide_cache(8, LONG, l.clone());
    for k in 1..=4u32 {
        if k == doomed {
            c.set_ttl(TTL);
            c.cache_set(k, k * 10);
            c.set_ttl(LONG);
        } else {
            c.cache_set(k, k * 10);
        }
    }
    assert_eq!(c.cache_size(), 4);
    (c, l)
}

/// Recency order (MRU -> LRU) of keys 1..=4 minus `doomed`.
fn survivors_of(doomed: u32) -> Vec<u32> {
    (1..=4u32).rev().filter(|k| *k != doomed).collect()
}

// With every key colliding, a removal that trusts the hash rather than the `Eq`
// probe evicts an arbitrary neighbour from the shared bucket chain. Sweeping the
// doomed key at EVERY position in the chain leaves no ordering the mutation can
// hide behind.
#[test]
fn lazy_sweep_under_hash_collisions_removes_only_the_expired_key() {
    for doomed in 1..=4u32 {
        let (mut c, l) = colliding_cache_with_one_doomed_key(doomed);
        std::thread::sleep(PAST_TTL);

        assert_eq!(c.cache_get(&doomed), None, "key {doomed} has expired");
        assert_eq!(
            drain(&l),
            vec![(doomed, doomed * 10)],
            "exactly the expired entry must be evicted, with its own key and value"
        );
        assert_eq!(c.cache_size(), 3);
        assert_eq!(
            c.key_order(),
            survivors_of(doomed),
            "the colliding neighbours must be untouched, in their original recency order"
        );
        for k in 1..=4u32 {
            if k != doomed {
                assert_eq!(c.cache_get(&k), Some(&(k * 10)), "survivor {k}");
            }
        }
        assert_eq!(
            drain(&l),
            vec![(doomed, doomed * 10)],
            "no further evictions from reading the survivors"
        );
    }
}

#[test]
fn lazy_sweep_mut_under_hash_collisions_removes_only_the_expired_key() {
    for doomed in 1..=4u32 {
        let (mut c, l) = colliding_cache_with_one_doomed_key(doomed);
        std::thread::sleep(PAST_TTL);

        assert_eq!(c.cache_get_mut(&doomed), None);
        assert_eq!(drain(&l), vec![(doomed, doomed * 10)]);
        assert_eq!(c.key_order(), survivors_of(doomed));
        for k in 1..=4u32 {
            if k != doomed {
                assert_eq!(c.cache_get_mut(&k), Some(&mut (k * 10)), "survivor {k}");
            }
        }
    }
}

// `cache_remove` / `cache_remove_entry` go through `pop_raw`, which computes the
// hash itself and then calls `pop_raw_with_hash`. Same collision trap.
#[test]
fn cache_remove_under_hash_collisions_removes_only_the_named_key() {
    let l = log();
    let mut c = collide_cache(8, Duration::from_secs(60), l.clone());
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);

    assert_eq!(c.cache_remove(&2), Some(20));
    assert_eq!(drain(&l), vec![(2, 20)]);
    assert_eq!(c.key_order(), vec![3, 1]);

    assert_eq!(c.cache_remove_entry(&1), Some((1, 10)));
    assert_eq!(c.key_order(), vec![3]);
    assert_eq!(
        c.cache_remove(&1),
        None,
        "removing an already-removed key must be a no-op"
    );
    assert_eq!(
        drain(&l),
        vec![(2, 20), (1, 10)],
        "no callback for the absent key"
    );
    assert_eq!(c.cache_get(&3), Some(&30));
}

// Borrowed lookup (`K = String`, `Q = str`): the reused hash comes from the
// `&str`, and must still find the `String`-keyed entry among colliding keys.
#[test]
fn borrowed_key_lazy_sweep_under_hash_collisions_removes_only_the_expired_key() {
    let l: Log<String, u32> = log();
    let l2 = l.clone();
    let mut c = LruTtlCache::<String, u32>::builder()
        .max_size(8)
        .ttl(TTL)
        .hasher(CollideBuildHasher)
        .on_evict(move |k: &String, v: &u32| {
            l2.lock()
                .expect("on_evict log poisoned")
                .push((k.clone(), *v))
        })
        .build()
        .expect("build String-keyed LruTtlCache with a colliding hasher");

    c.cache_set("doomed".to_string(), 1);
    std::thread::sleep(PAST_TTL);
    c.cache_set("alpha".to_string(), 2);
    c.cache_set("beta".to_string(), 3);

    assert_eq!(c.cache_get("doomed"), None, "expired, looked up as &str");
    assert_eq!(
        drain(&l),
        vec![("doomed".to_string(), 1)],
        "on_evict must receive the STORED String key"
    );
    assert_eq!(c.cache_size(), 2);
    assert_eq!(c.cache_get("alpha"), Some(&2));
    assert_eq!(c.cache_get("beta"), Some(&3));
}

// `evict` / `retain` remove by LRU slot index (the stored key is re-hashed), so
// collisions exercise a different lookup than the `pop_raw*` family.
#[test]
fn evict_and_retain_under_hash_collisions_remove_only_the_right_entries() {
    let l = log();
    let mut c = collide_cache(8, TTL, l.clone());
    c.cache_set(1, 10); // doomed
    c.cache_set(2, 20); // doomed
    std::thread::sleep(PAST_TTL);
    c.cache_set(3, 30);
    c.cache_set(4, 40);
    c.cache_set(5, 50);

    assert_eq!(c.evict(), 2, "exactly the two expired entries are swept");
    let mut fired = drain(&l);
    fired.sort_unstable();
    assert_eq!(fired, vec![(1, 10), (2, 20)]);
    assert_eq!(c.key_order(), vec![5, 4, 3]);

    c.retain(|k, _v| *k != 4);
    assert_eq!(
        c.key_order(),
        vec![5, 3],
        "retain must drop exactly the predicate's rejects and keep survivor order"
    );
    assert_eq!(c.cache_get(&3), Some(&30));
    assert_eq!(c.cache_get(&5), Some(&50));
    assert_eq!(c.cache_get(&4), None);
}

// Capacity eviction under collisions still picks the LRU victim.
#[test]
fn capacity_eviction_under_hash_collisions_evicts_the_lru_entry() {
    let l = log();
    let mut c = collide_cache(2, Duration::from_secs(60), l.clone());
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    assert_eq!(c.cache_get(&1), Some(&10)); // 2 becomes the LRU
    c.cache_set(3, 30);

    assert_eq!(drain(&l), vec![(2, 20)], "the LRU entry must be the victim");
    assert_eq!(c.cache_size(), 2);
    assert_eq!(c.key_order(), vec![3, 1]);
}

// A real (non-degenerate, non-default) custom hasher end to end: `LruTtlCache`
// had no custom-hasher coverage at all before this file.
#[test]
fn custom_fnv_hasher_lru_ttl_end_to_end() {
    let l: Log<String, u32> = log();
    let l2 = l.clone();
    let mut c = LruTtlCache::<String, u32>::builder()
        .max_size(3)
        .ttl(TTL)
        .hasher(FnvBuildHasher)
        .on_evict(move |k: &String, v: &u32| {
            l2.lock()
                .expect("on_evict log poisoned")
                .push((k.clone(), *v))
        })
        .build()
        .expect("build LruTtlCache with FnvBuildHasher");

    c.cache_set("a".to_string(), 1);
    c.cache_set("b".to_string(), 2);
    c.cache_set("c".to_string(), 3);
    assert_eq!(c.cache_get("a"), Some(&1));
    assert_eq!(c.cache_get("b"), Some(&2));
    assert_eq!(c.cache_get("c"), Some(&3));
    assert_eq!(c.cache_get("absent"), None);
    assert_eq!(c.cache_peek("a"), Some(&1));
    assert_eq!(c.cache_peek_with_expiry_status("a"), (Some(1), false));

    // Capacity eviction: after the reads above the LRU is "a".
    c.cache_set("d".to_string(), 4);
    assert_eq!(drain(&l), vec![("a".to_string(), 1)]);
    assert_eq!(c.cache_size(), 3);

    // Expiry + lazy sweep under the custom hasher.
    std::thread::sleep(PAST_TTL);
    assert_eq!(c.cache_get("b"), None, "expired under the custom hasher");
    assert_eq!(c.cache_size(), 2);
    assert_eq!(c.evict(), 2, "the rest expire too");
    assert_eq!(c.cache_size(), 0);
    assert_eq!(
        drain(&l).len(),
        4,
        "one callback per removed entry: 1 capacity + 1 lazy + 2 swept"
    );

    // Still usable afterwards.
    c.cache_set("e".to_string(), 5);
    assert_eq!(c.cache_get("e"), Some(&5));
}

// =============================================================================
// CHANGE 2 -- `drain_all`-backed clearing.
// =============================================================================

// `drain_all` clears the hash table wholesale and walks the LRU chain: it must
// perform ZERO hashing. A key-by-key drain would hash once per entry.
#[test]
fn cache_clear_with_on_evict_performs_no_hashing() {
    let (hasher, built) = CountingBuildHasher::new();
    let fired = Arc::new(AtomicUsize::new(0));
    let fired2 = fired.clone();
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(16)
        .ttl(Duration::from_secs(60))
        .hasher(hasher)
        .on_evict(move |_k: &u32, _v: &u32| {
            fired2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .expect("build LruTtlCache with a counting hasher");
    for i in 0..8u32 {
        c.cache_set(i, i * 10);
    }

    built.store(0, Ordering::Relaxed);
    c.cache_clear_with_on_evict();
    assert_eq!(
        built.load(Ordering::Relaxed),
        0,
        "drain_all must not hash any key"
    );
    assert_eq!(fired.load(Ordering::Relaxed), 8, "one callback per entry");
    assert_eq!(c.cache_size(), 0);
}

// Every entry fires exactly once with its own key and value, even when the LRU
// slab already has holes from capacity evictions, and the counters agree.
#[test]
fn cache_clear_with_on_evict_fires_once_per_entry_after_capacity_evictions() {
    let l = log();
    let mut c = logging_cache(3, Duration::from_secs(60), l.clone());
    for i in 1..=5u32 {
        c.cache_set(i, i * 10);
    }
    // Capacity 3: inserting 4 and 5 evicted 1 and 2 (recording two firings and
    // two evictions), leaving slab holes for the drain to walk over.
    assert_eq!(drain(&l), vec![(1, 10), (2, 20)]);
    assert_eq!(c.cache_size(), 3);
    assert_eq!(c.cache_evictions(), Some(2));

    c.cache_clear_with_on_evict();

    assert_eq!(
        drain(&l),
        vec![(1, 10), (2, 20), (5, 50), (4, 40), (3, 30)],
        "the drain must fire once per surviving entry, MRU -> LRU, with correct keys/values"
    );
    let m = c.metrics();
    assert_eq!(m.entry_count, Some(0), "no entries left");
    assert_eq!(
        m.evictions,
        Some(5),
        "2 capacity evictions + 3 drained entries"
    );
    assert_eq!(m.capacity, Some(3), "capacity is unchanged by a clear");
    assert_eq!(c.key_order(), Vec::<u32>::new());
    assert_eq!(CachedIter::iter(&c).count(), 0);
}

// Zero-entry boundary: the drain of an empty cache must fire nothing and must
// not touch the eviction counter (the `count > 0` guard).
#[test]
fn cache_clear_with_on_evict_on_an_empty_cache_fires_nothing() {
    let l = log();
    let mut c = logging_cache(4, Duration::from_secs(60), l.clone());

    c.cache_clear_with_on_evict();
    assert_eq!(drain(&l), Vec::new(), "nothing to evict");
    assert_eq!(c.cache_evictions(), Some(0));
    assert_eq!(c.cache_size(), 0);

    // Fill, drain, then drain again: the second drain is also a no-op.
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_clear_with_on_evict();
    assert_eq!(drain(&l), vec![(2, 20), (1, 10)]);
    assert_eq!(c.cache_evictions(), Some(2));

    c.cache_clear_with_on_evict();
    assert_eq!(
        drain(&l),
        vec![(2, 20), (1, 10)],
        "a second drain of an empty cache must fire nothing"
    );
    assert_eq!(
        c.cache_evictions(),
        Some(2),
        "and must not move the eviction counter"
    );

    // The cache is still fully functional after the empty drains.
    c.cache_set(3, 30);
    assert_eq!(c.cache_get(&3), Some(&30));
    assert_eq!(c.cache_size(), 1);
}

// Under collisions the drain still yields every entry exactly once (the table is
// cleared wholesale, so a bucket chain must not swallow entries).
#[test]
fn cache_clear_with_on_evict_under_hash_collisions_drains_every_entry_once() {
    let l = log();
    let mut c = collide_cache(8, Duration::from_secs(60), l.clone());
    for i in 1..=6u32 {
        c.cache_set(i, i * 10);
    }

    c.cache_clear_with_on_evict();
    assert_eq!(
        drain(&l),
        vec![(6, 60), (5, 50), (4, 40), (3, 30), (2, 20), (1, 10)],
        "every colliding entry drains exactly once, MRU -> LRU"
    );
    assert_eq!(c.cache_size(), 0);
    assert_eq!(c.cache_evictions(), Some(6));

    // Reusable, and the hash table really was emptied (no stale bucket entries).
    c.cache_set(1, 111);
    assert_eq!(c.cache_get(&1), Some(&111));
    assert_eq!(c.cache_size(), 1);
}

// =============================================================================
// CHANGE 1 -- one clock sample per call, threaded through.
// =============================================================================

// The eager collectors hoist ONE clock reading for the whole pass. A slow `Clone`
// stretches the pass past a later entry's expiry: with a per-entry clock read the
// entry judged last would be dropped from the view.
#[test]
fn iter_order_judges_every_entry_against_one_pass_start_snapshot() {
    let mut c: LruTtlCache<u32, SlowClone> = LruTtlCache::new(4, SNAPSHOT_TTL);
    c.cache_set(1, SlowClone { id: 1, slow: false });
    c.cache_set(
        2,
        SlowClone {
            id: 2,
            slow: true, // cloning entry 2 outlasts entry 1's ttl
        },
    );

    // MRU -> LRU is 2, 1: entry 1 is judged only after entry 2's slow clone.
    let ordered = c.iter_order();
    assert_eq!(
        ordered.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        vec![2, 1],
        "an entry live at the start of the pass must stay in the view for the whole pass"
    );
    assert_eq!(ordered[1].1.id, 1, "the value must travel with its key");
}

#[test]
fn value_order_judges_every_entry_against_one_pass_start_snapshot() {
    let mut c: LruTtlCache<u32, SlowClone> = LruTtlCache::new(4, SNAPSHOT_TTL);
    c.cache_set(1, SlowClone { id: 1, slow: false });
    c.cache_set(2, SlowClone { id: 2, slow: true });

    let vals = c.value_order();
    assert_eq!(
        vals.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![2, 1],
        "value_order must apply one pass-start snapshot to every entry"
    );
}

#[test]
fn key_order_judges_every_entry_against_one_pass_start_snapshot() {
    let mut c: LruTtlCache<SlowKey, u32> = LruTtlCache::new(4, SNAPSHOT_TTL);
    c.cache_set(SlowKey { id: 1, slow: false }, 10);
    c.cache_set(SlowKey { id: 2, slow: true }, 20);

    let keys = c.key_order();
    assert_eq!(
        keys.iter().map(|k| k.id).collect::<Vec<_>>(),
        vec![2, 1],
        "key_order must apply one pass-start snapshot to every entry"
    );
}

// The lazy `iter()` deliberately does the OPPOSITE: it reads the clock per item,
// so an entry that expires while the iterator is parked must not be yielded.
#[test]
fn lazy_iter_drops_entries_that_expire_mid_iteration() {
    let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(4, TTL);
    c.cache_set(1, 10);
    c.cache_set(2, 20);

    let mut it = CachedIter::iter(&c);
    assert_eq!(it.next(), Some((&2, &20)), "MRU first, still live");
    std::thread::sleep(PAST_TTL);
    assert_eq!(
        it.next(),
        None,
        "an entry that expires while the lazy iterator is parked must not be yielded"
    );
}

#[test]
fn lazy_iter_yields_every_live_entry() {
    let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(4, Duration::from_secs(60));
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);
    let seen: Vec<(u32, u32)> = CachedIter::iter(&c).map(|(k, v)| (*k, *v)).collect();
    assert_eq!(seen, vec![(3, 30), (2, 20), (1, 10)]);
}

// `retain` must not consult the predicate for already-expired entries: they are
// removed on the expiry test alone (and the predicate is a `FnMut` that may have
// side effects, so calling it extra times is observable).
#[test]
fn retain_never_consults_the_predicate_for_expired_entries() {
    let l = log();
    let mut c = logging_cache(8, TTL, l.clone());
    c.cache_set(1, 10); // doomed
    c.cache_set(2, 20); // doomed
    std::thread::sleep(PAST_TTL);
    c.cache_set(3, 30);
    c.cache_set(4, 40);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    c.retain(move |k, v| {
        seen2.lock().expect("predicate log poisoned").push((*k, *v));
        *k != 4
    });

    assert_eq!(
        seen.lock().expect("predicate log poisoned").clone(),
        vec![(4, 40), (3, 30)],
        "the predicate must see only the live entries, MRU -> LRU"
    );
    assert_eq!(c.key_order(), vec![3]);
    assert_eq!(
        drain(&l),
        vec![(4, 40), (2, 20), (1, 10)],
        "on_evict fires MRU -> LRU for both the predicate's rejects and the expired entries"
    );
    assert_eq!(
        c.cache_evictions(),
        Some(3),
        "2 expired + 1 predicate reject"
    );
}

// Zero-removed and all-removed sweeps: the pre-sized collectors must produce the
// same results at both extremes.
#[test]
fn evict_and_retain_at_the_zero_and_all_extremes() {
    let l = log();
    let mut c = logging_cache(8, Duration::from_secs(60), l.clone());
    for i in 1..=4u32 {
        c.cache_set(i, i * 10);
    }

    assert_eq!(c.evict(), 0, "nothing is expired");
    assert_eq!(drain(&l), Vec::new());
    assert_eq!(c.key_order(), vec![4, 3, 2, 1], "order untouched");
    assert_eq!(c.cache_evictions(), Some(0));

    c.retain(|_k, _v| true);
    assert_eq!(
        c.key_order(),
        vec![4, 3, 2, 1],
        "retain-all removes nothing"
    );
    assert_eq!(c.cache_evictions(), Some(0));

    c.retain(|_k, _v| false);
    assert_eq!(
        drain(&l),
        vec![(4, 40), (3, 30), (2, 20), (1, 10)],
        "retain-none removes everything, MRU -> LRU"
    );
    assert_eq!(c.cache_size(), 0);
    assert_eq!(c.cache_evictions(), Some(4));
    assert_eq!(c.evict(), 0, "an empty cache sweeps nothing");

    // Still usable, and the collectors agree on the empty cache.
    assert_eq!(c.key_order(), Vec::<u32>::new());
    assert!(c.iter_order().is_empty());
    assert!(c.value_order().is_empty());
    c.cache_set(9, 90);
    assert_eq!(c.cache_get(&9), Some(&90));
}

#[test]
fn evict_removes_every_entry_when_all_have_expired() {
    let l = log();
    let mut c = logging_cache(8, TTL, l.clone());
    for i in 1..=4u32 {
        c.cache_set(i, i * 10);
    }
    std::thread::sleep(PAST_TTL);

    // The collectors filter all of them out while they are still stored.
    assert_eq!(c.cache_size(), 4, "len is the RAW stored count");
    assert!(c.key_order().is_empty());
    assert!(c.iter_order().is_empty());
    assert!(c.value_order().is_empty());
    assert_eq!(CachedIter::iter(&c).count(), 0);

    assert_eq!(c.evict(), 4);
    assert_eq!(
        drain(&l),
        vec![(4, 40), (3, 30), (2, 20), (1, 10)],
        "one callback per swept entry, MRU -> LRU, with correct values"
    );
    let m = c.metrics();
    assert_eq!(m.entry_count, Some(0));
    assert_eq!(m.evictions, Some(4));
}

// `refresh_on_hit` with expiry disabled: `compute_expires_at` yields `None`, and
// the code falls back to the entry's EXISTING expiry. Dropping that fallback
// would silently make refreshed entries immortal.
#[test]
fn refresh_on_hit_with_a_disabled_ttl_keeps_the_original_expiry() {
    // One case per converted refresh site.
    #[allow(clippy::type_complexity)]
    let sites: Vec<(&str, Box<dyn Fn(&mut LruTtlCache<u32, u32>)>)> = vec![
        (
            "cache_get",
            Box::new(|c: &mut LruTtlCache<u32, u32>| {
                assert_eq!(c.cache_get(&1), Some(&10));
            }),
        ),
        (
            "cache_get_mut",
            Box::new(|c: &mut LruTtlCache<u32, u32>| {
                assert_eq!(c.cache_get_mut(&1), Some(&mut 10));
            }),
        ),
        (
            "cache_get_or_set_with",
            Box::new(|c: &mut LruTtlCache<u32, u32>| {
                assert_eq!(*c.cache_get_or_set_with(1, || 999), 10);
            }),
        ),
        (
            "cache_try_get_or_set_with",
            Box::new(|c: &mut LruTtlCache<u32, u32>| {
                assert_eq!(
                    c.cache_try_get_or_set_with(1, || Ok::<u32, ()>(999))
                        .copied(),
                    Ok(10)
                );
            }),
        ),
        (
            "cache_get_with_expiry_status",
            Box::new(|c: &mut LruTtlCache<u32, u32>| {
                assert_eq!(c.cache_get_with_expiry_status(&1), (Some(10), false));
            }),
        ),
    ];

    for (name, hit) in sites {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(TTL)
            .refresh_on_hit(true)
            .build()
            .expect("build refreshing LruTtlCache");
        c.cache_set(1, 10);
        // Disable expiry for FUTURE inserts; the stored entry keeps its deadline.
        assert_eq!(c.unset_ttl(), Some(TTL));

        hit(&mut c);

        std::thread::sleep(PAST_TTL);
        assert_eq!(
            c.cache_peek(&1),
            None,
            "{name}: a refresh under a disabled ttl must keep the original expiry, not make the entry immortal"
        );
    }
}

// The complement: with a live ttl, repeated hits keep pushing the deadline out,
// so the entry outlives an interval far longer than the ttl.
//
// This uses its own local ttl/interval, deliberately NOT the file-wide `TTL`
// (150ms): with a 100ms inter-hit sleep, `TTL` left only a ~50ms scheduling
// margin before an unrenewed entry would expire, so a >50ms stall under CI load
// flipped the "each hit must renew the ttl" assertion spuriously. The interval
// below leaves a much wider per-hit margin while the total time across all hits
// still comfortably exceeds the ttl, so an entry that silently stopped being
// renewed is still caught -- only the deliberately-past-expiry sleep at the end
// decides the final, idle-expiry assertion.
#[test]
fn refresh_on_hit_keeps_an_entry_alive_across_repeated_hits() {
    const RENEWAL_TTL: Duration = Duration::from_secs(1);
    const HIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);
    const IDLE_PAST_TTL: std::time::Duration = std::time::Duration::from_millis(1500);

    let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
        .max_size(4)
        .ttl(RENEWAL_TTL)
        .refresh_on_hit(true)
        .build()
        .expect("build refreshing LruTtlCache");
    c.cache_set(1, 10);
    // Six hits at 300ms apart: each single interval is well under the 1s ttl (a
    // large scheduling stall would have to eat ~700ms before it could flip a hit
    // to a spurious expiry), but the 1.8s total across all six hits comfortably
    // exceeds the ttl -- so an entry that is NOT refreshed on each hit would
    // already be gone well before the last iteration.
    for _ in 0..6 {
        std::thread::sleep(HIT_INTERVAL);
        assert_eq!(c.cache_get(&1), Some(&10), "each hit must renew the ttl");
    }
    std::thread::sleep(IDLE_PAST_TTL);
    assert_eq!(c.cache_get(&1), None, "and it must still expire once idle");
}

// `cache_set` judges the DISPLACED entry against the same clock sample that
// produced the new entry's expiry. The displaced-live and displaced-expired arms
// must keep their distinct return/accounting behaviour.
#[test]
fn cache_set_displacement_accounting_across_the_expiry_boundary() {
    let l = log();
    let mut c = logging_cache(4, TTL, l.clone());
    c.cache_set(1, 10);
    assert_eq!(
        c.cache_set(1, 11),
        Some(10),
        "displacing a live entry returns it"
    );
    assert_eq!(drain(&l), Vec::new(), "and fires no callback");
    assert_eq!(c.cache_evictions(), Some(0));

    std::thread::sleep(PAST_TTL);
    assert_eq!(
        c.cache_set(1, 12),
        None,
        "displacing an expired entry filters the old value"
    );
    assert_eq!(
        drain(&l),
        vec![(1, 11)],
        "and fires on_evict with the displaced value"
    );
    assert_eq!(c.cache_evictions(), Some(1));
    assert_eq!(c.cache_get(&1), Some(&12), "the new value is live");
}

// The `get_or_set` family replaces an expired entry: the callback must see the
// STORED key/value of the displaced entry, and the factory must run.
#[test]
fn get_or_set_family_replacing_an_expired_entry_fires_on_evict_once() {
    let l = log();
    let mut c = logging_cache(4, TTL, l.clone());
    c.cache_set(1, 10);
    std::thread::sleep(PAST_TTL);

    let ran = Arc::new(AtomicUsize::new(0));
    let ran2 = ran.clone();
    assert_eq!(
        *c.cache_get_or_set_with(1, move || {
            ran2.fetch_add(1, Ordering::Relaxed);
            111
        }),
        111
    );
    assert_eq!(ran.load(Ordering::Relaxed), 1, "the factory must run");
    assert_eq!(drain(&l), vec![(1, 10)], "the displaced entry is evicted");
    assert_eq!(c.cache_evictions(), Some(1));

    // Same for the fallible variant.
    c.cache_set(2, 20);
    std::thread::sleep(PAST_TTL);
    assert_eq!(
        c.cache_try_get_or_set_with(2, || Ok::<u32, ()>(222))
            .copied(),
        Ok(222)
    );
    assert_eq!(drain(&l), vec![(1, 10), (2, 20)]);

    // A failing factory over an expired entry: the store keeps whatever it had,
    // and the error propagates.
    c.cache_set(3, 30);
    std::thread::sleep(PAST_TTL);
    assert_eq!(
        c.cache_try_get_or_set_with(3, || Err::<u32, &str>("boom")),
        Err("boom")
    );
}

// =============================================================================
// Miss accounting on the fallible `get_or_set` error path.
//
// A lookup that did not find a live entry is a MISS regardless of whether the
// initializer then fails: the store was consulted, nothing usable was found, and
// the caller had to compute. Every other store in the crate counts it that way --
// `UnboundCache` (unbound.rs), `LruCache` (lru.rs), `TtlCache` (ttl.rs) and
// `ExpiringLruCache` (expiring_lru.rs) all increment `misses` BEFORE running the
// factory. `LruTtlCache` counted it after, so an `Err` factory silently lost the
// miss. Fixed by counting inside the setter, as `TtlCache` does.
//
// The eviction side is deliberately the opposite and must stay that way: on `Err`
// the expired entry is still physically stored, so `on_evict` must NOT fire and
// `evictions` must NOT move -- otherwise the next call that really does replace
// the entry double-fires for the same physical entry.
// =============================================================================

#[test]
fn try_get_or_set_with_counts_a_miss_when_the_factory_fails_on_an_absent_key() {
    let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(4, Duration::from_secs(60));

    assert_eq!(
        c.cache_try_get_or_set_with(1, || Err::<u32, &str>("boom")),
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "an absent-key lookup is a miss even when the factory fails"
    );
    assert_eq!(c.cache_hits(), Some(0));
    assert_eq!(c.cache_size(), 0, "a failed factory must store nothing");
    assert_eq!(c.cache_evictions(), Some(0));

    // The retry that succeeds counts a second, separate miss.
    assert_eq!(
        c.cache_try_get_or_set_with(1, || Ok::<u32, &str>(10))
            .copied(),
        Ok(10)
    );
    assert_eq!(c.cache_misses(), Some(2));
    assert_eq!(c.cache_hits(), Some(0));
}

#[test]
fn try_get_or_set_with_counts_a_miss_when_the_factory_fails_over_an_expired_entry() {
    let l = log();
    let mut c = logging_cache(4, TTL, l.clone());
    c.cache_set(1, 10);
    std::thread::sleep(PAST_TTL);
    c.cache_reset_metrics();

    assert_eq!(
        c.cache_try_get_or_set_with(1, || Err::<u32, &str>("boom")),
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "finding only an expired entry is a miss even when the factory fails"
    );
    assert_eq!(c.cache_hits(), Some(0));
    // ... but the entry is still physically stored, so nothing was evicted yet.
    assert_eq!(
        drain(&l),
        Vec::new(),
        "on_evict must not fire while the expired entry is still stored"
    );
    assert_eq!(c.cache_evictions(), Some(0));
    assert_eq!(c.cache_size(), 1, "the expired entry is left in place");

    // The later call that really replaces it fires on_evict exactly once, for the
    // one physical entry: no double-fire from the earlier failure.
    assert_eq!(
        c.cache_try_get_or_set_with(1, || Ok::<u32, &str>(11))
            .copied(),
        Ok(11)
    );
    assert_eq!(drain(&l), vec![(1, 10)], "exactly one eviction, once");
    assert_eq!(c.cache_evictions(), Some(1));
    assert_eq!(c.cache_misses(), Some(2), "the replacement is a miss too");
}

#[test]
fn try_get_or_set_with_counts_a_hit_and_skips_the_factory_on_a_live_entry() {
    // The complement: a live entry must not be turned into a miss by the fix.
    let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(4, Duration::from_secs(60));
    c.cache_set(1, 10);
    c.cache_reset_metrics();

    let ran = Arc::new(AtomicUsize::new(0));
    let ran2 = ran.clone();
    assert_eq!(
        c.cache_try_get_or_set_with(1, move || {
            ran2.fetch_add(1, Ordering::Relaxed);
            Err::<u32, &str>("boom")
        })
        .copied(),
        Ok(10)
    );
    assert_eq!(ran.load(Ordering::Relaxed), 0, "the factory must not run");
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(0));
}

// The infallible sibling must keep counting exactly one miss per replacement, so
// moving the counter into the setter cannot double-count.
#[test]
fn get_or_set_with_counts_exactly_one_miss_per_replacement() {
    let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(4, TTL);
    assert_eq!(*c.cache_get_or_set_with(1, || 10), 10);
    assert_eq!(c.cache_misses(), Some(1), "absent key: one miss");
    assert_eq!(*c.cache_get_or_set_with(1, || 99), 10);
    assert_eq!(c.cache_misses(), Some(1), "live hit: no new miss");
    assert_eq!(c.cache_hits(), Some(1));

    std::thread::sleep(PAST_TTL);
    assert_eq!(*c.cache_get_or_set_with(1, || 11), 11);
    assert_eq!(c.cache_misses(), Some(2), "expired replacement: one miss");
    assert_eq!(c.cache_hits(), Some(1));
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_try_get_or_set_with_counts_a_miss_when_the_factory_fails() {
    use cached::CachedGetOrSetAsync;

    let l = log();
    let mut c = logging_cache(4, TTL, l.clone());

    // Absent key.
    assert_eq!(
        CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 1, || async {
            Err::<u32, &str>("boom")
        })
        .await,
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "an absent-key lookup is a miss even when the async factory fails"
    );
    assert_eq!(c.cache_size(), 0);

    // Expired entry.
    c.cache_set(2, 20);
    std::thread::sleep(PAST_TTL);
    c.cache_reset_metrics();
    assert_eq!(
        CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 2, || async {
            Err::<u32, &str>("boom")
        })
        .await,
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "finding only an expired entry is a miss even when the async factory fails"
    );
    assert_eq!(
        drain(&l),
        Vec::new(),
        "the expired entry is still stored, so nothing was evicted"
    );
    assert_eq!(c.cache_evictions(), Some(0));

    // A live entry is still a hit, and the factory does not run.
    c.cache_set(3, 30);
    c.cache_reset_metrics();
    let v = CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 3, || async {
        Err::<u32, &str>("boom")
    })
    .await;
    assert_eq!(v.copied(), Ok(30));
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(0));
}

// =============================================================================
// The `_mut` methods directly (EXP-2 pin).
//
// The tests above drive the contract through the shared-reference wrapper
// (`cache_try_get_or_set_with` / `async_cache_try_get_or_set_with`), which is a
// provided default that delegates straight to the `_mut` method
// (`Cached::cache_try_get_or_set_with`, `CachedGetOrSetAsync::async_cache_try_get_or_set_with`
// in src/lib.rs). `LruTtlCache` itself only implements the `_mut` variants
// (src/stores/lru_ttl.rs:778 sync, :1118 async), so calling the `_mut` methods
// below exercises the exact code the store owns, with no indirection through a
// default method that a future change could alter independently. Same
// contract, named explicitly: on `Err`, exactly one miss, no `on_evict`, no
// eviction, and no entry inserted -- for both an absent key and an expired one.
// =============================================================================

#[test]
fn try_get_or_set_with_mut_counts_a_miss_and_inserts_nothing_when_the_factory_fails_on_an_absent_key()
 {
    let l = log();
    let mut c = logging_cache(4, Duration::from_secs(60), l.clone());

    assert_eq!(
        c.cache_try_get_or_set_with_mut(1, || Err::<u32, &str>("boom")),
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "an absent-key lookup through cache_try_get_or_set_with_mut is a miss even when the factory fails"
    );
    assert_eq!(c.cache_hits(), Some(0));
    assert_eq!(c.cache_size(), 0, "a failed factory must insert nothing");
    assert_eq!(drain(&l), Vec::new(), "on_evict must not fire");
    assert_eq!(c.cache_evictions(), Some(0));

    // A second failing call over the same still-absent key counts a second,
    // separate miss: the first failure must not have left anything behind.
    assert_eq!(
        c.cache_try_get_or_set_with_mut(1, || Err::<u32, &str>("boom again")),
        Err("boom again")
    );
    assert_eq!(c.cache_misses(), Some(2));
    assert_eq!(c.cache_size(), 0);
}

#[test]
fn try_get_or_set_with_mut_counts_a_miss_and_inserts_nothing_when_the_factory_fails_over_an_expired_entry()
 {
    let l = log();
    let mut c = logging_cache(4, TTL, l.clone());
    c.cache_set(1, 10);
    std::thread::sleep(PAST_TTL);
    c.cache_reset_metrics();

    assert_eq!(
        c.cache_try_get_or_set_with_mut(1, || Err::<u32, &str>("boom")),
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "finding only an expired entry through cache_try_get_or_set_with_mut is a miss even when the factory fails"
    );
    assert_eq!(c.cache_hits(), Some(0));
    assert_eq!(
        drain(&l),
        Vec::new(),
        "on_evict must not fire while the expired entry is still stored"
    );
    assert_eq!(c.cache_evictions(), Some(0));
    assert_eq!(
        c.cache_size(),
        1,
        "the expired entry is left in place, not replaced by a failed factory"
    );

    // A later call that really succeeds fires on_evict exactly once, for the one
    // physical entry: no double-fire from the earlier failure.
    assert_eq!(
        c.cache_try_get_or_set_with_mut(1, || Ok::<u32, &str>(11)),
        Ok(&mut 11)
    );
    assert_eq!(drain(&l), vec![(1, 10)], "exactly one eviction, once");
    assert_eq!(c.cache_evictions(), Some(1));
    assert_eq!(c.cache_misses(), Some(2), "the replacement is a miss too");
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_try_get_or_set_with_mut_counts_a_miss_and_inserts_nothing_when_the_factory_fails() {
    use cached::CachedGetOrSetAsync;

    let l = log();
    let mut c = logging_cache(4, TTL, l.clone());

    // Absent key.
    assert_eq!(
        CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 1, || async {
            Err::<u32, &str>("boom")
        })
        .await,
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "an absent-key lookup through async_cache_try_get_or_set_with_mut is a miss even when the factory fails"
    );
    assert_eq!(c.cache_size(), 0, "a failed factory must insert nothing");
    assert_eq!(drain(&l), Vec::new());
    assert_eq!(c.cache_evictions(), Some(0));

    // Expired entry.
    c.cache_set(2, 20);
    std::thread::sleep(PAST_TTL);
    c.cache_reset_metrics();
    assert_eq!(
        CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 2, || async {
            Err::<u32, &str>("boom")
        })
        .await,
        Err("boom")
    );
    assert_eq!(
        c.cache_misses(),
        Some(1),
        "finding only an expired entry through async_cache_try_get_or_set_with_mut is a miss even when the factory fails"
    );
    assert_eq!(
        drain(&l),
        Vec::new(),
        "the expired entry is still stored, so on_evict must not fire"
    );
    assert_eq!(c.cache_evictions(), Some(0));
    assert_eq!(
        c.cache_size(),
        1,
        "only the expired key-2 entry is stored; key 1's absent-key attempt inserted nothing"
    );

    // A live entry is still a hit, and the factory does not run.
    c.cache_set(3, 30);
    c.cache_reset_metrics();
    let v = CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 3, || async {
        Err::<u32, &str>("boom")
    })
    .await;
    assert_eq!(v.copied(), Ok(30));
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(0));

    // The later call that really replaces the expired key-2 entry fires on_evict
    // exactly once for it: no double-fire from the earlier failed attempt.
    let v2 = CachedGetOrSetAsync::async_cache_try_get_or_set_with_mut(&mut c, 2, || async {
        Ok::<u32, &str>(22)
    })
    .await;
    assert_eq!(v2.copied(), Ok(22));
    assert_eq!(drain(&l), vec![(2, 20)], "exactly one eviction, once");
    assert_eq!(c.cache_evictions(), Some(1));
}
