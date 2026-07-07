//! Regression tests for the v3.0.0 review fixes to `TtlSortedCache`
//! (findings C2/C9 and C3, plan items 1.2 and 1.3).
//!
//! - C2: `cache_get_or_set_with_mut` / `cache_try_get_or_set_with_mut` must run the factory
//!   before removing an expired entry, so a failing factory leaves the stale entry in place and
//!   does not fire `on_evict`.
//! - C3: `set_max_size` must evict down to the new bound immediately on shrink, not defer to the
//!   next insert.

#![cfg(feature = "time_stores")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cached::Cached;
use cached::stores::TtlSortedCache;
use cached::time::Duration;

/// C2: when the factory returns `Err` on an expired entry, the entry must remain and `on_evict`
/// must NOT fire. A subsequent successful call then replaces it and fires `on_evict` exactly once.
#[test]
fn try_get_or_set_with_mut_err_keeps_expired_entry() {
    let evicted = Arc::new(AtomicU32::new(0));
    let evicted_clone = evicted.clone();
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

    cache.cache_set(1, 10);
    // Let the entry expire.
    std::thread::sleep(Duration::from_millis(60));

    // Factory fails: the expired entry must be left in place, untouched.
    let res: Result<&mut u32, &'static str> =
        cache.cache_try_get_or_set_with_mut(1, || Err("boom"));
    assert_eq!(res, Err("boom"), "factory error must propagate");

    assert_eq!(
        cache.cache_size(),
        1,
        "expired entry must remain when the factory fails (stale-value fallback)"
    );
    assert_eq!(
        evicted.load(Ordering::Relaxed),
        0,
        "on_evict must NOT fire when the factory fails on an expired entry"
    );
    assert_eq!(
        cache.cache_evictions(),
        Some(0),
        "no eviction should be counted when the factory fails"
    );

    // A successful call now replaces the expired entry and fires on_evict exactly once.
    let val: Result<&mut u32, &'static str> = cache.cache_try_get_or_set_with_mut(1, || Ok(99));
    assert_eq!(
        val,
        Ok(&mut 99),
        "successful factory replaces the expired value"
    );
    assert_eq!(
        evicted.load(Ordering::Relaxed),
        1,
        "on_evict fires once when the expired entry is successfully replaced"
    );
    assert_eq!(cache.cache_evictions(), Some(1));
    assert_eq!(cache.cache_size(), 1);
}

/// C2 (infallible variant): a panicking factory must leave the expired entry in place and must
/// not fire `on_evict`.
#[test]
fn get_or_set_with_mut_panic_keeps_expired_entry() {
    let evicted = Arc::new(AtomicU32::new(0));
    let evicted_clone = evicted.clone();
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

    cache.cache_set(1, 10);
    std::thread::sleep(Duration::from_millis(60));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: &mut u32 = cache.cache_get_or_set_with_mut(1, || panic!("factory panicked"));
    }));
    assert!(result.is_err(), "the factory panic must unwind");

    assert_eq!(
        cache.cache_size(),
        1,
        "expired entry must remain when the factory panics"
    );
    assert_eq!(
        evicted.load(Ordering::Relaxed),
        0,
        "on_evict must NOT fire when the factory panics on an expired entry"
    );
    assert_eq!(cache.cache_evictions(), Some(0));
}

/// C2: a live (unexpired) entry is returned as a hit and the factory is never run.
#[test]
fn try_get_or_set_with_mut_live_hit_skips_factory() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(300))
        .build()
        .unwrap();

    cache.cache_set(1, 10);
    let val: Result<&mut u32, &'static str> =
        cache.cache_try_get_or_set_with_mut(1, || panic!("factory must not run on a live hit"));
    assert_eq!(val, Ok(&mut 10));
    assert_eq!(cache.cache_hits(), Some(1));
}

/// C3: shrinking `max_size` below the current entry count must evict immediately, so `cache_size`
/// and the eviction count reflect the new bound on return (not after the next insert).
#[test]
fn set_max_size_shrink_evicts_immediately() {
    let evicted = Arc::new(AtomicU32::new(0));
    let evicted_clone = evicted.clone();
    // No max_size at build: nothing is evicted on insert, isolating the shrink behavior.
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(300))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

    for k in 0..5u32 {
        cache.cache_set(k, k * 10);
    }
    assert_eq!(
        cache.cache_size(),
        5,
        "all five entries stored before shrink"
    );
    assert_eq!(cache.cache_evictions(), Some(0));

    let prev = cache.set_max_size(2);
    assert_eq!(prev, None, "no previous bound was set");

    assert_eq!(
        cache.cache_size(),
        2,
        "set_max_size must evict down to the new bound immediately"
    );
    assert_eq!(
        evicted.load(Ordering::Relaxed),
        3,
        "on_evict must fire for each entry dropped by the shrink"
    );
    assert_eq!(
        cache.cache_evictions(),
        Some(3),
        "eviction counter reflects the immediate shrink"
    );
}

/// C3: growing or setting a bound at/above the current size must not evict anything.
#[test]
fn set_max_size_at_or_above_len_evicts_nothing() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(300))
        .build()
        .unwrap();

    for k in 0..3u32 {
        cache.cache_set(k, k);
    }

    let _ = cache.set_max_size(3);
    assert_eq!(cache.cache_size(), 3, "bound equal to len evicts nothing");
    assert_eq!(cache.cache_evictions(), Some(0));

    let _ = cache.set_max_size(10);
    assert_eq!(cache.cache_size(), 3, "bound above len evicts nothing");
    assert_eq!(cache.cache_evictions(), Some(0));
}
