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
    K: std::hash::Hash + ?Sized,
    S: std::hash::BuildHasher + Clone + Send + Sync + 'static,
{
    fn shard_hash(&self, key: &K) -> u64 { self.hash_one(key) }
}
```

**Superseded, first by [0052](0052-sharded-borrowed-key-lookups.md) and then by
[0055](0055-shard-hasher-q-over-borrowed-key-routing.md), which is the current record:** the body
shown above no longer matches the code. `shard_hash` now builds a `Hasher` and feeds it the key
explicitly (`build_hasher()` -> `key.hash(&mut hasher)` -> `hasher.finish()`) instead of calling
`hash_one`. The reason is that `hash_one` is an overridable provided method allowed to dispatch on
its static type argument, and `ahash::RandomState` does exactly that; a key type that borrows to a
different type (for example a newtype over an integer) could then have its owned insert and its
borrowed lookup routed to different shards. 0052's own status is now "Superseded by 0055" (see
[design/README.md](README.md)); see 0052's "Outcome" for the ahash specialization case that was
originally analyzed and gotten wrong, and see 0055's "Outcome" for the trait's current shape,
including the `?Sized` relaxation added above.

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

**Update, per [0052](0052-sharded-borrowed-key-lookups.md), whose route was in turn superseded by
[0055](0055-shard-hasher-q-over-borrowed-key-routing.md):** the `hash_one` override this section
describes was itself deleted later, along with routing through `hash_one` generally. `shard_hash`
no longer delegates to `hash_one` on anything, `DefaultShardHasher`'s own included, so the
"single-value path is not lost" reasoning above and the "behavior is identical either way" bullet
both describe a mechanism the crate no longer uses. See the note after the code block above for
why, and see 0055 for the current blanket-impl shape.

## Consequences

Breaking, which is why it lands in 3.0 rather than a later 3.x. The blanket impl overlaps any
downstream `impl ShardHasher<K> for T where T: BuildHasher`, so adding it later would break such
a crate. It also makes the two traits mutually exclusive going forward: a type that implements
`BuildHasher` cannot also carry a hand-written `ShardHasher` impl. Custom shard routing (numeric
range, string prefix, tenant id) belongs on a type that does not implement `BuildHasher`, which is
what the existing `ShardHasher` doc example already shows.

The upper-32-bit distribution contract is unchanged and is not implied by `BuildHasher`. Shard
selection is `(hash >> 32) & shard_mask`. The `Hasher` finished by `std::hash::RandomState`
(SipHash-1-3) and by `ahash::RandomState` (ahash's own) diffuse key entropy across all 64 bits and
satisfy it; a hand-written `BuildHasher` whose `finish` leaves the high bits constant still routes
every key to shard 0, and the "zero upper bits" warning on `ShardHasher` stays for that reason.

## Coverage

- `tests/sharded_hasher_accepts_buildhasher.rs`: `std::hash::RandomState` and `ahash::RandomState`
  satisfy `ShardHasher<K>` and are accepted by `ShardedLruCache::builder().hasher(..)`;
  `DefaultShardHasher` works as a `HashMap` and `UnboundCache` hash builder; `shard_sizes()` over
  4096 keys and 16 shards shows the spread, and the upper 32 bits of `shard_hash` are checked
  directly.
- `src/stores/sharded/mod.rs`: `shard_hash` on `DefaultShardHasher` equals `hash_one`. This
  invariant still holds after the later changes recorded in
  [0052](0052-sharded-borrowed-key-lookups.md) and carried forward unchanged by
  [0055](0055-shard-hasher-q-over-borrowed-key-routing.md), but for the opposite reason:
  `shard_hash` no longer forwards to `hash_one`, and `DefaultShardHasher` no longer overrides
  `hash_one` either, so both sides now happen to run the same default provided `BuildHasher`
  construction (`build_hasher()` -> hash -> `finish()`) independently, rather than one calling the
  other.

Single-path enforcement needs no test of its own: two impls for one type is a compile error, so
the crate building is the check.

See [store-sharded.md](../store-sharded.md) SHARD-14.
