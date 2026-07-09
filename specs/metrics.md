# Cache metrics

`cache.metrics()` returns a `CacheMetrics` snapshot on any `Cached` store (and through the
concurrent traits). Introduced to expose sharded metrics through a trait, per
[design/0012-concurrent-metrics-trait.md](design/0012-concurrent-metrics-trait.md).

## METRIC-1

`CacheMetrics` is a `#[non_exhaustive]` struct deriving `Default`. Fields: `hits`, `misses`,
`evictions` (all `Option<u64>`), `entry_count: Option<usize>`, `capacity: Option<usize>`. It has
a `hit_ratio() -> Option<f64>` method.

## METRIC-2

A field is `None` when the store cannot report it: `entry_count` is `None` for stores that cannot
count (e.g. redis, redb); `evictions` is `None` for stores that never evict (e.g.
`UnboundCache`); `capacity` is `None` for unbounded stores.

## METRIC-3

Per-metric accessors on `Cached` (`cache_hits`, `cache_misses`, `cache_evictions`,
`cache_capacity`) mirror the snapshot fields. `cache_reset_metrics` clears the counters without
touching entries. A peek (`cache_peek`) does not record a hit or miss; see
[traits-core.md](traits-core.md).
