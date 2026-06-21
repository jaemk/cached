# 0010 - Read-optimized sharded LRU variant

Status: Needs research

## Current state

- `ShardedLruCache::cache_get` and `ShardedExpiringLruCache::cache_get` acquire an exclusive
  write lock on every read hit to update recency (`src/stores/sharded/lru.rs:356`,
  `src/stores/sharded/expiring_lru.rs:370`), serializing reads within a shard.
- The non-LRU sharded stores read under a shared lock.
- The crate docs already note this as a known limitation and point users to
  ShardedUnboundCache.

## Desired work

- A future store type using sampled/clock or TinyLFU recency that reads under a shared lock and
  only takes the write lock on insert/eviction or a sampled fraction of hits.

## Notes

- Deferred. Will ship as a separate distinct store type rather than changing the strict-LRU
  stores' semantics.
- Tracked here so we come back to it. Document the limitation in the LRU store docs in the
  meantime.
