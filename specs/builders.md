# Store builders and eviction callbacks

Every store is constructed through a `::builder()` returning a typed builder; in-memory and
sharded stores also provide infallible `::new(...)` conveniences. The in-memory and sharded
builders are additionally constructible directly via `Builder::new()` (equivalent to the
store's `::builder()`), matching the IO builders' public constructors.

## BUILD-1

`build()` returns `Result<Store, BuildError>`. The fallible `with_*` constructors and the
`try_build()` alias were removed in 2.0. The size-bound setter is `.max_size(n)` (renamed from
`.size(n)` in 2.0).

## BUILD-2

Infallible `::new(...)` conveniences exist for the in-memory and sharded stores (e.g.
`LruCache::new(100)`, `TtlCache::new(ttl)`, `UnboundCache::new()`, `ShardedUnboundCache::new()`).
The I/O stores (`RedbCache`, `RedisCache`, `AsyncRedisCache`) have required fields and are
builder-only. Whether infallible builders should return the cache directly is an open direction
([design/0014-infallible-builders.md](design/0014-infallible-builders.md)).

## BUILD-3

Builders accept `on_evict(|k, v| { ... })`, fired on every evicted entry (LRU capacity eviction,
TTL/expiry sweeps via `evict()`). `LruTtlCacheBuilder` and `ShardedLruTtlCacheBuilder` use
`HasEvict` / `NoEvict` type-state markers to track whether a callback is configured (both
re-exported at the crate root under the `time_stores` feature); other builders with `on_evict`
store a plain `Option`. The marker is the *last* type parameter on both
(`LruTtlCacheBuilder<K, V, S = DefaultHashBuilder, E = NoEvict>`,
`ShardedLruTtlCacheBuilder<K, V, H = DefaultShardHasher, E = NoEvict>`) so that the hasher is
the third parameter on all 13 builders; `tests/builder_hasher_slot_parity.rs` pins this. See [metrics.md](metrics.md) for the eviction counter and
[design/0002-size-iter-evict-semantics.md](design/0002-size-iter-evict-semantics.md) for the
size/iter/evict semantics.

## BUILD-4

Builders take a custom hasher `S` (defaulting to `DefaultHashBuilder`; `DefaultShardHasher` for
sharded stores), per
[design/0001-non-sharded-custom-hasher.md](design/0001-non-sharded-custom-hasher.md). `BuildError`
and the per-setter errors (`SetMaxSizeError`, `SetTtlError`) are re-exported at the crate root.

## BUILD-5

`TtlSortedSetBuilder` (the entry-setter returned by `TtlSortedCache::set_with(k, v)`, see
[store-ttl.md](store-ttl.md) TTL-4) gains `.ttl_secs(u64)` and `.ttl_millis(u64)` alongside its
existing `.ttl(Duration)`, matching the whole-cache TTL setters already on `TtlCacheBuilder`,
`TtlSortedCacheBuilder`, and `ShardedLruTtlCacheBuilder`. See [store-ttl.md](store-ttl.md) TTL-9.
