# 0015 - Collapse *Base + alias into a defaulted type param

Status: Implemented

## Problem

Each sharded store shipped three public names: `ShardedXBase<K, V, H>`, the two-parameter alias
`ShardedX<K, V> = ShardedXBase<K, V, DefaultShardHasher>`, and `ShardedXBuilder<K, V, H>`. `H`
lived only on the `*Base` struct, so `*Base` was the name rustc printed in every diagnostic and
rustdoc rendered every impl on, even though users write `ShardedX`. A missing-method error on a
default-hasher cache named a type absent from the calling code:

```text
error[E0599]: no method named `set_ttl` found for struct `ShardedUnboundCacheBase<K, V, H>`
```

## Shipped shape

One generic type per store, with the hasher as a defaulted third parameter, mirroring
`std::collections::HashMap<K, V, S = RandomState>`:

```rust
pub struct ShardedUnboundCache<K, V, H = DefaultShardHasher> { .. }
```

Applied to all six: `ShardedUnboundCache`, `ShardedLruCache`, `ShardedTtlCache`,
`ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`. The `*Base` structs and
the aliases over them are gone from the crate root, `cached::stores`, and
`cached::stores::sharded`.

Consequences:

- `ShardedX<K, V>` keeps naming the default-hasher store, so code that never spelled a `*Base`
  name compiles unchanged.
- Diagnostics and rustdoc now name `ShardedX`. The golden in
  `tests/ui/sharded_unbound_no_set_ttl.stderr` records the before/after.
- The custom-hasher store is `ShardedX<K, V, H>`, spellable in a type annotation or a generic
  bound without naming an internal type.
- Builder types are untouched: `ShardedXBuilder<K, V, H>` (and
  `ShardedLruTtlCacheBuilder<K, V, E, H>`) already carried `H`.

## No deprecated alias

`pub type ShardedXBase<K, V, H> = ShardedX<K, V, H>` was considered and rejected. Keeping it
would preserve the name in rustdoc and in `use` statements, which is the surface the change
exists to remove, and 3.0 is the break window. Migration is the mechanical rename `ShardedXBase`
-> `ShardedX`.

## Constructor constraint preserved

`new` and `builder` stay on the default-hasher inherent impl block
(`impl<K, V> ShardedX<K, V, DefaultShardHasher>`), so `ShardedX::<_, _, CustomHasher>::new()` and
`::builder()` still fail with E0599 rather than silently constructing a `DefaultShardHasher`
cache. The collapse does not weaken this: the turbofish now names the public type instead of
`*Base`, and rustc's "the associated function or constant was found for `ShardedX<K, V>`" note
points at the default-hasher instantiation. `tests/ui/sharded_custom_hasher_constructor.rs`
(renamed from `sharded_base_custom_hasher_constructor.rs`) asserts both errors.

A custom hasher is introduced only through `ShardedX::builder().hasher(h)`, which switches the
builder's `H` and whose `build` yields `ShardedX<K, V, H>`.
`tests/v3_sharded_hasher_type_param.rs` covers this for all six stores, asserting both the type
annotation and that the supplied hasher actually drives shard routing.

## Related

- `specs/store-sharded.md` SHARD-13 (supersedes SHARD-1's description of the exported surface).
