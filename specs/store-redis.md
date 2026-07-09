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
TLS feature axes and namespace/key escaping are open directions
([design/0017-redis-feature-axes.md](design/0017-redis-feature-axes.md),
[design/0018-redis-key-escaping.md](design/0018-redis-key-escaping.md)). See
[cargo-features.md](cargo-features.md).
