//! Outside-in coverage for the `copy_from(...)` fallibility change on the six sharded
//! builders.
//!
//! In the 3.0 window each sharded builder's `copy_from` changed from panicking on an
//! invalid builder config to returning `Result<_, BuildError>` (it now forwards
//! `self.build()?`). The existing store-level tests only exercise the `Ok` path (they
//! `.unwrap()` the result). These tests pin the *error* path: an invalid builder config
//! must surface as `Err(BuildError)` from `copy_from` rather than a panic or a silently
//! wrong cache.
//!
//! Reachable invalid config: `shards(0)`. Every sharded `build()` funnels the shard count
//! through `checked_shard_count`, which rejects a zero count with
//! `BuildError::InvalidValue { field: "shards", .. }`. Because `copy_from` calls
//! `self.build()?` before touching `existing`, that error propagates unchanged. A second
//! reachable case (`MissingRequired`) is covered for the LRU builder to prove `copy_from`
//! forwards *whatever* `BuildError` `build()` produces, not just the shard error.

use cached::{BuildError, ConcurrentCached, ShardedLruCache, ShardedUnboundCache};

// A value that never expires, for the value-defined-expiry sharded stores
// (`ShardedExpiringCache` / `ShardedExpiringLruCache` require `V: Expires`).
#[derive(Debug, Clone, PartialEq)]
struct Never(u32);

impl cached::Expires for Never {
    fn is_expired(&self) -> bool {
        false
    }
}

/// `ShardedUnboundCache::builder().shards(0).copy_from(&existing)` returns
/// `Err(BuildError::InvalidValue { field: "shards", .. })` — the same error `build()`
/// would produce — instead of panicking.
#[test]
fn unbound_copy_from_zero_shards_is_err() {
    let existing: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::builder()
        .build()
        .expect("valid existing");
    existing.cache_set(1, 10).unwrap();

    let result = ShardedUnboundCache::<u32, u32>::builder()
        .shards(0)
        .copy_from(&existing);

    assert!(
        matches!(
            result,
            Err(BuildError::InvalidValue {
                field: "shards",
                ..
            })
        ),
        "copy_from with shards(0) must return the shard-count BuildError, got a differing result",
    );

    // The source cache must be untouched by the failed copy.
    assert_eq!(existing.cache_get(&1).unwrap(), Some(10));
}

/// `ShardedLruCache` `copy_from` propagates the shard-count error even with a valid
/// `max_size` set.
#[test]
fn lru_copy_from_zero_shards_is_err() {
    let existing: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(16)
        .build()
        .expect("valid existing");
    existing.cache_set(1, 10).unwrap();

    let result = ShardedLruCache::<u32, u32>::builder()
        .max_size(16)
        .shards(0)
        .copy_from(&existing);

    assert!(
        matches!(
            result,
            Err(BuildError::InvalidValue {
                field: "shards",
                ..
            })
        ),
        "LRU copy_from with shards(0) must return the shard-count BuildError",
    );
    assert_eq!(existing.cache_get(&1).unwrap(), Some(10));
}

/// `ShardedLruCache` `copy_from` also forwards a *different* `BuildError` variant:
/// omitting `max_size` yields `MissingRequired("max_size")`. This proves `copy_from`
/// surfaces whatever `build()` rejects, not only the shard error.
#[test]
fn lru_copy_from_missing_max_size_is_err() {
    let existing: ShardedLruCache<u32, u32> = ShardedLruCache::builder()
        .max_size(16)
        .build()
        .expect("valid existing");

    // No max_size on the new builder -> build() fails with MissingRequired.
    let result = ShardedLruCache::<u32, u32>::builder().copy_from(&existing);

    assert!(
        matches!(result, Err(BuildError::MissingRequired("max_size"))),
        "LRU copy_from without max_size must surface MissingRequired, not a shard error or panic",
    );
}

/// `ShardedExpiringCache::builder().shards(0).copy_from(&existing)` returns the
/// shard-count `BuildError`. Exercises the value-defined-expiry sharded store.
#[test]
fn expiring_copy_from_zero_shards_is_err() {
    use cached::ShardedExpiringCache;

    let existing: ShardedExpiringCache<u32, Never> = ShardedExpiringCache::builder()
        .build()
        .expect("valid existing");
    existing.cache_set(1, Never(10)).unwrap();

    let result = ShardedExpiringCache::<u32, Never>::builder()
        .shards(0)
        .copy_from(&existing);

    assert!(
        matches!(
            result,
            Err(BuildError::InvalidValue {
                field: "shards",
                ..
            })
        ),
        "ShardedExpiringCache copy_from with shards(0) must return the shard-count BuildError",
    );
    assert_eq!(existing.cache_get(&1).unwrap(), Some(Never(10)));
}

/// `ShardedExpiringLruCache::builder()...shards(0).copy_from(&existing)` returns the
/// shard-count `BuildError` even with a valid `max_size`.
#[test]
fn expiring_lru_copy_from_zero_shards_is_err() {
    use cached::ShardedExpiringLruCache;

    let existing: ShardedExpiringLruCache<u32, Never> = ShardedExpiringLruCache::builder()
        .max_size(16)
        .build()
        .expect("valid existing");
    existing.cache_set(1, Never(10)).unwrap();

    let result = ShardedExpiringLruCache::<u32, Never>::builder()
        .max_size(16)
        .shards(0)
        .copy_from(&existing);

    assert!(
        matches!(
            result,
            Err(BuildError::InvalidValue {
                field: "shards",
                ..
            })
        ),
        "ShardedExpiringLruCache copy_from with shards(0) must return the shard-count BuildError",
    );
    assert_eq!(existing.cache_get(&1).unwrap(), Some(Never(10)));
}

#[cfg(feature = "time_stores")]
mod time_stores {
    use super::*;
    use cached::time::Duration;
    use cached::{ShardedLruTtlCache, ShardedTtlCache};

    /// `ShardedTtlCache` `copy_from` propagates the shard-count error even with a valid ttl.
    #[test]
    fn ttl_copy_from_zero_shards_is_err() {
        let existing: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("valid existing");
        existing.cache_set(1, 10).unwrap();

        let result = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .shards(0)
            .copy_from(&existing);

        assert!(
            matches!(
                result,
                Err(BuildError::InvalidValue {
                    field: "shards",
                    ..
                })
            ),
            "ShardedTtlCache copy_from with shards(0) must return the shard-count BuildError",
        );
        assert_eq!(existing.cache_get(&1), Ok(Some(10)));
    }

    /// `ShardedLruTtlCache` (NoEvict typestate) `copy_from` propagates the shard-count error.
    #[test]
    fn lru_ttl_copy_from_zero_shards_is_err() {
        let existing: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
            .max_size(16)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("valid existing");
        existing.cache_set(1, 10).unwrap();

        let result = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(16)
            .ttl(Duration::from_secs(60))
            .shards(0)
            .copy_from(&existing);

        assert!(
            matches!(
                result,
                Err(BuildError::InvalidValue {
                    field: "shards",
                    ..
                })
            ),
            "ShardedLruTtlCache (NoEvict) copy_from with shards(0) must return the shard BuildError",
        );
        assert_eq!(existing.cache_get(&1), Ok(Some(10)));
    }

    /// `ShardedLruTtlCache` in the `HasEvict` typestate (after `.on_evict(...)`) has a
    /// *separate* `copy_from` impl; it must also forward the shard-count error.
    #[test]
    fn lru_ttl_has_evict_copy_from_zero_shards_is_err() {
        let existing: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
            .max_size(16)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("valid existing");
        existing.cache_set(1, 10).unwrap();

        // `.on_evict` flips the builder to the HasEvict typestate, selecting the other
        // `copy_from` impl.
        let result = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(16)
            .ttl(Duration::from_secs(60))
            .shards(0)
            .on_evict(|_, _| {})
            .copy_from(&existing);

        assert!(
            matches!(
                result,
                Err(BuildError::InvalidValue {
                    field: "shards",
                    ..
                })
            ),
            "ShardedLruTtlCache (HasEvict) copy_from with shards(0) must return the shard BuildError",
        );
        assert_eq!(existing.cache_get(&1), Ok(Some(10)));
    }
}
