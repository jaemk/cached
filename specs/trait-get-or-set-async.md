# Async get-or-set

`CachedGetOrSetAsync<K, V>` provides `async_cache_get_or_set_with` and
`async_cache_try_get_or_set_with` over a sync in-memory store, so an async initializer can
populate a single-owner cache. Gated behind the async features.

## ASYNC-1

The trait was renamed from `CachedAsync` to `CachedGetOrSetAsync` to name its role precisely, per
[design/0032-cached-async-to-get-or-set-async-rename.md](design/0032-cached-async-to-get-or-set-async-rename.md).

## ASYNC-2

It runs the async initializer to produce a value, then stores it through the underlying sync
cache. It is distinct from the concurrent async trait `ConcurrentCachedAsync`
([traits-concurrent.md](traits-concurrent.md)), which is implemented by self-synchronizing stores
(redis, redb, sharded).

## ASYNC-3

Exposed in the prelude alongside `ConcurrentCachedAsync` and `SerializeCachedAsync`. Requires
`async` (runtime-agnostic) or a redis async runtime feature; see
[cargo-features.md](cargo-features.md).
