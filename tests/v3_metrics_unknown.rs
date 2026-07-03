//! Regression coverage for `CacheMetrics::entry_count` being `Option<usize>`.
//!
//! Before the 3.0 breaking window, `entry_count` was a plain `usize` and the
//! `ConcurrentCacheBase::metrics` default masked an unknown size (`cache_size()`
//! returning `Ok(None)`, as `RedisCache`/`RedbCache` do) as a false `0`. These tests
//! prove that "unknown" now propagates as `None` while stores that report an exact
//! size still surface `Some(n)`.

use cached::{CacheMetrics, ConcurrentCacheBase};

/// Minimal `ConcurrentCacheBase` impl that leaves `cache_size()` at its default,
/// which returns `Ok(None)` — mirroring redis/redb.
#[derive(Default)]
struct UnknownSizeStore;

impl ConcurrentCacheBase for UnknownSizeStore {
    type Error = std::convert::Infallible;
    // cache_size() intentionally not overridden: default is Ok(None).
}

/// A store whose `cache_size()` returns `Ok(None)` must yield `entry_count == None`
/// through the trait-default `metrics()` — not a masked `0`.
#[test]
fn concurrent_base_metrics_propagates_unknown_entry_count() {
    let store = UnknownSizeStore;
    let m: CacheMetrics = store.metrics();
    assert_eq!(
        m.entry_count, None,
        "unknown cache_size() must surface as None, not a false 0"
    );
}

/// A store that reports an exact size still surfaces `Some(n)`.
#[test]
fn concurrent_base_metrics_reports_known_entry_count() {
    struct KnownSizeStore;
    impl ConcurrentCacheBase for KnownSizeStore {
        type Error = std::convert::Infallible;
        fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
            Ok(Some(7))
        }
    }

    let m = KnownSizeStore.metrics();
    assert_eq!(m.entry_count, Some(7));
}

/// Sharded in-memory stores report an exact `Some(n)` count through their inherent
/// `metrics()`.
#[test]
fn sharded_store_reports_some_entry_count() {
    use cached::{ConcurrentCached, ShardedUnboundCache};

    let cache: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCache::builder().shards(4).build().unwrap();
    ConcurrentCached::cache_set(&cache, 1, 10).unwrap();
    ConcurrentCached::cache_set(&cache, 2, 20).unwrap();

    assert_eq!(cache.metrics().entry_count, Some(2));
}
