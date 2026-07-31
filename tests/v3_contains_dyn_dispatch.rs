//! `ConcurrentCached::cache_contains` dropped its `where Self: Sized` bound, so it is part of the
//! vtable and callable through `dyn ConcurrentCached`. Backed by `ShardedUnboundCache`, which is
//! unconditional (no feature gate needed).

use cached::{ConcurrentCached, ShardedUnboundCache};
use std::convert::Infallible;

#[test]
fn cache_contains_through_trait_object() {
    let cache = ShardedUnboundCache::<u32, String>::new();
    let boxed: Box<dyn ConcurrentCached<u32, String, Error = Infallible>> = Box::new(cache);

    // Absent key reads false through the trait object.
    assert!(!boxed.cache_contains(&1).unwrap());

    // Insert through the same trait object (cache_set is object-safe too), then observe presence.
    assert_eq!(boxed.cache_set(1, "one".to_string()).unwrap(), None);
    assert!(boxed.cache_contains(&1).unwrap());
    assert!(!boxed.cache_contains(&2).unwrap());

    // Removing the entry flips contains back to false, still through the trait object.
    assert_eq!(boxed.cache_remove(&1).unwrap(), Some("one".to_string()));
    assert!(!boxed.cache_contains(&1).unwrap());
}

/// A monomorphic function that only ever sees the erased trait object proves `cache_contains`
/// resolves through the vtable rather than a `Sized` concrete type.
fn contains_via_dyn(
    store: &dyn ConcurrentCached<u32, String, Error = Infallible>,
    key: u32,
) -> bool {
    store.cache_contains(&key).unwrap()
}

#[test]
fn cache_contains_via_borrowed_trait_object() {
    let cache = ShardedUnboundCache::<u32, String>::new();
    assert!(!contains_via_dyn(&cache, 7));
    cache.cache_set(7, "seven".to_string()).unwrap();
    assert!(contains_via_dyn(&cache, 7));
}

// A second, structurally different implementor exercised through the vtable. The TTL family's
// `cache_contains` has a non-trivial body (peek-based, expiry-aware) distinct from the unbound
// store's plain map lookup, so dispatching it through `dyn` guards against an impl regressing to
// `where Self: Sized` (which would drop it from the vtable and fail to compile here).
#[cfg(feature = "time_stores")]
#[test]
fn cache_contains_through_trait_object_ttl_family() {
    use cached::ShardedTtlCache;
    use std::time::Duration;

    let cache = ShardedTtlCache::<u32, String>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    let boxed: Box<dyn ConcurrentCached<u32, String, Error = Infallible>> = Box::new(cache);

    assert!(!boxed.cache_contains(&1).unwrap());
    assert_eq!(boxed.cache_set(1, "one".to_string()).unwrap(), None);
    assert!(boxed.cache_contains(&1).unwrap());
    assert!(!boxed.cache_contains(&2).unwrap());
    assert_eq!(boxed.cache_remove(&1).unwrap(), Some("one".to_string()));
    assert!(!boxed.cache_contains(&1).unwrap());
}

// Compile-only object-safety guard covering the IO implementors, which need live services to run.
// If `cache_contains` (or any method the erased handle uses) regained a `Self: Sized` bound, or an
// IO impl reintroduced one, coercing the concrete store to `&dyn ConcurrentCached` would stop
// compiling and fail the build -- no Redis/redb server required. Kept monomorphic over concrete
// `u64`/`String` so the guard needs no `serde` in the test crate's extern prelude. Never called.
#[cfg(feature = "redis_store")]
#[allow(dead_code)]
fn _redis_cache_is_object_safe(
    c: &cached::RedisCache<u64, String>,
) -> &dyn ConcurrentCached<u64, String, Error = cached::RedisCacheError> {
    c
}

#[cfg(feature = "redb_store")]
#[allow(dead_code)]
fn _redb_cache_is_object_safe(
    c: &cached::RedbCache<u64, String>,
) -> &dyn ConcurrentCached<u64, String, Error = cached::RedbCacheError> {
    c
}
