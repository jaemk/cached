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
`ConcurrentCached::cache_try_get_or_set_with`. `ConcurrentCachedAsyncExt` is its async
counterpart, see CTRAIT-7. The six sharded concrete types also expose inherent
`get<Q>(&self, &Q) -> Option<V>`, `remove<Q>(&self, &Q) -> Option<V>`,
`remove_entry<Q>(&self, &Q) -> Option<(K, V)>`, `delete<Q>(&self, &Q) -> bool`,
`contains<Q>(&self, &Q) -> bool`, and `peek<Q>(&self, &Q) -> Option<V>` (`contains` and `peek` are
peek-based: no recency, TTL, or metrics effects; `peek` clones the live value), generic since
design 0052 over any borrowed form of the key (`K: Borrow<Q>`, bounded on `H: BorrowedKeyRouting`,
exactly equivalent to `H: BuildHasher` but exported at the crate root and deliberately not in the
prelude so a failed bound names the real cause; see [store-sharded.md](store-sharded.md)
SHARD-15), and taking call-site priority over the ext-trait aliases.
They likewise expose inherent `retain<F: FnMut(&K, &V) -> bool>(&self, keep: F)` (see
[store-sharded.md](store-sharded.md) SHARD-6). It is deliberately not a `ConcurrentCached*` trait
method: it is generic over `F`, so a trait method would need `where Self: Sized` to stay object
safe, which would keep it off the vtable and out of reach through `dyn ConcurrentCached`. Adding a
required method to `ConcurrentCached*` pre-3.0-final would also break external implementors, with
no sensible default to provide instead. Revisit as a trait method post-3.0 if a generic consumer
needs it.

## CTRAIT-3

`ConcurrentCacheTtl` provides `&self` TTL control (`ttl()` / `set_ttl()` / `try_set_ttl()` /
`unset_ttl()`) on concurrent TTL stores; refresh-on-hit moved to `ConcurrentCacheRefreshOnHit`,
see CTRAIT-6. The implementing stores expose these only through the traits, with no inherent
duplicates.
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

## CTRAIT-6

`ConcurrentCacheRefreshOnHit` provides `&self` refresh-on-hit control (`refresh_on_hit()` /
`set_refresh_on_hit()`), split out of `ConcurrentCacheTtl` as the mirror of the
`CacheTtl` / `CacheRefreshOnHit` split on the single-owner side
([traits-core.md](traits-core.md) TRAIT-5). Implemented by `RedisCache`, `AsyncRedisCache`,
`RedbCache`, `ShardedTtlCache`, and `ShardedLruTtlCache`, which is every store implementing
`ConcurrentCacheTtl`.

The concurrent split discriminates nothing today: the two implementor sets are identical. It
exists so the trait families stay symmetric, so generic code ports between them without a bound
changing meaning, and so a future concurrent store that has a global TTL but cannot refresh on
hit can implement `ConcurrentCacheTtl` alone. The trait is ungated (only the built-in impls are
gated on their store's feature) and is in `cached::prelude`. See
[design/0045-refresh-on-hit-trait-split.md](design/0045-refresh-on-hit-trait-split.md).

## CTRAIT-7

`ConcurrentCachedAsyncExt<K, V>: ConcurrentCachedAsync<K, V>` is the async counterpart of
`ConcurrentCachedExt`: an alias trait providing `async_get`, `async_set`, `async_remove`,
`async_remove_entry`, `async_delete`, `async_contains`, `async_clear`, `async_reset`,
`async_get_or_set_with`, and `async_try_get_or_set_with`, each delegating to the
`async_cache_`-prefixed method of the same name. It is gated on `async_core` and is in
`cached::prelude`.

Shaped like `ConcurrentCachedExt`: the aliases are **required** methods filled in by a blanket
impl over `ConcurrentCachedAsync`, not defaulted methods. The blanket impl is therefore the only
implementation, so a downstream type cannot override an alias and make `async_get` disagree with
`async_cache_get`.

## CTRAIT-8

`ShardedTtlCache::cache_set` and `ShardedExpiringCache::cache_set` no longer choose their write
path based on whether an `on_evict` callback is configured. Previously the callback branch did
`remove_entry` + `insert` (replacing the stored key) while the no-callback branch did a plain
`HashMap::insert` (keeping it), so attaching a purely observational callback changed which key was
physically stored:

```
ttl  no-on_evict stored tag = "first"   with-on_evict stored tag = "second"
```

Both paths now take one shape: `get_mut` plus an in-place value swap on an overwrite, `insert` on a
vacant slot. That keeps the stored key (`HashMap::insert` semantics, matching their single-owner
counterparts `TtlCache` / `ExpiringCache`, [store-ttl.md](store-ttl.md) TTL-7) and is a single
lookup on the overwrite path. The stored key no longer depends on unrelated builder configuration.

Note the one place the sharded stores still differ from their single-owner counterparts. When a
displaced entry has already expired, `on_evict` fires after the shard lock is released, so the
callback is handed the CALLER's key rather than the stored one: the stored key stays in the map and
cannot be borrowed past the unlock. `TtlCache` / `ExpiringCache` fire while still holding the map
and therefore pass `occupied.key()`, the stored instance. The two keys are `Eq`-equal, so this is
observable only for a key type whose `Hash`/`Eq` cover part of the payload. Making it uniform would
cost a `K::clone` on the expired-displacement path; it has not been judged worth it.

The aliases keep the `async_` prefix instead of being bare `get` / `set`. Every store
implementing `ConcurrentCachedAsync` also implements the synchronous `ConcurrentCached`: the six
sharded stores and `RedbCache`. Since both alias traits are blanket implemented and both are in
the prelude, a bare `get` would be a second applicable candidate and `store.get(&k)` would be
`error[E0034]: multiple applicable items in scope` on `RedbCache`; on the sharded types, whose
inherent `get` takes call-site priority, the async alias would instead be unreachable through
method syntax. This is the same device `ConcurrentCachePeekAsync::async_peek` uses (CTRAIT-5).

The get-or-set pair is aliased despite carrying closure and future generics that the alias
signature restates verbatim: get-or-set is the operation this family exists to serve, and parity
with `ConcurrentCachedExt::get_or_set_with` / `try_get_or_set_with` is the point of the trait.

Introspection and metrics are deliberately **not** aliased here. `cache_size`, `cache_is_empty`,
`cache_hits`, `cache_misses`, `cache_capacity`, `cache_evictions`, and `metrics()` live on
`ConcurrentCacheBase` (CTRAIT-1), which is a supertrait of `ConcurrentCachedAsync`, so they are
already callable on any async store with no extension trait imported. Aliasing them would mean
`async_len` / `async_hits` names on methods that return plain values rather than futures, which
promises a future that is not there. `async_cache_reset_metrics` is likewise not forwarded,
matching `ConcurrentCachedExt`.

## CTRAIT-9

`ConcurrentCacheExpiry<K, V>` is the `&self` mirror of `CacheExpiry`
([traits-core.md](traits-core.md) TRAIT-6): required `cache_peek_expires_at(&self, &K) ->
(Option<V>, Option<Instant>)` plus a defaulted `peek_expires_at` alias, with the identical
side-effect-free contract (no LRU promotion, hit/miss counting, TTL renewal, or lazy removal) and
the identical `(None, None)` / `(Some(v), None)` / `(Some(v), Some(t))` result shape. It answers
GitHub issue #91 (refresh when the remaining TTL drops below a threshold) for the concurrent
stores, the same gap TRAIT-6 closes on the single-owner side.

`ConcurrentCacheExpiry` also provides `cache_expires_at(&self, &K) -> (bool, Option<Instant>)`
plus a defaulted `expires_at` alias, the `&self` mirror of TRAIT-6's value-free companion: the
`bool` is presence and the `Option<Instant>` is the deadline, giving the same `(false, None)` /
`(true, None)` / `(true, Some(t))` result shape and the same presence-flag rationale (absent and
never-expires call for opposite actions in a threshold-refresh policy, so a bare `Option<Instant>`
would lose that distinction). It carries no `V: Clone` bound: that bound moved off this trait's
impl blocks and onto the value-returning methods (`cache_peek_expires_at` and its
`peek_expires_at` alias), so a deadline-only read works for a value type that does not implement
`Clone`. It shares the identical side-effect-free contract and the same advisory-deadline caveat
on the `Expires`-based stores described below.

Implemented by `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`, and
`ShardedExpiringLruCache`. Not implemented by `RedisCache`, `AsyncRedisCache`, or `RedbCache`
(none of the three implement `ConcurrentCloneCached`), nor by the non-expiry stores
`ShardedUnboundCache` / `ShardedLruCache`, matching CTRAIT-2/CTRAIT-3's peek-is-an-in-memory-concept
rationale ([design/0040-peek-is-an-in-memory-concept.md](design/0040-peek-is-an-in-memory-concept.md)).
On `ShardedExpiringCache` / `ShardedExpiringLruCache`, whose deadline comes from
`Expires::expires_at()`, the returned `Instant` is advisory in the same way described at TRAIT-6:
it can be `None` for an expired entry. The two can disagree in both directions: `t` can be in the
past for an entry `Expires::is_expired` reports live, and `t` can be in the future for an entry
`is_expired` reports expired (a token with a fixed deadline that is also revocable). A future `t`
is therefore not evidence that the entry is live on these stores.

Deliberately a standalone trait rather than a new required method on `ConcurrentCloneCached`: that
would break external store implementations.

## CTRAIT-10

`ConcurrentCacheSetMaxSize` is the `&self` mirror of `CacheSetMaxSize`
([traits-core.md](traits-core.md) TRAIT-7): `set_max_size(&self, max_size: usize) ->
Option<usize>` and `try_set_max_size(&self, max_size: usize) -> Result<Option<usize>,
SetMaxSizeError>`. Both methods already existed as inherent-only methods on every implementor; the
trait adds no new capability, only the generic-code route. The requested bound is ceiling-divided
across shards with a floor of 16 per shard (`checked_per_shard_cap_from_total`,
`src/stores/sharded/mod.rs:~127-139`), so `set_max_size(4)` on a 16-shard cache leaves an effective
bound of 256. The returned previous bound is the previous EFFECTIVE TOTAL, not the previously
requested value, and is always `Some` on all three sharded implementors. The resize is not atomic
across shards: shards are updated one at a time, so two concurrent resizes can blend into a mix of
the two targets. Taking `&self` rather than `&mut self` matches every other concurrent-side trait
in this file, since the sharded stores are internally synchronized. Implemented by
`ShardedLruCache`, `ShardedLruTtlCache`, and `ShardedExpiringLruCache`, the three sharded stores
with a live, resizable capacity. `try_set_max_size` on these three can
additionally return `SetMaxSizeError::CapacityOverflow`, when `max_size` is close enough to
`usize::MAX` that dividing it across shards and multiplying back overflows; TRAIT-7's single-owner
implementors never construct that variant. Not implemented by the unbounded sharded stores
(`ShardedUnboundCache`, `ShardedTtlCache`, `ShardedExpiringCache`) or the IO stores, matching
TRAIT-7's exclusions. It is in `cached::prelude` and the trait is ungated (only the built-in impls
are gated on their store's feature). See
[design/0050-capability-traits-for-inherent-only-ops.md](design/0050-capability-traits-for-inherent-only-ops.md).

## CTRAIT-11

`ConcurrentCacheClearWithOnEvict` is the `&self` mirror of `CacheClearWithOnEvict`
([traits-core.md](traits-core.md) TRAIT-8): `cache_clear_with_on_evict(&self)`, clearing the store
while firing `on_evict` for every removed entry, and counting an eviction per removed entry on the
stores that track evictions at all. `ShardedUnboundCache` has no evictions counter at all
(`metrics().evictions` is always `None`) and is the sole exception: `on_evict` still fires for
every entry, but no counter moves. The method already existed as inherent-only on every
implementor; the trait adds no new capability. Coverage is total, matching TRAIT-8: all six
sharded stores implement it (`ShardedUnboundCache`, `ShardedLruCache`, `ShardedTtlCache`,
`ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`), since every one of them
has an `on_evict` callback. The split from
`CacheClearWithOnEvict` is for receiver-family symmetry, not because coverage differs, following
CTRAIT-6's rationale for splitting `ConcurrentCacheRefreshOnHit` out even with an identical
implementor set. `RedisCache`, `AsyncRedisCache`, and `RedbCache` have no `on_evict` mechanism and
implement neither trait. It is in `cached::prelude` and the trait is ungated (only the built-in
impls are gated on their store's feature). See
[design/0050-capability-traits-for-inherent-only-ops.md](design/0050-capability-traits-for-inherent-only-ops.md).
