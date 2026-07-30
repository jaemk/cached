# 0038 - `cache_set` over an existing key promotes to most-recently-used

Status: Implemented

## Previous state

`LruCache::cache_set` replaced an existing entry through `LRUList::set(index, (key, val))`, which
writes the new `(K, V)` into the slot the entry already occupies. The slot's position in the
recency chain was left alone, so an overwrite did not change which entry a later capacity
eviction selected. `LruCache::cache_set_returning_entry` did the same, and both behaviors
propagated to `LruTtlCache`, `ExpiringLruCache`, `ShardedLruCache`, and the no-callback write
branch of `ShardedLruTtlCache` / `ShardedExpiringLruCache`.

The non-promotion was an artifact of the original `SizedCache` implementation (an in-place
`order.set` was the cheap path), documented and pinned by tests only after the fact. It was
never chosen on the merits.

It also produced a wart on the sharded LRU-TTL and expiring-LRU stores. Those two need the
displaced *stored* key to hand to an `on_evict` callback, so their callback branch wrote through
a promoting helper (`get_or_set_with_if` with a never-valid predicate) while their no-callback
branch called the non-promoting `LruCache::cache_set`. Attaching a purely observational
`on_evict` callback therefore changed eviction order.

## The rule

`cache_set` on an existing key promotes that key to most-recently-used: after
`LRUList::set(index, ..)` the entry is moved with `LRUList::move_to_front(index)`. A write is an
access, so an overwrite behaves like inserting a fresh value.

The change is made in two primitives in `src/stores/lru.rs`, `LruCache::cache_set` and
`LruCache::cache_set_returning_entry`, and propagates to every store in the LRU family. Since
`cache_set_returning_entry` now both promotes and returns the displaced stored `(K, V)` pair, the
sharded stores' promoting helper is redundant and was deleted; both write branches call a
primitive with identical recency behavior, so the `on_evict` divergence is gone.

Reads documented as side-effect-free are untouched: `cache_peek`,
`cache_peek_with_expiry_status`, `cache_contains`, and the internal `pop_raw_with_hash` remain
non-promoting, so a write and a peek stay distinguishable. Inserting a NEW key already went to
the front via `push_front` and is unchanged. Key rebinding on overwrite (LRU-6) is a separate
question and is unaffected.

## Observable surface that changes

- Which entry a capacity eviction selects in any workload that overwrites existing keys. The
  overwritten key survives longer; some other key becomes the victim.
- `iter_order()` / `key_order()` / `value_order()` after an overwrite.
- The `on_evict` victim sequence and its order on `cache_clear_with_on_evict` (an MRU -> LRU
  drain) and on a `set_max_size` shrink (LRU-first).
- On `ShardedLruTtlCache` / `ShardedExpiringLruCache`, the no-callback path now matches the
  callback path. Code that configured `on_evict` already saw promotion and is unaffected.

There is no direct substitute for the old in-place behavior: no public API writes a value
without touching recency. The closest reconstruction is to read the entry's position first
(`key_order()`) and restore it afterwards, which is not a supported operation on the store API.

## Notes

- `LRUList::move_to_front` on a slot already at the head is a safe no-op: `unlink` repairs the
  neighbours and `link_after(index, OCCUPIED)` re-reads the sentinel's `next` after the unlink.
- See [store-lru.md](../store-lru.md) LRU-7 for the shipped behavior statement.
