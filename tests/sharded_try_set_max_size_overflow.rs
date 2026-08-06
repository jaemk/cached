//! `try_set_max_size` is documented as the no-panic counterpart of `set_max_size`. On a
//! multi-shard cache, requesting a `max_size` close to `usize::MAX` overflows the internal
//! per-shard-capacity multiplication (`n_shards * per_shard_cap`), so the fallible setter must
//! return `Err(SetMaxSizeError::CapacityOverflow)` rather than propagate the panic from
//! `set_max_size`.

use cached::{Expires, SetMaxSizeError, ShardedExpiringLruCache, ShardedLruCache};

#[cfg(feature = "time_stores")]
use cached::ShardedLruTtlCache;

/// A per-shard capacity of at least this many shards is needed to force the
/// `n_shards * per_shard_cap` multiplication to overflow `usize` for a `max_size` near
/// `usize::MAX`. Any value greater than one shard works; use a handful for good measure.
const OVERFLOW_SHARDS: usize = 8;

/// Minimal never-expiring value for exercising `ShardedExpiringLruCache`, which requires
/// `V: Expires`. Only `try_set_max_size` is under test here, so expiry itself is irrelevant.
#[derive(Clone)]
struct NeverExpires;

impl Expires for NeverExpires {
    fn is_expired(&self) -> bool {
        false
    }
}

#[test]
fn lru_try_set_max_size_rejects_zero() {
    let cache: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(16)
        .shards(OVERFLOW_SHARDS)
        .build()
        .expect("build ShardedLruCache");

    let err = cache.try_set_max_size(0).unwrap_err();
    assert_eq!(err, SetMaxSizeError::ZeroMaxSize);
}

#[test]
fn lru_try_set_max_size_rejects_overflow_on_multi_shard_cache() {
    let cache: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(16)
        .shards(OVERFLOW_SHARDS)
        .build()
        .expect("build ShardedLruCache");
    assert!(
        cache.shards() > 1,
        "fixture must be multi-shard, or the overflow this test targets cannot occur"
    );

    let err = cache
        .try_set_max_size(usize::MAX)
        .expect_err("try_set_max_size must not panic and must reject an unrepresentable total");
    assert_eq!(err, SetMaxSizeError::CapacityOverflow);
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_try_set_max_size_rejects_zero() {
    let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .max_size(16)
        .shards(OVERFLOW_SHARDS)
        .ttl(std::time::Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");

    let err = cache.try_set_max_size(0).unwrap_err();
    assert_eq!(err, SetMaxSizeError::ZeroMaxSize);
}

#[cfg(feature = "time_stores")]
#[test]
fn lru_ttl_try_set_max_size_rejects_overflow_on_multi_shard_cache() {
    let cache: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
        .max_size(16)
        .shards(OVERFLOW_SHARDS)
        .ttl(std::time::Duration::from_secs(60))
        .build()
        .expect("build ShardedLruTtlCache");
    assert!(
        cache.shards() > 1,
        "fixture must be multi-shard, or the overflow this test targets cannot occur"
    );

    let err = cache
        .try_set_max_size(usize::MAX)
        .expect_err("try_set_max_size must not panic and must reject an unrepresentable total");
    assert_eq!(err, SetMaxSizeError::CapacityOverflow);
}

#[test]
fn expiring_lru_try_set_max_size_rejects_zero() {
    let cache: ShardedExpiringLruCache<u32, NeverExpires> = ShardedExpiringLruCache::builder()
        .max_size(16)
        .shards(OVERFLOW_SHARDS)
        .build()
        .expect("build ShardedExpiringLruCache");

    let err = cache.try_set_max_size(0).unwrap_err();
    assert_eq!(err, SetMaxSizeError::ZeroMaxSize);
}

#[test]
fn expiring_lru_try_set_max_size_rejects_overflow_on_multi_shard_cache() {
    let cache: ShardedExpiringLruCache<u32, NeverExpires> = ShardedExpiringLruCache::builder()
        .max_size(16)
        .shards(OVERFLOW_SHARDS)
        .build()
        .expect("build ShardedExpiringLruCache");
    assert!(
        cache.shards() > 1,
        "fixture must be multi-shard, or the overflow this test targets cannot occur"
    );

    let err = cache
        .try_set_max_size(usize::MAX)
        .expect_err("try_set_max_size must not panic and must reject an unrepresentable total");
    assert_eq!(err, SetMaxSizeError::CapacityOverflow);
}
