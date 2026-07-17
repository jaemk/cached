# Disk (redb) backend

`RedbCache` is a disk-backed concurrent store using `redb`, gated behind `redb_store`. It is
self-synchronizing over a shared `&self` and builder-only: `builder(name)` takes the required
cache name positionally; the on-disk directory (`disk_dir`) is optional with a user cache dir fallback
(`BaseDirs::cache_dir()`, e.g. `~/.cache`) and a further OS temp dir fallback.

## REDB-1

Entries persist to a redb database file on disk. Values are serialized; reads that fail to
deserialize are treated as a miss (self-healing default), consistent with
[design/0029-self-healing-deserialization-default.md](design/0029-self-healing-deserialization-default.md).

## REDB-2

Refresh-on-hit re-validates inside the write transaction, per
[design/0033-redb-revalidate-in-write-txn.md](design/0033-redb-revalidate-in-write-txn.md).
Amortizing the refresh-on-hit write-txn cost is an open direction
([design/0021-redb-refresh-on-hit-cost.md](design/0021-redb-refresh-on-hit-cost.md)).

## REDB-3

Errors are `RedbCacheError` (build: `RedbCacheBuildError`) with named variants, per
[design/0005-store-error-consistency.md](design/0005-store-error-consistency.md).

## REDB-4

Implements the concurrent trait family (`ConcurrentCacheBase`, `ConcurrentCached`,
`ConcurrentCachedAsync`, `ConcurrentCacheTtl`, `SerializeCached`, `SerializeCachedAsync`).
`ConcurrentCacheEvict` is intentionally not implemented; expired-entry sweeping is the fallible
inherent `remove_expired_entries()`. `disk_path()` ships (returns the resolved database path
after build).
Builder-side resolved-path introspection and a configurable temp-dir fallback remain open
([design/0025-redb-disk-path-introspection.md](design/0025-redb-disk-path-introspection.md)). See
[traits-concurrent.md](traits-concurrent.md).
