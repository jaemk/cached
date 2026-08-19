# 0046 - Configurable key-replacement policy on overwrite

Status: Not implemented (declined)

## The proposal

Make it configurable which key instance survives a `cache_set` over an existing key. Every builder
with an `on_evict` setter would also take `replace_key_on_overwrite(bool)`, defaulting to `true`,
readable and settable afterwards through a `CacheKeyPolicy` / `ConcurrentCacheKeyPolicy` trait pair
mirroring the refresh-on-hit split ([0045](0045-refresh-on-hit-trait-split.md)).

The motivation was that the behavior is currently fixed per store family and the two families
disagree, which [store-lru.md](../store-lru.md) LRU-6 recorded as deliberate but which was really
inherited from the collection each family is built on.

## Why it was declined

It was implemented across all thirteen stores and then withdrawn. The cost landed on the write
path of every store, to change something only an unusual key type can observe:

- The backing maps are `std::collections::HashMap`, whose `OccupiedEntry` cannot re-key a slot
  (`replace_entry` is hashbrown-only, and the std equivalent is unstable). Replacing the key means
  `remove` followed by `insert`: a second hash and probe on every overwrite, on stores whose whole
  point is lookup speed.
- `TtlSortedCache` was worse. Its occupied path reuses the existing entry's `CacheArc` (a refcount
  bump); re-keying forces a `K::clone` and a fresh `Arc` per overwrite, and the deadline-ordered
  index has to be re-stamped in lockstep.
- The sharded LRU stores needed a published `AtomicBool` plus an `&self` setter that walks every
  shard under its write lock, for a value that is set once and then never changes in practice.
- It is observable ONLY for key types whose `Hash`/`Eq` cover part of the payload, so that two
  equal keys can still differ in some other field. For an ordinary key type the two policies are
  indistinguishable, so the majority of users would pay the cost for no visible behavior.

The consistency argument also turned out to be weaker than it looked. Making the key uniform
across families raised a second question with no clean answer: which key `on_evict` should receive
under the non-default policy, where the stored key is still in the map and the caller's key is the
one being dropped. Neither choice is obviously right, and the families disagreed there too.

## What was done instead

The per-family behavior stays as it is, and is documented precisely rather than made configurable:
[store-lru.md](../store-lru.md) LRU-6 for the LRU family (the caller's key rebinds the slot) and
[store-ttl.md](../store-ttl.md) TTL-7 for the HashMap-backed stores (the first-inserted key is
kept). Each store keeps its native single-probe write path.

One genuine defect found while implementing this was fixed on its own merits and kept:
`ShardedTtlCache::cache_set` and `ShardedExpiringCache::cache_set` chose their write path based on
whether an `on_evict` callback was configured, so attaching a purely observational callback changed
which key was physically stored. Both now always take the plain-`insert` path that keeps the stored
key, which is both the faster branch and consistent with their single-owner counterparts. See
[traits-concurrent.md](../traits-concurrent.md) CTRAIT-8.

## If this is revisited

It would need either a hashbrown-style `replace_entry` on the backing map (so the replacing path
costs no extra probe), or evidence that real callers hit the partial-key case often enough to
justify the write-path cost. A cheaper middle ground is a documented recipe: `cache_remove`
followed by `cache_set` already gives a caller full control over which key ends up stored, at a
cost paid only by the callers who need it.
