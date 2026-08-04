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
`try_get_or_set_with`, `len`, `is_empty`, `hits`, `misses`, `capacity`, `evictions`); it does not
forward `cache_reset_metrics` directly. `try_get_or_set_with` delegates to
`ConcurrentCached::cache_try_get_or_set_with`. The six sharded concrete types also expose
inherent `contains(&self, &K) -> bool` and `peek(&self, &K) -> Option<V>` (both peek-based: no
recency, TTL, or metrics effects; `peek` clones the live value) that take call-site priority over
the ext-trait aliases, consistent with the other inherent shims (`get`, `set`, `reset`).
They likewise expose inherent `retain<F: FnMut(&K, &V) -> bool>(&self, keep: F)` (see
[store-sharded.md](store-sharded.md) SHARD-6). It is deliberately not a `ConcurrentCached*` trait
method: it is generic over `F`, so a trait method would need `where Self: Sized` to stay object
safe, which would keep it off the vtable and out of reach through `dyn ConcurrentCached`. Adding a
required method to `ConcurrentCached*` pre-3.0-final would also break external implementors, with
no sensible default to provide instead. Revisit as a trait method post-3.0 if a generic consumer
needs it.

## CTRAIT-3

`ConcurrentCacheTtl` provides `&self` TTL control (`ttl()` / `set_ttl()` / `unset_ttl()` /
`try_set_ttl()` / `refresh_on_hit()` / `set_refresh_on_hit()`) on concurrent TTL stores; the
implementing stores expose these only through the trait, with no inherent duplicates.
`ConcurrentCacheEvict` provides the concurrent `evict()`. `ConcurrentCachePeek` provides
`cache_peek(&self, &K) -> Result<Option<V>, Self::Error>` (plus a defaulted `peek` alias): a
genuinely side-effect-free read (no recency, TTL refresh, hit/miss metrics, or lazy expiry
removal). It is implemented only by the six sharded stores (`Self::Error = Infallible`);
`RedisCache`, `RedbCache`, and `AsyncRedisCache` deliberately do not implement it. It is in
`cached::prelude`. It now has an async mirror, `ConcurrentCachePeekAsync`, with the same six-store
implementor set and the same deliberate omission on the IO stores; see CTRAIT-5.

## CTRAIT-4

`SerializeCached` / `SerializeCachedAsync` extend the concurrent traits for stores that persist
serialized values (redis, redb), adding `cache_set_ref(&self, &K, &V) -> Result<(), Self::Error>`
(and `async_cache_set_ref` on the async side). The method drops the previous value to avoid a
per-write read+decode; callers that need the old value must call `cache_get` first. Implemented
per [design/0022-serialize-cached-set-ref-return.md](design/0022-serialize-cached-set-ref-return.md)
(DEC-1=A).

## CTRAIT-5

`ConcurrentCachePeekAsync<K, V>: ConcurrentCacheBase` is the async mirror of
`ConcurrentCachePeek`. It declares a required
`async_cache_peek(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>>` carrying
the identical side-effect-free contract: no recency/LRU promotion, no TTL refresh, no hit/miss
metrics, and no lazy removal of expired entries; an expired entry reads as `None`. Unlike
`ConcurrentCachePeek::cache_peek`, `async_cache_peek` has deliberately **no default body** --
generic code bounded on the trait depends on the contract holding for *every* implementor, and a
defaulted body built on an ordinary read could not verify that for an arbitrary external type, so
it would open a contract hole. Mirroring `ConcurrentCachePeek`'s defaulted `peek` alias, the trait
also provides a defaulted `async_peek` alias delegating to `async_cache_peek`; it is named with the
`async_` prefix (not a bare `peek`) so it does not collide with the sync inherent
`peek(&self, &K) -> Option<V>` already exposed by the six sharded concrete types.

`ConcurrentCachePeekAsync` is implemented by exactly the six sharded stores (`Self::Error =
Infallible`), delegating to their existing side-effect-free sync `cache_peek`. It is added to
`cached::prelude`. This closes the gap where calling `async_cache_peek` on a sharded store
previously produced an E0599 whose rustc suggestion was actively wrong: it proposed appending
`.await` to the non-future sync `cache_peek`.

`RedisCache`, `RedbCache`, and `AsyncRedisCache` deliberately implement neither
`ConcurrentCachePeek` nor `ConcurrentCachePeekAsync`. This affirms the existing rustdoc rather than
reversing it: peek is an in-memory concept. For an IO-backed store there is no client-side recency
or TTL state to skip, the metrics distinction is meaningless, and a "peek" would still be a full
network or disk round trip, so it would advertise a cheapness the store cannot deliver. See
[design/0040-peek-is-an-in-memory-concept.md](design/0040-peek-is-an-in-memory-concept.md) for
the unified rationale.
