/*!
Refresh-on-hit is a separate capability from the global TTL.

`CacheRefreshOnHit` / `ConcurrentCacheRefreshOnHit` carry `refresh_on_hit` and
`set_refresh_on_hit`; `CacheTtl` / `ConcurrentCacheTtl` keep only `ttl`, `set_ttl`,
`try_set_ttl`, and `unset_ttl`.

The split exists because `TtlSortedCache` cannot honour refresh-on-hit. Its entries live in a
deadline-ordered index, so pushing one entry's expiry forward on a read would leave that index
unsorted. While the capability was part of `CacheTtl`, the store satisfied it with a
`set_refresh_on_hit` that ignored its argument and returned `false` unconditionally -- a result a
generic caller cannot tell apart from "the flag was already off". These tests pin the new shape:
the two stores that can refresh implement the capability trait and honour the documented
"returns the previous value" contract, `TtlSortedCache` does not implement it at all (so the
mistake is a compile error), and `TtlSortedCache` still implements `CacheTtl` in full.

No Redis server or redb file is required: the concurrent implementor set is asserted at the type
level.
*/

#![cfg(feature = "time_stores")]

use std::marker::PhantomData;

use cached::time::Duration;
use cached::{CacheRefreshOnHit, CacheTtl, Cached, ConcurrentCacheRefreshOnHit};
use cached::{LruTtlCache, TtlCache, TtlSortedCache};

// ── trait-presence probes ────────────────────────────────────────────────────
//
// An inherent associated const on a bounded impl takes resolution priority over a
// blanket-implemented trait const of the same name; when the bound is unsatisfied the inherent
// candidate does not apply and the fallback is chosen. That makes "does `T` implement this
// trait?" a `const bool` on stable, which is what a *negative* assertion needs: a plain
// `fn assert_impl<T: Trait>()` can only state presence.

struct SyncProbe<T>(PhantomData<T>);
struct ConcurrentProbe<T>(PhantomData<T>);

trait SyncFallback {
    const IMPLEMENTED: bool = false;
}
impl<T> SyncFallback for SyncProbe<T> {}
impl<T: CacheRefreshOnHit> SyncProbe<T> {
    const IMPLEMENTED: bool = true;
}

trait ConcurrentFallback {
    const IMPLEMENTED: bool = false;
}
impl<T> ConcurrentFallback for ConcurrentProbe<T> {}
impl<T: ConcurrentCacheRefreshOnHit> ConcurrentProbe<T> {
    const IMPLEMENTED: bool = true;
}

/// The probe itself must be able to report both answers, or every assertion below is vacuous.
#[test]
fn the_trait_presence_probe_distinguishes_both_answers() {
    const {
        assert!(
            SyncProbe::<TtlCache<u32, u32>>::IMPLEMENTED,
            "the probe must report `true` for a known implementor"
        );
        assert!(
            !SyncProbe::<Vec<u32>>::IMPLEMENTED,
            "the probe must report `false` for a type that plainly does not implement the trait"
        );
        assert!(
            !ConcurrentProbe::<Vec<u32>>::IMPLEMENTED,
            "the concurrent probe must report `false` for a non-implementor"
        );
    }
}

// ── single-owner implementor set ─────────────────────────────────────────────

/// `TtlCache` and `LruTtlCache` can extend an entry's deadline on read, so they implement
/// `CacheRefreshOnHit`.
#[test]
fn ttl_cache_and_lru_ttl_cache_implement_cache_refresh_on_hit() {
    const {
        assert!(SyncProbe::<TtlCache<u32, u32>>::IMPLEMENTED);
        assert!(SyncProbe::<LruTtlCache<u32, u32>>::IMPLEMENTED);
    }
}

/// `TtlSortedCache` does not implement `CacheRefreshOnHit`. This is the assertion the split
/// exists for: the store's no-op `set_refresh_on_hit` is gone rather than lying about a
/// capability, so generic code that needs refresh-on-hit fails to compile against this store
/// instead of silently doing nothing.
#[test]
fn ttl_sorted_cache_does_not_implement_cache_refresh_on_hit() {
    const {
        assert!(
            !SyncProbe::<TtlSortedCache<u32, u32>>::IMPLEMENTED,
            "TtlSortedCache must not implement CacheRefreshOnHit: its deadline-ordered index \
             cannot survive an entry's expiry moving forward on a read"
        );
    }
}

/// Dropping refresh-on-hit from `CacheTtl` must not cost `TtlSortedCache` the rest of the TTL
/// surface: `ttl`, `set_ttl` (zero disables expiry, returns the previous value), `try_set_ttl`,
/// and `unset_ttl` all still work through the trait.
#[test]
fn ttl_sorted_cache_still_implements_the_full_cache_ttl_surface() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlSortedCache");

    assert_eq!(CacheTtl::ttl(&cache), Some(Duration::from_secs(60)));

    // set_ttl returns the previous value.
    assert_eq!(
        CacheTtl::set_ttl(&mut cache, Duration::from_secs(30)),
        Some(Duration::from_secs(60))
    );
    assert_eq!(CacheTtl::ttl(&cache), Some(Duration::from_secs(30)));

    // try_set_ttl (the defaulted validated variant) still rejects a zero duration.
    assert_eq!(
        CacheTtl::try_set_ttl(&mut cache, Duration::ZERO),
        Err(cached::SetTtlError::ZeroTtl)
    );
    assert_eq!(CacheTtl::ttl(&cache), Some(Duration::from_secs(30)));

    // unset_ttl disables expiry and reports the previous value; `ttl()` then resolves to None.
    assert_eq!(
        CacheTtl::unset_ttl(&mut cache),
        Some(Duration::from_secs(30))
    );
    assert_eq!(CacheTtl::ttl(&cache), None);

    // A zero set_ttl is equivalent to unset_ttl and reports `None` when expiry was already off.
    assert_eq!(CacheTtl::set_ttl(&mut cache, Duration::ZERO), None);

    // The store is still a working cache after all of that.
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
}

// ── the "returns the previous value" contract, through a generic bound ───────

/// Generic code bounded on `CacheRefreshOnHit` compiles and can rely on the setter reporting the
/// state the store was actually in. Under the old combined trait `TtlSortedCache` satisfied the
/// same bound while returning `false` from both calls below.
fn assert_setter_reports_the_previous_value<C: CacheRefreshOnHit>(cache: &mut C) {
    let start = cache.refresh_on_hit();

    assert_eq!(
        cache.set_refresh_on_hit(!start),
        start,
        "set_refresh_on_hit must return the flag the store was in before the call"
    );
    assert_eq!(
        cache.refresh_on_hit(),
        !start,
        "the getter must observe the value the setter just wrote"
    );

    assert_eq!(
        cache.set_refresh_on_hit(start),
        !start,
        "set_refresh_on_hit must report the previous value on the way back too"
    );
    assert_eq!(cache.refresh_on_hit(), start);
}

#[test]
fn ttl_cache_honours_the_refresh_on_hit_contract() {
    let mut cache: TtlCache<u32, u32> = TtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlCache");
    assert!(!cache.refresh_on_hit(), "the builder default is off");
    assert_setter_reports_the_previous_value(&mut cache);
}

#[test]
fn lru_ttl_cache_honours_the_refresh_on_hit_contract() {
    let mut cache: LruTtlCache<u32, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build LruTtlCache");
    assert!(!cache.refresh_on_hit(), "the builder default is off");
    assert_setter_reports_the_previous_value(&mut cache);
}

/// A builder-time `refresh_on_hit(true)` is still visible through the relocated getter, so the
/// builder setter is unaffected by the trait move.
#[test]
fn builder_refresh_on_hit_reaches_the_relocated_getter() {
    let ttl: TtlCache<u32, u32> = TtlCache::builder()
        .ttl(Duration::from_secs(60))
        .refresh_on_hit(true)
        .build()
        .expect("build TtlCache");
    assert!(CacheRefreshOnHit::refresh_on_hit(&ttl));

    let lru_ttl: LruTtlCache<u32, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .refresh_on_hit(true)
        .build()
        .expect("build LruTtlCache");
    assert!(CacheRefreshOnHit::refresh_on_hit(&lru_ttl));
}

/// A `CacheTtl` bound accepts `TtlSortedCache`; adding `CacheRefreshOnHit` to the same bound
/// would not. This function pins that `CacheTtl` alone is still usable across all three
/// single-owner timed stores.
fn assert_ttl_roundtrips<C: CacheTtl>(cache: &mut C) {
    let previous = cache.set_ttl(Duration::from_secs(5));
    assert_eq!(cache.ttl(), Some(Duration::from_secs(5)));
    if let Some(p) = previous {
        cache.set_ttl(p);
    } else {
        cache.unset_ttl();
    }
}

#[test]
fn cache_ttl_bound_still_accepts_all_three_single_owner_timed_stores() {
    let mut ttl: TtlCache<u32, u32> = TtlCache::new(Duration::from_secs(60));
    let mut lru_ttl: LruTtlCache<u32, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build LruTtlCache");
    let mut sorted: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlSortedCache");

    assert_ttl_roundtrips(&mut ttl);
    assert_ttl_roundtrips(&mut lru_ttl);
    assert_ttl_roundtrips(&mut sorted);
}

// ── concurrent implementor set ───────────────────────────────────────────────

/// Every concurrent store with a global TTL implements `ConcurrentCacheRefreshOnHit`:
/// the two sharded TTL stores plus the three IO stores. Asserted at the type level so no
/// Redis server or redb file is needed.
#[test]
fn the_five_concurrent_ttl_stores_implement_concurrent_cache_refresh_on_hit() {
    use cached::{ShardedLruTtlCache, ShardedTtlCache};

    const {
        assert!(ConcurrentProbe::<ShardedTtlCache<u32, u32>>::IMPLEMENTED);
        assert!(ConcurrentProbe::<ShardedLruTtlCache<u32, u32>>::IMPLEMENTED);
    }

    #[cfg(feature = "redis_store")]
    const {
        assert!(ConcurrentProbe::<cached::RedisCache<u32, u32>>::IMPLEMENTED);
    }

    #[cfg(feature = "redb_store")]
    const {
        assert!(ConcurrentProbe::<cached::RedbCache<u32, u32>>::IMPLEMENTED);
    }

    // Same gate as the `AsyncRedisCache` re-export: the store needs a redis async runtime.
    #[cfg(any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
    ))]
    const {
        assert!(ConcurrentProbe::<cached::AsyncRedisCache<u32, u32>>::IMPLEMENTED);
    }
}

/// The non-TTL sharded stores implement neither TTL trait, so they do not accidentally pick up
/// a refresh-on-hit knob they have no expiry to apply it to.
#[test]
fn non_ttl_sharded_stores_do_not_implement_concurrent_cache_refresh_on_hit() {
    use cached::{
        ShardedExpiringCache, ShardedExpiringLruCache, ShardedLruCache, ShardedUnboundCache,
    };

    const {
        assert!(!ConcurrentProbe::<ShardedUnboundCache<u32, u32>>::IMPLEMENTED);
        assert!(!ConcurrentProbe::<ShardedLruCache<u32, u32>>::IMPLEMENTED);
        assert!(!ConcurrentProbe::<ShardedExpiringCache<u32, TestValue>>::IMPLEMENTED);
        assert!(!ConcurrentProbe::<ShardedExpiringLruCache<u32, TestValue>>::IMPLEMENTED);
    }
}

#[derive(Clone)]
struct TestValue;

impl cached::Expires for TestValue {
    fn is_expired(&self) -> bool {
        false
    }
}

/// Generic `&self` code bounded on `ConcurrentCacheRefreshOnHit` compiles and gets the same
/// "returns the previous value" contract as the single-owner side.
fn assert_concurrent_setter_reports_the_previous_value<C: ConcurrentCacheRefreshOnHit>(cache: &C) {
    let start = cache.refresh_on_hit();
    assert_eq!(cache.set_refresh_on_hit(!start), start);
    assert_eq!(cache.refresh_on_hit(), !start);
    assert_eq!(cache.set_refresh_on_hit(start), !start);
    assert_eq!(cache.refresh_on_hit(), start);
}

#[test]
fn sharded_ttl_stores_honour_the_concurrent_refresh_on_hit_contract() {
    use cached::{ShardedLruTtlCache, ShardedTtlCache};

    let ttl: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    assert!(!ttl.refresh_on_hit(), "the builder default is off");
    assert_concurrent_setter_reports_the_previous_value(&ttl);

    let lru_ttl: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .per_shard_max_size(8)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");
    assert!(!lru_ttl.refresh_on_hit(), "the builder default is off");
    assert_concurrent_setter_reports_the_previous_value(&lru_ttl);
}

/// The two traits stay independently usable on the same store: a `ConcurrentCacheTtl` bound
/// still reaches `set_ttl`/`unset_ttl`, and the refresh knob is reached through the other bound.
#[test]
fn concurrent_ttl_and_refresh_bounds_compose_on_one_store() {
    use cached::{ConcurrentCacheTtl, ShardedTtlCache};

    fn disable_expiry_and_refresh<C: ConcurrentCacheTtl + ConcurrentCacheRefreshOnHit>(cache: &C) {
        cache.set_refresh_on_hit(false);
        cache.unset_ttl();
    }

    let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
        .ttl(Duration::from_secs(60))
        .refresh_on_hit(true)
        .build()
        .expect("build ShardedTtlCache");

    disable_expiry_and_refresh(&cache);

    assert_eq!(ConcurrentCacheTtl::ttl(&cache), None);
    assert!(!ConcurrentCacheRefreshOnHit::refresh_on_hit(&cache));
}

/// Both new traits are reachable through `cached::prelude::*` alone, with no per-trait import.
#[test]
fn both_refresh_on_hit_traits_are_in_the_prelude() {
    use cached::prelude::*;

    let mut single: TtlCache<u32, u32> = TtlCache::new(Duration::from_secs(60));
    assert!(!single.set_refresh_on_hit(true));
    assert!(single.refresh_on_hit());

    let concurrent: cached::ShardedTtlCache<u32, u32> = cached::ShardedTtlCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");
    assert!(!concurrent.set_refresh_on_hit(true));
    assert!(concurrent.refresh_on_hit());
}
