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

Sharded stores implement the concurrent trait family: `ConcurrentCacheBase`,
`ConcurrentCached`, and `ConcurrentCachedAsync` on all six variants; `ConcurrentCacheTtl` on the
TTL variants; `ConcurrentCacheEvict` and `ConcurrentCloneCached` on the four expiry-capable
variants (TTL and expiring). The runtime TTL controls (`ttl`/`set_ttl`/`unset_ttl`/
`refresh_on_hit`/`set_refresh_on_hit`) exist only on `ConcurrentCacheTtl`, not as inherent
methods. Each of the six concrete sharded types also exposes inherent shims that return unwrapped
values and take call-site priority over the `ConcurrentCachedExt` aliases: `get`, `set`, `remove`,
`remove_entry`, `delete`, `reset`, `contains`, and `peek` (`contains` and `peek` are peek-based,
infallible, `&self`; `peek` returns a clone of the live value with no recency/TTL/metrics
effects). The same `peek` contract is also reachable generically through the `ConcurrentCachePeek`
trait (`cache_peek` plus a defaulted `peek` alias, `Result<Option<V>, Infallible>` on these six
stores); the inherent shim keeps call-site priority on the concrete types.
Metrics are exposed through the trait per
[design/0012-concurrent-metrics-trait.md](design/0012-concurrent-metrics-trait.md).
See [traits-concurrent.md](traits-concurrent.md).

## SHARD-4

`ShardedUnboundCache` does not track an evictions counter (it never evicts on its own); see the declined
[design/0007-unbound-evictions-counter.md](design/0007-unbound-evictions-counter.md). Open
directions: a read-optimized sharded LRU
([design/0010-read-optimized-sharded-lru.md](design/0010-read-optimized-sharded-lru.md)) and
collapsing the `*Base` alias into a defaulted type param
([design/0015-sharded-base-alias-collapse.md](design/0015-sharded-base-alias-collapse.md)).

## SHARD-5

The LRU-bounded variants (`ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache`)
support runtime capacity resizing via `set_max_size(&self)` / `try_set_max_size(&self)`, using
the builders' ceiling-division-plus-16-per-shard-floor policy. Shrinks evict per shard strictly
by LRU recency (TTL/expiry state is ignored); resize is not atomic across shards. The unbounded
variants' builders (`ShardedUnboundCacheBuilder`, `ShardedTtlCacheBuilder`,
`ShardedExpiringCacheBuilder`) take a `per_shard_initial_capacity` preallocation hint, the
sharded counterpart of the single-owner builders' `initial_capacity`.

## SHARD-6

Inherent `retain<F: FnMut(&K, &V) -> bool>(&self, keep: F)` on all six sharded stores: same
contract as the single-owner stores, applied per shard. Shards are locked and swept one at a
time (not atomic across shards, so concurrent readers may briefly observe some shards already
filtered and others not), and the predicate runs under the shard write lock, so it must not
re-enter the cache. `on_evict` fires after each shard's lock is released, once per removed
entry, in shard order. On the expiry-aware variants, expired entries are removed regardless of
the predicate and every removal counts an eviction; `ShardedUnboundCache` tracks no evictions
counter, so an entry there simply survives exactly when `keep` returns `true`. `retain` requires
no `K: Clone` bound and is inherent-only, not a trait method; see
[traits-concurrent.md](traits-concurrent.md).

## SHARD-7

For the LRU-bounded variants (`ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache`),
the DEFAULT shard count (no explicit `.shards(n)` on the builder) is capped by the requested
`max_size` rather than derived from `available_parallelism()` alone: it is
`next_power_of_two(max_size / 16).clamp(1, default_shard_count())`, where `default_shard_count()`
is `available_parallelism() * 4` clamped to `[8, 1024]` and rounded to a power of two. This keeps
a small bounded cache (e.g. `ShardedLruCache::new(100)`) from preallocating hundreds of shards on
a high-core-count machine when the 16-entries-per-shard floor (`per_shard_cap_from_total`) alone
would already dominate the effective capacity. Building without a `max_size` (the unbounded
sharded stores) is unaffected and keeps using `default_shard_count()` directly.
`.shards(n)` on the builder always overrides this default and is authoritative regardless of
`max_size`. See [design/0037-sharded-lru-default-shard-cap.md](design/0037-sharded-lru-default-shard-cap.md).

## SHARD-8

On the expiry-aware sharded stores, the liveness of an entry is decided against a single clock
sample taken by the calling thread BEFORE it acquires the shard lock, not re-read once the lock is
held. Under lock contention an entry that crosses its expiry while the caller is queued is
therefore judged live for that operation: `cache_set` returns `Some(old_value)` and fires no
`on_evict`, and the entry is swept on a later access instead. This is inside the lazy-expiry
contract in [SHARD-3](#shard-3), which promises only that an expired entry never reads as present,
never that removal is prompt. Callers that need a hard bound on how long an expired entry can
occupy space should call `evict()`.
