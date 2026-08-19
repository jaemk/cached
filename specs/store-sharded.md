# Sharded concurrent caches

Fully concurrent, sharded, `Arc`-backed stores with a shared `&self` API. The six variants map
one-to-one to the in-memory stores: `ShardedUnboundCache`, `ShardedLruCache`, `ShardedTtlCache`,
`ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`. The `*Ttl` variants
require `time_stores`. Each is the default store for the matching `#[concurrent_cached]`
configuration.

## SHARD-1

State is split across shards keyed by a `ShardHasher`; concurrent access to different shards does
not contend. `DefaultShardHasher` is the default. Superseded in part by [SHARD-13](#shard-13),
which collapsed the original base-type-plus-alias pair into a single generic type per store.

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
direction: a read-optimized sharded LRU
([design/0010-read-optimized-sharded-lru.md](design/0010-read-optimized-sharded-lru.md)).
Collapsing the `*Base` type and its alias into one generic type shipped; see
[SHARD-13](#shard-13).

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
counter, so an entry there simply survives exactly when `keep` returns `true`. `retain` is
inherent-only, not a trait method; see [traits-concurrent.md](traits-concurrent.md).

`retain` itself adds no `K: Clone` bound on any of the six. A panicking predicate is made safe by
sweeping each shard in two phases (select, then remove), and the selection is carried across the
phases as a `Vec<bool>` of decisions replayed through `extract_if`, not as a `Vec<K>` of cloned
keys -- so the panic-safety guarantee costs no bound. On the three `HashMap`-backed stores
(`ShardedUnboundCache`, `ShardedTtlCache`, `ShardedExpiringCache`) `retain` is therefore callable
with a non-`Clone` key. The three LRU-backed stores (`ShardedLruCache`, `ShardedLruTtlCache`,
`ShardedExpiringLruCache`) still require `K: Clone`, inherited from their enclosing impl block
because the LRU ring needs it independently of `retain`.

## SHARD-7

For the LRU-bounded variants (`ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache`),
the DEFAULT shard count (no explicit `.shards(n)` on the builder) is capped by the requested
`max_size`. The helper `default_shard_count_for_capacity(Some(max_size))` yields
`next_power_of_two(max_size / 16).clamp(1, default_shard_count())`, where `default_shard_count()`
is `available_parallelism() * 4` clamped to `[8, 1024]` and rounded to a power of two. Each
LRU-family builder calls it on the default path via `resolve_shard_count`.

Combined with the 16-entries-per-shard floor (`per_shard_cap_from_total`), the effective capacity
can still exceed the requested `max_size`, but only modestly: `ShardedLruCache::new(100)` resolves
to 8 shards and an effective capacity of 128 (8 x 16), not the old 256 shards / 4096 capacity a
64-core box produced. Read `capacity()` after construction for the exact figure.

Building without a `max_size` (the unbounded sharded stores `ShardedUnboundCache`, and the non-LRU
`ShardedTtlCache` / `ShardedExpiringCache`) passes `None` and keeps `default_shard_count()`
directly, unaffected. The `per_shard_max_size` path (no total) likewise keeps
`default_shard_count()`. An explicit `.shards(n)` always overrides the default and remains
authoritative regardless of `max_size` (routed through `checked_shard_count`, which preserves the
`Some(0)` rejection and the rounding-overflow guard). See
[design/0037-sharded-lru-default-shard-cap.md](design/0037-sharded-lru-default-shard-cap.md).

## SHARD-8

On the TTL-based sharded stores (`ShardedTtlCache`, `ShardedLruTtlCache`), the liveness of an
entry is decided against a single clock sample taken by the calling thread BEFORE it acquires the
shard lock, not re-read once the lock is held. Under lock contention an entry that crosses its
expiry while the caller is queued is therefore judged live for that operation: `cache_set` returns
`Some(old_value)` and fires no `on_evict`, and the entry is swept on a later access instead. This
is inside the lazy-expiry contract in [SHARD-3](#shard-3), which promises only that an expired
entry never reads as present, never that removal is prompt. Callers that need a hard bound on how
long an expired entry can occupy space should call `evict()`.

The per-value-`Expires` family (`ShardedExpiringCache`, `ShardedExpiringLruCache`) does not follow
this contract: `is_expired()` is evaluated on the stored value while the shard write lock is
already held in `cache_set`, so a value that crosses its own expiry boundary while the caller is
queued for the lock is judged expired, not live, for that operation. In that case `cache_set`
returns `None` and fires `on_evict` for the displaced entry, the opposite of the TTL-family
outcome above.

## SHARD-9

The inherent `retain` from [SHARD-6](#shard-6) now returns `usize` (the count of entries removed
across all shards) instead of `()`, matching the single-owner stores (see
[store-lru.md](store-lru.md) LRU-8). On the four expiry-aware sharded stores the count folds
together predicate-rejected entries and entries swept for having already expired; on
`ShardedUnboundCache` (no eviction dimension) the count is exactly the number of entries `keep`
rejected. This is a BREAKING change.

## SHARD-10

The inherent-vs-trait return-shape split (see [SHARD-3](#shard-3)) is documented as a sharp edge
on all six sharded store types. The inherent shims return unwrapped values (`Option<V>`, `()`,
`bool`) and take call-site priority over the `ConcurrentCached*` trait methods, which return
`Result<_, Self::Error>`. The consequence worth stating plainly: `s.set(k, v).unwrap()` compiles
and resolves to `Option::unwrap`, so it panics on a first insert (there is no displaced value).
The rustdoc note gives the UFCS disambiguation (`ConcurrentCached::cache_set` /
`ConcurrentCachedExt::set`). Signatures are unchanged; this is a documentation change only.

`#[must_use]` on the inherent `set`/`remove` was considered and rejected. It cannot fire on the
hazard it would target, because `s.set(k, v).unwrap()` consumes the return value, and it fires
instead on fire-and-forget `s.set(k, v);`, which is the common correct call. The attribute was
added only to inherent `contains`, matching the existing bare `#[must_use]` on `get`/`peek`.

The shims are inherent methods on the one generic store type per store family; see
[SHARD-13](#shard-13) and
[design/0015-sharded-base-alias-collapse.md](design/0015-sharded-base-alias-collapse.md).

## SHARD-11

The three LRU-bounded sharded stores' `capacity()` getter (see [SHARD-7](#shard-7)) gains
`#[doc(alias = "max_size")]`, matching the four single-owner bounded stores (`LruCache`,
`LruTtlCache`, `ExpiringLruCache`, `TtlSortedCache`).

## SHARD-12

Sharded stores implement no iteration or snapshot capability: no `iter`/`keys`/`values`, unlike
every single-owner store, which implements `CachedIter`. This is a deliberate limitation. See
[design/0039-sharded-iteration-snapshot-api.md](design/0039-sharded-iteration-snapshot-api.md).

## SHARD-13

Each sharded store is one generic type with a defaulted hasher parameter,
`ShardedX<K, V, H = DefaultShardHasher>`, mirroring `std::collections::HashMap<K, V, S =
RandomState>`. This replaces the `ShardedXBase<K, V, H>` struct plus the two-parameter
`ShardedX<K, V>` alias described in [SHARD-1](#shard-1). The six `Sharded*Base` names are gone
from the public API with no deprecated alias; migration is the mechanical rename
`ShardedXBase` -> `ShardedX`. `ShardedX<K, V>` still names the default-hasher store, so code that
never spelled a `*Base` name is unaffected. This is a BREAKING change.

`new` and `builder` remain constrained to the default-hasher instantiation
`ShardedX<K, V, DefaultShardHasher>`, so a `ShardedX::<_, _, H>::new()` turbofish that would
silently discard `H` still fails to compile; a custom hasher is introduced only through
`ShardedX::builder().hasher(h)`, which returns a builder whose `build` yields
`ShardedX<K, V, H>`. Builder type names (`ShardedXBuilder<K, V, H>`) are unchanged. See
[design/0015-sharded-base-alias-collapse.md](design/0015-sharded-base-alias-collapse.md).

## SHARD-14

`ShardHasher<K>` is blanket-implemented for every `std::hash::BuildHasher` that is
`Clone + Send + Sync + 'static`, for every `K: Hash`, forwarding to `BuildHasher::hash_one`. The
same hasher value therefore works on both cache families: `std::hash::RandomState`,
`ahash::RandomState`, and any other thread-safe `BuildHasher` accepted by the single-owner
builders' `hasher` method (see [store-lru.md](store-lru.md),
[design/0001-non-sharded-custom-hasher.md](design/0001-non-sharded-custom-hasher.md)) are now
accepted by `ShardedXBuilder::hasher` too. `DefaultShardHasher` reaches `ShardHasher` through
that blanket impl and no longer carries its own; it implements `BuildHasher` instead, so it is
symmetrically usable as the hash builder of a non-sharded store or a plain `HashMap`.

The upper-32-bit distribution contract of [SHARD-1](#shard-1) is unchanged and is not implied by
`BuildHasher`: `hash_one` on `std::hash::RandomState` (SipHash-1-3) and on `ahash::RandomState`
diffuse key entropy across all 64 bits and satisfy it, but a hand-written `BuildHasher` whose
`finish` leaves the high bits constant still routes every key to shard 0.

Coherence makes the blanket impl exclusive: a type that implements `BuildHasher` can no longer
also carry a hand-written `ShardHasher` impl. Custom shard routing belongs on a type that does
not implement `BuildHasher`. This is a BREAKING change. See
[design/0044-blanket-shardhasher-over-buildhasher.md](design/0044-blanket-shardhasher-over-buildhasher.md).
