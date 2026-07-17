# 0029 - self-healing deserialization default

Status: Implemented

## Current state

Both IO-backed stores (`RedisCache`/`AsyncRedisCache` and `RedbCache`) offer two behaviors on
deserialization failure:

**Self-heal mode (default, `strict_deserialization = false`):** when a stored entry cannot be
decoded into `V`, the store deletes the corrupt entry and returns `Ok(None)`. The caller gets a
cache miss and recomputes the value. The corrupt entry is gone on the next call.

**Strict mode (`strict_deserialization(true)`):** deserialization failure returns
`Err(CacheDeserialization { ... })`. The corrupt entry is also removed (both modes delete it) but
the error is surfaced to the caller.

The builder default is self-heal. Both stores expose `strict_deserialization(bool)` on their
builders (`RedisCacheBuilder::strict_deserialization`, `RedbCacheBuilder::strict_deserialization`).

## Design decisions recorded here

**Self-heal is the default.** A corrupt or schema-mismatched entry in a cache is a recoverable
condition in almost all deployments. A rolling deploy may leave old MessagePack entries for a
type whose shape changed; those entries should degrade gracefully to a miss rather than flooding
the application with errors. Self-heal mode matches the general cache contract: a miss is always
acceptable.

**Both modes delete the corrupt entry.** Whether or not the error is surfaced, the entry is
removed. Leaving corrupt bytes in place would cause every subsequent read to repeat the same
failure path.

**`is_deserialization()` classifies errors without downcast.** `RedisCacheError::is_deserialization()`
and `RedbCacheError::is_deserialization()` let strict-mode callers distinguish a corrupt-value
error from a network or IO error without reaching into the opaque `source` box
(`src/stores/redis.rs:1406`, `src/stores/redb.rs:770`).

**The GET-then-DEL self-heal on redis is not atomic.** A concurrent `PSETEX` of a valid value
between the GET and the DEL can be lost. This race is closed on the redis path with a Lua
conditional-delete script (DEC-9(b); `src/stores/redis.rs:11-35`): the script deletes the key
only if the stored bytes still match what was read, so a fresher valid write is preserved. The
redb path re-reads under the write transaction before deleting (C5 fix; `src/stores/redb.rs:866-911`),
so a concurrent `cache_set` that commits between the read and the self-heal is not silently lost.

## Notes

- Tests: `tests/v3_redb_races.rs` (redb self-heal), redis integration tests covering strict and
  self-heal paths.
- The `CacheDeserialization` variant carries the raw `cached_value: Vec<u8>` for diagnostic
  inspection. The derived `Debug` implementation redacts these bytes as `<N bytes redacted>`
  to avoid accidental logging of large or sensitive payloads (spec 0005 area; A13 fix).
