# 0044 - Blanket `ShardHasher` over `BuildHasher`

Status: Implemented

## Problem

`ShardHasher<K>` and `std::hash::BuildHasher` were unrelated traits. The single-owner builders
take `S: BuildHasher` (`LruCacheBuilder::hasher`, `UnboundCacheBuilder::hasher`, ...; see
[0001-non-sharded-custom-hasher.md](0001-non-sharded-custom-hasher.md)), the sharded builders
take `H: ShardHasher<K>`, and no std hasher satisfied the second one:

```rust
// compiled
UnboundCache::<u64, u64>::builder().hasher(RandomState::new())
// E0277: the trait bound `RandomState: ShardHasher<u64>` is not satisfied
ShardedLruCache::<u64, u64>::builder().hasher(RandomState::new())
```

A downstream crate hit this while moving a cache from `LruCache` to `ShardedLruCache`: the hasher
argument that had been working stopped compiling, with no suggestion pointing at
`DefaultShardHasher` or at writing a `ShardHasher` impl by hand.

## Decision

Blanket-implement `ShardHasher<K>` for every thread-safe `BuildHasher`:

```rust
impl<K, S> ShardHasher<K> for S
where
    K: std::hash::Hash,
    S: std::hash::BuildHasher + Clone + Send + Sync + 'static,
{
    fn shard_hash(&self, key: &K) -> u64 { self.hash_one(key) }
}
```

`Clone + Send + Sync + 'static` are the existing `ShardHasher` supertrait bounds, required
because the hasher lives inside the store's `Arc<Inner>`.

### `DefaultShardHasher` takes the blanket path

`DefaultShardHasher` carried `impl<K: Hash> ShardHasher<K> for DefaultShardHasher` and did not
implement `BuildHasher`. Keeping both would be a duplicate-impl error (E0119), so exactly one had
to remain. The explicit impl was deleted and `DefaultShardHasher` now implements `BuildHasher`
(`type Hasher` resolves through `DefaultHashBuilder`, and `hash_one` delegates to the wrapped
builder's own `hash_one` so `ahash::RandomState`'s single-value path is not lost).

Reasons for that direction over keeping the explicit impl:

- One rule to document instead of a rule plus an exception. `ShardHasher` is "any thread-safe
  `BuildHasher`", and the default hasher is an instance of the rule rather than a special case.
- It fixes the interop gap symmetrically. The reported failure was a `BuildHasher` rejected by a
  sharded builder; the mirror failure was `DefaultShardHasher` rejected by a single-owner builder
  and by `HashMap::with_hasher`. Both now work.
- The behavior is identical either way: the old explicit impl already forwarded to
  `BuildHasher::hash_one` on the wrapped builder.

The cost is that `DefaultShardHasher::Hasher` is public API and resolves to `ahash::AHasher` under
the `ahash` feature. `ahash` is already public API through the `DefaultHashBuilder` alias
(`pub type DefaultHashBuilder = ahash::RandomState`), so this adds no new exposure.

## Consequences

Breaking, which is why it lands in 3.0 rather than a later 3.x. The blanket impl overlaps any
downstream `impl ShardHasher<K> for T where T: BuildHasher`, so adding it later would break such
a crate. It also makes the two traits mutually exclusive going forward: a type that implements
`BuildHasher` cannot also carry a hand-written `ShardHasher` impl. Custom shard routing (numeric
range, string prefix, tenant id) belongs on a type that does not implement `BuildHasher`, which is
what the existing `ShardHasher` doc example already shows.

The upper-32-bit distribution contract is unchanged and is not implied by `BuildHasher`. Shard
selection is `(hash >> 32) & shard_mask`. `hash_one` on `std::hash::RandomState` (SipHash-1-3) and
on `ahash::RandomState` diffuse key entropy across all 64 bits and satisfy it; a hand-written
`BuildHasher` whose `finish` leaves the high bits constant still routes every key to shard 0, and
the "zero upper bits" warning on `ShardHasher` stays for that reason.

## Coverage

- `tests/sharded_hasher_accepts_buildhasher.rs`: `std::hash::RandomState` and `ahash::RandomState`
  satisfy `ShardHasher<K>` and are accepted by `ShardedLruCache::builder().hasher(..)`;
  `DefaultShardHasher` works as a `HashMap` and `UnboundCache` hash builder; `shard_sizes()` over
  4096 keys and 16 shards shows the spread, and the upper 32 bits of `shard_hash` are checked
  directly.
- `src/stores/sharded/mod.rs`: `shard_hash` on `DefaultShardHasher` equals `hash_one`.

Single-path enforcement needs no test of its own: two impls for one type is a compile error, so
the crate building is the check.

See [store-sharded.md](../store-sharded.md) SHARD-14.
