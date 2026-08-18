//! Unwind safety for `TtlSortedCache`, exercised only through the crate's public API.
//!
//! The store keeps a `HashMap` of rows and an expiry-ordered `BTreeSet` index of stamps, and
//! every sweep it has (`evict`, `retain_latest`, the `max_size` trim) is driven from the index.
//! Two invariants therefore have to survive a panic out of *user* code -- an `on_evict`
//! callback, a `Drop` for `V`, or a `retain` predicate:
//!
//! 1. No orphaned row: a map row whose stamp is gone can never be reclaimed by any sweep, yet
//!    it still counts towards `cache_size()` and still consumes a slot of `max_size`.
//! 2. No silent loss: an entry that leaves the cache is counted in `cache_evictions()` and
//!    reported to `on_evict`; it must not simply vanish during unwinding.

#![cfg(feature = "time_stores")]

use cached::Cached;
use cached::stores::TtlSortedCache;
use cached::time::Duration;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Consume `cache` to prove every map row is still reachable from the expiry index.
///
/// `retain_latest(0, false)` walks the index and removes the map row behind each stamp, so it
/// must drop exactly `cache_size()` entries and leave the cache empty. An orphaned row makes
/// the count too low and survives the trim; a stale stamp makes it too high.
fn assert_every_row_is_indexed<V>(mut cache: TtlSortedCache<u32, V>, ctx: &str) {
    let size = cache.cache_size();
    assert_eq!(
        cache.retain_latest(0, false),
        size,
        "{ctx}: the index must reach every one of the {size} stored rows"
    );
    assert_eq!(
        cache.cache_size(),
        0,
        "{ctx}: trimming to zero must empty the map"
    );
}

/// A value whose `Drop` panics only when armed, so an unwind never double-panics into a
/// process abort.
struct PanicOnDrop {
    armed: bool,
}

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        if self.armed {
            panic!("PanicOnDrop::drop fired");
        }
    }
}

/// A value that records its own destruction, for observing entries that leave the cache
/// without ever being reported.
struct DropCounter {
    drops: Arc<AtomicUsize>,
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------------------
// The `max_size` trim that protects the entry `cache_get_or_set_with` just inserted.
// ---------------------------------------------------------------------------------------

/// A panicking `on_evict` during the size trim of a `cache_get_or_set_with` insert must not
/// orphan the just-inserted entry.
///
/// The trim has to keep that entry out of the victim pool. Doing so by unlinking its stamp for
/// the duration of the trim leaves a window in which its row has no stamp -- and the callback
/// that panics runs inside exactly that window, so the stamp is never restored: the row is
/// stranded in the map, invisible to `evict()` forever.
#[test]
fn get_or_set_size_trim_on_evict_panic_does_not_orphan_the_new_entry() {
    let armed = Arc::new(AtomicBool::new(false));
    let armed2 = Arc::clone(&armed);
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(120))
        .max_size(2)
        .on_evict(move |_k: &u32, _v: &u32| {
            if armed2.load(Ordering::Relaxed) {
                panic!("on_evict boom");
            }
        })
        .build()
        .unwrap();

    cache.cache_set(1u32, 10u32);
    cache.cache_set(2u32, 20u32);
    assert_eq!(cache.cache_size(), 2);

    armed.store(true, Ordering::Relaxed);
    let caught = catch_unwind(AssertUnwindSafe(|| {
        // Inserts key 3 (size 3 > max_size 2), then trims key 1 -- whose callback panics.
        let _ = cache.cache_get_or_set_with(3u32, || 30u32);
    }));
    assert!(caught.is_err(), "the callback panic must propagate");
    armed.store(false, Ordering::Relaxed);

    assert_eq!(
        cache.cache_size(),
        2,
        "one entry was trimmed, the other two remain"
    );

    // Both remaining rows must still be reachable from the expiry index: once they expire the
    // sweep has to reclaim BOTH, not just the one that kept its stamp.
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert_eq!(
        cache.evict(),
        2,
        "every surviving row is still indexed after the panic"
    );
    assert_eq!(cache.cache_size(), 0, "the sweep emptied the cache");
}

/// The same orphaning, with NO callback configured: a panicking `Drop` for `V` is enough,
/// because the trimmed value is dropped inside the very same window.
#[test]
fn get_or_set_size_trim_value_drop_panic_does_not_orphan_the_new_entry() {
    let mut cache: TtlSortedCache<u32, PanicOnDrop> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(120))
        .max_size(2)
        .build()
        .unwrap();

    // Key 1 is inserted first, so it has the earliest expiry and is the trim's victim. Only
    // its value is armed, so the unwind panics exactly once.
    cache.cache_set(1u32, PanicOnDrop { armed: true });
    cache.cache_set(2u32, PanicOnDrop { armed: false });
    assert_eq!(cache.cache_size(), 2);

    let caught = catch_unwind(AssertUnwindSafe(|| {
        let _ = cache.cache_get_or_set_with(3u32, || PanicOnDrop { armed: false });
    }));
    assert!(caught.is_err(), "the value Drop panic must propagate");

    assert_eq!(cache.cache_size(), 2);
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert_eq!(
        cache.evict(),
        2,
        "every surviving row is still indexed after the Drop panic"
    );
    assert_eq!(cache.cache_size(), 0);
}

/// The fallible sibling reaches the same trim through `cache_try_get_or_set_with`.
#[test]
fn try_get_or_set_size_trim_on_evict_panic_does_not_orphan_the_new_entry() {
    let armed = Arc::new(AtomicBool::new(false));
    let armed2 = Arc::clone(&armed);
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .max_size(2)
        .on_evict(move |_k: &u32, _v: &u32| {
            if armed2.load(Ordering::Relaxed) {
                panic!("on_evict boom");
            }
        })
        .build()
        .unwrap();

    cache.cache_set(1u32, 10u32);
    cache.cache_set(2u32, 20u32);

    armed.store(true, Ordering::Relaxed);
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let _: Result<&u32, &'static str> = cache.cache_try_get_or_set_with(3u32, || Ok(30u32));
    }));
    assert!(caught.is_err(), "the callback panic must propagate");
    armed.store(false, Ordering::Relaxed);

    assert_eq!(cache.cache_size(), 2);
    assert_every_row_is_indexed(cache, "after a try_get_or_set trim panic");
}

/// The async get-or-set pair funnels into the same insert-then-trim path.
#[cfg(feature = "async")]
#[tokio::test]
async fn async_get_or_set_size_trim_on_evict_panic_does_not_orphan_the_new_entry() {
    use cached::CachedGetOrSetAsync;

    let armed = Arc::new(AtomicBool::new(false));
    let armed2 = Arc::clone(&armed);
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .max_size(2)
        .on_evict(move |_k: &u32, _v: &u32| {
            if armed2.load(Ordering::Relaxed) {
                panic!("on_evict boom");
            }
        })
        .build()
        .unwrap();

    cache.cache_set(1u32, 10u32);
    cache.cache_set(2u32, 20u32);

    armed.store(true, Ordering::Relaxed);
    // The future resolves its factory immediately, so the panic surfaces on the first poll.
    let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
        &mut cache,
        3u32,
        || async { 30u32 },
    ));
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let _ = std::future::Future::poll(fut.as_mut(), &mut cx);
    }));
    assert!(caught.is_err(), "the callback panic must propagate");
    drop(fut);
    armed.store(false, Ordering::Relaxed);

    assert_eq!(cache.cache_size(), 2);
    assert_every_row_is_indexed(cache, "after an async trim panic");
}

// ---------------------------------------------------------------------------------------
// `retain`: the caller's predicate is user code and may panic mid-pass.
// ---------------------------------------------------------------------------------------

/// A panicking `retain` predicate must not destroy the entries it already accepted for
/// removal. Removing eagerly while the predicate runs leaves those entries in flight when the
/// panic unwinds: they are dropped without ever reaching `on_evict` and without being counted,
/// so the cache silently shrinks. Deciding first and removing afterwards makes the pass
/// all-or-nothing.
#[test]
fn retain_predicate_panic_does_not_lose_entries_unreported() {
    let fired = Arc::new(AtomicUsize::new(0));
    let fired2 = Arc::clone(&fired);
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .on_evict(move |_k: &u32, _v: &u32| {
            fired2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();
    for k in 0u32..20 {
        cache.cache_set(k, k * 10);
    }
    assert_eq!(cache.cache_size(), 20);

    // Reject everything, then panic partway through the pass. `HashMap` iteration order is
    // unspecified, so the panic is triggered by a call count rather than by a key.
    let calls = AtomicUsize::new(0);
    let caught = catch_unwind(AssertUnwindSafe(|| {
        cache.retain(|_k, _v| {
            if calls.fetch_add(1, Ordering::Relaxed) == 8 {
                panic!("predicate boom");
            }
            false
        });
    }));
    assert!(caught.is_err(), "the predicate panic must propagate");

    let lost = 20 - cache.cache_size();
    let fired = fired.load(Ordering::Relaxed);
    let evictions = cache.cache_evictions().unwrap() as usize;
    assert_eq!(
        (lost, fired, evictions),
        (0, 0, 0),
        "a panicking predicate must leave the cache untouched: \
         lost={lost} on_evict_fired={fired} evictions={evictions}"
    );
    assert_eq!(cache.cache_size(), 20);
    assert_every_row_is_indexed(cache, "after a panicking retain predicate");
}

/// The same loss with NO callback configured, observed directly on the values: entries that
/// vanish during the unwind are destroyed without any trace in `cache_evictions()`.
///
/// The value's `Drop` records rather than panics here on purpose: under the eager-removal
/// shape the lost values are dropped *while already unwinding* from the predicate panic, so a
/// panicking `Drop` would abort the process instead of failing the test.
#[test]
fn retain_predicate_panic_without_a_callback_destroys_nothing() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut cache: TtlSortedCache<u32, DropCounter> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    for k in 0u32..20 {
        cache.cache_set(
            k,
            DropCounter {
                drops: Arc::clone(&drops),
            },
        );
    }
    assert_eq!(drops.load(Ordering::Relaxed), 0, "nothing dropped yet");

    let calls = AtomicUsize::new(0);
    let caught = catch_unwind(AssertUnwindSafe(|| {
        cache.retain(|_k, _v| {
            if calls.fetch_add(1, Ordering::Relaxed) == 8 {
                panic!("predicate boom");
            }
            false
        });
    }));
    assert!(caught.is_err(), "the predicate panic must propagate");

    assert_eq!(
        drops.load(Ordering::Relaxed),
        0,
        "no value may be destroyed by a panicking predicate"
    );
    assert_eq!(cache.cache_size(), 20, "no row may leave the map");
    assert_eq!(cache.cache_evictions(), Some(0));
    assert_every_row_is_indexed(cache, "after a callback-less panicking retain predicate");
}

/// A predicate that panics after accepting entries for removal must also leave the two
/// structures in lockstep, so a later sweep neither misses a row nor counts a phantom drop.
#[test]
fn retain_predicate_panic_leaves_evict_and_retain_latest_accurate() {
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(120))
        .build()
        .unwrap();
    for k in 0u32..10 {
        cache.cache_set(k, k);
    }

    let calls = AtomicUsize::new(0);
    let caught = catch_unwind(AssertUnwindSafe(|| {
        cache.retain(|k, _v| {
            if calls.fetch_add(1, Ordering::Relaxed) == 4 {
                panic!("predicate boom");
            }
            k % 2 == 0
        });
    }));
    assert!(caught.is_err());

    let size = cache.cache_size();
    assert_eq!(size, 10, "the interrupted pass removed nothing");
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert_eq!(
        cache.evict(),
        size,
        "the expiry sweep must reclaim exactly the rows still stored"
    );
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(
        cache.cache_evictions(),
        Some(size as u64),
        "every reclaimed row is counted exactly once"
    );
}
