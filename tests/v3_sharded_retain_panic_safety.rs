//! A panicking user predicate must never make entries vanish silently from a sharded store.
//!
//! `retain` (all six stores) runs the caller's `keep`, and `evict` on the two `Expires`-driven
//! stores runs the caller's `is_expired`. Both used to drive `HashMap::extract_if` (or an
//! in-place `HashMap::retain` plus a `before - guard.len()` delta), which removes eagerly: an
//! unwind out of the predicate dropped every entry already yielded -- gone from the cache, never
//! handed to `on_evict`, never counted in `metrics().evictions`. The measured leak was 4 of 20
//! entries with `on_evict_fired == 0` and `evictions == 0`.
//!
//! The invariant asserted here is the one the LRU-backed sharded stores already satisfied:
//! entries lost == `on_evict` fires == the `evictions` delta. Because the predicate now runs in
//! a pass that only *selects*, a panic means no entry has been removed yet, so all three are
//! zero and the cache is intact.

use cached::{
    ConcurrentCacheBase, Expires, ShardedExpiringCache, ShardedExpiringLruCache, ShardedLruCache,
    ShardedUnboundCache,
};

#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const ENTRIES: usize = 20;
/// The predicate rejects this many entries and then panics on the next call.
const REJECT_BEFORE_PANIC: usize = 4;

/// Value that never expires, so only `keep` decides.
#[derive(Clone, Debug, PartialEq)]
struct Live(u32);

impl Expires for Live {
    fn is_expired(&self) -> bool {
        false
    }
}

/// Reports "expired" for the first [`REJECT_BEFORE_PANIC`] calls, then panics. Models a
/// user `Expires` impl that blows up part-way through an `evict` sweep.
#[derive(Clone, Debug)]
struct Bomb {
    calls: Arc<AtomicUsize>,
}

impl PartialEq for Bomb {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Expires for Bomb {
    fn is_expired(&self) -> bool {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= REJECT_BEFORE_PANIC {
            panic!("is_expired blew up");
        }
        true
    }
}

/// A `keep` that rejects the first [`REJECT_BEFORE_PANIC`] entries, then panics.
fn bomb_predicate<K, V>(calls: Arc<AtomicUsize>) -> impl FnMut(&K, &V) -> bool {
    move |_k: &K, _v: &V| {
        if calls.fetch_add(1, Ordering::SeqCst) >= REJECT_BEFORE_PANIC {
            panic!("keep blew up");
        }
        false
    }
}

struct Probe {
    fired: Arc<AtomicUsize>,
}

impl Probe {
    fn new() -> Self {
        Self {
            fired: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn sink(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.fired)
    }
    fn count(&self) -> usize {
        self.fired.load(Ordering::SeqCst)
    }
}

/// Assert the three-way invariant after a panicking sweep.
fn assert_consistent(label: &str, before: usize, after: usize, fired: usize, evictions_delta: u64) {
    let lost = before - after;
    assert_eq!(
        lost, fired,
        "{label}: {lost} entries left the cache but on_evict fired {fired} times"
    );
    assert_eq!(
        lost as u64, evictions_delta,
        "{label}: {lost} entries left the cache but evictions moved by {evictions_delta}"
    );
    assert_eq!(
        lost, 0,
        "{label}: the selection pass removes nothing, so a panicking predicate must lose no entry"
    );
}

fn evictions_of<C: ConcurrentCacheBase>(c: &C) -> u64 {
    c.cache_evictions().unwrap_or(0)
}

// --- retain -------------------------------------------------------------------------------

#[test]
fn sharded_unbound_retain_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedUnboundCache::<u32, u32>::builder().shards(1);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &u32| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        for i in 0..ENTRIES as u32 {
            c.set(i, i);
        }
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let calls = Arc::new(AtomicUsize::new(0));
        let result = catch_unwind(AssertUnwindSafe(|| c.retain(bomb_predicate(calls))));
        assert!(result.is_err(), "the predicate must have panicked");
        assert_consistent(
            &format!("ShardedUnboundCache::retain(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

#[test]
fn sharded_lru_retain_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedLruCache::<u32, u32>::builder()
            .shards(1)
            .max_size(1024);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &u32| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        for i in 0..ENTRIES as u32 {
            c.set(i, i);
        }
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let calls = Arc::new(AtomicUsize::new(0));
        let result = catch_unwind(AssertUnwindSafe(|| c.retain(bomb_predicate(calls))));
        assert!(result.is_err(), "the predicate must have panicked");
        assert_consistent(
            &format!("ShardedLruCache::retain(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedTtlCache::<u32, u32>::builder()
            .shards(1)
            .ttl_secs(600);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &u32| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        for i in 0..ENTRIES as u32 {
            c.set(i, i);
        }
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let calls = Arc::new(AtomicUsize::new(0));
        let result = catch_unwind(AssertUnwindSafe(|| c.retain(bomb_predicate(calls))));
        assert!(result.is_err(), "the predicate must have panicked");
        assert_consistent(
            &format!("ShardedTtlCache::retain(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_lru_ttl_retain_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let b = ShardedLruTtlCache::<u32, u32>::builder()
            .shards(1)
            .max_size(1024)
            .ttl_secs(600);
        let c = if with_on_evict {
            b.on_evict(move |_: &u32, _: &u32| {
                sink.fetch_add(1, Ordering::SeqCst);
            })
            .build()
            .unwrap()
        } else {
            b.build().unwrap()
        };
        for i in 0..ENTRIES as u32 {
            c.set(i, i);
        }
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let calls = Arc::new(AtomicUsize::new(0));
        let result = catch_unwind(AssertUnwindSafe(|| c.retain(bomb_predicate(calls))));
        assert!(result.is_err(), "the predicate must have panicked");
        assert_consistent(
            &format!("ShardedLruTtlCache::retain(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

#[test]
fn sharded_expiring_retain_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedExpiringCache::<u32, Live>::builder().shards(1);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &Live| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        for i in 0..ENTRIES as u32 {
            c.set(i, Live(i));
        }
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let calls = Arc::new(AtomicUsize::new(0));
        let result = catch_unwind(AssertUnwindSafe(|| c.retain(bomb_predicate(calls))));
        assert!(result.is_err(), "the predicate must have panicked");
        assert_consistent(
            &format!("ShardedExpiringCache::retain(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

#[test]
fn sharded_expiring_lru_retain_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedExpiringLruCache::<u32, Live>::builder()
            .shards(1)
            .max_size(1024);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &Live| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        for i in 0..ENTRIES as u32 {
            c.set(i, Live(i));
        }
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let calls = Arc::new(AtomicUsize::new(0));
        let result = catch_unwind(AssertUnwindSafe(|| c.retain(bomb_predicate(calls))));
        assert!(result.is_err(), "the predicate must have panicked");
        assert_consistent(
            &format!("ShardedExpiringLruCache::retain(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

// --- evict, through a panicking `Expires::is_expired` --------------------------------------

#[test]
fn sharded_expiring_evict_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedExpiringCache::<u32, Bomb>::builder().shards(1);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &Bomb| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        for i in 0..ENTRIES as u32 {
            // Inserting a fresh key never consults `is_expired`, so the fuse is untouched here.
            c.set(
                i,
                Bomb {
                    calls: Arc::clone(&calls),
                },
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "setup must not arm the fuse"
        );
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let result = catch_unwind(AssertUnwindSafe(|| c.evict()));
        assert!(result.is_err(), "is_expired must have panicked");
        assert_consistent(
            &format!("ShardedExpiringCache::evict(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

#[test]
fn sharded_expiring_lru_evict_panic_loses_nothing() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedExpiringLruCache::<u32, Bomb>::builder()
            .shards(1)
            .max_size(1024);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &Bomb| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        for i in 0..ENTRIES as u32 {
            c.set(
                i,
                Bomb {
                    calls: Arc::clone(&calls),
                },
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "setup must not arm the fuse"
        );
        let before = c.len();
        let before_evictions = evictions_of(&c);
        let result = catch_unwind(AssertUnwindSafe(|| c.evict()));
        assert!(result.is_err(), "is_expired must have panicked");
        assert_consistent(
            &format!("ShardedExpiringLruCache::evict(with_on_evict={with_on_evict})"),
            before,
            c.len(),
            probe.count(),
            evictions_of(&c) - before_evictions,
        );
    }
}

// --- the non-panicking path must be unchanged ----------------------------------------------

#[test]
fn retain_without_a_panic_still_removes_counts_and_notifies() {
    for with_on_evict in [false, true] {
        let probe = Probe::new();
        let sink = probe.sink();
        let mut b = ShardedExpiringCache::<u32, Live>::builder().shards(4);
        if with_on_evict {
            b = b.on_evict(move |_: &u32, _: &Live| {
                sink.fetch_add(1, Ordering::SeqCst);
            });
        }
        let c = b.build().unwrap();
        for i in 0..64u32 {
            c.set(i, Live(i));
        }
        let removed = c.retain(|k, _v| k % 2 == 0);
        assert_eq!(removed, 32, "with_on_evict={with_on_evict}");
        assert_eq!(c.len(), 32, "with_on_evict={with_on_evict}");
        assert_eq!(
            evictions_of(&c),
            32,
            "the eviction count must not depend on whether a callback is attached"
        );
        assert_eq!(
            probe.count(),
            if with_on_evict { 32 } else { 0 },
            "with_on_evict={with_on_evict}"
        );
    }
}
