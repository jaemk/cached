# 0037 - sharded LRU default shard count bounded by max_size

Status: Not implemented

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
DEFAULT shard count becomes:

```text
next_power_of_two(max_size / 16).clamp(1, default_shard_count())
```

implemented by `default_shard_count_for_capacity(Option<usize>)` in
`src/stores/sharded/mod.rs`. Passing `None` (the unbounded stores' path) reproduces
`default_shard_count()` exactly, unchanged. This keeps each shard at roughly its 16-entry floor
by construction instead of relying on the floor to silently inflate a shard count picked without
regard to capacity.

**An explicit `.shards(n)` on a builder remains authoritative.** `default_shard_count_for_capacity`
is consulted only on the default (unconfigured) shard-count path; a caller who sets `.shards(n)`
gets exactly `n` shards regardless of `max_size`.

## Why this is default tuning, not a bug fix

The over-capacity outcome is currently *documented*, not merely an accident of the formula:
`src/stores/sharded/lru.rs` around lines 738-745 (the `max_size` builder doc) and around lines
86-91 (`ShardedLruCache::new` doc) both state that "the effective total capacity can exceed
`max_size` for small values" and point at `capacity()` to read the actual figure. A caller could
be relying on the current default shard count for its own reasons (e.g. deliberately
over-provisioning to reduce shard contention). Changing the default is a considered tuning
decision (trading some of that headroom for allocation cost proportional to the requested size),
not a correctness fix for behavior that violated its own contract.

## Observable surface that changes

For caches built through the default (no explicit `.shards(n)`) path with a `max_size`:

- `capacity()`: the effective total capacity is smaller for small `max_size` values on
  high-core-count machines, though it can still exceed `max_size` when the 16-per-shard floor
  applies (e.g. `max_size = 100` now yields 8 shards x 16 = 128, versus 256 shards x 16 = 4096
  before).
- `shards()`: reports the capped default shard count, not the raw
  `available_parallelism() * 4` figure.
- `shard_sizes()`: fewer, larger shards under the new default.
- Eviction distribution: with fewer shards, each shard holds a larger fraction of the total
  entries, so LRU eviction (which is per-shard) is coarser-grained under concurrent load.

Existing tests that construct sharded LRU caches with an explicit `.shards(n)` are unaffected,
since `.shards(n)` overrides the default path entirely. Only default-shard-count construction
(`::new(max_size)` or `.max_size(n).build()` with no `.shards()` call) observes the new sizing.

This must land before the 3.0.0 final release: it changes an observable default (not just an
internal allocation detail), and 3.0 is the last point where a default like this can move without
a deprecation cycle.

## Notes

- `default_shard_count_for_capacity` and its test suite already exist in
  `src/stores/sharded/mod.rs`; the LRU-family builders (`ShardedLruCacheBuilder`,
  `ShardedLruTtlCacheBuilder`, `ShardedExpiringLruCacheBuilder`) need to call it on their default
  shard-count path in place of the bare `default_shard_count()`. See
  `checked_shard_count` in `src/stores/sharded/mod.rs`, which is the current unconditional
  `shards.unwrap_or_else(default_shard_count)` call site each builder's `build()` goes through.
- The 16-per-shard floor itself (`per_shard_cap_from_total`) is unchanged; this record only
  changes how many shards the default path asks for.
- See [store-sharded.md](../store-sharded.md) for the shipped behavior statement.
