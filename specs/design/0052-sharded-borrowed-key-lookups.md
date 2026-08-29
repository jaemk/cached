# 0052 - Borrowed-key lookups on the sharded inherent methods

Status: Implemented

## Current state

The single-owner family accepts any borrowed form of the key. `Cached::cache_get` is
`fn cache_get<Q>(&mut self, k: &Q) -> Option<&V> where K: Borrow<Q>, Q: Hash + Eq + ?Sized`
(`src/lib.rs:1289-1292`), and its own doc example calls it both ways on a `String`-keyed store
(`src/lib.rs:1285-1287`). Every other single-owner lookup is the same shape: `cache_get_mut`
(`src/lib.rs:1295-1298`), `cache_remove` (`src/lib.rs:1381-1384`), `cache_remove_entry`
(`src/lib.rs:1415-1418`), `cache_contains` (`src/lib.rs:1453-1456`), `cache_delete`
(`src/lib.rs:1487-1490`), `CachedPeek::cache_peek` (`src/lib.rs:1888-1891`),
`CloneCached::cache_peek_with_expiry_status` (`src/lib.rs:1990-1993`), and
`CacheExpiry::cache_peek_expires_at` / `cache_expires_at` (`src/lib.rs:2143-2146`,
`src/lib.rs:2173-2176`).

The sharded stores do not. Their inherent methods take `&K` exactly, so
`sharded.get("a")` does not compile where `lru.get("a")` does. The asymmetry is deliberate and
documented at `src/lib.rs:253-258` (the crate-level family comparison, regenerated into
`README.md`), and again on the traits at `src/lib.rs:3053-3063` and `src/lib.rs:2206-2209`.

### Inventory: sharded inherent methods that take `&K`

Six methods on each of the six sharded stores, 36 in total. All six are pure lookups, so all
six can become `Borrow<Q>`-generic:

| Method | lru.rs | unbound.rs | ttl.rs | lru_ttl.rs | expiring.rs | expiring_lru.rs |
|---|---|---|---|---|---|---|
| `get(&self, k: &K) -> Option<V>` | 192 | 196 | 251 | 279 | 208 | 229 |
| `remove(&self, k: &K) -> Option<V>` | 223 | 227 | 282 | 310 | 239 | 260 |
| `remove_entry(&self, k: &K) -> Option<(K, V)>` | 230 | 234 | 289 | 317 | 246 | 267 |
| `delete(&self, k: &K) -> bool` | 237 | 241 | 296 | 324 | 253 | 274 |
| `contains(&self, k: &K) -> bool` | 250 | 254 | 309 | 337 | 266 | 287 |
| `peek(&self, k: &K) -> Option<V>` | 260 | 264 | 320 | 348 | 277 | 298 |

(All paths relative to `src/stores/sharded/`.)

Not in scope, despite looking like part of the same set: `set(&self, k: K,
v: V)` (`src/stores/sharded/lru.rs:202`) and `get_or_set_with(&self, k: K, f: F)`
(`src/stores/sharded/lru.rs:216`) take the key **by value** because they insert it. There is no
borrowed form to generalize to. A borrowed-key `set` would additionally have to answer "which key
does the store end up holding on an overwrite", which is the question 0046 already declined to
make configurable.

Five of the six methods delegate to the `ConcurrentCached` trait impl on the same type (for
example `src/stores/sharded/lru.rs:192-194` calls `ConcurrentCached::cache_get(self, k).unwrap()`),
which is why they inherited the `&K`. `peek` is the exception: it already reaches past the trait
and calls the shard's own `CachedPeek::cache_peek` directly
(`src/stores/sharded/lru.rs:260-263`), which is exactly the shape the other five need.

### The layer below is already generic

Nothing under the shard lock blocks this. The per-shard stores are `LruCache<K, V>`,
`HashMap<K, V, RandomState>`, and their timed variants, and their lookups are already
`Borrow<Q>`-generic: `LruCache::pop_raw` (`src/stores/lru.rs:398-404`) and `LruCache::hash`
(`src/stores/lru.rs:479-487`), plus `Cached`/`CachedPeek` as listed above. The only thing that
takes `&K` between the caller and the entry is shard routing.

### Shard routing, the crux

Every store routes through a private helper (`src/stores/sharded/lru.rs:138-141`, and
`unbound.rs:113-116`, `ttl.rs:169-172`, `lru_ttl.rs:198-201`, `expiring.rs:139-142`,
`expiring_lru.rs:168-171`):

```rust
fn shard_of(&self, k: &K) -> &CachePadded<Shard<LruCache<K, V>>> {
    let h = self.inner.hasher.shard_hash(k);
    &self.inner.shards[shard_index(h, self.inner.shard_mask)]
}
```

`shard_index` is `(hash >> 32) as usize & mask` (`src/stores/sharded/mod.rs:174-176`), so the
whole question is whether a borrowed `&Q` can be made to produce the same `u64` the owned `&K`
produces. Two facts about `ShardHasher` decide it:

1. `pub trait ShardHasher<K>: Clone + Send + Sync + 'static { fn shard_hash(&self, key: &K) -> u64
   }` (`src/stores/sharded/mod.rs:290-292`). `K` carries the implicit `Sized` bound, so
   `ShardHasher<str>` cannot even be written today, let alone implemented.
2. A blanket impl covers every thread-safe `BuildHasher` and defines `shard_hash` as
   `self.hash_one(key)` with `key: &K` (`src/stores/sharded/mod.rs:302-310`, design 0044), and
   coherence makes that impl exclusive: a `BuildHasher` cannot also carry a hand-written
   `ShardHasher` impl (`src/stores/sharded/mod.rs:227-230`, 0044 "Consequences"). So for a
   `BuildHasher` hasher, `shard_hash(&k)` **is** `hash_one(&k)` by construction, with no
   possibility of a competing impl.

That second fact is what makes this implementable, because `hash_one` is the same call std's and
hashbrown's own borrowed lookups already make: `make_hash<Q>(hash_builder, val: &Q) ->
hash_builder.hash_one(val)` (`hashbrown-0.17.0/src/map.rs:236-242`), passing `&K` on insert and
`&Q` on lookup. Every `HashMap<String, V>::get(&str)` in the ecosystem, including the ones inside
this crate's own `ShardedUnboundCache` shards, already depends on `hash_one::<&K>` and
`hash_one::<&Q>` agreeing for `K: Borrow<Q>`. That is not an extra assumption this design
introduces; it is the `Borrow` contract (equivalent `Hash`/`Eq`/`Ord` between the owned and
borrowed forms) that `HashMap` is built on.

Checked against the crate's actual hasher, `ahash 0.8.12`: `RandomState::hash_one` dispatches
through `CallHasher::get_hash` (`ahash-0.8.12/src/random_state.rs:352-358`), whose default impl is
`build_hasher(); value.hash(..); finish()` (`ahash-0.8.12/src/specialize.rs:23-47`). The
specialized impls are all on **unreferenced** types (`str`, `String`, `[u8]`, `Vec<u8>`, the
integers: `ahash-0.8.12/src/specialize.rs:60-128`), and `hash_one` always receives a reference, so
`hash_one::<&String>` and `hash_one::<&str>` both take the unspecialized path and agree.
Note that ahash's `specialize` cfg is set by its build script on a compiler that supports
specialization (`ahash-0.8.12/build.rs:7-9`), not by a cargo feature, so it can switch on under a
nightly toolchain without any change here. It does not change the answer, but it does mean the
routing-parity test below has to exist rather than being argued from the feature set.

### Verdict: feasible, additive, and no trait change

Recommended shape, per method, on the existing inherent impl blocks:

```rust
pub fn get<Q>(&self, k: &Q) -> Option<V>
where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
    H: std::hash::BuildHasher,     // extra method-level bound; the impl block keeps H: ShardHasher<K>
```

with one private routing helper per store, `shard_of_borrowed<Q>(&self, k: &Q)`, computing
`shard_index(self.inner.hasher.hash_one(k), self.inner.shard_mask)`. The crate already does
exactly this kind of borrowed-key routing for `sync_writes = "by_key"`:
`KeyedCache::bucket_for<Q: Hash + ?Sized>` hashes a `&Q` to pick a bucket
(`src/lib.rs:1171-1175`).

The `H: BuildHasher` method bound is the whole trick. It restricts the borrowed path to precisely
the hashers whose `ShardHasher` impl is the blanket one, where owned-versus-borrowed agreement is
guaranteed by the `Borrow` contract. `DefaultShardHasher` implements `BuildHasher` (0044), so the
default instantiation `ShardedLruCache<String, u32>` gets `get("a")` and that is the entire
request. A custom, hand-written `ShardHasher` (the `FibHasher` shape at
`src/stores/sharded/mod.rs:270-284`) keeps working for owned lookups and simply has no borrowed
lookup, which is the honest answer: two unrelated `ShardHasher` impls carry no consistency
contract at all.

The alternative, relaxing `ShardHasher<K>` to `K: ?Sized` and bounding the methods on
`H: ShardHasher<Q>`, is rejected as the first choice. It would let custom routers opt in, but it
buys that by inventing a new cross-impl consistency contract (`shard_hash(&k)` must equal
`shard_hash(k.borrow())`) that the type system cannot check and that would silently route lookups
to the wrong shard when violated, producing phantom misses on entries that are present. It is also
a public trait signature change, where the `H: BuildHasher` route changes no trait at all. Keep it
as a documented fallback if a consumer actually needs borrowed lookups through a custom router.

## Desired work

1. Add a private `shard_of_borrowed<Q>` to each of the six sharded stores, next to the existing
   `shard_of`.
2. Make the 36 inherent methods in the table above generic over `Q`, with the bounds shown. The
   five that currently delegate to `ConcurrentCached` cannot keep delegating (the trait takes
   `&K`); give each store a private `&Q`-generic core that both the trait method (with `Q = K`)
   and the inherent method call, so the two paths cannot drift.
3. Carry the metrics with the core. Hit/miss counters live in the trait impl bodies today
   (`src/stores/sharded/lru.rs:590-608`), as do the eviction counter and the `on_evict` callback
   on the remove path (`src/stores/sharded/lru.rs:619-635`). The borrowed path must produce
   byte-identical metric and callback behavior, not a second, subtly different implementation.
4. Update the family-comparison doc at `src/lib.rs:253-258`, which currently states the `&K`
   restriction without qualification, and regenerate `README.md` from it.
5. Leave every trait alone. See below.

### The trait side is 4.0 work, not 3.2

Making the inherent methods generic is source-compatible and additive. Doing the same to
`ConcurrentCached` (`src/lib.rs:3064`) is breaking: adding a `Q` parameter to a required trait
method breaks every external implementation of it, and the crate's own docs teach users to write
those impls (`src/lib.rs:3022-3036` is a worked `MyStore` impl). It is also not one method but a
family sweep: `ConcurrentCached::cache_get` (3071), `cache_remove` (3100), `cache_remove_entry`
(3156), `cache_delete` (3169), `cache_contains` (3191), `ConcurrentCachePeek::cache_peek`
(`src/lib.rs:2901`), `ConcurrentCachePeekAsync` (`src/lib.rs:2956`), `ConcurrentCachedAsync`
(`src/lib.rs:3519`), `ConcurrentCloneCached::cache_peek_with_expiry_status` (`src/lib.rs:2264`),
and `ConcurrentCacheExpiry` (`src/lib.rs:2413`, `2440`) all take `&K`.

The reason they take `&K` is already documented verbatim at `src/lib.rs:3053-3063`:

> The IO stores (`RedbCache`, `RedisCache`) must serialize the key to perform a lookup; a generic
> `&Q` where only `K: Borrow<Q>` carries no serialization guarantee, and adding a `Q: Serialize`
> bound would bleed a serde dependency into every `ConcurrentCached` implementation.

and restated on `ConcurrentCloneCached` at `src/lib.rs:2206-2209`. That reasoning is unaffected by
anything here: it is about the trait's implementor set, which includes stores that never hash at
all. The shard-routing half of that same paragraph (`src/lib.rs:3057-3058`, "a borrowed `&Q` may
hash differently from the stored `K`, routing the lookup to the wrong shard") is the half this
record answers, and it should be narrowed rather than deleted when this lands: it is true for an
arbitrary `ShardHasher`, and not true for the blanket `BuildHasher` impl.

### Known consequence carried from 0047

0047 shipped `CacheExpiry` with a `Borrow<Q>` key (`src/lib.rs:2143`, `2173`) and
`ConcurrentCacheExpiry` with `&K` (`src/lib.rs:2413`, `2440`), on the stated rationale above
(0047 "Rationale", the "`&K`, not `Borrow<Q>`, on the concurrent trait" paragraph). That is the
same asymmetry as `Cached` versus `ConcurrentCached`, now baked into a trait that shipped in a
released 3.x. Adding `Q` to the concurrent side later is breaking for the same reason as the rest
of the family, so `ConcurrentCacheExpiry` joins the 4.0 sweep or stays `&K` forever. This record
does not change that; the inherent-method work does not reach the expiry reads at all, since the
sharded expiry stores expose them only as trait methods (there is no inherent `peek_expires_at`).

## Pitfalls

- **Generalizing an inherent `&K` parameter to `&Q` is not perfectly source-compatible.** A call
  site that relied on deref coercion at the argument (`store.get(&arc_key)` with
  `arc_key: Arc<String>` and `K = String`, or `&Box<K>`, or a `&SmartString` that was coercing to
  `&String`) infers `Q` as the outer type and then fails on the missing `K: Borrow<Q>`. `&k` where
  `k: K` and literal `&"a".to_string()` are unaffected. This is the same hazard std accepts for
  `HashMap::get`, but it must be called out in the changelog rather than announced as
  "purely additive". `smartstring` is already a dev-dependency (`Cargo.toml:174-175`), so check
  the existing tests for such call sites before assuming there are none.
- **Do not reimplement the lookups.** Two bodies per method (one owned, one borrowed) is how the
  hit/miss counters, the eviction counter, and `on_evict` drift apart. One `&Q` core, two thin
  callers.
- **The routing must be proven equal, not assumed.** `shard_index(hash_one(&owned))` and
  `shard_index(hash_one(borrowed))` agreeing is the entire correctness argument, it depends on a
  third-party hasher's dispatch, and when it breaks the failure is silent: the entry is present,
  the lookup lands on the wrong shard, and the store reports a miss and a `misses` increment. Pin
  it with a test over many keys, not one.
- **`peek` routes but does not go through the trait** (`src/stores/sharded/lru.rs:260-263`), so it
  is the one method whose borrowed form is a one-line change and the one most likely to be
  "done" while the other five are half-converted. Convert all six per store or none.
- **A custom-hasher store loses nothing but gains nothing.** On a store whose `H` is a
  hand-written `ShardHasher`, the borrowed methods are simply not callable, and the error the
  `H: BuildHasher` bound produces names `BuildHasher` rather than the real cause. Document the
  restriction on each method and in the `ShardHasher` docs.
- **`README.md` is generated from `src/lib.rs`** (`Makefile:344-348`) and CI verifies they match
  (`make check/readme`, `Makefile:158`, `358`). Touching the family-comparison paragraph without
  regenerating fails CI, and the check is sensitive to the `cargo-readme` version CI installs
  (3.4.0 changed fence output and broke `check/readme` with no source change).

## Verification

- `ulimit -v 8000000; cargo test --all-features` (detected `test` command is `cargo test`; CI runs
  the nine feature combinations in `make tests`, `Makefile:226`). The memory-cap prefix is
  mandatory in this repo: a corrupted LRU ring turns a `Vec` push into an OOM.
- `cargo clippy --all-targets` (detected `lint`), `make check` for fmt plus the README check.
- New `tests/v3_sharded_borrowed_key.rs`:
  - `set("a".to_string(), 1)` then `get("a")`, `contains("a")`, `peek("a")`, `remove_entry("a")`,
    `delete("a")`, `remove("a")` on all six stores. Each must fail to compile before the change,
    which is the mutation check for this file.
  - Routing parity, the load-bearing test: for a few thousand generated `String` keys, assert the
    shard chosen for the owned key equals the shard chosen for its `&str` form, on every store.
    The existing test helpers `owning_shard` (`src/stores/sharded/expiring_lru.rs:3707-3709`,
    `ttl.rs:2246`, `lru_ttl.rs:3844`) already compute exactly this index and are the model.
  - The same parity for a `Vec<u8>` / `&[u8]` key pair, which exercises a different `Hash` forward
    than the `String` / `str` pair.
  - Round-trip through the real API rather than just the index: store under the owned key with a
    non-default shard count, read back through every borrowed method, and assert the hit/miss
    counters moved exactly as the owned path moves them (`CacheMetrics`).
- A `trybuild` case under `tests/ui` (the suite already exists) pinning the custom-hasher story: a
  store over a hand-written non-`BuildHasher` `ShardHasher` still compiles for owned lookups and
  produces the expected error for a borrowed one.
- Existing coverage that must stay green rather than be edited:
  `tests/sharded_hasher_accepts_buildhasher.rs`, `tests/v3_sharded_hasher_type_param.rs`,
  `tests/v3_custom_hasher_threaded.rs`, `tests/v3_sharded_lru_read_path.rs`,
  `tests/v3_cert_sharded_lru_semantics.rs`.
- If the ahash `specialize` path is a live worry for a consumer on nightly, the routing-parity
  test is what covers it; there is nothing to configure.

## Notes

- Reported friction: moving a `String`-keyed cache from a single-owner store to a sharded one
  turns every `cache.get("a")` into `cache.get(&"a".to_string())`, allocating on each lookup. The
  crate documents the difference (`src/lib.rs:253-258`) but does not offer a way out. 0044 records
  the sibling report, a `BuildHasher` rejected by a sharded builder, from the same kind of move.
- Related: 0044 (the blanket impl and its coherence exclusivity, which this design depends on),
  0047 (shipped the same `&K` asymmetry into `ConcurrentCacheExpiry`), 0046 (declined to make the
  stored key on overwrite configurable, which is why `set` stays owned-only).
- If this lands, `specs/store-sharded.md` needs a new statement for the borrowed-key methods and
  `specs/traits-concurrent.md:35` ("inherent `contains(&self, &K)` and `peek(&self, &K)`") needs
  correcting.

## Outcome

Implemented as recommended: `get`, `remove`, `remove_entry`, `delete`, `contains`, and `peek`
became generic over `Borrow<Q>` on all six sharded stores, bounded on `H: BorrowedKeyRouting`
(the diagnostic alias for `BuildHasher` proposed above), with one `&Q`-generic core per store that
both the inherent method and the trait method (at `Q = K`) call. `set` and `get_or_set_with` stay
owned-key, as scoped. `specs/store-sharded.md` (SHARD-15) and `specs/traits-concurrent.md`
(CTRAIT-2) were corrected as flagged above, and the crate-doc family comparison
(`src/lib.rs`, near the old `253-258`) was rewritten to state the inherent/trait split precisely.

Two claims in this record's own reasoning did not hold up and are corrected here rather than
silently fixed:

- **"Coherence makes the blanket impl exclusive" overstated what that exclusivity means.**
  Coherence does forbid one type from implementing both `BuildHasher` and a hand-written
  `ShardHasher<K>` (0044), but the record's use of "exclusive" reads as though hand-written
  `ShardHasher` impls were consequently a marginal or unsupported path. They are not: `ShardHasher`
  is a normal, actively documented trait, and its own rustdoc examples (`FibHasher`, the
  `IdentityHasher` anti-pattern warning, `src/stores/sharded/mod.rs`) are written entirely around
  hand-rolled, non-`BuildHasher` implementations as the trait's primary illustrated use. Coherence
  restricts one type from doing both at once; it does not restrict custom shard routing in
  general, and this record should not have implied otherwise.
- **"A custom hasher keeps working for owned lookups" was false for the shape actually shipped.**
  The record proposed one `&Q`-generic method per name (`get<Q>`, etc.) bounded on `H: BuildHasher`
  regardless of `Q`, which is what shipped. Because there is only one method named `get` and its
  bound covers every caller including `Q = K`, a hand-written `ShardHasher` that is not a
  `BuildHasher` loses `get` entirely, not just its borrowed form; there is no separate owned-key
  `get` left to fall back to on the inherent surface. Owned-key access survives only through the
  trait (`ConcurrentCachedExt::get(&cache, &key)`, `ConcurrentCachePeek::peek(&cache, &key)` for
  `peek`), which was already true before this change and is unaffected by it. The CHANGELOG and
  crate doc were written to state this plainly rather than repeat the record's original claim.

Both corrections point the same direction: the `H: BuildHasher` route was chosen anyway, deliberate
tradeoffs and all, because the alternative (relaxing `ShardHasher<K>` to `K: ?Sized` and bounding
on `H: ShardHasher<Q>`) trades a compile-time-enforced restriction for a runtime-silent one - a
cross-impl consistency contract the type system cannot check, whose failure is a phantom miss on
an entry that is actually present. A documented, discoverable compile error for the (assumed
near-zero, one-week-old) custom-router population was judged the better failure mode than a
correctness landmine for everyone else.
