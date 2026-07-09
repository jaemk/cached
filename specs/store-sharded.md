# Sharded concurrent caches

Fully concurrent, sharded, `Arc`-backed stores with a shared `&self` API. The six variants map
one-to-one to the in-memory stores: `ShardedUnboundCache`, `ShardedLruCache`, `ShardedTtlCache`,
`ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`. The `*Ttl` variants
require `time_stores`. Each is the default store for the matching `#[concurrent_cached]`
configuration.

## SHARD-1

State is split across shards keyed by a `ShardHasher`; concurrent access to different shards does
not contend. `DefaultShardHasher` is the default. The base type (`Sharded*Base`) plus a public
alias form the exported surface.

## SHARD-2

`#[concurrent_cached]` selects the variant from its attributes: no extra attrs ->
`ShardedUnboundCache`; `max_size` -> LRU; `ttl_secs`/`ttl_millis`/`ttl` -> TTL; `expires = true`
-> expiring; combinations pick the LRU+TTL or LRU+expiring variant. See
[macro-concurrent-cached.md](macro-concurrent-cached.md).

## SHARD-3

Sharded stores implement the concurrent trait family (`ConcurrentCacheBase`,
`ConcurrentCached`, and `ConcurrentCacheTtl` on TTL variants). Metrics are exposed through the
trait per [design/0012-concurrent-metrics-trait.md](design/0012-concurrent-metrics-trait.md).
See [traits-concurrent.md](traits-concurrent.md).

## SHARD-4

`ShardedUnboundCache` does not track an evictions counter (it never evicts); see the declined
[design/0007-unbound-evictions-counter.md](design/0007-unbound-evictions-counter.md). Open
directions: a read-optimized sharded LRU
([design/0010-read-optimized-sharded-lru.md](design/0010-read-optimized-sharded-lru.md)) and
collapsing the `*Base` alias into a defaulted type param
([design/0015-sharded-base-alias-collapse.md](design/0015-sharded-base-alias-collapse.md)).
