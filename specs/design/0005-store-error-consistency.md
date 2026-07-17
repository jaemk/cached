# 0005 - redb/redis error naming and variant shape

Status: Implemented

## Current state

- `RedbCacheBuildError::Connection(redb::Error)` (`src/stores/redb.rs:61`) is misnamed: redb is
  an embedded file database with no connection. Its message says "Storage connection error".
  The runtime enum names the same family `RedbCacheError::Storage(redb::Error)`
  (`src/stores/redb.rs:450`), so build-time and runtime disagree on the vocabulary.
- Variant shapes are inconsistent across the two backends. Redis uses struct variants with named
  fields: `CacheDeserialization { cached_value: String, error: ... }` and
  `CacheSerialization { error: ... }` (`src/stores/redis.rs:1325`). redb uses tuple variants:
  `CacheDeserialization(...)`, `CacheSerialization(...)`, `Storage(...)`
  (`src/stores/redb.rs:450`).

## Desired work

- Rename `RedbCacheBuildError::Connection` to `Storage`, matching `RedbCacheError::Storage`, and
  update its message to drop "connection".
- Convert the redb error enums to struct variants for consistency with redis (named fields,
  `#[source]`/`#[from]` on the single error field where applicable). Keep both enums
  `#[non_exhaustive]`.
- Pick one field naming convention for the serialize/deserialize variants and apply it to both
  backends.

## Notes

- Interacts with [0011](0011-redis-serialization-codec.md): switching Redis to MessagePack
  changes the concrete error types those variants carry. Land them together so redis.rs error
  edits happen once.
- Migration: mechanical match-arm updates; enums are already `#[non_exhaustive]`.
- 5.4 refresh: `RedbCacheError::CacheDeserialization` carries a `cached_value: Vec<u8>` field
  holding the raw bytes that failed to deserialize (`src/stores/redb.rs:724,727`). Any work
  that adds a custom `Debug` impl (A13) or exposes the field publicly must account for this
  type.
