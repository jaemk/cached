# 0029 - self-healing deserialization default

Status: Implemented

## Behavior

`RedisCache`, `AsyncRedisCache`, and `RedbCache` default to
`strict_deserialization(false)`. A lookup that encounters a corrupt or schema-incompatible value
tries to remove it and returns `Ok(None)`, allowing the caller to recompute. With
`strict_deserialization(true)`, the lookup returns `CacheDeserialization` and leaves the entry
in place.

With zstd support, both modes return a deserialization error and preserve the entry when the
frame exceeds the reader's configured limit or decompression allocation fails. See
[0057](0057-io-store-value-compression.md).

A redb expiry sweep treats corrupt values as eviction candidates in default mode. In strict
mode it aborts the sweep and leaves the table unchanged. Redb statistics scans count corrupt
values in either mode. The compression exceptions also apply to sweeps and scans.

Explicit removals delete the entry in either mode. When its previous value fails decoding,
Redis reports that decode error in strict mode and returns `Ok(None)` in default mode. Redb
returns `Ok(None)` in either mode. A successful replacement also returns `Ok(None)` if it
cannot decode the displaced value.

## Concurrent writes

Redis uses a Lua script to conditionally delete the entry only when its current bytes match
the bytes that failed decoding. Redb re-reads and decodes the current entry under the write
transaction before acting. See [0033](0033-redb-revalidate-in-write-txn.md).

## Diagnostics

`RedisCacheError::is_deserialization()` and `RedbCacheError::is_deserialization()` classify
value decoding errors separately from storage and connection errors. The
`CacheDeserialization` variant carries the original stored bytes in `cached_value`. Its
`Debug` implementation redacts those bytes.

The Redis strict and self-healing tests and `tests/v3_redb_races.rs` cover these paths.
