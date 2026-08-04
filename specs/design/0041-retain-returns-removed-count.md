# 0041 - `retain` returns the removed count

Status: Implemented (breaking change)

## Previous state

Inherent `retain<F: FnMut(&K, &V) -> bool>(keep: F)` returned `()` on all 13 stores that have it:
the seven single-owner stores (`UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`,
`TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`, taking `&mut self`) and the six sharded
stores (taking `&self`).

The caller therefore had no way to learn how much a sweep removed except to snapshot `len()`
before and after, which on the sharded stores is itself only approximate under concurrent load,
and on the expiry-aware single-owner stores is confounded by lazy expiry counting stale entries in
`cache_size()`.

Two things made the `()` return inconsistent with the surrounding API:

- The sibling trim on the same store family, `TtlSortedCache::retain_latest(count, evict)`,
  already returns `usize`.
- On the expiry-aware stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`,
  `ExpiringLruCache`, and the sharded TTL and expiring variants), `retain` removes expired entries
  regardless of the predicate. The number removed is therefore not a function of the predicate's
  own return values: a caller counting its own `false` returns inside `keep` still does not know
  the total, because the expired sweep happens independently of `keep`.

## The rule

`retain` returns `usize`: the number of entries removed by the call.

The count includes both entries the predicate rejected and entries swept because they had expired,
matching what `on_evict` fires on and what the store's evictions counter records where it has one.
The two categories are not distinguished in the return value; a caller who needs the split can
count its own `false` returns inside `keep` and subtract.

This applies uniformly to all 13 stores, single-owner and sharded, so the signature stays the same
shape across the families. On the sharded stores the returned count is the total across all shards
for the whole call, and, like the sweep itself, it is not atomic across shards (see
[store-sharded.md](../store-sharded.md) SHARD-6): it is the number this call removed, not a
statement about the cache's state at any single instant.

## Deliberate divergence from `HashMap::retain`

`std::collections::HashMap::retain` returns `()`, and normally matching std is the right default
for a method that borrowed its name from std. The divergence is justified because this `retain`
does strictly more than filter:

- It fires `on_evict` per removed entry.
- It increments the store's evictions counter.
- On the expiry-aware stores it removes expired entries the predicate never rejected.

A std `retain` removes exactly what the caller's predicate rejected, so the caller already knows
the count and a return value would be redundant. Here the caller does not, so the count is
information only the store has.

## Observable surface that changes

Breaking. `retain` returns `usize` instead of `()` on all 13 stores.

- Callers using `retain` as a statement (`cache.retain(|k, v| ...);`) are unaffected: the trailing
  semicolon discards the value. This is the overwhelming majority of call sites.
- Callers using it in a `()`-typed tail position (e.g. as the final expression of a function
  returning `()`, or in a closure whose return type is inferred as `()`) get a type error and need
  a trailing semicolon or an explicit discard.
- `#[must_use]` is not applied: discarding the count is a legitimate and common use.

Landing this before the 3.0.0 final release avoids a deprecation cycle for a return-type change,
which cannot be made compatibly afterwards.

## Notes

- The single-owner stores keep `&mut self` and the sharded stores keep `&self`; only the return
  type changes.
- `retain` remains inherent on the sharded stores rather than a `ConcurrentCached*` trait method
  (see [traits-concurrent.md](../traits-concurrent.md) CTRAIT-2); adding the return value does not
  change that.
- See [store-sharded.md](../store-sharded.md) SHARD-6 and [store-lru.md](../store-lru.md) for the
  shipped behavior statements.
