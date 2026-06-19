//! Outside-in certification of the v3 "`set_ttl(Duration::ZERO)` == expiry disabled"
//! semantic on the SINGLE-OWNER time stores (`TtlCache`, `LruTtlCache`).
//!
//! The existing `try_set_ttl_tests` in `v3_traits.rs` only prove that a disabled
//! ttl keeps a just-inserted entry live through `cache_get`. These tests pin the
//! semantic on every OTHER read/sweep/state path that routes through
//! `entry_live(ttl, instant)`: `iter`, `retain`, `cache_peek`,
//! `cache_get_with_expiry_status`, `cache_peek_with_expiry_status`, `evict`, the
//! async get-or-set, the full `set_ttl`/`unset_ttl`/`ttl()` state machine with its
//! prior-value contract, the `build()`/`try_set_ttl` zero-rejection regression
//! guard, and the `TtlSortedCache` out-of-scope boundary.
#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{
    CacheTtl, Cached, CachedIter, CachedPeek, CloneCached, LruTtlCache, SetTtlError, TtlCache,
};

// A duration short enough that any nonzero ttl set in these tests is already
// elapsed by the time we read (so a regression that treats 0 as "expired now"
// or fails to disable expiry is caught without a sleep where possible).
const SHORT: Duration = Duration::from_millis(30);
const SLEEP: std::time::Duration = std::time::Duration::from_millis(80);

// ─────────────────────────── TtlCache ───────────────────────────

// gap 3: a disabled ttl must make entries permanently live on EVERY read path,
// not just `cache_get`.

#[test]
fn ttl_cache_disabled_ttl_keeps_entry_live_on_iter_peek_and_status() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    // Insert under a real (short) ttl, then disable expiry and sleep well past it.
    c.cache_set(1, 10);
    let prev = c.set_ttl(Duration::ZERO);
    assert_eq!(prev, Some(SHORT), "set_ttl returns the prior ttl");
    assert_eq!(c.ttl(), None, "zero ttl resolves to None");
    std::thread::sleep(SLEEP);

    // iter must yield the entry (entry_live returns true for zero ttl).
    let items: Vec<(u32, u32)> = c.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(
        items,
        vec![(1, 10)],
        "iter must include entries when expiry is disabled"
    );

    // cache_peek must see it (no side effects).
    assert_eq!(
        c.cache_peek(&1),
        Some(&10),
        "cache_peek must return the value under disabled ttl"
    );

    // CloneCached status reads must report not-expired.
    let (val, expired) = c.cache_peek_with_expiry_status(&1);
    assert_eq!(val, Some(10));
    assert!(
        !expired,
        "peek_with_expiry_status must report live under zero ttl"
    );

    let (val2, expired2) = c.cache_get_with_expiry_status(&1);
    assert_eq!(val2, Some(10));
    assert!(
        !expired2,
        "get_with_expiry_status must report live under zero ttl"
    );

    // And a plain cache_get still hits.
    assert_eq!(c.cache_get(&1), Some(&10));
}

#[test]
fn ttl_cache_disabled_ttl_evict_is_noop() {
    // `TtlCache` has no public `retain`; the LRU variant covers retain below.
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    for i in 0..5u32 {
        c.cache_set(i, i * 10);
    }
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);

    // evict() must sweep nothing — a zero ttl disables expiry.
    assert_eq!(c.evict(), 0, "evict must be a no-op under disabled ttl");
    assert_eq!(
        c.cache_size(),
        5,
        "no entry may be swept under disabled ttl"
    );

    // A subsequent read still hits — entries remain live.
    assert_eq!(c.cache_get(&2), Some(&20));
}

#[test]
fn ttl_cache_disabled_ttl_get_or_set_with_treats_old_entry_as_live() {
    // get_or_set_with_mut must HIT the existing entry (not recompute) because a
    // disabled ttl makes it live, even though it was inserted under a short ttl
    // and we slept past it.
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    c.cache_set(1, 10);
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);

    let v = c.cache_get_or_set_with_mut(1, || 999);
    assert_eq!(
        *v, 10,
        "disabled ttl => existing entry is live => get_or_set must not recompute"
    );
}

#[cfg(feature = "async")]
#[tokio::test]
async fn ttl_cache_disabled_ttl_async_get_or_set_treats_old_entry_as_live() {
    use cached::CachedAsync;
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    c.cache_set(1, 10);
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);

    let v = c.async_cache_get_or_set_with(1, || async { 999 }).await;
    assert_eq!(
        *v, 10,
        "async get_or_set must hit the live (disabled-ttl) entry, not recompute"
    );
}

// gap 4: full state machine + prior-value contract on TtlCache.

#[test]
fn ttl_cache_set_unset_ttl_state_machine_and_prior_values() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlCache");
    assert_eq!(c.ttl(), Some(Duration::from_secs(60)));

    // built(60) -> set(30): returns prior Some(60).
    assert_eq!(
        c.set_ttl(Duration::from_secs(30)),
        Some(Duration::from_secs(60))
    );
    assert_eq!(c.ttl(), Some(Duration::from_secs(30)));

    // set(30) -> set(0): returns prior Some(30); ttl resolves None.
    assert_eq!(c.set_ttl(Duration::ZERO), Some(Duration::from_secs(30)));
    assert_eq!(c.ttl(), None);

    // set(0) -> set(0) again: prior was disabled so returns None.
    assert_eq!(
        c.set_ttl(Duration::ZERO),
        None,
        "setting zero when already disabled reports no prior ttl"
    );

    // set(0) -> set(nonzero): prior was disabled => None.
    assert_eq!(
        c.set_ttl(Duration::from_secs(15)),
        None,
        "re-arming from disabled reports no prior ttl"
    );
    assert_eq!(c.ttl(), Some(Duration::from_secs(15)));

    // set(15) -> unset(): returns prior Some(15); ttl None.
    assert_eq!(c.unset_ttl(), Some(Duration::from_secs(15)));
    assert_eq!(c.ttl(), None);

    // unset when already disabled => None.
    assert_eq!(
        c.unset_ttl(),
        None,
        "unset when disabled reports no prior ttl"
    );
}

#[test]
fn ttl_cache_set_zero_observably_identical_to_unset() {
    // Two caches, same history, one disabled via set_ttl(0), the other via unset_ttl.
    let mut via_zero = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build");
    let mut via_unset = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build");

    via_zero.cache_set(1, 10);
    via_unset.cache_set(1, 10);

    assert_eq!(via_zero.set_ttl(Duration::ZERO), Some(SHORT));
    assert_eq!(via_unset.unset_ttl(), Some(SHORT));

    assert_eq!(via_zero.ttl(), via_unset.ttl());
    assert_eq!(via_zero.ttl(), None);

    std::thread::sleep(SLEEP);
    // Both keep the (would-be-expired) entry live.
    assert_eq!(via_zero.cache_get(&1), Some(&10));
    assert_eq!(via_unset.cache_get(&1), Some(&10));
}

// ─────────────────────────── LruTtlCache ───────────────────────────

#[test]
fn lru_ttl_cache_disabled_ttl_keeps_entry_live_on_iter_peek_status_and_orders() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    assert_eq!(c.set_ttl(Duration::ZERO), Some(SHORT));
    assert_eq!(c.ttl(), None);
    std::thread::sleep(SLEEP);

    // iter (CachedIter) must include both.
    let mut items: Vec<(u32, u32)> = c.iter().map(|(k, v)| (*k, *v)).collect();
    items.sort_unstable();
    assert_eq!(items, vec![(1, 10), (2, 20)]);

    // iter_order / key_order / value_order all filter by entry_live and must
    // include the would-be-expired entries.
    assert_eq!(c.iter_order().len(), 2, "iter_order must keep live entries");
    assert_eq!(c.key_order().len(), 2, "key_order must keep live entries");
    assert_eq!(
        c.value_order().len(),
        2,
        "value_order must keep live entries"
    );

    // peek and status reads.
    assert_eq!(c.cache_peek(&1), Some(&10));
    let (v, exp) = c.cache_peek_with_expiry_status(&1);
    assert_eq!((v, exp), (Some(10), false));
    let (v2, exp2) = c.cache_get_with_expiry_status(&2);
    assert_eq!((v2, exp2), (Some(20), false));

    assert_eq!(c.cache_get(&1), Some(&10));
}

#[test]
fn lru_ttl_cache_disabled_ttl_evict_is_noop_and_retain_keeps_live() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");
    for i in 0..5u32 {
        c.cache_set(i, i * 10);
    }
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);

    assert_eq!(c.evict(), 0, "evict must be a no-op under disabled ttl");
    assert_eq!(c.cache_size(), 5);

    c.retain(|k, _v| k % 2 == 0);
    let mut kept: Vec<u32> = c.iter().map(|(k, _)| *k).collect();
    kept.sort_unstable();
    assert_eq!(
        kept,
        vec![0, 2, 4],
        "retain under disabled ttl must not treat would-be-expired entries as expired"
    );
}

#[test]
fn lru_ttl_cache_disabled_ttl_get_or_set_with_treats_old_entry_as_live() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");
    c.cache_set(1, 10);
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);
    let v = c.cache_get_or_set_with_mut(1, || 999);
    assert_eq!(
        *v, 10,
        "disabled ttl => existing LRU entry is live => no recompute"
    );
}

#[cfg(feature = "async")]
#[tokio::test]
async fn lru_ttl_cache_disabled_ttl_async_get_or_set_treats_old_entry_as_live() {
    use cached::CachedAsync;
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");
    c.cache_set(1, 10);
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);
    let v = c.async_cache_get_or_set_with(1, || async { 999 }).await;
    assert_eq!(
        *v, 10,
        "async get_or_set must hit the live (disabled-ttl) LRU entry"
    );
}

#[test]
fn lru_ttl_cache_set_unset_ttl_state_machine_and_prior_values() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build LruTtlCache");
    assert_eq!(c.ttl(), Some(Duration::from_secs(60)));
    assert_eq!(
        c.set_ttl(Duration::from_secs(30)),
        Some(Duration::from_secs(60))
    );
    assert_eq!(c.set_ttl(Duration::ZERO), Some(Duration::from_secs(30)));
    assert_eq!(c.ttl(), None);
    assert_eq!(c.set_ttl(Duration::ZERO), None);
    assert_eq!(c.set_ttl(Duration::from_secs(15)), None);
    assert_eq!(c.ttl(), Some(Duration::from_secs(15)));
    assert_eq!(c.unset_ttl(), Some(Duration::from_secs(15)));
    assert_eq!(c.ttl(), None);
    assert_eq!(c.unset_ttl(), None);
}

// gap 5: build()/try_set_ttl still reject zero (regression guard) for the
// single-owner stores. (Sharded build rejection lives in unit tests; this file
// pins the single-owner public path here alongside the rest.)

#[test]
fn single_owner_build_rejects_zero_ttl() {
    let ttl_built = TtlCache::<u32, u32>::builder().ttl(Duration::ZERO).build();
    assert!(
        matches!(
            ttl_built,
            Err(cached::BuildError::InvalidValue { field: "ttl", .. })
        ),
        "TtlCache build must reject a zero ttl, got {ttl_built:?}"
    );

    let lru_built = LruTtlCache::<u32, u32>::builder()
        .max_size(4)
        .ttl(Duration::ZERO)
        .build();
    assert!(
        matches!(
            lru_built,
            Err(cached::BuildError::InvalidValue { field: "ttl", .. })
        ),
        "LruTtlCache build must reject a zero ttl, got {lru_built:?}"
    );
}

#[test]
fn single_owner_try_set_ttl_rejects_zero_but_set_ttl_disables() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    assert_eq!(c.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
    assert_eq!(
        c.ttl(),
        Some(Duration::from_secs(60)),
        "rejected try_set_ttl must not change ttl"
    );
    // The non-strict route disables.
    assert_eq!(c.set_ttl(Duration::ZERO), Some(Duration::from_secs(60)));
    assert_eq!(c.ttl(), None);

    let mut l = LruTtlCache::<u32, u32>::builder()
        .max_size(4)
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    assert_eq!(l.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
    assert_eq!(l.ttl(), Some(Duration::from_secs(60)));
    assert_eq!(l.set_ttl(Duration::ZERO), Some(Duration::from_secs(60)));
    assert_eq!(l.ttl(), None);
}

// gap 6: TtlSortedCache is intentionally OUT OF SCOPE. Its `ttl()` reports the
// raw configured duration (never None), `set_ttl` always returns `Some(prev)`,
// `unset_ttl` is a no-op returning None, and `set_ttl(0)` does NOT disable
// expiry (it silently breaks reads). Guard that the zero-as-disabled semantic
// was NOT accidentally extended to it.

#[test]
fn ttl_sorted_cache_zero_is_not_disabled() {
    use cached::TtlSortedCache;
    let mut c = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlSortedCache");

    // ttl() reports the raw value, including after a zero set, NOT None.
    assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(60)));
    let prev = CacheTtl::set_ttl(&mut c, Duration::from_secs(30));
    assert_eq!(
        prev,
        Some(Duration::from_secs(60)),
        "set_ttl always returns Some(prev)"
    );

    let prev_zero = CacheTtl::set_ttl(&mut c, Duration::ZERO);
    assert_eq!(prev_zero, Some(Duration::from_secs(30)));
    assert_eq!(
        CacheTtl::ttl(&c),
        Some(Duration::ZERO),
        "TtlSortedCache ttl() must report the raw zero, NOT resolve it to None"
    );

    // unset_ttl is a no-op returning None and does not change the stored ttl.
    assert_eq!(
        CacheTtl::unset_ttl(&mut c),
        None,
        "unset_ttl is a no-op on TtlSortedCache"
    );
    assert_eq!(CacheTtl::ttl(&c), Some(Duration::ZERO));

    // set_ttl(0) does NOT disable expiry: a freshly inserted entry is treated as
    // already expired (the out-of-scope, unchanged breakage that motivates
    // try_set_ttl on this store).
    c.cache_set(1, 10);
    assert_eq!(
        c.cache_get(&1),
        None,
        "TtlSortedCache with a zero ttl must NOT keep entries live (out of scope)"
    );
}
