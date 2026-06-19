//! Integration tests for the 3.0 trait additions and audit-fix batch:
//! - `*_mut` get-or-set variants (#179)
//! - `SerializeCached`/`SerializeCachedAsync` borrowed set (#196)
//! - `CacheSetError` concrete error type for `cache_try_set`
//! - `ConcurrentCached::refresh_on_hit` getter default and override
//! - `ConcurrentCached::cache_get_or_set_with` / `async_cache_get_or_set_with`
//! - `store()` getter removal verified via public API

use cached::{Cached, LruCache, UnboundCache};

/// The `_mut` variants return a mutable reference that callers can mutate
/// in place; the resulting change is observable on the next read.
#[test]
fn cache_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: UnboundCache<u32, u32> =
        UnboundCache::builder().build().expect("build UnboundCache");

    // Insert via the mutable variant and mutate the returned `&mut V`.
    let v: &mut u32 = cache.cache_get_or_set_with_mut(1, || 10);
    assert_eq!(*v, 10);
    *v += 5;
    assert_eq!(cache.cache_get(&1), Some(&15));

    // The shared-reference variant returns `&V` (it sees the mutated value on hit).
    let shared: &u32 = cache.cache_get_or_set_with(1, || 999);
    assert_eq!(*shared, 15);
}

#[test]
fn cache_try_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: UnboundCache<u32, u32> =
        UnboundCache::builder().build().expect("build UnboundCache");

    // Err: propagated, nothing cached.
    let result: Result<&mut u32, ()> = cache.cache_try_get_or_set_with_mut(1, || Err(()));
    assert!(result.is_err());
    assert_eq!(cache.cache_get(&1), None);

    // Ok miss: value inserted; mutate through the returned `&mut V`.
    let v: &mut u32 = cache
        .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(10))
        .unwrap();
    assert_eq!(*v, 10);
    *v *= 2;
    assert_eq!(cache.cache_get(&1), Some(&20));

    // Shared-ref fallible variant returns `Result<&V, E>`.
    let shared: &u32 = cache
        .cache_try_get_or_set_with(1, || Ok::<u32, ()>(999))
        .unwrap();
    assert_eq!(*shared, 20);
}

#[test]
fn lru_cache_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: LruCache<u32, u32> = LruCache::builder()
        .max_size(10)
        .build()
        .expect("build LruCache");

    // Miss: body runs, value inserted; mutate through the returned `&mut V`.
    let v: &mut u32 = cache.cache_get_or_set_with_mut(1, || 10);
    assert_eq!(*v, 10);
    *v += 5;
    assert_eq!(cache.cache_get(&1), Some(&15));

    // Hit: body does not run; returns the mutated value.
    let hit: &mut u32 = cache.cache_get_or_set_with_mut(1, || 999);
    assert_eq!(*hit, 15);
}

#[test]
fn lru_cache_try_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: LruCache<u32, u32> = LruCache::builder()
        .max_size(10)
        .build()
        .expect("build LruCache");

    // Err: propagated, nothing cached.
    let result: Result<&mut u32, ()> = cache.cache_try_get_or_set_with_mut(1, || Err(()));
    assert!(result.is_err());
    assert_eq!(cache.cache_get(&1), None);

    // Ok miss: value inserted; mutate through the returned `&mut V`.
    let v: &mut u32 = cache
        .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(10))
        .unwrap();
    assert_eq!(*v, 10);
    *v *= 2;
    assert_eq!(cache.cache_get(&1), Some(&20));

    // Hit: body does not run; stored value returned.
    let hit: &mut u32 = cache
        .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(999))
        .unwrap();
    assert_eq!(*hit, 20);
}

// ExpiringCache values must implement Expires; use a simple never-expiring wrapper.
mod expiring_cache_mut {
    use cached::{Cached, Expires, ExpiringCache};

    // A value that never expires. ExpiringCache requires V: Expires.
    #[derive(Debug, PartialEq, Clone)]
    struct Never(u32);

    impl Expires for Never {
        fn is_expired(&self) -> bool {
            false
        }
    }

    /// `cache_get_or_set_with_mut` on ExpiringCache: value computed once on miss,
    /// returned from cache on hit (body does not run again).
    #[test]
    fn expiring_cache_get_or_set_with_mut() {
        let mut cache: ExpiringCache<u32, Never> = ExpiringCache::builder()
            .build()
            .expect("build ExpiringCache");

        // Miss: body runs, value inserted.
        let v: &mut Never = cache.cache_get_or_set_with_mut(1, || Never(10));
        assert_eq!(*v, Never(10));

        // Mutate in place and confirm the change is visible on a subsequent get.
        v.0 += 5;
        assert_eq!(cache.cache_get(&1), Some(&Never(15)));

        // Hit: body does not run; returns the previously stored (mutated) value.
        let hit: &mut Never = cache.cache_get_or_set_with_mut(1, || Never(999));
        assert_eq!(*hit, Never(15));
    }

    /// `cache_try_get_or_set_with_mut` on ExpiringCache: Err from setter is propagated
    /// and the key is not inserted; Ok path stores and returns a mutable ref.
    #[test]
    fn expiring_cache_try_get_or_set_with_mut() {
        let mut cache: ExpiringCache<u32, Never> = ExpiringCache::builder()
            .build()
            .expect("build ExpiringCache");

        // Err: propagated, nothing cached.
        let result: Result<&mut Never, ()> = cache.cache_try_get_or_set_with_mut(1, || Err(()));
        assert!(result.is_err());
        assert_eq!(cache.cache_get(&1), None);

        // Ok miss: value inserted.
        let v: &mut Never = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<Never, ()>(Never(20)))
            .unwrap();
        assert_eq!(*v, Never(20));
        v.0 *= 2;
        assert_eq!(cache.cache_get(&1), Some(&Never(40)));

        // Hit: body does not run; stored value returned.
        let hit: &mut Never = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<Never, ()>(Never(999)))
            .unwrap();
        assert_eq!(*hit, Never(40));
    }
}

#[cfg(feature = "time_stores")]
mod ttl_sorted_cache_mut {
    use cached::{Cached, TtlSortedCache};
    use std::time::Duration;

    /// `cache_get_or_set_with_mut` on TtlSortedCache: value computed once on miss,
    /// returned from cache on hit (body does not run again).
    #[test]
    fn ttl_sorted_cache_get_or_set_with_mut() {
        let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build TtlSortedCache");

        // Miss: body runs, value inserted.
        let v: &mut u32 = cache.cache_get_or_set_with_mut(1, || 10);
        assert_eq!(*v, 10);

        // Mutate in place and confirm the change persists.
        *v += 5;
        assert_eq!(cache.cache_get(&1), Some(&15));

        // Hit: body does not run; stored (mutated) value returned.
        let hit: &mut u32 = cache.cache_get_or_set_with_mut(1, || 999);
        assert_eq!(*hit, 15);
    }

    /// `cache_try_get_or_set_with_mut` on TtlSortedCache: Err from setter is propagated
    /// and the key is not inserted; Ok path stores and returns a mutable ref.
    #[test]
    fn ttl_sorted_cache_try_get_or_set_with_mut() {
        let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build TtlSortedCache");

        // Err: propagated, nothing cached.
        let result: Result<&mut u32, ()> = cache.cache_try_get_or_set_with_mut(1, || Err(()));
        assert!(result.is_err());
        assert_eq!(cache.cache_get(&1), None);

        // Ok miss: value inserted.
        let v: &mut u32 = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(20))
            .unwrap();
        assert_eq!(*v, 20);
        *v *= 2;
        assert_eq!(cache.cache_get(&1), Some(&40));

        // Hit: body does not run; stored value returned.
        let hit: &mut u32 = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(999))
            .unwrap();
        assert_eq!(*hit, 40);
    }
}

// ── try_set_ttl (#10) ─────────────────────────────────────────────────────────

#[cfg(feature = "time_stores")]
mod try_set_ttl_tests {
    use cached::{CacheTtl, Cached, LruTtlCache, SetTtlError, TtlCache, TtlSortedCache};
    use std::time::Duration;

    /// `try_set_ttl` returns `Err(SetTtlError::ZeroTtl)` for a zero Duration
    /// and does not change the TTL.
    #[test]
    fn ttl_cache_try_set_ttl_rejects_zero() {
        let mut cache = TtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build TtlCache");
        let prev_ttl = cache.ttl();
        let result = cache.try_set_ttl(Duration::ZERO);
        assert_eq!(result, Err(SetTtlError::ZeroTtl));
        // TTL must be unchanged after a rejected call.
        assert_eq!(cache.ttl(), prev_ttl);
    }

    /// `try_set_ttl` returns `Ok(prev_ttl)` for a non-zero Duration and
    /// updates the TTL.
    #[test]
    fn ttl_cache_try_set_ttl_accepts_nonzero() {
        let mut cache = TtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build TtlCache");
        let result = cache.try_set_ttl(Duration::from_secs(30));
        assert_eq!(result, Ok(Some(Duration::from_secs(10))));
        assert_eq!(cache.ttl(), Some(Duration::from_secs(30)));
    }

    /// `try_set_ttl` on LruTtlCache: same contract as TtlCache.
    #[test]
    fn lru_ttl_cache_try_set_ttl_rejects_zero() {
        let mut cache = LruTtlCache::<u32, u32>::builder()
            .max_size(8)
            .ttl(Duration::from_secs(5))
            .build()
            .expect("build LruTtlCache");
        assert_eq!(cache.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
    }

    #[test]
    fn lru_ttl_cache_try_set_ttl_accepts_nonzero() {
        let mut cache = LruTtlCache::<u32, u32>::builder()
            .max_size(8)
            .ttl(Duration::from_secs(5))
            .build()
            .expect("build LruTtlCache");
        let prev = cache.try_set_ttl(Duration::from_secs(20));
        assert_eq!(prev, Ok(Some(Duration::from_secs(5))));
        assert_eq!(cache.ttl(), Some(Duration::from_secs(20)));
    }

    /// `try_set_ttl` on TtlSortedCache: same contract.
    #[test]
    fn ttl_sorted_cache_try_set_ttl_rejects_zero() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(15))
            .build()
            .expect("build TtlSortedCache");
        assert_eq!(cache.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
    }

    #[test]
    fn ttl_sorted_cache_try_set_ttl_accepts_nonzero() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(15))
            .build()
            .expect("build TtlSortedCache");
        let prev = cache.try_set_ttl(Duration::from_secs(45));
        assert_eq!(prev, Ok(Some(Duration::from_secs(15))));
        assert_eq!(cache.ttl(), Some(Duration::from_secs(45)));
    }

    /// Display impl for SetTtlError prints the expected message.
    #[test]
    fn set_ttl_error_display() {
        let msg = format!("{}", SetTtlError::ZeroTtl);
        assert_eq!(msg, "ttl must be greater than zero");
    }

    /// Panic-prevention contract for `try_set_ttl(Duration::ZERO)`.
    ///
    /// `try_set_ttl` exists so callers can reject a zero ttl explicitly instead of
    /// silently installing one. The footgun it guards against is NOT a panic in
    /// `set_ttl` itself: the `CacheTtl::set_ttl` impls for these in-memory stores
    /// accept any Duration and never panic. The hazard is that a zero ttl makes
    /// every inserted entry immediately expired (`elapsed() >= 0` is always true),
    /// silently breaking the cache. These tests assert:
    ///   1. `try_set_ttl(ZERO)` returns `Err(ZeroTtl)` and leaves the ttl unchanged,
    ///      so the cache keeps working;
    ///   2. the bypassed `set_ttl(ZERO)` path is the broken one: after it, a freshly
    ///      inserted live entry reads back as absent (proving why callers want the
    ///      fallible variant).
    ///
    /// All three `CacheTtl` impls are covered.
    fn assert_zero_ttl_silently_breaks<C: Cached<u32, u32> + CacheTtl>(cache: &mut C) {
        // Force the broken state via the panic-free set_ttl, then prove it is broken.
        let _ = cache.set_ttl(Duration::ZERO);
        cache.cache_set(7, 70);
        assert_eq!(
            cache.cache_get(&7),
            None,
            "a zero ttl must make a just-inserted entry read back as expired/absent",
        );
    }

    #[test]
    fn ttl_cache_try_set_ttl_prevents_zero_ttl_breakage() {
        let mut cache = TtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build TtlCache");

        // try_set_ttl rejects zero without panicking and without touching the ttl.
        let prev = cache.ttl();
        assert_eq!(cache.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
        assert_eq!(
            cache.ttl(),
            prev,
            "rejected try_set_ttl must not change ttl"
        );

        // The cache still works after the rejected call.
        cache.cache_set(1, 10);
        assert_eq!(cache.cache_get(&1), Some(&10));

        // Document the bypassed set_ttl(ZERO) breakage on a separate instance.
        let mut broken = TtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build TtlCache");
        assert_zero_ttl_silently_breaks(&mut broken);
    }

    #[test]
    fn lru_ttl_cache_try_set_ttl_prevents_zero_ttl_breakage() {
        let mut cache = LruTtlCache::<u32, u32>::builder()
            .max_size(8)
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build LruTtlCache");

        let prev = cache.ttl();
        assert_eq!(cache.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
        assert_eq!(
            cache.ttl(),
            prev,
            "rejected try_set_ttl must not change ttl"
        );
        cache.cache_set(1, 10);
        assert_eq!(cache.cache_get(&1), Some(&10));

        let mut broken = LruTtlCache::<u32, u32>::builder()
            .max_size(8)
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build LruTtlCache");
        assert_zero_ttl_silently_breaks(&mut broken);
    }

    #[test]
    fn ttl_sorted_cache_try_set_ttl_prevents_zero_ttl_breakage() {
        let mut cache = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build TtlSortedCache");

        let prev = cache.ttl();
        assert_eq!(cache.try_set_ttl(Duration::ZERO), Err(SetTtlError::ZeroTtl));
        assert_eq!(
            cache.ttl(),
            prev,
            "rejected try_set_ttl must not change ttl"
        );
        cache.cache_set(1, 10);
        assert_eq!(cache.cache_get(&1), Some(&10));

        let mut broken = TtlSortedCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(10))
            .build()
            .expect("build TtlSortedCache");
        assert_zero_ttl_silently_breaks(&mut broken);
    }

    /// `SetTtlError` is a well-formed std error type: it is `Debug`, implements
    /// `std::error::Error`, can be boxed as `Box<dyn Error>`, and has no source.
    #[test]
    fn set_ttl_error_is_std_error() {
        use std::error::Error;

        let err = SetTtlError::ZeroTtl;

        // Debug formatting works and names the variant.
        assert_eq!(format!("{err:?}"), "ZeroTtl");

        // It implements std::error::Error and can be boxed as a trait object.
        let boxed: Box<dyn Error> = Box::new(err.clone());
        assert_eq!(boxed.to_string(), "ttl must be greater than zero");

        // It is a leaf error: no underlying source.
        assert!(
            err.source().is_none(),
            "SetTtlError::ZeroTtl must not report a source"
        );
        assert!(boxed.source().is_none());
    }
}

// ── len / is_empty on ConcurrentCached (Tier3) ────────────────────────────────

mod concurrent_len_is_empty {
    use cached::{ConcurrentCached, ShardedLruCache, ShardedUnboundCache};

    /// `len` and `is_empty` on ShardedUnboundCache agree with the number of
    /// inserted entries. Uses fully-qualified syntax because the sharded base types
    /// have inherent `len`/`is_empty` methods with different signatures (returning
    /// plain `usize`/`bool`); the trait methods return `Result<Option<...>>`.
    #[test]
    fn sharded_unbound_cache_len_is_empty() {
        let cache: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::builder()
            .build()
            .expect("build ShardedUnboundCache");

        // Empty initially.
        assert_eq!(ConcurrentCached::is_empty(&cache), Ok(Some(true)));
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(0)));

        cache.cache_set(1, 10).unwrap();
        assert_eq!(ConcurrentCached::is_empty(&cache), Ok(Some(false)));
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(1)));

        cache.cache_set(2, 20).unwrap();
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(2)));

        cache.cache_remove(&1).unwrap();
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(1)));
        assert_eq!(ConcurrentCached::is_empty(&cache), Ok(Some(false)));

        cache.cache_clear().unwrap();
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(0)));
        assert_eq!(ConcurrentCached::is_empty(&cache), Ok(Some(true)));
    }

    /// `len` and `is_empty` on ShardedLruCache.
    #[test]
    fn sharded_lru_cache_len_is_empty() {
        let cache: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
            .max_size(16)
            .build()
            .expect("build ShardedLruCache");

        assert_eq!(ConcurrentCached::is_empty(&cache), Ok(Some(true)));
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(0)));

        cache.cache_set(42, 99).unwrap();
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(1)));
        assert_eq!(ConcurrentCached::is_empty(&cache), Ok(Some(false)));

        cache.cache_set(43, 100).unwrap();
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(2)));

        cache.cache_reset().unwrap();
        assert_eq!(ConcurrentCached::len(&cache), Ok(Some(0)));
        assert_eq!(ConcurrentCached::is_empty(&cache), Ok(Some(true)));
    }
}

// ── async len / is_empty on ConcurrentCachedAsync (Tier3) ─────────────────────

#[cfg(feature = "async")]
mod concurrent_len_is_empty_async {
    #[cfg(feature = "time_stores")]
    use cached::ShardedTtlCache;
    use cached::{ConcurrentCachedAsync, ShardedUnboundCache};

    /// Async `len`/`is_empty` on ShardedUnboundCache track the live entry count.
    /// Fully-qualified syntax is required because the concrete sharded type has
    /// inherent `len`/`is_empty` returning plain `usize`/`bool`; the trait methods
    /// return `Result<Option<...>>`.
    #[tokio::test]
    async fn sharded_unbound_cache_async_len_is_empty() {
        let cache: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::builder()
            .build()
            .expect("build ShardedUnboundCache");

        assert_eq!(ConcurrentCachedAsync::is_empty(&cache), Ok(Some(true)));
        assert_eq!(ConcurrentCachedAsync::len(&cache), Ok(Some(0)));

        ConcurrentCachedAsync::async_cache_set(&cache, 1, 10)
            .await
            .expect("infallible");
        assert_eq!(ConcurrentCachedAsync::is_empty(&cache), Ok(Some(false)));
        assert_eq!(ConcurrentCachedAsync::len(&cache), Ok(Some(1)));

        ConcurrentCachedAsync::async_cache_set(&cache, 2, 20)
            .await
            .expect("infallible");
        assert_eq!(ConcurrentCachedAsync::len(&cache), Ok(Some(2)));

        ConcurrentCachedAsync::async_cache_clear(&cache)
            .await
            .expect("infallible");
        assert_eq!(ConcurrentCachedAsync::len(&cache), Ok(Some(0)));
        assert_eq!(ConcurrentCachedAsync::is_empty(&cache), Ok(Some(true)));
    }

    /// Async `len`/`is_empty` on ShardedTtlCache, exercising the time-bounded store.
    #[cfg(feature = "time_stores")]
    #[tokio::test]
    async fn sharded_ttl_cache_async_len_is_empty() {
        use std::time::Duration;

        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedTtlCache");

        assert_eq!(ConcurrentCachedAsync::is_empty(&cache), Ok(Some(true)));
        assert_eq!(ConcurrentCachedAsync::len(&cache), Ok(Some(0)));

        ConcurrentCachedAsync::async_cache_set(&cache, 1, 10)
            .await
            .expect("infallible");
        ConcurrentCachedAsync::async_cache_set(&cache, 2, 20)
            .await
            .expect("infallible");
        assert_eq!(ConcurrentCachedAsync::len(&cache), Ok(Some(2)));
        assert_eq!(ConcurrentCachedAsync::is_empty(&cache), Ok(Some(false)));

        ConcurrentCachedAsync::async_cache_reset(&cache)
            .await
            .expect("infallible");
        assert_eq!(ConcurrentCachedAsync::len(&cache), Ok(Some(0)));
        assert_eq!(ConcurrentCachedAsync::is_empty(&cache), Ok(Some(true)));
    }
}

// ── ConcurrentCloneCached::cache_peek_with_expiry_status (integration) ─────────

#[cfg(feature = "time_stores")]
mod concurrent_clone_cached_peek {
    use cached::{ConcurrentCached, ConcurrentCloneCached, ShardedTtlCache};
    use std::time::Duration;

    /// Through the public `ShardedTtlCache` alias, `cache_peek_with_expiry_status`
    /// is side-effect-free: a live entry returns `(Some(v), false)`, an absent key
    /// returns `(None, false)`, and neither touches hit/miss/eviction counters.
    #[test]
    fn peek_live_and_absent_no_counter_change() {
        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedTtlCache");

        ConcurrentCached::cache_set(&cache, 1, 42).expect("infallible");

        let before = cache.metrics();

        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &1);
        assert_eq!(val, Some(42), "live peek returns the value");
        assert!(!expired, "live entry reports expired=false");

        let (absent, absent_expired) =
            ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &999);
        assert_eq!(absent, None, "absent key returns None");
        assert!(!absent_expired, "absent key reports expired=false");

        let after = cache.metrics();
        assert_eq!(after.hits, before.hits, "peek must not change hits");
        assert_eq!(after.misses, before.misses, "peek must not change misses");
        assert_eq!(
            after.evictions, before.evictions,
            "peek must not change evictions"
        );
    }

    /// An expired entry is returned as a stale fallback (`(Some(v), true)`) and is
    /// neither removed nor counted. The entry survives the peek so a later read can
    /// still see it.
    #[test]
    fn peek_expired_returns_stale_without_removal() {
        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_millis(10))
            .build()
            .expect("build ShardedTtlCache");

        ConcurrentCached::cache_set(&cache, 1, 77).expect("infallible");
        std::thread::sleep(Duration::from_millis(50));

        let before = cache.metrics();

        let (val, expired) = ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &1);
        assert_eq!(val, Some(77), "expired peek returns the stale value");
        assert!(expired, "expired entry reports expired=true");

        let after = cache.metrics();
        assert_eq!(after.hits, before.hits, "expired peek must not change hits");
        assert_eq!(
            after.misses, before.misses,
            "expired peek must not change misses"
        );
        assert_eq!(
            after.evictions, before.evictions,
            "expired peek must not evict"
        );

        // Entry still present (not removed by peek): a second peek still finds it stale.
        let (val2, expired2) = ConcurrentCloneCached::cache_peek_with_expiry_status(&cache, &1);
        assert_eq!(val2, Some(77), "entry must survive the peek");
        assert!(expired2, "entry must still be expired after peek");
    }
}

#[cfg(feature = "disk_store")]
mod redb_serialize_cached {
    use cached::stores::RedbCache;
    use cached::time::Duration;
    use cached::{ConcurrentCached, SerializeCached};
    use tempfile::TempDir;

    fn build_cache(dir: &TempDir, name: &str) -> RedbCache<u32, String> {
        RedbCache::<u32, String>::builder()
            .name(name)
            .disk_directory(dir.path())
            .build()
            .expect("error building redb cache")
    }

    /// `cache_set_ref` takes `&K, &V` (no clone needed at the call site) and
    /// round-trips through the same store as `cache_set`.
    #[test]
    fn cache_set_ref_round_trip() {
        let dir = TempDir::new().unwrap();
        let cache = build_cache(&dir, "serialize_cached_round_trip");

        let key: u32 = 42;
        let value: String = "hello".to_string();

        // Borrowed set: `key` and `value` are still owned by the caller afterward.
        let prev = cache
            .cache_set_ref(&key, &value)
            .expect("cache_set_ref failed");
        assert_eq!(prev, None);
        assert_eq!(key, 42);
        assert_eq!(value, "hello");

        // Read back the value written via the borrowed setter.
        assert_eq!(cache.cache_get(&key).unwrap(), Some("hello".to_string()));

        // Overwriting returns the previous value (proving same storage as cache_set).
        let prev = cache
            .cache_set_ref(&key, &"world".to_string())
            .expect("cache_set_ref overwrite failed");
        assert_eq!(prev, Some("hello".to_string()));
        assert_eq!(cache.cache_get(&key).unwrap(), Some("world".to_string()));
    }

    /// A value written via `cache_set` reads back identically to one written via
    /// `cache_set_ref` — the borrowed serialize path is byte-compatible.
    #[test]
    fn cache_set_ref_matches_cache_set() {
        let dir = TempDir::new().unwrap();
        let cache = build_cache(&dir, "serialize_cached_compat");

        cache.cache_set(1, "owned".to_string()).unwrap();
        cache.cache_set_ref(&2, &"owned".to_string()).unwrap();

        assert_eq!(cache.cache_get(&1).unwrap(), cache.cache_get(&2).unwrap());
    }

    /// A value written via `cache_set_ref` carries a `created_at` timestamp that the
    /// expiry check reads. After sleeping past the TTL the entry must be absent.
    #[test]
    fn cache_set_ref_ttl_expiry() {
        let dir = TempDir::new().unwrap();
        let cache: RedbCache<u32, String> = RedbCache::builder()
            .name("serialize_cached_ttl_expiry")
            .disk_directory(dir.path())
            .ttl(Duration::from_millis(100))
            .build()
            .expect("error building redb cache");

        let key: u32 = 1;
        let value: String = "expires".to_string();

        let prev = cache
            .cache_set_ref(&key, &value)
            .expect("cache_set_ref failed");
        assert_eq!(prev, None);

        // Entry is present immediately after insertion.
        assert_eq!(cache.cache_get(&key).unwrap(), Some("expires".to_string()));

        // Sleep past the TTL; the entry must now be treated as expired (absent).
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(cache.cache_get(&key).unwrap(), None);
    }
}

#[cfg(all(feature = "disk_store", feature = "async"))]
mod redb_serialize_cached_async {
    use cached::stores::RedbCache;
    use cached::{ConcurrentCachedAsync, SerializeCachedAsync};
    use tempfile::TempDir;

    #[tokio::test]
    async fn async_cache_set_ref_round_trip() {
        let dir = TempDir::new().unwrap();
        let cache: RedbCache<u32, String> = RedbCache::builder()
            .name("serialize_cached_async_round_trip")
            .disk_directory(dir.path())
            .build()
            .expect("error building redb cache");

        let key: u32 = 7;
        let value: String = "async".to_string();

        let prev = cache
            .async_cache_set_ref(&key, &value)
            .await
            .expect("async_cache_set_ref failed");
        assert_eq!(prev, None);
        // Caller still owns the borrowed inputs.
        assert_eq!(key, 7);
        assert_eq!(value, "async");

        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some("async".to_string())
        );
    }

    /// Overwriting an existing entry via `async_cache_set_ref` returns the previous value
    /// and the store reflects the new value on the next read.
    #[tokio::test]
    async fn async_cache_set_ref_overwrite() {
        let dir = TempDir::new().unwrap();
        let cache: RedbCache<u32, String> = RedbCache::builder()
            .name("serialize_cached_async_overwrite")
            .disk_directory(dir.path())
            .build()
            .expect("error building redb cache");

        let key: u32 = 99;

        // First insert: no previous value.
        let prev = cache
            .async_cache_set_ref(&key, &"first".to_string())
            .await
            .expect("async_cache_set_ref first failed");
        assert_eq!(prev, None);

        // Overwrite: previous value is returned.
        let prev = cache
            .async_cache_set_ref(&key, &"second".to_string())
            .await
            .expect("async_cache_set_ref overwrite failed");
        assert_eq!(prev, Some("first".to_string()));

        // Store reflects the new value.
        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some("second".to_string())
        );
    }
}

// ── Item 1: CacheSetError ─────────────────────────────────────────────────────

/// `CacheSetError` is a well-formed concrete `std::error::Error` type:
/// it is `Debug`, has a `Display` impl, and can be boxed as a trait object.
#[test]
fn cache_set_error_is_std_error() {
    use cached::CacheSetError;
    use std::error::Error;

    let err = CacheSetError::TimeBounds;

    // Debug and Display both work.
    assert!(format!("{err:?}").contains("TimeBounds"));
    assert_eq!(err.to_string(), "ttl is outside Instant bounds");

    // It is a leaf error: no source.
    assert!(err.source().is_none());

    // Can be boxed as a trait object.
    let boxed: Box<dyn Error> = Box::new(CacheSetError::TimeBounds);
    assert_eq!(boxed.to_string(), "ttl is outside Instant bounds");
    assert!(boxed.source().is_none());
}

/// The default `cache_try_set` on stores that do not override it is infallible:
/// it always returns `Ok(prev)`.
#[test]
fn cache_try_set_default_is_infallible() {
    use cached::{CacheSetError, Cached, UnboundCache};

    let mut cache: UnboundCache<u32, u32> =
        UnboundCache::builder().build().expect("build UnboundCache");

    // First insert: no previous value.
    let result: Result<Option<u32>, CacheSetError> = cache.cache_try_set(1, 10);
    assert_eq!(result.unwrap(), None);

    // Second insert: returns the previous value.
    let result: Result<Option<u32>, CacheSetError> = cache.cache_try_set(1, 20);
    assert_eq!(result.unwrap(), Some(10));
}

/// `TtlSortedCache::cache_try_set` returns `Err(CacheSetError::TimeBounds)` when
/// the computed expiry `Instant` would overflow. With a normally-representable TTL
/// it succeeds infallibly.
#[cfg(feature = "time_stores")]
#[test]
fn ttl_sorted_cache_try_set_succeeds_normal_ttl() {
    use cached::time::Duration;
    use cached::{CacheSetError, Cached, TtlSortedCache};

    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlSortedCache");

    // A normal insert via cache_try_set must succeed and return the previous value.
    let result: Result<Option<u32>, CacheSetError> = cache.cache_try_set(1, 42);
    assert_eq!(result.unwrap(), None);

    // A second insert returns the previous value.
    let result: Result<Option<u32>, CacheSetError> = cache.cache_try_set(1, 99);
    assert_eq!(result.unwrap(), Some(42));
}

/// `TtlSortedCache::cache_try_set` returns `Err(CacheSetError::TimeBounds)` when the
/// configured ttl makes the computed expiry `Instant` overflow.
///
/// The overflow is triggered deterministically and portably: the public default ttl
/// drives the expiry (`insert` -> `insert_inner` computes `Instant::now() + self.ttl`),
/// and the builder's `validate_ttl` only rejects a *zero* ttl, so a near-`Duration::MAX`
/// ttl passes `build()` and then overflows `Instant::checked_add` on every platform
/// (no real `Instant` is anywhere near `Duration::MAX` from the epoch). The fallible
/// `cache_try_set` path uses `TtlOverflow::Error`, so it must report the overflow rather
/// than silently saturating or panicking, and the cache must be left unmutated.
#[cfg(feature = "time_stores")]
#[test]
fn ttl_sorted_cache_try_set_overflow_returns_time_bounds() {
    use cached::time::Duration;
    use cached::{CacheSetError, Cached, TtlSortedCache};

    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::MAX)
        .build()
        .expect("Duration::MAX is non-zero so build() must succeed");

    let result: Result<Option<u32>, CacheSetError> = cache.cache_try_set(1, 42);
    assert!(
        matches!(result, Err(CacheSetError::TimeBounds)),
        "near-MAX ttl must overflow Instant and surface TimeBounds, got {result:?}"
    );

    // The failed try_set must not have stored anything (Error path mutates nothing).
    assert_eq!(cache.cache_size(), 0, "overflowing try_set must not insert");
    assert_eq!(
        cache.cache_get(&1),
        None,
        "overflowing try_set must not insert"
    );

    // The ergonomic alias surfaces the same error.
    let via_alias: Result<Option<u32>, CacheSetError> = cache.try_set(2, 7);
    assert!(
        matches!(via_alias, Err(CacheSetError::TimeBounds)),
        "try_set alias must also surface TimeBounds, got {via_alias:?}"
    );
    assert_eq!(cache.cache_size(), 0);
}

/// `try_set` (the ergonomic alias) delegates to `cache_try_set` and returns the
/// same `Result<Option<V>, CacheSetError>` type.
#[cfg(feature = "time_stores")]
#[test]
fn try_set_alias_returns_cache_set_error() {
    use cached::time::Duration;
    use cached::{CacheSetError, Cached, TtlSortedCache};

    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build TtlSortedCache");

    let result: Result<Option<u32>, CacheSetError> = cache.try_set(1, 7);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
}

// ── Item 5: refresh_on_hit getter on ConcurrentCached ────────────────────────

/// `ConcurrentCached::refresh_on_hit` defaults to `false` on stores that have
/// no TTL (e.g. `ShardedUnboundCache`).
#[test]
fn concurrent_refresh_on_hit_default_false() {
    use cached::{ConcurrentCached, ShardedUnboundCache};

    let cache: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCache::builder().build().expect("build");

    assert!(!ConcurrentCached::refresh_on_hit(&cache));
}

/// On a TTL-capable sharded store, the `ConcurrentCached::set_refresh_on_hit` impl
/// persists the flag in an `AtomicBool`; the store's inherent `refresh_on_hit()`
/// reads it back. The trait-default `refresh_on_hit` returns `false` for stores
/// that do not override it in the `ConcurrentCached` impl (sharded stores expose the
/// getter as an inherent method instead).
#[cfg(feature = "time_stores")]
#[test]
fn concurrent_set_refresh_on_hit_updates_inner_state() {
    use cached::time::Duration;
    use cached::{ConcurrentCached, ShardedTtlCache};

    let cache = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    // Trait default is false (no trait-impl override for the getter on this store).
    assert!(!ConcurrentCached::refresh_on_hit(&cache));

    // `set_refresh_on_hit` returns the previous value (from the AtomicBool swap).
    let prev = ConcurrentCached::set_refresh_on_hit(&cache, true);
    assert!(!prev, "previous value must be false");

    // The inherent `refresh_on_hit()` reads the AtomicBool - confirms the setter worked.
    assert!(cache.refresh_on_hit());

    // Disable via the trait method.
    let prev = ConcurrentCached::set_refresh_on_hit(&cache, false);
    assert!(prev, "previous value must be true");
    assert!(!cache.refresh_on_hit());
}

/// Async counterpart of `concurrent_set_refresh_on_hit_updates_inner_state`:
/// `ConcurrentCachedAsync::set_refresh_on_hit` on `ShardedTtlCache` swaps the inner
/// `AtomicBool` (returning the previous flag), and the store's inherent
/// `refresh_on_hit()` reads it back. The trait-level `refresh_on_hit` getter is the
/// defaulted `false` on this store (neither trait overrides the getter), so the
/// asymmetry between the real inherent state and the trait default is intentional
/// and locked here.
#[cfg(all(feature = "time_stores", feature = "async"))]
#[test]
fn concurrent_async_set_refresh_on_hit_updates_inner_state() {
    use cached::time::Duration;
    use cached::{ConcurrentCachedAsync, ShardedTtlCache};

    let cache = ShardedTtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build ShardedTtlCache");

    // Trait-level getter is the default false (no override on this store).
    assert!(!ConcurrentCachedAsync::refresh_on_hit(&cache));

    // Setter swaps the AtomicBool and reports the previous value.
    let prev = ConcurrentCachedAsync::set_refresh_on_hit(&cache, true);
    assert!(!prev, "previous flag must be false");

    // Inherent getter reflects the new state; trait default still reports false.
    assert!(
        cache.refresh_on_hit(),
        "inherent getter must read the swapped flag"
    );
    assert!(
        !ConcurrentCachedAsync::refresh_on_hit(&cache),
        "trait-default getter intentionally stays false"
    );

    // Round-trip back to false.
    let prev = ConcurrentCachedAsync::set_refresh_on_hit(&cache, false);
    assert!(prev, "previous flag must be true");
    assert!(!cache.refresh_on_hit());
}

// ── short remove/remove_entry aliases remain callable for-effect (no #[must_use]) ──

/// Item 12 locks an intentional asymmetry: `#[must_use]` is on `cache_remove` /
/// `cache_remove_entry` but NOT on the short `remove` / `remove_entry` aliases. This
/// test calls the short aliases purely for-effect (discarding the return value with no
/// `let _ =`); it only compiles cleanly because those aliases are not `#[must_use]`. A
/// regression that added `#[must_use]` to the aliases would raise `unused_must_use` here,
/// and CI runs `clippy --tests -- -D warnings`, so this is an enforced gate.
#[test]
fn short_remove_aliases_callable_for_effect() {
    use cached::{Cached, UnboundCache};

    let mut cache: UnboundCache<u32, u32> =
        UnboundCache::builder().build().expect("build UnboundCache");
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);

    // For-effect calls: return values intentionally dropped (no `let _ =`).
    // These would warn if the aliases were #[must_use].
    cache.remove(&1);
    cache.remove_entry(&2);

    assert_eq!(
        cache.cache_size(),
        0,
        "both entries removed via short aliases"
    );

    // Sanity: the must_use'd canonical methods still return the value as before.
    cache.cache_set(3, 30);
    assert_eq!(cache.cache_remove(&3), Some(30));
}

// ── Item 8: cache_get_or_set_with on ConcurrentCached ────────────────────────

/// On a miss, `cache_get_or_set_with` calls the factory, stores the result, and
/// returns it. On a hit, the factory is not called.
#[test]
fn concurrent_cache_get_or_set_with_hit_and_miss() {
    use cached::{ConcurrentCached, ShardedUnboundCache};

    let cache: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCache::builder().build().expect("build");

    // Miss: factory is invoked and result is stored.
    let v = ConcurrentCached::cache_get_or_set_with(&cache, 1, || 42).expect("infallible");
    assert_eq!(v, 42);

    // Confirm it was stored.
    assert_eq!(cache.cache_get(&1).unwrap(), Some(42));

    // Hit: factory must NOT be called (use a panicking closure to verify).
    let v = ConcurrentCached::cache_get_or_set_with(&cache, 1, || panic!("must not be called"))
        .expect("infallible");
    assert_eq!(v, 42);
}

/// Locks the get-then-return contract with an explicit invocation counter (rather
/// than a panicking closure): the factory runs exactly once across a miss followed by
/// a hit. On the hit the stored value is returned without recomputation.
#[test]
fn concurrent_cache_get_or_set_with_factory_runs_once() {
    use cached::{ConcurrentCached, ShardedUnboundCache};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cache: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCache::builder().build().expect("build");

    let calls = AtomicUsize::new(0);

    // Miss: factory runs and the result is stored.
    let v = ConcurrentCached::cache_get_or_set_with(&cache, 1, || {
        calls.fetch_add(1, Ordering::SeqCst);
        42
    })
    .expect("infallible");
    assert_eq!(v, 42);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "miss must invoke the factory once"
    );

    // Hit: factory must NOT run; the previously stored value is returned verbatim.
    let v = ConcurrentCached::cache_get_or_set_with(&cache, 1, || {
        calls.fetch_add(1, Ordering::SeqCst);
        999
    })
    .expect("infallible");
    assert_eq!(
        v, 42,
        "hit must return the stored value, not the recomputed one"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "hit must not invoke the factory again"
    );
}

/// The ergonomic alias `get_or_set_with` delegates to `cache_get_or_set_with`.
#[test]
fn concurrent_get_or_set_with_alias() {
    use cached::{ConcurrentCached, ShardedUnboundCache};

    let cache: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCache::builder().build().expect("build");

    let v = ConcurrentCached::get_or_set_with(&cache, 10, || 99).expect("infallible");
    assert_eq!(v, 99);

    // Hit path via alias.
    let v2 = ConcurrentCached::get_or_set_with(&cache, 10, || panic!("must not be called"))
        .expect("infallible");
    assert_eq!(v2, 99);
}

// ── Item 8 async: async_cache_get_or_set_with on ConcurrentCachedAsync ────────

#[cfg(feature = "async")]
mod async_cache_get_or_set_with_tests {
    use cached::{ConcurrentCachedAsync, ShardedUnboundCache};

    /// On a miss, `async_cache_get_or_set_with` calls the async factory, stores
    /// the result, and returns it. On a hit, the factory is not called.
    #[tokio::test]
    async fn hit_and_miss() {
        let cache: ShardedUnboundCache<u32, u32> =
            ShardedUnboundCache::builder().build().expect("build");

        // Miss: factory runs.
        let v = ConcurrentCachedAsync::async_cache_get_or_set_with(&cache, 1, || async { 55 })
            .await
            .expect("infallible");
        assert_eq!(v, 55);

        // Confirm stored.
        let stored = ConcurrentCachedAsync::async_cache_get(&cache, &1)
            .await
            .unwrap();
        assert_eq!(stored, Some(55));

        // Hit: factory must NOT run.
        let v = ConcurrentCachedAsync::async_cache_get_or_set_with(&cache, 1, || async {
            panic!("must not be called")
        })
        .await
        .expect("infallible");
        assert_eq!(v, 55);
    }

    /// Counter-based version of the get-then-return contract for the async variant:
    /// the async factory runs exactly once across a miss followed by a hit, and the
    /// hit returns the stored value without recomputation.
    #[tokio::test]
    async fn async_factory_runs_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache: ShardedUnboundCache<u32, u32> =
            ShardedUnboundCache::builder().build().expect("build");

        let calls = AtomicUsize::new(0);

        let v = ConcurrentCachedAsync::async_cache_get_or_set_with(&cache, 1, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            42
        })
        .await
        .expect("infallible");
        assert_eq!(v, 42);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "miss must run the factory once"
        );

        let v = ConcurrentCachedAsync::async_cache_get_or_set_with(&cache, 1, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            999
        })
        .await
        .expect("infallible");
        assert_eq!(v, 42, "hit returns the stored value");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "hit must not run the factory again"
        );
    }

    /// `async_cache_get_or_set_with` on a TTL-capable async sharded store
    /// (`ShardedTtlCache`): a miss computes and stores through the time-bounded
    /// store, and a subsequent hit on the live entry skips the factory.
    #[cfg(feature = "time_stores")]
    #[tokio::test]
    async fn ttl_store_hit_and_miss() {
        use cached::ShardedTtlCache;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedTtlCache");

        let calls = AtomicUsize::new(0);

        // Miss: factory runs and the value is stored in the TTL store.
        let v = ConcurrentCachedAsync::async_cache_get_or_set_with(&cache, 1, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            77
        })
        .await
        .expect("infallible");
        assert_eq!(v, 77);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Confirm it was stored.
        let stored = ConcurrentCachedAsync::async_cache_get(&cache, &1)
            .await
            .unwrap();
        assert_eq!(stored, Some(77));

        // Hit on the live entry: factory must NOT run.
        let v = ConcurrentCachedAsync::async_cache_get_or_set_with(&cache, 1, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            999
        })
        .await
        .expect("infallible");
        assert_eq!(v, 77, "live hit returns the stored value");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "live hit must not run the factory"
        );
    }
}

// ── Item 6: DiskCache aliases removed ────────────────────────────────────────

// No runtime test needed; the aliases were removed as a compile-time change.
// The existing test `redb_cache_builder_zero_ttl_validation` in tests/cached.rs
// confirms that `RedbCache` is the sole name and that builder validation works.

// ── Item 7: store() getters removed - public API assertions cover the same ground ──

/// `UnboundCache` entry count is accessible via `cache_size()`; `store()` is gone.
#[test]
fn unbound_cache_size_via_public_api() {
    use cached::Cached;
    let mut cache = UnboundCache::<u32, u32>::builder().build().unwrap();
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    assert_eq!(cache.cache_size(), 2);
    assert!(cache.cache_get(&1).is_some());
    assert!(cache.cache_get(&2).is_some());
}

/// `TtlCache` entry count and lookups are accessible via public API; `store()` is gone.
#[cfg(feature = "time_stores")]
#[test]
fn ttl_cache_size_and_lookup_via_public_api() {
    use cached::time::Duration;
    use cached::{Cached, TtlCache};
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_size(), 1);
    assert_eq!(cache.cache_get(&1), Some(&10));
}

/// `LruTtlCache` metrics are accessible directly on the cache; `store()` is gone.
#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_cache_metrics_via_public_api() {
    use cached::time::Duration;
    use cached::{Cached, LruTtlCache};
    let mut cache = LruTtlCache::<u32, u32>::builder()
        .max_size(4)
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();
    assert!(cache.cache_get(&1).is_some());
    assert_eq!(cache.cache_hits(), Some(1));
    assert_eq!(cache.cache_misses(), Some(0));
}

// ── set_ttl(Duration::ZERO) on sharded TTL stores no longer panics (I2) ───────
//
// The inherent `set_ttl` and the `ConcurrentCached::set_ttl` delegation used to
// `assert!(!ttl.is_zero())` and panic on a zero ttl. After the fix a zero ttl is
// stored unchecked, so the call returns normally and any subsequently inserted
// entry is immediately expired (read back as a miss).
#[cfg(feature = "time_stores")]
mod sharded_set_ttl_zero {
    use cached::time::Duration;
    use cached::{ConcurrentCached, ShardedLruTtlCache, ShardedTtlCache};

    #[test]
    fn sharded_ttl_inherent_set_ttl_zero_does_not_panic_and_expires() {
        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedTtlCache");

        // Inherent set_ttl(ZERO) must not panic.
        let prev = cache.set_ttl(Duration::ZERO);
        assert_eq!(prev, Some(Duration::from_secs(60)));
        assert_eq!(cache.ttl(), Some(Duration::ZERO));

        // A freshly inserted entry is immediately expired -> read back as a miss.
        cache.cache_set(1, 10).unwrap();
        assert_eq!(cache.cache_get(&1), Ok(None));
    }

    #[test]
    fn sharded_ttl_trait_set_ttl_zero_does_not_panic_and_expires() {
        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedTtlCache");

        // The `ConcurrentCached::set_ttl` delegation must not panic either.
        let prev = ConcurrentCached::set_ttl(&cache, Duration::ZERO);
        assert_eq!(prev, Some(Duration::from_secs(60)));

        cache.cache_set(2, 20).unwrap();
        assert_eq!(cache.cache_get(&2), Ok(None));
    }

    #[test]
    fn sharded_lru_ttl_inherent_set_ttl_zero_does_not_panic_and_expires() {
        let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedLruTtlCache");

        let prev = cache.set_ttl(Duration::ZERO);
        assert_eq!(prev, Some(Duration::from_secs(60)));
        assert_eq!(cache.ttl(), Some(Duration::ZERO));

        cache.cache_set(1, 10).unwrap();
        assert_eq!(cache.cache_get(&1), Ok(None));
    }

    #[test]
    fn sharded_lru_ttl_trait_set_ttl_zero_does_not_panic_and_expires() {
        let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
            .max_size(8)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedLruTtlCache");

        let prev = ConcurrentCached::set_ttl(&cache, Duration::ZERO);
        assert_eq!(prev, Some(Duration::from_secs(60)));

        cache.cache_set(2, 20).unwrap();
        assert_eq!(cache.cache_get(&2), Ok(None));
    }

    #[test]
    fn sharded_ttl_unset_ttl_after_zero_disables_expiry() {
        // unset_ttl must still mean "never expires", distinct from set_ttl(ZERO).
        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build ShardedTtlCache");

        let _ = cache.set_ttl(Duration::ZERO);
        assert_eq!(cache.unset_ttl(), Some(Duration::ZERO));
        assert_eq!(cache.ttl(), None);

        cache.cache_set(3, 30).unwrap();
        assert_eq!(cache.cache_get(&3), Ok(Some(30)));
    }
}

// ── Builder missing-required errors are server-free (C1) ──────────────────────
//
// The redis/redb builders are now no-arg; the former positional args
// (`prefix`/`ttl` for redis, `name` for redb) are required setters. A `build()`
// with a required field unset must return `BuildError::MissingRequired(...)`
// WITHOUT attempting any IO/connection, so these tests need no live server.
#[cfg(feature = "disk_store")]
#[test]
fn redb_builder_missing_name_is_server_free_error() {
    use cached::{BuildError, RedbCache, RedbCacheBuildError};
    let result = RedbCache::<u32, u32>::builder().build();
    assert!(
        matches!(
            result,
            Err(RedbCacheBuildError::Build(BuildError::MissingRequired(
                "name"
            )))
        ),
        "expected Build(MissingRequired(\"name\"))"
    );
}

#[cfg(feature = "redis_store")]
#[test]
fn redis_builder_missing_required_is_server_free_error() {
    use cached::{BuildError, RedisCache, RedisCacheBuildError};

    // No prefix and no ttl -> prefix is reported first.
    let result = RedisCache::<u32, u32>::builder().build();
    assert!(
        matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::MissingRequired(
                "prefix"
            )))
        ),
        "expected Build(MissingRequired(\"prefix\"))"
    );

    // prefix set, ttl unset -> ttl is reported, still before any connection attempt.
    let result = RedisCache::<u32, u32>::builder().prefix("x").build();
    assert!(
        matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::MissingRequired(
                "ttl"
            )))
        ),
        "expected Build(MissingRequired(\"ttl\"))"
    );
}
