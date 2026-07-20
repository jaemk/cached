# Concurrent cache traits

The self-synchronizing cache trait family with a shared `&self` API, implemented by the sharded,
redis, and redb stores. Distinct from the `&mut self` single-owner family in
[traits-core.md](traits-core.md).

## CTRAIT-1

`ConcurrentCacheBase` is the shared supertrait: it owns the associated `type Error` (bounded by
`std::error::Error + Send + Sync + 'static`), the `cache_size` / `cache_is_empty` accessors, the
metric accessors (`cache_hits` / `cache_misses` / `cache_capacity` / `cache_evictions`), and a
provided `metrics()`. Both `ConcurrentCached<K, V>` and `ConcurrentCachedAsync<K, V>` extend it,
per [design/0012-concurrent-metrics-trait.md](design/0012-concurrent-metrics-trait.md).

## CTRAIT-2

`ConcurrentCached<K, V>` is the sync self-synchronizing API (`cache_get`, `cache_set`,
`cache_remove`, `cache_remove_entry`, `cache_delete`, `cache_contains`, `cache_clear`,
`cache_reset`, `cache_reset_metrics`, `cache_get_or_set_with`, `cache_try_get_or_set_with`,
all returning `Result<_, Self::Error>`). `cache_contains` is a required method with no
`V: Clone` bound; the built-in sharded stores implement it with a peek-based read (read lock,
no clone, no metrics); `RedisCache` and `RedbCache` use a get-based implementation. External
implementors of `ConcurrentCached` must provide `cache_contains`.
`cache_try_get_or_set_with` is provided (defaulted): the fallible-init get-or-set returning
`Result<Result<V, E>, Self::Error>` with the store error outer and the closure error inner.
`ConcurrentCachedAsync<K, V>` is its async counterpart; `async_cache_contains` is likewise
required with no `V: Clone + Send` bound (its get-based implementors are `AsyncRedisCache` and
`RedbCache`), and `async_cache_try_get_or_set_with` mirrors the sync default.
`ConcurrentCachedExt` provides deduplicated short-name methods (`get`, `set`, `remove`,
`remove_entry`, `delete`, `contains` (no `V: Clone` bound), `clear`, `reset`, `get_or_set_with`,
`len`, `is_empty`, `hits`, `misses`, `capacity`, `evictions`); it does not forward
`cache_reset_metrics` directly. The six sharded concrete types also expose inherent
`contains(&self, &K) -> bool` and `peek(&self, &K) -> Option<V>` (both peek-based: no recency,
TTL, or metrics effects; `peek` clones the live value) that take call-site priority over the
ext-trait aliases, consistent with the other inherent shims (`get`, `set`, `reset`).

## CTRAIT-3

`ConcurrentCacheTtl` provides `&self` TTL control (`ttl()` / `set_ttl()` / `unset_ttl()` /
`try_set_ttl()` / `refresh_on_hit()` / `set_refresh_on_hit()`) on concurrent TTL stores; the
implementing stores expose these only through the trait, with no inherent duplicates.
`ConcurrentCacheEvict` provides the concurrent `evict()`.

## CTRAIT-4

`SerializeCached` / `SerializeCachedAsync` extend the concurrent traits for stores that persist
serialized values (redis, redb), adding `cache_set_ref(&self, &K, &V) -> Result<(), Self::Error>`
(and `async_cache_set_ref` on the async side). The method drops the previous value to avoid a
per-write read+decode; callers that need the old value must call `cache_get` first. Implemented
per [design/0022-serialize-cached-set-ref-return.md](design/0022-serialize-cached-set-ref-return.md)
(DEC-1=A).
