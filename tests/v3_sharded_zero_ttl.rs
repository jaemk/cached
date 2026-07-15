//! Outside-in coverage for the zero-TTL semantics on the sharded TTL stores
//! (`ShardedTtlCache` / `ShardedLruTtlCache`), pinning I2.
//!
//! Semantic (v3): `set_ttl(Duration::ZERO)` means "expiry disabled / no expiry" and is
//! exactly equivalent to `unset_ttl()`. A zero ttl is the single sentinel for "disabled";
//! it does NOT mean "expire immediately". `set_ttl(nonzero)` re-arms expiry. The builder
//! still rejects a zero ttl, and `try_set_ttl(0)` still returns `SetTtlError::ZeroTtl` —
//! disabling is done via `set_ttl(0)` or `unset_ttl()`.
//!
//! This module covers: the full state-transition cycle with prior-value semantics, the
//! `evict`/`on_evict` no-op under a disabled ttl, the LruTtl refresh-on-hit and
//! expiry-status read paths, `cache_get_or_set_with`, `Debug`, `deep_clone` propagation,
//! and a concurrency stress that flips the ttl while other threads read and write.

#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{
    ConcurrentCacheEvict, ConcurrentCacheTtl, ConcurrentCached, ConcurrentCloneCached,
    ShardedLruTtlCache, ShardedTtlCache,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ─────────────────────────────────────────────────────────────────────────────
// State-transition cycle + prior-value semantics
//
// A zero ttl is the disabled sentinel, so `set_ttl(0)` and `unset_ttl()` are observably
// identical: both store the disabled state and the cache's ttl resolves to `None`.
// Each `set_ttl`/`unset_ttl` must return the *prior* resolved ttl, where the disabled
// state (zero or unset) reads back as `None` and a non-zero ttl reads back as `Some(d)`.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_state_transition_cycle_returns_prior_ttl() {
    let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    // set(nonzero): prior is the builder's 60s.
    assert_eq!(
        cache.set_ttl(Duration::from_secs(30)),
        Some(Duration::from_secs(60)),
        "set_ttl must return the builder ttl as prior"
    );
    assert_eq!(cache.ttl(), Some(Duration::from_secs(30)));

    // set(zero): prior is the previous 30s; ttl now resolves to None (disabled).
    assert_eq!(
        cache.set_ttl(Duration::ZERO),
        Some(Duration::from_secs(30)),
        "set_ttl(ZERO) must return the prior non-zero ttl"
    );
    assert_eq!(
        cache.ttl(),
        None,
        "a zero ttl disables expiry — ttl resolves to None"
    );

    // unset from the disabled state: prior is None (already disabled).
    assert_eq!(
        cache.unset_ttl(),
        None,
        "unset_ttl after a zero set must report None as prior (already disabled)"
    );
    assert_eq!(cache.ttl(), None, "after unset, ttl resolves to None");

    // set(zero) again from the disabled state: prior is None (idempotent disable).
    assert_eq!(
        cache.set_ttl(Duration::ZERO),
        None,
        "set_ttl(ZERO) from the disabled state must report None as prior"
    );
    assert_eq!(cache.ttl(), None);

    // set(nonzero) from a disabled state: prior is None (was disabled).
    assert_eq!(
        cache.set_ttl(Duration::from_secs(5)),
        None,
        "set_ttl(nonzero) from a disabled state must report None as prior"
    );
    assert_eq!(cache.ttl(), Some(Duration::from_secs(5)));

    // unset from a nonzero state: prior is Some(5s).
    assert_eq!(
        cache.unset_ttl(),
        Some(Duration::from_secs(5)),
        "unset_ttl must report the prior non-zero ttl"
    );
    assert_eq!(cache.ttl(), None);

    // unset again from the already-disabled state: prior is None (idempotent).
    assert_eq!(
        cache.unset_ttl(),
        None,
        "unset_ttl on an already-disabled cache must report None"
    );
}

#[test]
fn sharded_lru_ttl_state_transition_cycle_returns_prior_ttl() {
    let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    assert_eq!(
        cache.set_ttl(Duration::from_secs(30)),
        Some(Duration::from_secs(60))
    );
    assert_eq!(cache.set_ttl(Duration::ZERO), Some(Duration::from_secs(30)));
    assert_eq!(cache.ttl(), None, "zero ttl disables expiry");
    assert_eq!(cache.unset_ttl(), None, "already disabled");
    assert_eq!(cache.ttl(), None);
    assert_eq!(cache.set_ttl(Duration::ZERO), None);
    assert_eq!(cache.ttl(), None);
    assert_eq!(
        cache.set_ttl(Duration::from_secs(5)),
        None,
        "set_ttl(nonzero) from a disabled state must report None as prior"
    );
    assert_eq!(cache.unset_ttl(), Some(Duration::from_secs(5)));
    assert_eq!(cache.unset_ttl(), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// set_ttl(0) is observably equivalent to unset_ttl(): a just-inserted entry survives.
//
// The entry inserted under a live ttl must remain present after disabling expiry via
// either route, and a newly inserted entry must also persist (never expires).
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_set_zero_disables_expiry_like_unset() {
    let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    cache.cache_set(1, 10).unwrap();
    // Disable via set_ttl(0): the existing entry must remain live.
    cache.set_ttl(Duration::ZERO);
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "set_ttl(0) must NOT expire a just-inserted entry"
    );
    cache.cache_set(2, 20).unwrap();
    assert_eq!(
        cache.cache_get(&2),
        Ok(Some(20)),
        "entries inserted under a disabled ttl never expire"
    );

    // Re-arm with a real ttl, then disable again via unset_ttl(): same observable result.
    cache.set_ttl(Duration::from_secs(60));
    cache.unset_ttl();
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "unset_ttl must also keep entries live"
    );
}

#[test]
fn sharded_lru_ttl_set_zero_disables_expiry_like_unset() {
    let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .max_size(8)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    cache.cache_set(1, 10).unwrap();
    cache.set_ttl(Duration::ZERO);
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "set_ttl(0) must NOT expire a just-inserted LRU entry"
    );

    cache.set_ttl(Duration::from_secs(60));
    cache.unset_ttl();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)));
}

// ─────────────────────────────────────────────────────────────────────────────
// Re-arming expiry: a non-zero set after a disabled ttl makes FUTURE inserts
// expirable, but entries inserted while TTL was disabled keep expires_at = None.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_set_nonzero_after_disable_only_affects_future_inserts() {
    let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    cache.set_ttl(Duration::ZERO);
    // Entry 1 is inserted while TTL is disabled: gets expires_at = None.
    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)));

    // Re-arm with a short ttl: only FUTURE inserts get a real expires_at.
    cache.set_ttl(Duration::from_millis(20));
    // Entry 2 is inserted now with the short TTL: gets expires_at = now + 20ms.
    cache.cache_set(2, 20).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(60));

    // Entry 1 (inserted under disabled TTL) must still be live (expires_at = None).
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "entry inserted under disabled ttl keeps expires_at=None; must survive re-arming"
    );
    // Entry 2 (inserted under the short TTL) must have expired.
    assert_eq!(
        cache.cache_get(&2),
        Ok(None),
        "entry inserted after set_ttl(nonzero) must expire at the new deadline"
    );
}

#[test]
fn sharded_lru_ttl_set_nonzero_after_disable_only_affects_future_inserts() {
    let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .max_size(8)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    cache.set_ttl(Duration::ZERO);
    // Entry 1 inserted while disabled: expires_at = None.
    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)));

    cache.set_ttl(Duration::from_millis(20));
    // Entry 2 inserted after re-arming: expires_at = now + 20ms.
    cache.cache_set(2, 20).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(60));

    // Entry 1 (expires_at = None) must still be live.
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "disabled-TTL entry keeps expires_at=None; must survive re-arming"
    );
    // Entry 2 must have expired.
    assert_eq!(
        cache.cache_get(&2),
        Ok(None),
        "entry inserted after set_ttl(nonzero) must expire at the new deadline on LRU store"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// evict() / ConcurrentCacheEvict is a no-op under a disabled (zero) ttl.
//
// With expiry disabled, no entry is ever expired, so an explicit sweep removes nothing
// and does not fire on_evict — identical to the unset case.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_evict_is_noop_under_zero_ttl() {
    let count = Arc::new(AtomicU64::new(0));
    let count2 = count.clone();
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .shards(1)
        .on_evict(move |_, _| {
            count2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .expect("build ShardedTtlCache");

    for i in 0..10u32 {
        cache.cache_set(i, i).unwrap();
    }
    cache.set_ttl(Duration::ZERO);

    let removed = ConcurrentCacheEvict::evict(&cache);
    assert_eq!(removed, 0, "evict under a disabled ttl must remove nothing");
    assert_eq!(
        count.load(Ordering::Relaxed),
        0,
        "on_evict must not fire under a disabled ttl"
    );
    assert_eq!(cache.metrics().evictions, Some(0));
    assert_eq!(cache.len(), 10, "all entries survive a disabled-ttl evict");
}

#[test]
fn sharded_lru_ttl_evict_is_noop_under_zero_ttl() {
    let count = Arc::new(AtomicU64::new(0));
    let count2 = count.clone();
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(64)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .on_evict(move |_, _| {
            count2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .expect("build ShardedLruTtlCache");

    for i in 0..10u32 {
        cache.cache_set(i, i).unwrap();
    }
    cache.set_ttl(Duration::ZERO);

    let removed = ConcurrentCacheEvict::evict(&cache);
    assert_eq!(removed, 0, "evict under a disabled ttl must remove nothing");
    assert_eq!(
        count.load(Ordering::Relaxed),
        0,
        "on_evict must not fire under a disabled ttl"
    );
    assert_eq!(cache.len(), 10);
}

// evict() under unset ttl must also be a no-op — confirms set_ttl(0) and unset_ttl()
// behave identically for the explicit sweep.
#[test]
fn sharded_ttl_evict_under_unset_ttl_is_noop() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .shards(1)
        .build()
        .expect("build ShardedTtlCache");
    for i in 0..5u32 {
        cache.cache_set(i, i).unwrap();
    }
    cache.unset_ttl();
    assert_eq!(
        ConcurrentCacheEvict::evict(&cache),
        0,
        "evict under unset ttl must remove nothing"
    );
    assert_eq!(cache.len(), 5, "entries must survive an unset-ttl evict");
}

// ─────────────────────────────────────────────────────────────────────────────
// cache_remove / cache_remove_entry under a disabled (zero) ttl.
//
// A disabled-ttl entry is live (never expired): cache_remove must return Some(v),
// and cache_remove_entry must return Some too.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_remove_under_zero_ttl_returns_live_value() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .shards(1)
        .build()
        .expect("build ShardedTtlCache");
    cache.cache_set(1, 100).unwrap();
    cache.cache_set(2, 200).unwrap();
    cache.set_ttl(Duration::ZERO);

    assert_eq!(
        cache.cache_remove(&1),
        Ok(Some(100)),
        "cache_remove must return the live value for a disabled-ttl entry"
    );
    assert_eq!(
        cache.cache_remove_entry(&2),
        Ok(Some((2, 200))),
        "cache_remove_entry must return Some for a disabled-ttl entry"
    );
}

#[test]
fn sharded_lru_ttl_remove_under_zero_ttl_returns_live_value() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(64)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");
    cache.cache_set(1, 100).unwrap();
    cache.cache_set(2, 200).unwrap();
    cache.set_ttl(Duration::ZERO);

    assert_eq!(
        cache.cache_remove(&1),
        Ok(Some(100)),
        "cache_remove must return the live value for a disabled-ttl LRU entry"
    );
    assert_eq!(
        cache.cache_remove_entry(&2),
        Ok(Some((2, 200))),
        "cache_remove_entry must return Some for a disabled-ttl LRU entry"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// LruTtl-specific: refresh_on_hit under a disabled ttl keeps the entry live.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_lru_ttl_refresh_on_hit_keeps_zero_ttl_entry_live() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .shards(1)
        .refresh_on_hit(true)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)), "live before disable");

    cache.set_ttl(Duration::ZERO);
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "with expiry disabled, the entry stays live across hits"
    );
    assert_eq!(cache.len(), 1, "disabled-ttl entry must not be removed");
}

#[test]
fn sharded_ttl_refresh_on_hit_keeps_zero_ttl_entry_live() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .shards(1)
        .refresh_on_hit(true)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)));

    cache.set_ttl(Duration::ZERO);
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "with expiry disabled, the entry stays live across hits"
    );
    assert_eq!(cache.len(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// ConcurrentCloneCached expiry-status reads under a disabled (zero) ttl.
//
// cache_get_with_expiry_status / cache_peek_with_expiry_status must report
// (Some(v), false) — the entry is live (never expired) — with no side effects.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_expiry_status_reads_report_zero_ttl_as_live() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    cache.cache_set(1, 42).unwrap();
    cache.set_ttl(Duration::ZERO);

    let (val, expired) = ConcurrentCloneCached::cache_get_with_expiry_status(&cache, &1);
    assert_eq!(val, Some(42), "expiry-status read must return the value");
    assert!(!expired, "disabled-ttl entry must report expired=false");

    let (pval, pexpired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &1);
    assert_eq!(pval, Some(42));
    assert!(!pexpired, "peek must report a disabled-ttl entry as live");
    assert_eq!(
        cache.metrics().evictions,
        Some(0),
        "no eviction from status reads"
    );
}

#[test]
fn sharded_lru_ttl_expiry_status_reads_report_zero_ttl_as_live() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");
    cache.cache_set(1, 42).unwrap();
    cache.set_ttl(Duration::ZERO);

    let (val, expired) = ConcurrentCloneCached::cache_get_with_expiry_status(&cache, &1);
    assert_eq!(val, Some(42), "expiry-status read must return the value");
    assert!(!expired, "disabled-ttl LRU entry must report expired=false");

    let (pval, pexpired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &1);
    assert_eq!(pval, Some(42));
    assert!(
        !pexpired,
        "peek must report a disabled-ttl LRU entry as live"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// cache_get_or_set_with under a disabled (zero) ttl.
//
// The first call computes and stores the value; subsequent calls hit the live entry,
// so the closure runs exactly once.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_get_or_set_with_caches_under_zero_ttl() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    cache.set_ttl(Duration::ZERO);

    let calls = Arc::new(AtomicU64::new(0));
    for _ in 0..3 {
        let calls = calls.clone();
        let v = cache
            .cache_get_or_set_with(1, || {
                calls.fetch_add(1, Ordering::Relaxed);
                7
            })
            .unwrap();
        assert_eq!(v, 7, "closure value is returned each time");
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "disabled ttl keeps the entry live -> closure runs once"
    );
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(7)),
        "the entry persists under a disabled ttl"
    );
}

#[test]
fn sharded_lru_ttl_get_or_set_with_caches_under_zero_ttl() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");
    cache.set_ttl(Duration::ZERO);

    let calls = Arc::new(AtomicU64::new(0));
    for _ in 0..3 {
        let calls = calls.clone();
        let v = cache
            .cache_get_or_set_with(1, || {
                calls.fetch_add(1, Ordering::Relaxed);
                7
            })
            .unwrap();
        assert_eq!(v, 7);
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "disabled ttl keeps the entry live on the LRU store too"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Debug output: both a zero ttl and an unset ttl resolve to None and print as None.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_debug_prints_disabled_ttl_as_none() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    cache.set_ttl(Duration::ZERO);
    let zero_dbg = format!("{cache:?}");
    assert!(
        zero_dbg.contains("ttl: None"),
        "a disabled (zero) ttl must Debug-print as None, got: {zero_dbg}"
    );

    cache.unset_ttl();
    let unset_dbg = format!("{cache:?}");
    assert!(
        unset_dbg.contains("ttl: None"),
        "unset ttl must Debug-print as None, got: {unset_dbg}"
    );

    // A real ttl prints as Some(..).
    cache.set_ttl(Duration::from_secs(5));
    let some_dbg = format!("{cache:?}");
    assert!(
        some_dbg.contains("ttl: Some("),
        "a non-zero ttl must Debug-print as Some(..), got: {some_dbg}"
    );
}

#[test]
fn sharded_lru_ttl_debug_prints_disabled_ttl_as_none() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    cache.set_ttl(Duration::ZERO);
    let zero_dbg = format!("{cache:?}");
    assert!(
        zero_dbg.contains("ttl: None"),
        "a disabled (zero) ttl must Debug-print as None, got: {zero_dbg}"
    );

    cache.unset_ttl();
    let unset_dbg = format!("{cache:?}");
    assert!(
        unset_dbg.contains("ttl: None"),
        "unset ttl must Debug-print as None, got: {unset_dbg}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// deep_clone carries the disabled ttl: a zero-ttl source clones to a never-expire cache,
// identical to an unset source.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_deep_clone_carries_disabled_ttl() {
    let src = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    src.set_ttl(Duration::ZERO);

    let clone = src.deep_clone();
    assert_eq!(
        clone.ttl(),
        None,
        "deep_clone must carry the disabled ttl (resolves to None)"
    );
    clone.cache_set(1, 10).unwrap();
    assert_eq!(
        clone.cache_get(&1),
        Ok(Some(10)),
        "the deep-cloned disabled-ttl cache must never expire entries"
    );
}

#[test]
fn sharded_ttl_deep_clone_carries_unset_ttl() {
    let src = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    src.unset_ttl();

    let clone = src.deep_clone();
    assert_eq!(
        clone.ttl(),
        None,
        "deep_clone of an unset cache must stay unset"
    );
    clone.cache_set(1, 10).unwrap();
    assert_eq!(
        clone.cache_get(&1),
        Ok(Some(10)),
        "the deep-cloned unset cache must never expire"
    );
}

#[test]
fn sharded_ttl_deep_clone_carries_nonzero_ttl() {
    let src = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    src.set_ttl(Duration::from_secs(30));

    let clone = src.deep_clone();
    assert_eq!(
        clone.ttl(),
        Some(Duration::from_secs(30)),
        "deep_clone must carry a non-zero ttl unchanged"
    );
}

#[test]
fn sharded_lru_ttl_deep_clone_carries_disabled_and_nonzero_ttl() {
    let src = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    src.set_ttl(Duration::ZERO);
    let zclone = src.deep_clone();
    assert_eq!(
        zclone.ttl(),
        None,
        "deep_clone must carry the disabled ttl on the LRU store"
    );
    zclone.cache_set(1, 10).unwrap();
    assert_eq!(zclone.cache_get(&1), Ok(Some(10)));

    src.set_ttl(Duration::from_secs(30));
    let nclone = src.deep_clone();
    assert_eq!(
        nclone.ttl(),
        Some(Duration::from_secs(30)),
        "deep_clone must carry a non-zero ttl on the LRU store"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Concurrency: one thread flips set_ttl(nonzero) / set_ttl(ZERO) / unset_ttl while
// others insert and read. Asserts no panic / no deadlock, reads never return another
// key's value, and the cache stays usable and metrics internally sane afterward.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_concurrent_ttl_flips_stay_consistent() {
    let cache = Arc::new(
        ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .shards(4)
            .build()
            .expect("build ShardedTtlCache"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    const KEYS: u32 = 64;
    const ITERS: usize = 2_000;

    let mut handles = Vec::new();

    // Flipper: cycles through nonzero -> zero(disabled) -> unset(disabled).
    {
        let cache = cache.clone();
        let stop = stop.clone();
        handles.push(std::thread::spawn(move || {
            let mut i = 0usize;
            while !stop.load(Ordering::Relaxed) {
                match i % 3 {
                    0 => {
                        cache.set_ttl(Duration::from_secs(60));
                    }
                    1 => {
                        cache.set_ttl(Duration::ZERO);
                    }
                    _ => {
                        cache.unset_ttl();
                    }
                }
                i += 1;
            }
        }));
    }

    // Writers.
    for w in 0..2 {
        let cache = cache.clone();
        handles.push(std::thread::spawn(move || {
            for n in 0..ITERS {
                let k = ((n as u32).wrapping_add(w)) % KEYS;
                cache.cache_set(k, k.wrapping_mul(10)).unwrap();
            }
        }));
    }

    // Readers — each read returns Ok (Infallible); a present value is always this key's.
    for _ in 0..2 {
        let cache = cache.clone();
        handles.push(std::thread::spawn(move || {
            for n in 0..ITERS {
                let k = (n as u32) % KEYS;
                let got = cache.cache_get(&k).expect("infallible");
                if let Some(v) = got {
                    assert_eq!(v, k.wrapping_mul(10), "read must return this key's value");
                }
            }
        }));
    }

    std::thread::sleep(std::time::Duration::from_millis(50));
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().expect("no thread panicked");
    }

    // Settle to a known ttl and verify the cache is still usable and consistent.
    cache.set_ttl(Duration::from_secs(60));
    cache.clear();
    cache.cache_set(1, 10).unwrap();
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "cache must remain usable after concurrent ttl flips"
    );

    let m = cache.metrics();
    let entry_count = m
        .entry_count
        .expect("sharded stores report an exact entry count");
    assert!(
        entry_count <= KEYS as usize,
        "size must never exceed the distinct key count, got {entry_count}"
    );
}

#[test]
fn sharded_lru_ttl_concurrent_ttl_flips_stay_consistent() {
    let cache = Arc::new(
        ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(256)
            .shards(4)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedLruTtlCache"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    const KEYS: u32 = 64;
    const ITERS: usize = 2_000;

    let mut handles = Vec::new();

    {
        let cache = cache.clone();
        let stop = stop.clone();
        handles.push(std::thread::spawn(move || {
            let mut i = 0usize;
            while !stop.load(Ordering::Relaxed) {
                match i % 3 {
                    0 => {
                        cache.set_ttl(Duration::from_secs(60));
                    }
                    1 => {
                        cache.set_ttl(Duration::ZERO);
                    }
                    _ => {
                        cache.unset_ttl();
                    }
                }
                i += 1;
            }
        }));
    }

    for w in 0..2 {
        let cache = cache.clone();
        handles.push(std::thread::spawn(move || {
            for n in 0..ITERS {
                let k = ((n as u32).wrapping_add(w)) % KEYS;
                cache.cache_set(k, k.wrapping_mul(10)).unwrap();
            }
        }));
    }

    for _ in 0..2 {
        let cache = cache.clone();
        handles.push(std::thread::spawn(move || {
            for n in 0..ITERS {
                let k = (n as u32) % KEYS;
                let got = cache.cache_get(&k).expect("infallible");
                if let Some(v) = got {
                    assert_eq!(v, k.wrapping_mul(10), "read must return this key's value");
                }
            }
        }));
    }

    std::thread::sleep(std::time::Duration::from_millis(50));
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().expect("no thread panicked");
    }

    cache.set_ttl(Duration::from_secs(60));
    cache.clear();
    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)));

    let m = cache.metrics();
    let entry_count = m
        .entry_count
        .expect("sharded stores report an exact entry count");
    assert!(
        entry_count <= KEYS as usize,
        "size must never exceed the distinct key count, got {entry_count}"
    );
}
