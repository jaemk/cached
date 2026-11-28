# Disk (redb) backend

`RedbCache` is a disk-backed concurrent store using `redb`, gated behind `redb_store`. It is
self-synchronizing over a shared `&self` and builder-only: `builder(name)` takes the required
cache name positionally; the on-disk directory (`disk_dir`) is optional with a user cache dir fallback
(`BaseDirs::cache_dir()`, e.g. `~/.cache`) and a further OS temp dir fallback.

## REDB-1

Entries persist to a redb database file on disk. Lookups self-heal corrupt or
schema-incompatible entries by default. Strict lookups return a deserialization error and
preserve them. See [design 0029](design/0029-self-healing-deserialization-default.md) and the
compression exceptions in [REDB-9](#redb-9).

## REDB-2

Refresh-on-hit re-validates inside the write transaction, per
[design/0033-redb-revalidate-in-write-txn.md](design/0033-redb-revalidate-in-write-txn.md).
Lookup reuses the decoded value when the stored bytes match the read snapshot and
decodes newly written bytes when they differ.
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
after build). `scan_stats()` is the opt-in O(n) counterpart to `cache_size()` (see REDB-7).
Builder-side resolved-path introspection and a configurable temp-dir fallback remain open
([design/0025-redb-disk-path-introspection.md](design/0025-redb-disk-path-introspection.md)). See
[traits-concurrent.md](traits-concurrent.md).

## REDB-5

`RedbCacheError` and `RedbCacheBuildError` `Display` interpolates the underlying source (e.g.
`#[error("storage error: {source}")]`) instead of discarding it, so the cause is readable from
`Display` even though the source type itself is not public API (per
[design/0005-store-error-consistency.md](design/0005-store-error-consistency.md)). Previously
`{e}` rendered only the bare variant string (e.g. "Storage error"); `Debug` already showed the
cause. `Display` text is not semver-guarded, so this is a non-breaking change.

## REDB-6

Stored entries are MessagePack in the compact (non-named) form: every write goes through
`rmp_serde::to_vec`, never `to_vec_named`, so an entry is a positional 2-element array of `value`
then `created_at` and the field names are not on the wire. Field order is part of the frozen 3.x
on-disk layout, alongside the version in the file name and the redb table name: reordering,
inserting or removing a field reinterprets every stored entry and must bump `DISK_FILE_VERSION`.
`tests/frozen_format_golden.rs` pins the serialized bytes (no server required).

## REDB-7

`scan_stats()` (and `async_scan_stats()` under `async`) scans the whole table under one read
transaction and returns a `RedbCacheStats`: `entries` (every stored row), `expired` (decodable
entries whose age is >= the current TTL, 0 with no TTL), `undecodable`, and the `oldest`/`newest`
`created_at`. It never writes: no self-heal, no eviction, no refresh, and undecodable entries are
counted in both deserialization modes. Decompression limits and allocation errors are handled
as specified in [REDB-9](#redb-9). `cache_size()` stays `Ok(None)`. See [design/0056-redb-scan-stats.md](design/0056-redb-scan-stats.md).

## REDB-8

`redb_store` depends on redb 5. Cache files written under redb 4 (cached 4.1.1 and earlier) open
and read back unchanged: the table is `&str -> &[u8]`, so redb 5's incompatible `Option` value
encoding does not apply, and no `DISK_FILE_VERSION` bump is needed.
`tests/fixtures/redb4_fixture_v3.redb` is a file written by cached 4.1.1 on redb 4.6, and
`tests/frozen_format_golden.rs` opens it with the current redb. redb types stay out of the public
API: `RedbCacheError`/`RedbCacheBuildError` box their sources (REDB-5), so the redb major version
is not part of cached's semver surface. redb 5 changed its file-locking protocol, so two processes
on different redb majors must not open the same cache file at the same time.

## REDB-9

With `compression_zstd`, `RedbCacheBuilder::compression` can opt into zstd writes.
Sync and async operations, including refresh, expiry sweeps, and statistics scans, read
mixed raw and compressed entries under the existing file layout. Reads, expiry sweeps, and
stats scans return a deserialization error for a frame above the reader's limit or a decompression
allocation failure in either mode. The entry is preserved and a failed sweep leaves its
transaction uncommitted. Disabling compressed writes on reopen still permits reading old frames
if the feature is compiled in. Refresh-on-hit re-encodes the value using the current builder's
compression settings. Compressor context and output buffer allocation failures return serialization
errors before writing the value. See [design 0057](design/0057-io-store-value-compression.md).
