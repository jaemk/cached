# 0015 - Collapse *Base + alias into a defaulted type param

Status: Needs research

## Current state

- Each sharded store ships three public names: `ShardedXBase<K,V,H>`,
  `ShardedX<K,V> = ...Base<K,V,DefaultShardHasher>`, and `ShardedXBuilder<K,V,H>` (e.g.
  `src/stores/sharded/unbound.rs:43,51`).
- The `*Base` name leaks into doc links and error messages.

## Desired work

- Collapse to one generic type per store with a defaulted hasher param,
  `ShardedX<K, V, H = DefaultShardHasher>`, like `std::collections::HashMap<K, V, S =
  RandomState>`, dropping the separate `*Base` alias.

## Notes

- Lower priority now that the turbofish-drops-hasher footgun is already fixed.
- Migration is a mechanical rename `ShardedXBase` -> `ShardedX`; a deprecated alias could ease
  it. Touches every custom-hasher user and doc reference.
