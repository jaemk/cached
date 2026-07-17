# Cargo feature flags

The `cached` crate gates optional stores, backends, and runtimes behind Cargo features. Defaults:
`proc_macro`, `ahash`, `time_stores`.

## FEAT-1

Core: `proc_macro` (the `#[cached]` / `#[once]` / `#[concurrent_cached]` macros), `ahash` (ahash
hasher for internal maps), `time_stores` (`TtlCache`, `LruTtlCache`, `TtlSortedCache` and their
sharded variants).

## FEAT-2

Async: `async_core` (runtime-agnostic async trait definitions without async-lock; kept public
for callers who want the trait surface without the async-lock dependency), `async`
(enables `async_core` and pulls `async-lock`). Making `async_core` internal was declined
([design/0016-async-core-internal-feature.md](design/0016-async-core-internal-feature.md),
DEC-2=B).

## FEAT-3

Redis: `redis_store` (sync), `redis_tokio` / `redis_smol` (async runtimes, imply `redis_store` +
`async`), their `_native_tls` / `_rustls` TLS variants, plus the capability features
`redis_connection_manager` and `redis_async_cache` (RESP3 client-side caching). A capability
feature requires a runtime feature (documented in `Cargo.toml`; the `redis` crate itself fails to
build otherwise). Orthogonal runtime x TLS axes are an open direction
([design/0017-redis-feature-axes.md](design/0017-redis-feature-axes.md)). See
[store-redis.md](store-redis.md).

## FEAT-4

Disk: `redb_store` (disk-backed cache via `redb`; see [store-redb.md](store-redb.md)). The crate
MSRV is 1.89 (set unconditionally in `Cargo.toml`; historically required by redb 4.x).

## FEAT-5

`ahash` remains in the default set (DEC-3=A per
[design/0019-ahash-default-feature.md](design/0019-ahash-default-feature.md)). The explicit
`serde = ["dep:serde", "dep:rmp-serde"]` feature shipped (DEC-6=A per
[design/0026-serde-feature.md](design/0026-serde-feature.md)); `redis_store` and `redb_store`
depend on it transitively.
