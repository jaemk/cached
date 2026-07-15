# LRU cache

`LruCache<K, V, S>` is a size-bounded store with least-recently-used eviction. Renamed from the
pre-1.0 `SizedCache`. Exported from `cached::stores`.

## LRU-1

Bounded by `max_size`: inserting beyond capacity evicts the least-recently-used entry. A read
(`cache_get`) refreshes recency; a peek (`cache_peek`) does not.

## LRU-2

Constructors: `LruCache::new(max_size)` (returns the cache directly; panics on zero), or
`LruCache::builder().max_size(n)` for a custom hasher. `max_size` is the setter (renamed from
`.size()` in 2.0). Building with a zero/invalid size is a `BuildError`. See
[builders.md](builders.md).

## LRU-3

Eviction fires the `on_evict` callback when configured, and increments the `evictions` metric.
See [metrics.md](metrics.md).

## LRU-4

Implements `Cached`, `CachedPeek`, and `CachedIter`. Size/iter/evict semantics follow
[design/0002-size-iter-evict-semantics.md](design/0002-size-iter-evict-semantics.md).
Inherent `retain(keep)` removes entries failing the predicate (firing `on_evict` and counting
evictions); the expiry-aware LRU stores (`LruTtlCache`, `ExpiringLruCache`) share the contract
but also remove expired entries regardless of the predicate. `set_max_size` /
`try_set_max_size` resize a live cache.
