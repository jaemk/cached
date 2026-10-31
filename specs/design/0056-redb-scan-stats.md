# 0056 - `RedbCache::scan_stats`: an explicit full-table scan

Status: Implemented

## Previous state

`RedbCache` had no public way to count its entries or read their timestamps.
`ConcurrentCacheBase::cache_size` returns `Ok(None)` because a count is an O(n) scan, and the
store has no iterator. Issue #336 (qsv's `geocode cache-info`) needed the entry count and the
oldest/newest `created_at` per cache, so qsv opened the redb file itself with
`redb::ReadOnlyDatabase` and decoded `CachedDiskValue` with `rmp_serde`. That couples a downstream
crate to the frozen on-disk layout (REDB-6), and it has to drop its `RedbCache` first because redb
holds an exclusive lock on the file.

## Decision

An inherent, opt-in scan on `RedbCache`, with an async counterpart:

```rust
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RedbCacheStats {
    pub entries: usize,
    pub expired: usize,
    pub undecodable: usize,
    pub oldest: Option<SystemTime>,
    pub newest: Option<SystemTime>,
}

impl<K, V> RedbCache<K, V> {
    pub fn scan_stats(&self) -> Result<RedbCacheStats, RedbCacheError>;
    pub async fn async_scan_stats(&self) -> Result<RedbCacheStats, RedbCacheError>; // `async`
}
```

- **One read transaction, no writes.** The scan is a single MVCC read, so it never blocks or is
  blocked by a writer and the result is a consistent point-in-time view of the table. It never
  opens a write transaction: no self-heal, no expiry eviction, no refresh.
- **One time snapshot.** `expired` is computed against one `SystemTime::now()` taken before the
  scan, the same rule `remove_expired_entries` follows, and against the TTL current at the call
  (TTL is retroactive, see `ConcurrentCacheTtl::set_ttl`). With no TTL, `expired` is 0.
- **Undecodable entries are counted in both modes.** `strict_deserialization` governs reads that
  return a value; a diagnostic scan reports corruption rather than aborting on it, so it never
  returns `CacheDeserialization` and never deletes the entry.
- **Decoded as `CachedDiskValue<V>`.** Decoding only `created_at` (an `IgnoredAny` value) would be
  cheaper, but then `undecodable` would not mean "an entry `cache_get` would self-heal or reject":
  a value whose bytes no longer match `V` would count as healthy. Decoding with `V` keeps the count
  consistent with `cache_get` and `remove_expired_entries`.
- **No `Copy`.** `#[non_exhaustive]` leaves room for new fields in a minor release; deriving
  `Copy` would rule out any non-`Copy` field later. Matches `CacheMetrics`.
- **`entries` counts every stored row**, expired and undecodable included. `oldest`/`newest` are
  the min/max `created_at` over decodable entries, expired included. With `refresh_on_hit`
  enabled, `created_at` is the last write or refresh, not the original insert.
- **`cache_size()` and `metrics()` are unchanged** and keep returning `Ok(None)` / `None`: the
  cost is only paid when the caller asks for it by name.

## Naming

`scan_stats`, not the issue's `stats`. `metrics()` already exists on every store and is O(1); a
bare `stats()` beside it reads as an equally cheap sibling. Same concern as 0039, which declined
whole-cache traversal partly because a one-word name hides the cost.

## Relation to 0039

0039 declined iteration on the sharded stores because a cross-shard view is either a global lock,
an unbounded clone, or weakly consistent. None of that applies here: `RedbCache` is one table, a
redb read transaction is a consistent snapshot that holds no lock writers wait on, and the scan
returns fixed-size aggregates rather than the entries themselves.

## Notes

- Additive: a new struct and two inherent methods. Ships in a minor release.
- Related: REDB-6 (frozen format), 0029 (self-healing default), 0039 (declined sharded iteration).
