//! Panic-safety and eviction-accounting certification for the four single-owner
//! in-memory expiry stores: `TtlCache`, `LruTtlCache`, `ExpiringCache`, `ExpiringLruCache`.
//!
//! The invariants under test, for every sweep / removal path:
//!
//! 1. **A panicking sweep predicate removes nothing.** The selection pass must be
//!    side-effect free, so a `retain` predicate that unwinds leaves the cache exactly as
//!    it was: no entry removed, no eviction counted, no `on_evict` fired. The old code
//!    fired `on_evict` from inside the predicate, so entries whose cleanup hook had
//!    already run (releasing an FD, deleting a temp file, closing a connection) were
//!    still served afterwards by `cache_get` / `iter()`.
//! 2. **A panicking `on_evict` never leaves an entry counted-but-present.** Everything
//!    the sweep selected is out of the store and counted *before* the first notification,
//!    so `removed == evictions delta` always, and a key the callback fired for is never
//!    served again. Retrying the same operation cannot double-count one physical entry.
//! 3. **The eviction count does not depend on whether a callback exists.**
//!    `cache_clear_with_on_evict` counts identically with a no-op callback and with none.
//! 4. **Expiry is judged once, at removal time.** A slow `on_evict` must not make
//!    `cache_remove` report `None` for a value that was live when it was taken out.
//! 5. **Refresh-on-hit under an overflowing TTL never expires**, matching a fresh insert,
//!    while a zero (disabled) TTL still leaves an existing deadline untouched.
//!
//! Caught panics print to stderr; that is expected noise, not a failure.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;

use cached::time::{Duration, Instant};
#[cfg(feature = "time_stores")]
use cached::{CacheTtl, LruTtlCache, TtlCache};
use cached::{Cached, CachedIter, CachedPeek, Expires, ExpiringCache, ExpiringLruCache};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Records every key `on_evict` fires for, so tests can assert the fire count and
/// check that nothing fired for is still reachable.
#[derive(Default)]
struct Fired(Mutex<Vec<u32>>);

impl Fired {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn push(&self, k: u32) {
        self.0.lock().expect("fired log poisoned").push(k);
    }
    fn keys(&self) -> Vec<u32> {
        self.0.lock().expect("fired log poisoned").clone()
    }
    fn count(&self) -> usize {
        self.0.lock().expect("fired log poisoned").len()
    }
}

/// Value with explicitly controlled expiry, so a test decides exactly which entries a
/// sweep treats as stale.
#[derive(Clone)]
struct Flagged {
    id: u32,
    expired: bool,
}

impl Expires for Flagged {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

/// Value with a real wall-clock deadline, for the "expiry is sampled at removal time"
/// tests: a slow callback must not be able to push it past the deadline.
struct Deadline {
    id: u32,
    at: Instant,
}

impl Expires for Deadline {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.at
    }
}

/// A `retain` predicate that dooms the first entry it sees and then panics on the
/// second. Order-independent, so it works for hash-ordered and LRU-ordered stores alike.
fn doom_then_panic(seen: &AtomicUsize) -> bool {
    if seen.fetch_add(1, Ordering::Relaxed) == 1 {
        panic!("retain predicate boom");
    }
    false
}

#[cfg(feature = "time_stores")]
fn sorted_keys<'a, I: Iterator<Item = (&'a u32, &'a u32)>>(it: I) -> Vec<u32> {
    let mut ks: Vec<u32> = it.map(|(k, _)| *k).collect();
    ks.sort_unstable();
    ks
}

// ===========================================================================
// 1. A panicking `retain` predicate must remove nothing and fire nothing
// ===========================================================================

#[cfg(feature = "time_stores")]
#[test]
fn ttl_retain_panicking_predicate_leaves_cache_untouched() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = TtlCache::builder()
        .ttl(Duration::from_secs(60))
        .on_evict(move |k: &u32, _v: &u32| f.push(*k))
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);

    let seen = AtomicUsize::new(0);
    let r = catch_unwind(AssertUnwindSafe(|| {
        c.retain(|_k, _v| doom_then_panic(&seen))
    }));

    assert!(r.is_err(), "the predicate must have panicked");
    assert_eq!(c.cache_size(), 3, "no entry may be removed");
    assert_eq!(c.cache_evictions(), Some(0), "nothing may be counted");
    assert_eq!(fired.count(), 0, "on_evict must not fire during selection");
    assert_eq!(sorted_keys(c.iter()), vec![1, 2, 3]);
    assert_eq!(c.cache_get(&1), Some(&10));
    assert_eq!(c.cache_get(&2), Some(&20));
    assert_eq!(c.cache_get(&3), Some(&30));
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_retain_panicking_predicate_leaves_cache_untouched() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .on_evict(move |k: &u32, _v: &u32| f.push(*k))
        .build()
        .expect("builder has max_size and a non-zero ttl");
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);

    let seen = AtomicUsize::new(0);
    let r = catch_unwind(AssertUnwindSafe(|| {
        c.retain(|_k, _v| doom_then_panic(&seen))
    }));

    assert!(r.is_err(), "the predicate must have panicked");
    assert_eq!(c.cache_size(), 3, "no entry may be removed");
    assert_eq!(c.cache_evictions(), Some(0), "nothing may be counted");
    assert_eq!(fired.count(), 0, "on_evict must not fire during selection");
    assert_eq!(sorted_keys(c.iter()), vec![1, 2, 3]);
    assert_eq!(c.cache_get(&1), Some(&10));
    assert_eq!(c.cache_get(&2), Some(&20));
    assert_eq!(c.cache_get(&3), Some(&30));
}

#[test]
fn expiring_retain_panicking_predicate_leaves_cache_untouched() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringCache::<u32, Flagged>::builder()
        .on_evict(move |k: &u32, _v: &Flagged| f.push(*k))
        .build()
        .expect("ExpiringCache build is infallible");
    for id in 1..=3u32 {
        c.cache_set(
            id,
            Flagged {
                id: id * 10,
                expired: false,
            },
        );
    }

    let seen = AtomicUsize::new(0);
    let r = catch_unwind(AssertUnwindSafe(|| {
        c.retain(|_k, _v| doom_then_panic(&seen))
    }));

    assert!(r.is_err(), "the predicate must have panicked");
    assert_eq!(c.cache_size(), 3, "no entry may be removed");
    assert_eq!(c.cache_evictions(), Some(0), "nothing may be counted");
    assert_eq!(fired.count(), 0, "on_evict must not fire during selection");
    let mut ks: Vec<u32> = c.iter().map(|(k, _)| *k).collect();
    ks.sort_unstable();
    assert_eq!(ks, vec![1, 2, 3]);
    for id in 1..=3u32 {
        assert_eq!(
            c.cache_get(&id).map(|v| v.id),
            Some(id * 10),
            "key {id} must still be served"
        );
    }
}

#[test]
fn expiring_lru_retain_panicking_predicate_leaves_cache_untouched() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringLruCache::<u32, Flagged>::builder()
        .max_size(8)
        .on_evict(move |k: &u32, _v: &Flagged| f.push(*k))
        .build()
        .expect("builder has a non-zero max_size");
    for id in 1..=3u32 {
        c.cache_set(
            id,
            Flagged {
                id: id * 10,
                expired: false,
            },
        );
    }

    let seen = AtomicUsize::new(0);
    let r = catch_unwind(AssertUnwindSafe(|| {
        c.retain(|_k, _v| doom_then_panic(&seen))
    }));

    assert!(r.is_err(), "the predicate must have panicked");
    assert_eq!(c.cache_size(), 3, "no entry may be removed");
    assert_eq!(c.cache_evictions(), Some(0), "nothing may be counted");
    let mut ks: Vec<u32> = c.iter().map(|(k, _)| *k).collect();
    ks.sort_unstable();
    assert_eq!(ks, vec![1, 2, 3]);
    for id in 1..=3u32 {
        assert_eq!(
            c.cache_get(&id).map(|v| v.id),
            Some(id * 10),
            "key {id} must still be served"
        );
    }
}

// ===========================================================================
// 2. A panicking `on_evict` must still have removed the entry it fired for
//    (removed == evictions delta == fire count for a single doomed entry)
// ===========================================================================

#[cfg(feature = "time_stores")]
#[test]
fn ttl_retain_panicking_callback_removes_the_entry_it_fired_for() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = TtlCache::builder()
        .ttl(Duration::from_secs(60))
        .on_evict(move |k: &u32, _v: &u32| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);

    let r = catch_unwind(AssertUnwindSafe(|| c.retain(|k, _v| *k != 2)));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(fired.keys(), vec![2], "on_evict fired for exactly key 2");
    assert_eq!(c.cache_evictions(), Some(1), "one removal, one eviction");
    assert_eq!(c.cache_size(), 2, "the doomed entry must be gone");
    assert_eq!(
        c.cache_get(&2),
        None,
        "a key whose on_evict ran must never be served again"
    );
    assert_eq!(sorted_keys(c.iter()), vec![1, 3]);
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_retain_panicking_callback_removes_the_entry_it_fired_for() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .on_evict(move |k: &u32, _v: &u32| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has max_size and a non-zero ttl");
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);

    let r = catch_unwind(AssertUnwindSafe(|| c.retain(|k, _v| *k != 2)));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(fired.keys(), vec![2], "on_evict fired for exactly key 2");
    assert_eq!(c.cache_evictions(), Some(1), "one removal, one eviction");
    assert_eq!(c.cache_size(), 2, "the doomed entry must be gone");
    assert_eq!(
        c.cache_get(&2),
        None,
        "a key whose on_evict ran must never be served again"
    );
    assert_eq!(sorted_keys(c.iter()), vec![1, 3]);
}

#[test]
fn expiring_retain_panicking_callback_removes_the_entry_it_fired_for() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringCache::<u32, Flagged>::builder()
        .on_evict(move |k: &u32, _v: &Flagged| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("ExpiringCache build is infallible");
    for id in 1..=3u32 {
        c.cache_set(
            id,
            Flagged {
                id: id * 10,
                expired: false,
            },
        );
    }

    let r = catch_unwind(AssertUnwindSafe(|| c.retain(|k, _v| *k != 2)));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(fired.keys(), vec![2], "on_evict fired for exactly key 2");
    assert_eq!(c.cache_evictions(), Some(1), "one removal, one eviction");
    assert_eq!(c.cache_size(), 2, "the doomed entry must be gone");
    assert!(
        c.cache_get(&2).is_none(),
        "a key whose on_evict ran must never be served again"
    );
}

#[test]
fn expiring_lru_retain_panicking_callback_removes_the_entry_it_fired_for() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringLruCache::<u32, Flagged>::builder()
        .max_size(8)
        .on_evict(move |k: &u32, _v: &Flagged| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has a non-zero max_size");
    for id in 1..=3u32 {
        c.cache_set(
            id,
            Flagged {
                id: id * 10,
                expired: false,
            },
        );
    }

    let r = catch_unwind(AssertUnwindSafe(|| c.retain(|k, _v| *k != 2)));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(fired.keys(), vec![2], "on_evict fired for exactly key 2");
    assert_eq!(c.cache_evictions(), Some(1), "one removal, one eviction");
    assert_eq!(c.cache_size(), 2, "the doomed entry must be gone");
    assert!(
        c.cache_get(&2).is_none(),
        "a key whose on_evict ran must never be served again"
    );
}

// ===========================================================================
// 3. `evict` with a panicking callback: every selected entry is out and counted
// ===========================================================================

#[cfg(feature = "time_stores")]
#[test]
fn ttl_evict_panicking_callback_still_removes_and_counts_every_entry() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = TtlCache::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |k: &u32, _v: &u32| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);
    sleep(Duration::from_millis(60));

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c.evict();
    }));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(c.cache_size(), 0, "every expired entry must be removed");
    assert_eq!(
        c.cache_evictions(),
        Some(3),
        "evictions must equal the number removed"
    );
    assert_eq!(fired.count(), 1, "the callback panicked on its first call");
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_evict_panicking_callback_still_removes_and_counts_every_entry() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(20))
        .on_evict(move |k: &u32, _v: &u32| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has max_size and a non-zero ttl");
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    c.cache_set(3, 30);
    sleep(Duration::from_millis(60));

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c.evict();
    }));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(c.cache_size(), 0, "every expired entry must be removed");
    assert_eq!(
        c.cache_evictions(),
        Some(3),
        "evictions must equal the number removed"
    );
    assert_eq!(fired.count(), 1, "the callback panicked on its first call");
}

#[test]
fn expiring_evict_panicking_callback_still_removes_and_counts_every_entry() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringCache::<u32, Flagged>::builder()
        .on_evict(move |k: &u32, _v: &Flagged| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("ExpiringCache build is infallible");
    for id in 1..=3u32 {
        c.cache_set(
            id,
            Flagged {
                id: id * 10,
                expired: true,
            },
        );
    }

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c.evict();
    }));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(c.cache_size(), 0, "every expired entry must be removed");
    assert_eq!(
        c.cache_evictions(),
        Some(3),
        "evictions must equal the number removed"
    );
    assert_eq!(fired.count(), 1, "the callback panicked on its first call");
}

#[test]
fn expiring_lru_evict_panicking_callback_still_removes_and_counts_every_entry() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringLruCache::<u32, Flagged>::builder()
        .max_size(8)
        .on_evict(move |k: &u32, _v: &Flagged| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has a non-zero max_size");
    for id in 1..=3u32 {
        c.cache_set(
            id,
            Flagged {
                id: id * 10,
                expired: true,
            },
        );
    }

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c.evict();
    }));

    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(c.cache_size(), 0, "every expired entry must be removed");
    assert_eq!(
        c.cache_evictions(),
        Some(3),
        "evictions must equal the number removed"
    );
    assert_eq!(fired.count(), 1, "the callback panicked on its first call");
}

// ===========================================================================
// 4. `cache_clear_with_on_evict` counts the same with and without a callback
// ===========================================================================

#[cfg(feature = "time_stores")]
#[test]
fn ttl_clear_with_on_evict_counts_independently_of_the_callback() {
    let mut with_cb = TtlCache::builder()
        .ttl(Duration::from_secs(60))
        .on_evict(|_k: &u32, _v: &u32| {})
        .build()
        .expect("builder has a non-zero ttl");
    let mut without_cb: TtlCache<u32, u32> = TtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("builder has a non-zero ttl");
    for c in [&mut with_cb, &mut without_cb] {
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
    }
    assert_eq!(with_cb.cache_evictions(), Some(2));
    assert_eq!(
        without_cb.cache_evictions(),
        with_cb.cache_evictions(),
        "the eviction count must not depend on a callback being configured"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_clear_with_on_evict_counts_independently_of_the_callback() {
    let mut with_cb = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .on_evict(|_k: &u32, _v: &u32| {})
        .build()
        .expect("builder has max_size and a non-zero ttl");
    let mut without_cb: LruTtlCache<u32, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("builder has max_size and a non-zero ttl");
    for c in [&mut with_cb, &mut without_cb] {
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
    }
    assert_eq!(with_cb.cache_evictions(), Some(2));
    assert_eq!(
        without_cb.cache_evictions(),
        with_cb.cache_evictions(),
        "the eviction count must not depend on a callback being configured"
    );
}

#[test]
fn expiring_clear_with_on_evict_counts_independently_of_the_callback() {
    let mut with_cb = ExpiringCache::<u32, Flagged>::builder()
        .on_evict(|_k: &u32, _v: &Flagged| {})
        .build()
        .expect("ExpiringCache build is infallible");
    let mut without_cb = ExpiringCache::<u32, Flagged>::builder()
        .build()
        .expect("ExpiringCache build is infallible");
    for c in [&mut with_cb, &mut without_cb] {
        c.cache_set(
            1,
            Flagged {
                id: 10,
                expired: false,
            },
        );
        c.cache_set(
            2,
            Flagged {
                id: 20,
                expired: false,
            },
        );
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
    }
    assert_eq!(with_cb.cache_evictions(), Some(2));
    assert_eq!(
        without_cb.cache_evictions(),
        with_cb.cache_evictions(),
        "the eviction count must not depend on a callback being configured"
    );
}

#[test]
fn expiring_lru_clear_with_on_evict_counts_independently_of_the_callback() {
    let mut with_cb = ExpiringLruCache::<u32, Flagged>::builder()
        .max_size(8)
        .on_evict(|_k: &u32, _v: &Flagged| {})
        .build()
        .expect("builder has a non-zero max_size");
    let mut without_cb = ExpiringLruCache::<u32, Flagged>::builder()
        .max_size(8)
        .build()
        .expect("builder has a non-zero max_size");
    for c in [&mut with_cb, &mut without_cb] {
        c.cache_set(
            1,
            Flagged {
                id: 10,
                expired: false,
            },
        );
        c.cache_set(
            2,
            Flagged {
                id: 20,
                expired: false,
            },
        );
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
    }
    assert_eq!(with_cb.cache_evictions(), Some(2));
    assert_eq!(
        without_cb.cache_evictions(),
        with_cb.cache_evictions(),
        "the eviction count must not depend on a callback being configured"
    );
}

// ===========================================================================
// 5. `cache_remove` judges expiry at removal time, not after the callback
// ===========================================================================

/// The callback is deliberately slower than the entry's remaining life.
const SLOW_CALLBACK: Duration = Duration::from_millis(150);

#[cfg(feature = "time_stores")]
#[test]
fn ttl_cache_remove_reports_a_live_value_despite_a_slow_callback() {
    let mut c = TtlCache::builder()
        .ttl(Duration::from_millis(60))
        .on_evict(|_k: &u32, _v: &u32| sleep(SLOW_CALLBACK))
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 10);
    assert_eq!(
        c.cache_remove(&1),
        Some(10),
        "the entry was live when it was removed, so a slow on_evict must not turn it into None"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_cache_remove_reports_a_live_value_despite_a_slow_callback() {
    let mut c = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(60))
        .on_evict(|_k: &u32, _v: &u32| sleep(SLOW_CALLBACK))
        .build()
        .expect("builder has max_size and a non-zero ttl");
    c.cache_set(1, 10);
    assert_eq!(
        c.cache_remove(&1),
        Some(10),
        "the entry was live when it was removed, so a slow on_evict must not turn it into None"
    );
}

#[test]
fn expiring_cache_remove_reports_a_live_value_despite_a_slow_callback() {
    let mut c = ExpiringCache::<u32, Deadline>::builder()
        .on_evict(|_k: &u32, _v: &Deadline| sleep(SLOW_CALLBACK))
        .build()
        .expect("ExpiringCache build is infallible");
    c.cache_set(
        1,
        Deadline {
            id: 10,
            at: Instant::now() + Duration::from_millis(60),
        },
    );
    assert_eq!(
        c.cache_remove(&1).map(|v| v.id),
        Some(10),
        "the entry was live when it was removed, so a slow on_evict must not turn it into None"
    );
}

#[test]
fn expiring_lru_cache_remove_reports_a_live_value_despite_a_slow_callback() {
    let mut c = ExpiringLruCache::<u32, Deadline>::builder()
        .max_size(8)
        .on_evict(|_k: &u32, _v: &Deadline| sleep(SLOW_CALLBACK))
        .build()
        .expect("builder has a non-zero max_size");
    c.cache_set(
        1,
        Deadline {
            id: 10,
            at: Instant::now() + Duration::from_millis(60),
        },
    );
    assert_eq!(
        c.cache_remove(&1).map(|v| v.id),
        Some(10),
        "the entry was live when it was removed, so a slow on_evict must not turn it into None"
    );
}

// ===========================================================================
// 6. Replacing an expired entry: the replacement lands before the callback, so a
//    panicking callback cannot leave the stale entry in place to be counted twice
// ===========================================================================

#[cfg(feature = "time_stores")]
#[test]
fn ttl_get_or_set_over_expired_panicking_callback_counts_one_eviction() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = TtlCache::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |k: &u32, _v: &u32| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 100);
    sleep(Duration::from_millis(60));

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c.cache_get_or_set_with_mut(1u32, || 200u32);
    }));
    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(fired.keys(), vec![1]);
    assert_eq!(c.cache_evictions(), Some(1));
    assert_eq!(
        c.cache_peek(&1),
        Some(&200),
        "the replacement must already be installed when the callback runs"
    );

    // The retry finds a live entry: one physical entry, one eviction, one callback.
    assert_eq!(*c.cache_get_or_set_with_mut(1u32, || 300u32), 200);
    assert_eq!(
        c.cache_evictions(),
        Some(1),
        "retrying must not count a second eviction for one physical entry"
    );
    assert_eq!(fired.keys(), vec![1], "on_evict must not fire twice");
}

#[cfg(feature = "time_stores")]
#[test]
fn ttl_try_get_or_set_over_expired_panicking_callback_counts_one_eviction() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = TtlCache::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |k: &u32, _v: &u32| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 100);
    sleep(Duration::from_millis(60));

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(1u32, || Ok(200u32));
    }));
    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(c.cache_evictions(), Some(1));
    assert_eq!(
        c.cache_peek(&1),
        Some(&200),
        "the replacement must already be installed when the callback runs"
    );

    let retried: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(1u32, || Ok(300u32));
    assert_eq!(*retried.expect("infallible factory"), 200);
    assert_eq!(
        c.cache_evictions(),
        Some(1),
        "retrying must not count a second eviction for one physical entry"
    );
    assert_eq!(fired.keys(), vec![1], "on_evict must not fire twice");
}

#[test]
fn expiring_get_or_set_over_expired_panicking_callback_counts_one_eviction() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringCache::<u32, Flagged>::builder()
        .on_evict(move |k: &u32, _v: &Flagged| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("ExpiringCache build is infallible");
    c.cache_set(
        1,
        Flagged {
            id: 100,
            expired: true,
        },
    );

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c.cache_get_or_set_with_mut(1u32, || Flagged {
            id: 200,
            expired: false,
        });
    }));
    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(fired.keys(), vec![1]);
    assert_eq!(c.cache_evictions(), Some(1));
    assert_eq!(
        c.cache_peek(&1).map(|v| v.id),
        Some(200),
        "the replacement must already be installed when the callback runs"
    );

    assert_eq!(
        c.cache_get_or_set_with_mut(1u32, || Flagged {
            id: 300,
            expired: false,
        })
        .id,
        200
    );
    assert_eq!(
        c.cache_evictions(),
        Some(1),
        "retrying must not count a second eviction for one physical entry"
    );
    assert_eq!(fired.keys(), vec![1], "on_evict must not fire twice");
}

#[test]
fn expiring_try_get_or_set_over_expired_panicking_callback_counts_one_eviction() {
    let fired = Fired::new();
    let f = fired.clone();
    let mut c = ExpiringCache::<u32, Flagged>::builder()
        .on_evict(move |k: &u32, _v: &Flagged| {
            f.push(*k);
            panic!("on_evict boom");
        })
        .build()
        .expect("ExpiringCache build is infallible");
    c.cache_set(
        1,
        Flagged {
            id: 100,
            expired: true,
        },
    );

    let r = catch_unwind(AssertUnwindSafe(|| {
        let _: Result<&mut Flagged, ()> = c.cache_try_get_or_set_with_mut(1u32, || {
            Ok(Flagged {
                id: 200,
                expired: false,
            })
        });
    }));
    assert!(r.is_err(), "on_evict must have panicked");
    assert_eq!(c.cache_evictions(), Some(1));
    assert_eq!(
        c.cache_peek(&1).map(|v| v.id),
        Some(200),
        "the replacement must already be installed when the callback runs"
    );

    let retried: Result<&mut Flagged, ()> = c.cache_try_get_or_set_with_mut(1u32, || {
        Ok(Flagged {
            id: 300,
            expired: false,
        })
    });
    assert_eq!(retried.expect("infallible factory").id, 200);
    assert_eq!(
        c.cache_evictions(),
        Some(1),
        "retrying must not count a second eviction for one physical entry"
    );
    assert_eq!(fired.keys(), vec![1], "on_evict must not fire twice");
}

// ===========================================================================
// 7. Refresh-on-hit: overflowing TTL never expires, zero TTL keeps the deadline
// ===========================================================================

#[cfg(feature = "time_stores")]
#[test]
fn ttl_refresh_under_an_overflowing_ttl_never_expires() {
    let mut c = TtlCache::builder()
        .ttl(Duration::from_millis(60))
        .refresh_on_hit(true)
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 10);
    // A TTL this large overflows `now + ttl`, which a fresh insert stores as
    // "never expires"; a refresh must agree instead of keeping the old 60ms deadline.
    c.set_ttl(Duration::MAX);
    assert_eq!(c.cache_get(&1), Some(&10), "still live, and refreshed");
    sleep(Duration::from_millis(120));
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "a refresh under an overflowing TTL must clear the old deadline, as a fresh insert does"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn ttl_refresh_under_a_zero_ttl_keeps_the_existing_deadline() {
    let mut c = TtlCache::builder()
        .ttl(Duration::from_millis(60))
        .refresh_on_hit(true)
        .build()
        .expect("builder has a non-zero ttl");
    c.cache_set(1, 10);
    // A zero TTL disables expiry for *new* entries; it must not silently clear the
    // deadline an existing entry already carries.
    c.unset_ttl();
    assert_eq!(c.cache_get(&1), Some(&10), "still live");
    sleep(Duration::from_millis(120));
    assert_eq!(
        c.cache_get(&1),
        None,
        "a zero TTL must leave the existing deadline in place on refresh"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_refresh_under_an_overflowing_ttl_never_expires() {
    let mut c = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(60))
        .refresh_on_hit(true)
        .build()
        .expect("builder has max_size and a non-zero ttl");
    c.cache_set(1, 10);
    c.set_ttl(Duration::MAX);
    assert_eq!(c.cache_get(&1), Some(&10), "still live, and refreshed");
    sleep(Duration::from_millis(120));
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "a refresh under an overflowing TTL must clear the old deadline, as a fresh insert does"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_refresh_under_a_zero_ttl_keeps_the_existing_deadline() {
    let mut c = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(60))
        .refresh_on_hit(true)
        .build()
        .expect("builder has max_size and a non-zero ttl");
    c.cache_set(1, 10);
    c.unset_ttl();
    assert_eq!(c.cache_get(&1), Some(&10), "still live");
    sleep(Duration::from_millis(120));
    assert_eq!(
        c.cache_get(&1),
        None,
        "a zero TTL must leave the existing deadline in place on refresh"
    );
}
