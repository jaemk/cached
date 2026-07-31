# 0037 - sharded LRU default shard count bounded by max_size

Status: Implemented

## Current state

The sharded LRU-family builders (`ShardedLruCacheBuilder`, and the LRU-half of
`ShardedLruTtlCacheBuilder` / `ShardedExpiringLruCacheBuilder`) derive the default shard count
from `default_shard_count()`: `available_parallelism() * 4`, clamped to `[8, 1024]` and rounded up
to a power of two. This computation has no reference to the requested `max_size` at all.

Combined with two other pieces of existing behavior, this makes the default path allocate far
more capacity than requested for a bounded cache on a high-core-count machine:

- `per_shard_cap_from_total` applies a 16-entries-per-shard floor whenever `n_shards > 1`, so a
  small `max_size` divided across many shards still gives each shard at least 16 slots.
- Each shard's `LruCache::builder().build()` eagerly preallocates: `HashTable::try_reserve(cap)`
  plus `LRUList::try_with_capacity(cap)`.

`ShardedLruCache::new(100)` on a 64-core box resolves to `default_shard_count() == 256`, and
`per_shard_cap_from_total(100, 256)` applies the 16-per-shard floor, yielding 256 shards x 16 =
4096 effective capacity: 256 hash tables and 256 `Vec`s eagerly allocated for a cache asked to
hold only 100 entries.

## The rule

When a builder has a total `max_size` (as opposed to an explicit `per_shard_max_size`), the
DEFAULT shard count is:

```text
next_power_of_two(max_size / 16).clamp(1, default_shard_count())
```

implemented by `default_shard_count_for_capacity(Option<usize>)` in
`src/stores/sharded/mod.rs`. Passing `None` (the unbounded stores' path, and the
`per_shard_max_size` path, which has no total to scale against) reproduces `default_shard_count()`
exactly, unchanged. This keeps each shard at roughly its 16-entry floor by construction instead of
relying on the floor to silently inflate a shard count picked without regard to capacity.

Each LRU-family builder consults `default_shard_count_for_capacity(self.max_size)` on the default
path via a `resolve_shard_count` helper: when `self.shards` is `None` it derives the count from
capacity, and when `.shards(n)` is set it defers to `checked_shard_count` unchanged.

An explicit `.shards(n)` on a builder remains authoritative. `default_shard_count_for_capacity` is
consulted only on the default (unconfigured) shard-count path; a caller who sets `.shards(n)` gets
exactly `n` shards (rounded up to a power of two) regardless of `max_size`, preserving the
`Some(0)` rejection and the rounding-overflow guard in `checked_shard_count`.

## Why this is default tuning, not a bug fix

The over-capacity outcome was already *documented*, not merely an accident of the formula: the
`max_size` builder doc and the `ShardedLruCache::new` doc in `src/stores/sharded/lru.rs` both
state that the effective total capacity can exceed `max_size` for small values and point at
`capacity()` to read the actual figure. A caller could be relying on the old default shard count
for its own reasons (e.g. deliberately over-provisioning to reduce shard contention). Changing the
default is a considered tuning decision (trading some of that headroom for allocation cost
proportional to the requested size), not a correctness fix for behavior that violated its own
contract. The docs are updated to describe the capped default (the overshoot is now roughly
`max_size` rounded up to the 16-per-shard floor rather than a fixed CPU-derived shard count times
16).

## Observable surface that changed

For caches built through the default (no explicit `.shards(n)`) path with a `max_size`:

- `capacity()`: the effective total capacity is smaller for small `max_size` values on
  high-core-count machines, though it can still exceed `max_size` when the 16-per-shard floor
  applies (e.g. `max_size = 100` yields 8 shards x 16 = 128, versus 256 shards x 16 = 4096 under
  the old default on a 64-core box).
- `shards()`: reports the capped default shard count, not the raw `available_parallelism() * 4`
  figure.
- `shard_sizes()`: fewer, larger shards under the default.
- Eviction distribution: with fewer shards, each shard holds a larger fraction of the total
  entries, so LRU eviction (which is per-shard) is coarser-grained under concurrent load.

Caches built with an explicit `.shards(n)` are unaffected, since `.shards(n)` overrides the
default path entirely. The unbounded sharded stores (`ShardedUnboundCache`, and the non-LRU
`ShardedTtlCache` / `ShardedExpiringCache` without a `max_size`) are unaffected: their default
path passes `None` and keeps `default_shard_count()`. The `per_shard_max_size` path is also
unaffected. Only default-shard-count construction with a total `max_size` (`::new(max_size)` or
`.max_size(n).build()` with no `.shards()` call) observes the new sizing.

This landed before the 3.0.0 final release: it changes an observable default (not just an
internal allocation detail), and 3.0 is the last point where a default like this can move without
a deprecation cycle.

## Notes

- `default_shard_count_for_capacity` lives in `src/stores/sharded/mod.rs`. The LRU-family
  builders (`ShardedLruCacheBuilder`, `ShardedLruTtlCacheBuilder`, `ShardedExpiringLruCacheBuilder`)
  call it on their default shard-count path through a `resolve_shard_count` helper; the explicit
  `.shards(n)` path still goes through `checked_shard_count`.
- The 16-per-shard floor itself (`per_shard_cap_from_total`) is unchanged; this record only
  changes how many shards the default path asks for.
- See [store-sharded.md](../store-sharded.md) SHARD-7 for the live behavior.
