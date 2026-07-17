# Concurrent cache traits

The self-synchronizing cache trait family with a shared `&self` API, implemented by the sharded,
redis, and redb stores. Distinct from the `&mut self` single-owner family in
[traits-core.md](traits-core.md).

## CTRAIT-1

`ConcurrentCacheBase` is the shared supertrait: it owns the associated `type Error`, the
`cache_size` / `cache_is_empty` accessors, the metric accessors (`cache_hits` / `cache_misses` /
`cache_capacity` / `cache_evictions`), and a provided `metrics()`. Both `ConcurrentCached<K, V>`
and `ConcurrentCachedAsync<K, V>` extend it, per
[design/0012-concurrent-metrics-trait.md](design/0012-concurrent-metrics-trait.md).

## CTRAIT-2

`ConcurrentCached<K, V>` is the sync self-synchronizing API (`cache_get`, `cache_set`,
`cache_remove`, `cache_remove_entry`, `cache_delete`, `cache_clear`, `cache_reset`,
`cache_reset_metrics`, `cache_get_or_set_with`, all returning
`Result<_, Self::Error>`). `ConcurrentCachedAsync<K, V>` is its async counterpart.
`ConcurrentCachedExt` provides deduplicated short-name methods (`get`, `set`, `remove`,
`remove_entry`, `delete`, `clear`, `reset`, `get_or_set_with`); it does not forward
`cache_reset_metrics`.

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
