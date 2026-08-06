# Redis backend

Redis-backed concurrent stores: `RedisCache` (synchronous, `redis_store`) and `AsyncRedisCache`
(async, `redis_tokio` / `redis_smol` and their TLS variants). Both are self-synchronizing over a
shared `&self` and are builder-only (required connection fields).

## REDIS-1

Values are serialized with MessagePack; see
[design/0011-redis-serialization-codec.md](design/0011-redis-serialization-codec.md) (a pluggable
codec remains a research direction). Deserialization is self-healing by default: an entry that
fails to decode is treated as a miss rather than an error, per
[design/0029-self-healing-deserialization-default.md](design/0029-self-healing-deserialization-default.md).
Backward-read compatibility is version-gated per
[design/0031-redis-backward-read-version-gate.md](design/0031-redis-backward-read-version-gate.md).

## REDIS-2

TTL is set in milliseconds where requested (`PSETEX` / `PEXPIRE`), per
[design/0003-redis-millisecond-ttl.md](design/0003-redis-millisecond-ttl.md). TTL control is
exposed through `ConcurrentCacheTtl`. See [traits-concurrent.md](traits-concurrent.md).

## REDIS-3

The `connection_string()` getter returns a redacted value (credentials masked) via the
`ConnectionString` type, per
[design/0004-redis-connection-string-redaction.md](design/0004-redis-connection-string-redaction.md).

## REDIS-4

Errors are `RedisCacheError` (build: `RedisCacheBuildError`) with named, struct-style variants,
per [design/0005-store-error-consistency.md](design/0005-store-error-consistency.md). Optional
support: `redis_connection_manager`, `redis_async_cache` (RESP3 client-side caching). Runtime x
TLS feature axes remain an open direction
([design/0017-redis-feature-axes.md](design/0017-redis-feature-axes.md)); namespace/key escaping
shipped in 3.0, see [REDIS-6](#redis-6). See [cargo-features.md](cargo-features.md).

## REDIS-5

`RedisCacheError` and `RedisCacheBuildError` `Display` interpolates the underlying source (e.g.
`#[error("storage error: {source}")]`) instead of discarding it, matching the same fix applied to
`RedbCacheError` / `RedbCacheBuildError` ([REDB-5](store-redb.md#redb-5)), per
[design/0005-store-error-consistency.md](design/0005-store-error-consistency.md). `Display` text
is not semver-guarded, so this is a non-breaking change.

## REDIS-6

Keys are `{namespace}:{prefix}:{key}` with fixed arity (every field contributes a field and a
separator, so an empty namespace writes `:{prefix}:{key}`) and each field percent-escaped
(`:` -> `%3A`, `%` -> `%25`). Distinct (namespace, prefix, key) triples therefore always map to
distinct Redis keys. Trailing colons on the namespace are trimmed, so `"ns"` and `"ns:"` name the
same namespace: injectivity is over that canonical namespace. `cache_clear` scans a `SCAN MATCH`
glob built from the same escaped fields through the same framing (then glob-escaped) so it covers
exactly this cache's keys: a differently-split namespace/prefix pair is out of scope, and so is a
cache whose namespace equals a namespace-less cache's prefix (the scope is `:{prefix}:*`, which no
non-empty namespace can produce). Per
[design/0018-redis-key-escaping.md](design/0018-redis-key-escaping.md).

Breaking in 3.0: a cache with an empty namespace, or whose namespace, prefix or keys contain `:`
or `%`, writes at a different key than in 2.x. The value envelope's `version` field
([REDIS-7](#redis-7)) lives inside the value and so cannot describe the key layout: pre-upgrade
entries are simply not found, and are recomputed and rewritten at the new key. They are not
deleted by the upgrade; they expire with their TTL, or persist until removed by hand when the
cache has none.

## REDIS-7

The stored value envelope is MessagePack in its compact (non-named) form: every write goes
through `rmp_serde::to_vec`, never `to_vec_named`, so an entry is a positional 2-element array of
`value` then `version` and the field names are not on the wire. Field order is part of the frozen
3.x layout: reordering, inserting or removing a field reinterprets every stored entry and must
bump the embedded version. `tests/frozen_format_golden.rs` pins the serialized bytes.
