# 0039 - Sharded stores have no iteration or snapshot API

Status: Not implemented (declined)

## Current state

Every single-owner store implements `CachedIter<K, V>` and therefore exposes `iter()` (plus the
`keys()` / `values()` helpers built on it): `UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`,
`TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`, and the blanket `HashMap` impl.

None of the six sharded stores (`ShardedUnboundCache`, `ShardedLruCache`, `ShardedTtlCache`,
`ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`) exposes `iter`, `keys`,
`values`, or any other whole-cache snapshot method, and there is no concurrent counterpart to
`CachedIter`. The rustdoc on the sharded stores states the omission (`src/lib.rs:285` and the
per-store module docs) and points callers at `evict()` for an accurate live count, but until now
there was no design record behind it.

This record exists because a review found it to be the only asymmetry between the single-owner and
the sharded store families with no decision recorded anywhere. Everything else that differs (no
evictions counter on `ShardedUnboundCache`, `retain` being inherent rather than a trait method,
the `*Base` + alias naming, the shard-count defaults) already had a record.

## Desired work

Add some form of traversal to the sharded stores: either a `ConcurrentCacheIter` trait mirroring
`CachedIter`, or inherent `iter()` / `keys()` / `values()` / `snapshot()` methods returning owned
data.

## Why this is declined for 3.0

A whole-cache view over a sharded store has exactly two honest implementations, and both are
unacceptable as a casual method call:

1. **Hold every shard lock simultaneously.** This is the only way to produce a genuinely
   consistent cross-shard view. It introduces a lock-ordering obligation on a type whose entire
   purpose is to avoid one, and it stalls every writer in the cache for as long as the caller
   holds the iterator - which, for a lazy iterator, is a duration the store cannot bound. An
   `iter()` that can deadlock or that pauses all writes for the length of a user loop is not an
   API a cache should hand out casually.
2. **Clone every shard's contents into an owned snapshot.** This avoids the lock hazard (each
   shard is locked and copied in turn) but allocates memory proportional to the entire cache, with
   no bound the caller set. For the sharded stores, whose expected use is large and highly
   concurrent, that is a memory cliff hidden behind a one-word method name.

A third option was considered and rejected: a weakly-consistent, shard-at-a-time iterator that
locks one shard, yields its entries, releases, and moves on. It is cheap and cannot deadlock, but
the sequence it produces matches no point-in-time state of the cache. It misses a key inserted
into a shard the traversal has already passed, and it yields a key removed from a shard the
traversal has already passed, so an entry can be both absent from the result while present for the
whole traversal and present in the result while absent by the end. The natural uses (counting
entries, summing sizes, exporting a view for metrics) are exactly the uses that then produce
quietly wrong answers, and shipping it under the `iter()` name the single-owner stores use would
imply the single-owner guarantees.

The one traversal use case that IS safe on a sharded store is already served:
`retain(&self, keep)` visits every entry, locks one shard at a time, and is documented as
non-atomic across shards (see [store-sharded.md](../store-sharded.md) SHARD-6). It is safe
precisely because the caller cannot observe the intermediate sequence - it only observes the end
state - so weak cross-shard consistency has no way to mislead. That covers filtering and
predicate-driven cleanup, which is the bulk of what iteration would be used for on a cache.

## Notes

- Declining this does NOT box in a later release. Adding a `ConcurrentCacheIter` trait, or
  inherent snapshot methods, is purely additive: no existing signature changes and no implementor
  breaks. If a concrete need appears with a use case that fixes the consistency question (e.g. an
  explicit `snapshot_shard(i)` whose per-shard scope is in the name, or a bounded
  `sample(n)`), it can land in 3.x.
- The per-store rustdoc already documents the omission and directs callers to `evict()` for an
  accurate live count; no doc change is needed for this record.
- Related: 0002 (`len`/`size` vs `iter` vs `evict` semantics), 0007 (the other documented
  single-owner/sharded asymmetry).
