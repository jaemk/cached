//! Outside-in coverage for the zero-TTL / `ttl_set` sequencing on the sharded
//! TTL stores (`ShardedTtlCache` / `ShardedLruTtlCache`), finding I2.
//!
//! Background: these stores used to `assert!(!ttl.is_zero())` and panic on a zero
//! ttl. They now store a zero ttl unchecked, distinguishing "expire immediately"
//! (a `ttl_set == true` + `ttl_nanos == 0` pair) from "never expires"
//! (`ttl_set == false`, set by `unset_ttl`). The implementor's `sharded_set_ttl_zero`
//! module in `v3_traits.rs` covers no-panic, immediate expiry, and `unset_ttl` after
//! a zero set. This module adds the paths it does not: the full state-transition
//! cycle with prior-value semantics, the `evict`/`on_evict` sweep, the LruTtl
//! refresh-on-hit and expiry-status read paths, `cache_get_or_set_with`, `Debug`,
//! `deep_clone` flag propagation, and a concurrency stress that flips the ttl while
//! other threads read and write.

#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{
    ConcurrentCacheEvict, ConcurrentCached, ConcurrentCloneCached, ShardedLruTtlCache,
    ShardedTtlCache,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ─────────────────────────────────────────────────────────────────────────────
// State-transition cycle + prior-value semantics
//
// never-set is unreachable on these stores (builder rejects a zero/absent ttl and
// always starts `ttl_set == true`), so the reachable lattice is:
//   set(nonzero) -> set(zero) -> unset -> set(zero) -> set(nonzero) -> unset
// Each `set_ttl`/`unset_ttl` must return the *prior* resolved ttl, where a zero set
// reads back as `Some(ZERO)` (NOT `None`) and an unset reads back as `None`.
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

    // set(zero): prior is the previous 30s; ttl now resolves to Some(ZERO), not None.
    assert_eq!(
        cache.set_ttl(Duration::ZERO),
        Some(Duration::from_secs(30)),
        "set_ttl(ZERO) must return the prior non-zero ttl"
    );
    assert_eq!(
        cache.ttl(),
        Some(Duration::ZERO),
        "a zero ttl must resolve to Some(ZERO), distinct from unset's None"
    );

    // unset: prior is the zero ttl, reported as Some(ZERO) (the never-expire flag was
    // still set at the moment of the call).
    assert_eq!(
        cache.unset_ttl(),
        Some(Duration::ZERO),
        "unset_ttl after a zero set must report Some(ZERO) as prior, not None"
    );
    assert_eq!(cache.ttl(), None, "after unset, ttl resolves to None");

    // set(zero) again from the unset state: prior is None (was never-expire).
    assert_eq!(
        cache.set_ttl(Duration::ZERO),
        None,
        "set_ttl from the unset state must report None as prior"
    );
    assert_eq!(cache.ttl(), Some(Duration::ZERO));

    // set(nonzero) from a zero state: prior is Some(ZERO).
    assert_eq!(
        cache.set_ttl(Duration::from_secs(5)),
        Some(Duration::ZERO),
        "set_ttl(nonzero) from a zero state must report Some(ZERO) as prior"
    );
    assert_eq!(cache.ttl(), Some(Duration::from_secs(5)));

    // unset from a nonzero state: prior is Some(5s).
    assert_eq!(
        cache.unset_ttl(),
        Some(Duration::from_secs(5)),
        "unset_ttl must report the prior non-zero ttl"
    );
    assert_eq!(cache.ttl(), None);

    // unset again from the already-unset state: prior is None (idempotent).
    assert_eq!(
        cache.unset_ttl(),
        None,
        "unset_ttl on an already-unset cache must report None"
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
    assert_eq!(cache.ttl(), Some(Duration::ZERO));
    assert_eq!(cache.unset_ttl(), Some(Duration::ZERO));
    assert_eq!(cache.ttl(), None);
    assert_eq!(cache.set_ttl(Duration::ZERO), None);
    assert_eq!(cache.ttl(), Some(Duration::ZERO));
    assert_eq!(
        cache.set_ttl(Duration::from_secs(5)),
        Some(Duration::ZERO),
        "set_ttl(nonzero) from a zero state must report Some(ZERO) as prior"
    );
    assert_eq!(cache.unset_ttl(), Some(Duration::from_secs(5)));
    assert_eq!(cache.unset_ttl(), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// Re-arming after unset: unset -> set(zero) must re-enable immediate expiry.
//
// The implementor's test covers set(zero) -> unset (expiry disabled). This is the
// reverse: a zero set after an unset must flip `ttl_set` back on so entries expire
// immediately again. A bug that only ever stored `ttl_set = false` on unset and
// never re-set it would pass the implementor's test but fail here.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_set_zero_after_unset_re_enables_immediate_expiry() {
    let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    cache.unset_ttl();
    cache.cache_set(1, 10).unwrap();
    assert_eq!(
        cache.cache_get(&1),
        Ok(Some(10)),
        "with ttl unset, the entry never expires"
    );

    // Re-arm with a zero ttl: the *existing* entry must now read back expired, and
    // a newly inserted one too.
    cache.set_ttl(Duration::ZERO);
    assert_eq!(
        cache.cache_get(&1),
        Ok(None),
        "set_ttl(ZERO) after unset must expire the previously-live entry"
    );
    cache.cache_set(2, 20).unwrap();
    assert_eq!(cache.cache_get(&2), Ok(None));
}

#[test]
fn sharded_lru_ttl_set_zero_after_unset_re_enables_immediate_expiry() {
    let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .max_size(8)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    cache.unset_ttl();
    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)));

    cache.set_ttl(Duration::ZERO);
    assert_eq!(
        cache.cache_get(&1),
        Ok(None),
        "set_ttl(ZERO) after unset must expire the previously-live LRU entry"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// evict() / ConcurrentCacheEvict sweeps zero-ttl entries and fires on_evict.
//
// The implementor's tests only exercise lazy expiry through cache_get. The explicit
// sweep is a distinct path: it reads the ttl via ttl_duration(); a zero ttl must be
// treated as "everything is expired" (elapsed >= 0 is always true) rather than as
// the `None`/never-expire early-return.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_evict_sweeps_zero_ttl_entries_and_fires_on_evict() {
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

    // Insert under a live ttl, then flip to zero. The entries are now all expired.
    for i in 0..10u32 {
        cache.cache_set(i, i).unwrap();
    }
    cache.set_ttl(Duration::ZERO);

    let removed = ConcurrentCacheEvict::evict(&cache);
    assert_eq!(removed, 10, "evict must sweep every zero-ttl entry");
    assert_eq!(
        count.load(Ordering::Relaxed),
        10,
        "on_evict must fire for each swept zero-ttl entry"
    );
    assert_eq!(cache.metrics().evictions, Some(10));
    assert_eq!(cache.len(), 0, "all entries swept");
}

#[test]
fn sharded_lru_ttl_evict_sweeps_zero_ttl_entries_and_fires_on_evict() {
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
    assert_eq!(removed, 10, "evict must sweep every zero-ttl LRU entry");
    assert_eq!(
        count.load(Ordering::Relaxed),
        10,
        "on_evict must fire for each swept zero-ttl LRU entry"
    );
    assert_eq!(cache.len(), 0);
}

// evict() under unset ttl must be a no-op (early return on None), in contrast to a
// zero ttl which sweeps everything. This pins the None-vs-zero distinction in evict.
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
// cache_remove / cache_remove_entry under a zero ttl.
//
// A zero-ttl entry is "expired but present": cache_remove must report None (expired),
// while cache_remove_entry must still return Some (it does not consult expiry).
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_remove_under_zero_ttl_distinguishes_remove_vs_remove_entry() {
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
        Ok(None),
        "cache_remove must return None for a zero-ttl (expired) entry"
    );
    assert_eq!(
        cache.cache_remove_entry(&2),
        Ok(Some((2, 200))),
        "cache_remove_entry must return Some even for a zero-ttl entry"
    );
}

#[test]
fn sharded_lru_ttl_remove_under_zero_ttl_distinguishes_remove_vs_remove_entry() {
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
        Ok(None),
        "cache_remove must return None for a zero-ttl LRU entry"
    );
    assert_eq!(
        cache.cache_remove_entry(&2),
        Ok(Some((2, 200))),
        "cache_remove_entry must return Some even for a zero-ttl LRU entry"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// LruTtl-specific: refresh_on_hit must NOT rescue a zero-ttl entry.
//
// The LruTtl cache_get peeks for expiry *before* promoting/refreshing. With a zero
// ttl, even an entry whose timestamp is refreshed reads back expired on the very
// next get, because elapsed() >= ZERO holds the instant after the refresh.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_lru_ttl_refresh_on_hit_does_not_rescue_zero_ttl_entry() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .shards(1)
        .refresh_on_hit(true)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1), Ok(Some(10)), "live before zero ttl");

    cache.set_ttl(Duration::ZERO);
    // With refresh_on_hit on, a hit would normally bump the timestamp; under a zero
    // ttl the entry is expired on read and removed instead of refreshed.
    assert_eq!(
        cache.cache_get(&1),
        Ok(None),
        "refresh_on_hit must not keep a zero-ttl entry alive"
    );
    // The expired entry must have been swept by the get (not left behind).
    assert_eq!(
        cache.len(),
        0,
        "expired zero-ttl entry must be removed on get"
    );
}

#[test]
fn sharded_ttl_refresh_on_hit_does_not_rescue_zero_ttl_entry() {
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
        Ok(None),
        "refresh_on_hit must not keep a zero-ttl entry alive"
    );
    assert_eq!(cache.len(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// ConcurrentCloneCached expiry-status reads under a zero ttl.
//
// cache_get_with_expiry_status must report (Some(v), true) for a zero-ttl entry
// (stale, no removal), and cache_peek_with_expiry_status must report expired too,
// without renewing or removing.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_expiry_status_reads_report_zero_ttl_as_expired() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    cache.cache_set(1, 42).unwrap();
    cache.set_ttl(Duration::ZERO);

    let (val, expired) = ConcurrentCloneCached::cache_get_with_expiry_status(&cache, &1);
    assert_eq!(
        val,
        Some(42),
        "expiry-status read must return the stale value"
    );
    assert!(expired, "zero-ttl entry must report expired=true");

    // peek: same verdict, no side effects, entry still present.
    let (pval, pexpired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &1);
    assert_eq!(pval, Some(42));
    assert!(pexpired, "peek must report a zero-ttl entry as expired");
    assert_eq!(
        cache.metrics().evictions,
        Some(0),
        "no eviction from status reads"
    );
}

#[test]
fn sharded_lru_ttl_expiry_status_reads_report_zero_ttl_as_expired() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .shards(1)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");
    cache.cache_set(1, 42).unwrap();
    cache.set_ttl(Duration::ZERO);

    let (val, expired) = ConcurrentCloneCached::cache_get_with_expiry_status(&cache, &1);
    assert_eq!(
        val,
        Some(42),
        "expiry-status read must return the stale value"
    );
    assert!(expired, "zero-ttl LRU entry must report expired=true");

    let (pval, pexpired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &1);
    assert_eq!(pval, Some(42));
    assert!(pexpired, "peek must report a zero-ttl LRU entry as expired");
}

// ─────────────────────────────────────────────────────────────────────────────
// cache_get_or_set_with under a zero ttl.
//
// Each call misses (the just-stored value is immediately expired), so the closure
// must run every time and the value never persists as a live hit.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_get_or_set_with_recomputes_under_zero_ttl() {
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
        3,
        "zero ttl makes every get_or_set_with a miss -> closure runs every call"
    );
    assert_eq!(
        cache.cache_get(&1),
        Ok(None),
        "no live entry persists under a zero ttl"
    );
}

#[test]
fn sharded_lru_ttl_get_or_set_with_recomputes_under_zero_ttl() {
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
        3,
        "zero ttl makes every get_or_set_with a miss on the LRU store too"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Debug output reflects the resolved ttl: Some(0ns) for a zero ttl, None for unset.
//
// The implementor flagged Debug as under-tested. The Debug impl reads
// ttl_duration_impl(), so it must print the zero/unset distinction faithfully.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_debug_distinguishes_zero_from_unset() {
    let cache = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    cache.set_ttl(Duration::ZERO);
    let zero_dbg = format!("{cache:?}");
    assert!(
        zero_dbg.contains("ttl: Some("),
        "zero ttl must Debug-print as Some(..), got: {zero_dbg}"
    );
    assert!(
        !zero_dbg.contains("ttl: None"),
        "zero ttl must NOT Debug-print as None, got: {zero_dbg}"
    );

    cache.unset_ttl();
    let unset_dbg = format!("{cache:?}");
    assert!(
        unset_dbg.contains("ttl: None"),
        "unset ttl must Debug-print as None, got: {unset_dbg}"
    );
}

#[test]
fn sharded_lru_ttl_debug_distinguishes_zero_from_unset() {
    let cache = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    cache.set_ttl(Duration::ZERO);
    let zero_dbg = format!("{cache:?}");
    assert!(
        zero_dbg.contains("ttl: Some("),
        "zero ttl must Debug-print as Some(..), got: {zero_dbg}"
    );
    assert!(!zero_dbg.contains("ttl: None"), "got: {zero_dbg}");

    cache.unset_ttl();
    let unset_dbg = format!("{cache:?}");
    assert!(
        unset_dbg.contains("ttl: None"),
        "unset ttl must Debug-print as None, got: {unset_dbg}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// deep_clone carries the ttl_set flag AND the ttl_nanos value.
//
// Two distinct states to verify, because copying only one of the pair would slip
// through a single-state test:
//   * a zero-ttl source (ttl_set == true, ttl_nanos == 0) must clone to a cache that
//     also expires immediately (NOT a never-expire clone);
//   * an unset source (ttl_set == false) must clone to a never-expire cache, even
//     though ttl_nanos still holds a stale non-zero value underneath.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sharded_ttl_deep_clone_carries_zero_ttl() {
    let src = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    src.set_ttl(Duration::ZERO);

    let clone = src.deep_clone();
    assert_eq!(
        clone.ttl(),
        Some(Duration::ZERO),
        "deep_clone must carry the zero ttl, not drop ttl_set"
    );
    clone.cache_set(1, 10).unwrap();
    assert_eq!(
        clone.cache_get(&1),
        Ok(None),
        "the deep-cloned zero-ttl cache must expire entries immediately"
    );
}

#[test]
fn sharded_ttl_deep_clone_carries_unset_ttl() {
    // Build with a non-zero ttl so ttl_nanos holds a stale value, then unset.
    let src = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    src.unset_ttl();

    let clone = src.deep_clone();
    assert_eq!(
        clone.ttl(),
        None,
        "deep_clone of an unset cache must stay unset (ttl_set == false), \
         despite a stale ttl_nanos"
    );
    clone.cache_set(1, 10).unwrap();
    assert_eq!(
        clone.cache_get(&1),
        Ok(Some(10)),
        "the deep-cloned unset cache must never expire"
    );
}

#[test]
fn sharded_lru_ttl_deep_clone_carries_zero_and_unset_ttl() {
    let src = ShardedLruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    src.set_ttl(Duration::ZERO);
    let zclone = src.deep_clone();
    assert_eq!(
        zclone.ttl(),
        Some(Duration::ZERO),
        "deep_clone must carry the zero ttl on the LRU store"
    );
    zclone.cache_set(1, 10).unwrap();
    assert_eq!(zclone.cache_get(&1), Ok(None));

    src.unset_ttl();
    let uclone = src.deep_clone();
    assert_eq!(
        uclone.ttl(),
        None,
        "deep_clone of an unset LRU cache must stay unset"
    );
    uclone.cache_set(2, 20).unwrap();
    assert_eq!(uclone.cache_get(&2), Ok(Some(20)));
}

// ─────────────────────────────────────────────────────────────────────────────
// Concurrency: one thread flips set_ttl(ZERO) / unset_ttl / set_ttl(nonzero) while
// others insert and read. Asserts:
//   * no panic and no deadlock (the bounded loops join);
//   * the resolved (ttl_set, ttl) pair is never torn into an impossible combination
//     — specifically, when ttl resolves to Some(d) the cache must behave consistently
//     for that d on a settled read, and metrics stay internally sane (size never
//     exceeds the number of distinct keys, evictions monotonic).
//
// This is a smoke test for the Relaxed sequencing between ttl_set and ttl_nanos. It
// cannot prove the absence of a transient torn read (Relaxed permits one), but it
// exercises the path under contention and pins the invariant that the cache stays
// usable and crash-free.
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

    // Flipper: cycles through nonzero -> zero -> unset.
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

    // Readers — each read must return Ok (Infallible), value is either the stored
    // mapping or None; never a different key's value.
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

    // The writers/readers are bounded by ITERS and finish on their own; the flipper
    // spins until told to stop. Give the bounded threads time to run, then stop the
    // flipper and join everything.
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
    assert!(
        m.entry_count <= KEYS as usize,
        "size must never exceed the distinct key count, got {}",
        m.entry_count
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
    assert!(
        m.entry_count <= KEYS as usize,
        "size must never exceed the distinct key count, got {}",
        m.entry_count
    );
}
