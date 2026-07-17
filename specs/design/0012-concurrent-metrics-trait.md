# 0012 - Expose sharded metrics through a trait

Status: Implemented

## Current state

- Non-sharded stores expose metrics through the `Cached` trait: `cache_hits`, `cache_misses`,
  `cache_capacity`, `cache_evictions`, and a default `metrics() -> CacheMetrics`
  (`src/lib.rs:1059`-`1085`). `CacheMetrics` is shared (`src/lib.rs:1122`).
- Sharded stores return the same `CacheMetrics` struct but only via inherent methods
  (`src/stores/sharded/unbound.rs:252`, `src/stores/sharded/lru.rs:245`,
  `src/stores/sharded/ttl.rs:311`, `src/stores/sharded/lru_ttl.rs:330`,
  `src/stores/sharded/expiring.rs:254`, `src/stores/sharded/expiring_lru.rs:268`), plus inherent
  `shards()`, `shard_sizes()`, `clear()`, `cache_clear_with_on_evict()`. None of these is on a
  trait, so generic code over `ConcurrentCached` cannot read a hit rate or shard distribution.

## Desired work

- Mirror the non-sharded design: expose the metric accessors (`cache_hits`/`cache_misses`/
  `cache_capacity`/`cache_evictions` and a default `metrics()`) on the concurrent trait family
  (extend `ConcurrentCacheBase`, or add an introspection trait) so they are reachable through a
  `ConcurrentCached` bound, consistent with `Cached`.
- Keep the inherent methods so `store.metrics()` still resolves without an import.
- Decide whether `shards()`/`shard_sizes()` and the callback-firing clear also belong on the
  trait, or stay inherent-only as sharded-specific introspection.

## Notes

- Consistency target: the non-sharded metric surface lives on the base read trait, so the
  concurrent metric surface should live on the concurrent base trait too.
