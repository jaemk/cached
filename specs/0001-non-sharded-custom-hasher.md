# 0001 - Custom hasher on non-sharded stores

Status: Implemented

## Current state

- Non-sharded stores hardcode the hash builder and expose no hasher type parameter and no
  `hasher()` builder method: `UnboundCache<K, V>` with `HashMap<K, V, RandomState>`
  (`src/stores/unbound.rs:25`), and likewise `LruCache` (`src/stores/lru.rs:28`), `TtlCache`
  (`src/stores/ttl.rs:29`), `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`.
- The `RandomState` is selected at compile time by the `ahash` feature; a user cannot supply
  `FxHasher`, a seeded/deterministic hasher for reproducible tests, or a per-instance hasher.
- Sharded stores parameterize only the shard-router hasher (`H: ShardHasher`,
  `src/stores/sharded/mod.rs:143`); the per-shard inner map is still
  `HashMap<K, V, RandomState>` (`src/stores/sharded/unbound.rs:25`). So no store currently lets
  the caller choose the map hasher.

## Desired work

- Add a hasher type parameter with a default to each non-sharded store, e.g.
  `UnboundCache<K, V, S = DefaultHashBuilder>`, and a `.hasher(s: S)` builder method that
  switches the builder's `S` (mirrors the sharded `.hasher()` pattern).
- Keep the named types defaulting to today's `RandomState` so existing code is unaffected at the
  type level. `S: BuildHasher` bounds appear on the constructor and `Cached` impls.
- Thread the chosen hasher into the LRU internals (`src/stores/lru.rs` already carries a
  `hash_builder` field) and the other backing maps.

## Notes

- Also surface the sharded inner-map hasher as part of this work, so sharded stores can pick the
  map hasher too (not just the shard router). Optional; can be a follow-on if it widens scope.
- Migration: low for ordinary users (defaulted param); turbofish call sites and stored concrete
  types may need the extra parameter. Compiler-guided.
