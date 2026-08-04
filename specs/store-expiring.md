# Per-value expiring caches

`ExpiringCache<K, V, S>` (unbounded) and `ExpiringLruCache<K, V, S>` (LRU, size-bounded) store
values whose expiry is carried by the value itself via the `Expires` trait. Renamed from the
pre-1.0 `ExpiringValueCache` (for `ExpiringLruCache`). Available without `time_stores`.

## EXPIRE-1

Values implement `Expires` (`is_expired()`); the store consults the value, not a store-wide TTL.
`ExpiringCache` is the default store for `#[cached(expires = true)]`; `ExpiringLruCache` is
selected when `max_size` is also specified.

## EXPIRE-2

Neither store exposes a public `store()` accessor; inspect via the `Cached` trait API.

## EXPIRE-3

`cache_get` / `cache_get_mut` on `ExpiringCache` use two hash lookups on the hit path (a
stable-Rust NLL borrow-checker limitation, documented in source). This is intentional.

## EXPIRE-4

`cache_get_with_expiry_status` (from `CloneCached`) leaves an expired entry in the map so
`result_fallback` can return it as a stale-but-present value on `Err`. The stale entry is still
counted by `cache_size()` (but skipped by `CachedIter`, which filters expired entries) until the
next `cache_get`, `evict()`, or `cache_remove`. With `result_fallback = true` and
`expires = true`, callers get `Ok(stale_value)` where `stale_value.is_expired() == true` and must
check expiry themselves. See [design/0030-force-refresh-result-fallback-interaction.md](design/0030-force-refresh-result-fallback-interaction.md).

## EXPIRE-5

`CachedIter::iter()` filters expired entries but does not remove them; call `evict()`
(`CacheEvict`) periodically for high-cardinality workloads. See
[design/0002-size-iter-evict-semantics.md](design/0002-size-iter-evict-semantics.md).

## EXPIRE-6

`ExpiringLruCache` has the LRU-family order accessors (`iter_order` / `key_order` /
`value_order`) with `CacheValue<V>` values (no per-entry metadata; expiry lives on the value
itself via `Expires`). See [store-lru.md](store-lru.md) LRU-5.

## EXPIRE-7

`ExpiringLruCache::cache_set` fires `on_evict` only when the displaced entry has already
**expired**; a live overwrite returns the old value via `Some` and fires no callback. When it does
fire, it passes the **stored** key of the displaced entry, not the caller's key, matching
`LruTtlCache`. The two keys compare equal (same `Hash`/`Eq`) but can differ in fields outside
`Hash`/`Eq`; observable only for key types with such fields. See [store-lru.md](store-lru.md) LRU-6.

## EXPIRE-8

`retain(keep)` on `ExpiringCache` and `ExpiringLruCache` now returns `usize` (the count of
entries removed) instead of `()`, matching every other store; see
[store-lru.md](store-lru.md) LRU-8. As with the other expiry-aware stores, the count folds
together predicate-rejected entries and entries swept for having already expired.
