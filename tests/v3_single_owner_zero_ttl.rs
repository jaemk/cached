//! Outside-in certification of the v3 "`set_ttl(Duration::ZERO)` == expiry disabled"
//! semantic on the SINGLE-OWNER time stores (`TtlCache`, `LruTtlCache`).
//!
//! # v3 per-entry expiry semantics
//!
//! As of v3, each entry stores an absolute `expires_at: Option<Instant>` computed
//! at INSERT time from the TTL that was active when the entry was inserted.
//! `set_ttl` only affects FUTURE inserts; existing entries keep their original
//! `expires_at`.  `set_ttl(Duration::ZERO)` makes new entries never expire
//! (`expires_at = None`), but entries already in the cache still expire at their
//! original deadline.
//!
//! Tests that used to verify that `set_ttl(ZERO)` retroactively kept already-
//! inserted entries live have been updated to match the new contract.
#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{
    CacheTtl, Cached, CachedIter, CachedPeek, CloneCached, LruTtlCache, SetTtlError, TtlCache,
};

// A duration short enough that any nonzero ttl used in these tests expires
// well before the sleep. Must be shorter than SLEEP.
const SHORT: Duration = Duration::from_millis(30);
const SLEEP: std::time::Duration = std::time::Duration::from_millis(80);

// ─────────────────────────── TtlCache ───────────────────────────

// Entries inserted BEFORE set_ttl(ZERO) keep their original expires_at and
// therefore still expire.  Entries inserted AFTER never expire.
#[test]
fn ttl_cache_set_zero_only_affects_future_inserts() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");

    // Insert under SHORT ttl; this entry gets expires_at = now + 30ms.
    c.cache_set(1, 10);

    let prev = c.set_ttl(Duration::ZERO);
    assert_eq!(prev, Some(SHORT), "set_ttl returns the prior ttl");
    assert_eq!(c.ttl(), None, "zero ttl resolves to None");

    // Insert AFTER set_ttl(ZERO); this entry gets expires_at = None (never-expires).
    c.cache_set(2, 20);

    std::thread::sleep(SLEEP); // 80ms > 30ms: entry 1 has expired

    // Entry 1 (inserted before set_ttl) must be expired now.
    assert_eq!(
        c.cache_get(&1),
        None,
        "entry inserted before set_ttl(ZERO) must expire at its original deadline"
    );

    // Entry 2 (inserted after set_ttl(ZERO)) must still be live.
    assert_eq!(
        c.cache_get(&2),
        Some(&20),
        "entry inserted after set_ttl(ZERO) must never expire"
    );
}

#[test]
fn ttl_cache_pre_zero_entry_expired_on_all_paths() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    c.cache_set(1, 10);
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);

    // cache_peek must not see the expired entry.
    assert_eq!(
        c.cache_peek(&1),
        None,
        "cache_peek must not return an expired entry"
    );

    // CloneCached status reads must report expired.
    let (val, expired) = c.cache_peek_with_expiry_status(&1);
    assert_eq!(val, Some(10));
    assert!(expired, "peek_with_expiry_status must report expired entry");

    let (val2, expired2) = c.cache_get_with_expiry_status(&1);
    assert_eq!(val2, Some(10));
    assert!(expired2, "get_with_expiry_status must report expired entry");

    // iter must not yield the expired entry.
    let items: Vec<(u32, u32)> = c.iter().map(|(k, v)| (*k, *v)).collect();
    assert!(items.is_empty(), "iter must exclude expired entries");
}

#[test]
fn ttl_cache_post_zero_entry_live_on_all_paths() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    c.set_ttl(Duration::ZERO);
    // Insert AFTER disabling; entry gets expires_at = None.
    c.cache_set(1, 10);
    std::thread::sleep(SLEEP);

    // iter must yield the entry.
    let items: Vec<(u32, u32)> = c.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(
        items,
        vec![(1, 10)],
        "iter must include entries whose expires_at is None"
    );

    // cache_peek must see it.
    assert_eq!(c.cache_peek(&1), Some(&10));

    // CloneCached status reads must report not-expired.
    let (val, expired) = c.cache_peek_with_expiry_status(&1);
    assert_eq!(val, Some(10));
    assert!(!expired, "peek_with_expiry_status must report live entry");

    let (val2, expired2) = c.cache_get_with_expiry_status(&1);
    assert_eq!(val2, Some(10));
    assert!(!expired2, "get_with_expiry_status must report live entry");

    // Plain cache_get must hit.
    assert_eq!(c.cache_get(&1), Some(&10));
}

#[test]
fn ttl_cache_evict_expires_pre_zero_entries_but_not_post_zero() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    // Insert 3 entries under SHORT ttl.
    for i in 0..3u32 {
        c.cache_set(i, i * 10);
    }
    c.set_ttl(Duration::ZERO);
    // Insert 2 entries under disabled (never-expire) ttl.
    c.cache_set(10, 100);
    c.cache_set(11, 110);

    std::thread::sleep(SLEEP);

    // evict() must only remove the 3 expired entries (keys 0-2), not the never-expire ones.
    let removed = c.evict();
    assert_eq!(
        removed, 3,
        "evict must remove exactly the 3 expired entries"
    );
    assert_eq!(c.cache_size(), 2, "two never-expire entries must remain");
    assert_eq!(c.cache_get(&10), Some(&100));
    assert_eq!(c.cache_get(&11), Some(&110));
}

#[test]
fn ttl_cache_disabled_ttl_get_or_set_with_recomputes_expired_entry() {
    // When an entry was inserted under SHORT ttl and that ttl has elapsed,
    // get_or_set_with_mut must recompute (the entry is expired), not hit.
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    c.cache_set(1, 10);
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);

    let v = c.cache_get_or_set_with_mut(1, || 999);
    assert_eq!(
        *v, 999,
        "expired entry must be replaced by get_or_set; new entry never expires"
    );
    // The newly inserted entry must itself never expire.
    std::thread::sleep(SLEEP);
    assert_eq!(c.cache_get(&1), Some(&999));
}

#[cfg(feature = "async")]
#[tokio::test]
async fn ttl_cache_disabled_ttl_async_get_or_set_recomputes_expired_entry() {
    use cached::CachedGetOrSetAsync;
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build TtlCache");
    c.cache_set(1, 10);
    c.set_ttl(Duration::ZERO);
    std::thread::sleep(SLEEP);

    let v = c.async_cache_get_or_set_with(1, || async { 999 }).await;
    assert_eq!(*v, 999, "async get_or_set must recompute the expired entry");
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
fn ttl_cache_set_zero_and_unset_both_disable_future_expiry() {
    // Both set_ttl(ZERO) and unset_ttl() should make FUTURE inserts never expire.
    let mut via_zero = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build");
    let mut via_unset = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .build()
        .expect("build");

    // Disable expiry first, then insert.
    assert_eq!(via_zero.set_ttl(Duration::ZERO), Some(SHORT));
    assert_eq!(via_unset.unset_ttl(), Some(SHORT));

    via_zero.cache_set(1, 10);
    via_unset.cache_set(1, 10);

    assert_eq!(via_zero.ttl(), via_unset.ttl());
    assert_eq!(via_zero.ttl(), None);

    std::thread::sleep(SLEEP);
    // Both entries (inserted after disabling) must still be live.
    assert_eq!(via_zero.cache_get(&1), Some(&10));
    assert_eq!(via_unset.cache_get(&1), Some(&10));
}

// ─────────────────────────── LruTtlCache ───────────────────────────

#[test]
fn lru_ttl_cache_set_zero_only_affects_future_inserts() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");
    c.cache_set(1, 10);
    c.cache_set(2, 20);

    assert_eq!(c.set_ttl(Duration::ZERO), Some(SHORT));
    assert_eq!(c.ttl(), None);

    // Insert AFTER disabling; these get expires_at = None.
    c.cache_set(3, 30);
    c.cache_set(4, 40);

    std::thread::sleep(SLEEP);

    // Entries 1 and 2 (inserted before disabling) must be expired.
    assert_eq!(
        c.cache_get(&1),
        None,
        "entry 1 inserted before set_ttl(ZERO) must expire"
    );
    assert_eq!(
        c.cache_get(&2),
        None,
        "entry 2 inserted before set_ttl(ZERO) must expire"
    );

    // Entries 3 and 4 (inserted after disabling) must be live.
    assert_eq!(c.cache_get(&3), Some(&30));
    assert_eq!(c.cache_get(&4), Some(&40));
}

#[test]
fn lru_ttl_cache_post_zero_entry_live_on_iter_peek_status_and_orders() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");

    // Disable first, then insert; entries get expires_at = None.
    c.set_ttl(Duration::ZERO);
    c.cache_set(1, 10);
    c.cache_set(2, 20);
    std::thread::sleep(SLEEP);

    // iter (CachedIter) must include both.
    let mut items: Vec<(u32, u32)> = c.iter().map(|(k, v)| (*k, *v)).collect();
    items.sort_unstable();
    assert_eq!(items, vec![(1, 10), (2, 20)]);

    // iter_order / key_order / value_order all filter by entry_live and must
    // include the never-expiring entries.
    let ordered = c.iter_order();
    assert_eq!(ordered.len(), 2, "iter_order must keep live entries");
    for (_k, v) in &ordered {
        assert_eq!(
            v.expires_at(),
            None,
            "entries inserted under zero ttl carry no expiry"
        );
    }
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
fn lru_ttl_cache_evict_expires_pre_zero_entries_but_not_post_zero() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(16)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");

    // Insert 5 entries under SHORT ttl, then disable, then insert 2 more.
    for i in 0..5u32 {
        c.cache_set(i, i * 10);
    }
    c.set_ttl(Duration::ZERO);
    c.cache_set(10, 100);
    c.cache_set(11, 110);

    std::thread::sleep(SLEEP);

    let removed = c.evict();
    assert_eq!(
        removed, 5,
        "evict must remove exactly the 5 expired entries"
    );
    assert_eq!(c.cache_size(), 2, "two never-expire entries must remain");
    assert_eq!(c.cache_get(&10), Some(&100));
    assert_eq!(c.cache_get(&11), Some(&110));
}

#[test]
fn lru_ttl_cache_retain_keeps_never_expire_entries() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(16)
        .ttl(SHORT)
        .build()
        .expect("build LruTtlCache");

    // Disable first, insert never-expire entries.
    c.set_ttl(Duration::ZERO);
    for i in 0..5u32 {
        c.cache_set(i, i * 10);
    }
    std::thread::sleep(SLEEP);

    // retain by even keys.
    c.retain(|k, _v| k % 2 == 0);
    let mut kept: Vec<u32> = c.iter().map(|(k, _)| *k).collect();
    kept.sort_unstable();
    assert_eq!(
        kept,
        vec![0, 2, 4],
        "retain must keep live never-expire entries matching the predicate"
    );
}

#[test]
fn lru_ttl_cache_disabled_ttl_get_or_set_with_recomputes_expired_entry() {
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
        *v, 999,
        "expired entry must be replaced; new entry inserted under disabled ttl never expires"
    );
    // The new entry must itself never expire.
    std::thread::sleep(SLEEP);
    assert_eq!(c.cache_get(&1), Some(&999));
}

#[cfg(feature = "async")]
#[tokio::test]
async fn lru_ttl_cache_disabled_ttl_async_get_or_set_recomputes_expired_entry() {
    use cached::CachedGetOrSetAsync;
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
        *v, 999,
        "async get_or_set must recompute the expired LRU entry"
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

// gap 6: As of the v3 zero-ttl-disables change, TtlSortedCache is now IN SCOPE and
// fully consistent with TtlCache / LruTtlCache: a zero ttl disables expiry for future
// inserts (entries never expire), AND `ttl()` resolves a zero ttl to `None` (expiry
// disabled) exactly like the per-entry stores. `set_ttl`/`unset_ttl` return the previous
// ttl, or `None` when expiry was already disabled (previous ttl was zero). Guard both the
// never-expires semantic and the None-resolving `ttl()` / return conventions.

#[test]
fn ttl_sorted_cache_zero_disables_expiry() {
    use cached::TtlSortedCache;
    let mut c = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlSortedCache");

    // ttl() reports the configured non-zero value.
    assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(60)));
    let prev = CacheTtl::set_ttl(&mut c, Duration::from_secs(30));
    assert_eq!(
        prev,
        Some(Duration::from_secs(60)),
        "set_ttl returns the previous (non-zero) ttl"
    );

    let prev_zero = CacheTtl::set_ttl(&mut c, Duration::ZERO);
    assert_eq!(prev_zero, Some(Duration::from_secs(30)));
    assert_eq!(
        CacheTtl::ttl(&c),
        None,
        "TtlSortedCache ttl() must resolve a zero ttl to None (expiry disabled), \
         matching TtlCache / LruTtlCache"
    );

    // set_ttl while expiry is already disabled returns None (previous ttl was zero).
    assert_eq!(
        CacheTtl::set_ttl(&mut c, Duration::ZERO),
        None,
        "set_ttl returns None when the previous ttl was already zero (disabled)"
    );

    // unset_ttl on an already-disabled store returns None and leaves expiry disabled.
    assert_eq!(
        CacheTtl::unset_ttl(&mut c),
        None,
        "unset_ttl returns None when expiry was already disabled"
    );
    assert_eq!(
        CacheTtl::ttl(&c),
        None,
        "unset_ttl leaves expiry disabled, so ttl() stays None"
    );

    // unset_ttl from a non-zero ttl returns the previous ttl (sibling convention).
    assert_eq!(
        CacheTtl::set_ttl(&mut c, Duration::from_secs(45)),
        None,
        "restoring a ttl from disabled returns None (previous was zero)"
    );
    assert_eq!(
        CacheTtl::unset_ttl(&mut c),
        Some(Duration::from_secs(45)),
        "unset_ttl returns the previous non-zero ttl before disabling"
    );

    // set_ttl(0) disables expiry for future inserts: a freshly inserted entry is
    // stored with no expiry and is retrievable indefinitely.
    c.cache_set(1, 10);
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "TtlSortedCache with a zero ttl must keep just-inserted entries live (never expires)"
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "the entry must persist (never expires) under zero ttl"
    );
}

// The infallible `cache_set` must NOT drop the value when the computed expiry
// `Instant` overflows. Siblings (`TtlCache` / `LruTtlCache`) store the value with no
// expiry (never expires) on overflow rather than silently discarding it, and
// `TtlSortedCache` must match. `cache_try_set` behaves identically (the store's
// `Cached::Error` is `Infallible`).
//
// The overflow is triggered portably with a near-`Duration::MAX` default ttl: it is
// non-zero (so `build()` succeeds and it is NOT treated as "expiry disabled"), yet
// `Instant::now() + ttl` overflows on every platform because no real `Instant` is
// anywhere near `Duration::MAX` from its epoch.
#[test]
fn ttl_sorted_cache_set_overflow_stores_never_expiring_value() {
    use cached::TtlSortedCache;

    let mut c = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::MAX)
        .build()
        .expect("Duration::MAX is non-zero so build() must succeed");

    // Infallible cache_set with an unrepresentable expiry must STORE the value (never
    // expires), not drop it. Prior to the fix this returned without inserting anything.
    let prev = c.cache_set(1, 42);
    assert_eq!(prev, None, "first cache_set has no previous value");
    assert_eq!(
        c.cache_get(&1),
        Some(&42),
        "cache_set on TTL overflow must store the value (never expires), not drop it"
    );
    assert_eq!(
        c.cache_size(),
        1,
        "the overflowing cache_set must have inserted the entry"
    );

    // The stored entry never expires: it is still live after a wait.
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(
        c.cache_get(&1),
        Some(&42),
        "the overflow-stored entry must never expire"
    );

    // Overwriting returns the prior value, confirming the entry is a real live entry.
    let prev = c.cache_set(1, 99);
    assert_eq!(
        prev,
        Some(42),
        "overwriting the never-expiring entry returns the prior value"
    );

    // cache_try_set matches: the same overflow stores a never-expiring entry.
    let result: Result<Option<u32>, std::convert::Infallible> = c.cache_try_set(2, 7);
    assert_eq!(result.unwrap(), None);
    assert_eq!(
        c.cache_get(&2),
        Some(&7),
        "cache_try_set on TTL overflow must store the value (never expires)"
    );
}

// Gap A: `CacheTtl::set_ttl` on `TtlSortedCache` must honour the "previous ttl was zero"
// contract: return `None` (not `Some(Duration::ZERO)`) when expiry was disabled, and
// return `Some(prev)` when the previous ttl was a real non-zero duration. The new ttl
// must take effect for subsequent inserts. Runtime TTL control is via the `CacheTtl`
// trait; there is no separate inherent `set_ttl`.
#[test]
fn ttl_sorted_cache_trait_set_ttl_semantics() {
    use cached::TtlSortedCache;

    // ── previous-was-NONZERO: set_ttl must return Some(prev) ──
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build cache");

    // Disable expiry via the trait method; previous ttl was 60s so Some(60s) is expected.
    let prev = CacheTtl::set_ttl(&mut cache, Duration::ZERO);
    assert_eq!(
        prev,
        Some(Duration::from_secs(60)),
        "previous non-zero ttl must be reported as Some(prev)"
    );

    // Cache is now disabled (ttl == zero); ttl() must resolve to None.
    assert_eq!(CacheTtl::ttl(&cache), None);

    // ── previous-was-ZERO: set_ttl must return None (NOT Some(Duration::ZERO)) ──
    // The cache currently has a zero (disabled) ttl, so the next set observes prev==zero.
    let prev_zero = CacheTtl::set_ttl(&mut cache, Duration::from_secs(5));
    assert_eq!(
        prev_zero, None,
        "a zero previous ttl must be reported as None by CacheTtl::set_ttl"
    );

    // Cache is now re-armed to 5s; ttl() must reflect the new value.
    assert_eq!(CacheTtl::ttl(&cache), Some(Duration::from_secs(5)));
}

// Gap B: `try_set_ttl` (the `CacheTtl` default) on `TtlSortedCache` after the `ttl()`
// change. A zero argument must still be rejected with `Err(SetTtlError::ZeroTtl)` without
// mutating state, and crucially when expiry is ALREADY disabled the rejected call must
// leave `ttl()` reporting `None` (not flip it to `Some(ZERO)` or otherwise perturb it).
// The existing `single_owner_try_set_ttl_*` test covers only TtlCache / LruTtlCache.
#[test]
fn ttl_sorted_cache_try_set_ttl_rejects_zero_and_preserves_state() {
    use cached::TtlSortedCache;

    let mut c = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlSortedCache");

    // Live, non-zero ttl: try_set_ttl(ZERO) must error and NOT change the ttl.
    assert_eq!(c.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
    assert_eq!(
        CacheTtl::ttl(&c),
        Some(Duration::from_secs(60)),
        "a rejected try_set_ttl must leave the live ttl untouched"
    );

    // A non-zero try_set_ttl succeeds and reports the previous (non-zero) ttl.
    assert_eq!(
        c.try_set_ttl(Duration::from_secs(30)),
        Ok(Some(Duration::from_secs(60))),
        "a valid try_set_ttl returns the previous ttl"
    );
    assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(30)));

    // Now disable expiry so the previous ttl is zero and ttl() resolves to None.
    assert_eq!(
        CacheTtl::set_ttl(&mut c, Duration::ZERO),
        Some(Duration::from_secs(30))
    );
    assert_eq!(CacheTtl::ttl(&c), None);

    // try_set_ttl(ZERO) while ALREADY disabled: still Err, and ttl() must stay None.
    assert_eq!(
        c.try_set_ttl(Duration::ZERO),
        Err(SetTtlError::ZeroTtl),
        "try_set_ttl(ZERO) must be rejected even when expiry is already disabled"
    );
    assert_eq!(
        CacheTtl::ttl(&c),
        None,
        "a rejected try_set_ttl must leave a disabled cache reporting ttl() == None"
    );

    // Re-arming from the disabled state via the valid path returns None (previous was zero).
    assert_eq!(
        c.try_set_ttl(Duration::from_secs(15)),
        Ok(None),
        "re-arming a disabled cache reports no previous ttl (it was zero)"
    );
    assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(15)));
}

// Gap C: the `NeverExpire` overflow policy sets `overflowed = false`, so normal
// size-limit eviction still runs on the overflowing `cache_set`. With a small `max_size`
// and an overflowing (`Duration::MAX`) ttl, EVERY entry is stored with `expiry = None`
// (never expires). Under size pressure `retain_latest` must still bound the store: with
// all-`None` expiries the entries sort by key, so the lowest keys are dropped first. The
// existing overflow test has no size bound, so this eviction interaction is uncovered.
#[test]
fn ttl_sorted_cache_overflow_with_size_limit_stays_bounded() {
    use cached::TtlSortedCache;

    let mut c = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::MAX) // non-zero => build ok; now + ttl overflows on every insert
        .max_size(2)
        .build()
        .expect("Duration::MAX is non-zero so build() succeeds");

    // Each cache_set overflows -> NeverExpire (expiry = None) and, because overflowed is
    // false, size eviction runs. Insert past the limit.
    assert_eq!(c.cache_set(1, 10), None);
    assert_eq!(c.cache_set(2, 20), None);
    assert_eq!(c.cache_set(3, 30), None); // size would be 3 -> evicts lowest key (1)
    assert_eq!(c.cache_set(4, 40), None); // -> evicts next lowest (2)

    // The store must never exceed max_size despite every entry being never-expiring.
    assert_eq!(
        c.cache_size(),
        2,
        "size_limit must be enforced even when overflow makes every entry never-expiring"
    );

    // All-None expiries sort by key, so the two highest keys survive and the lowest were
    // evicted. This pins the deterministic eviction order under the NeverExpire policy.
    assert_eq!(
        c.cache_get(&1),
        None,
        "lowest key must be evicted under size pressure"
    );
    assert_eq!(
        c.cache_get(&2),
        None,
        "next lowest key must be evicted under size pressure"
    );
    assert_eq!(
        c.cache_get(&3),
        Some(&30),
        "surviving never-expiring entry must be readable"
    );
    assert_eq!(
        c.cache_get(&4),
        Some(&40),
        "surviving never-expiring entry must be readable"
    );

    // The survivors truly never expire: still live after a wait.
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(
        c.cache_size(),
        2,
        "no TTL sweep may drop the never-expiring survivors"
    );
    assert_eq!(c.cache_get(&3), Some(&30));
    assert_eq!(c.cache_get(&4), Some(&40));

    // Overwriting a survivor returns its prior value (confirms a real live entry, not a
    // phantom), and does not grow the store.
    assert_eq!(
        c.cache_set(4, 99),
        Some(40),
        "overwriting a never-expiring survivor returns its prior value"
    );
    assert_eq!(c.cache_size(), 2);

    // ttl() reflects the configured (non-zero) Duration::MAX rather than resolving to None.
    assert_eq!(CacheTtl::ttl(&c), Some(Duration::MAX));
}
