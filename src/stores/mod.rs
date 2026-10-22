/*!
Cache store implementations.

Every store is also re-exported at the crate root (`cached::LruCache`). The macros pick a store
for you; use these types directly when you need a cache outside of a memoized function. Import
`cached::prelude::*` alongside the store, which brings every cache trait into scope:

```rust
use cached::LruCache;
use cached::prelude::*;

let mut cache: LruCache<String, u32> = LruCache::new(100);
cache.set("a".to_string(), 1);
assert_eq!(cache.get("a"), Some(&1));
```

- [Method naming](#method-naming)
- [Store comparison](#store-comparison)
- [Sharded stores](#sharded-stores)
- [Behavioral guarantees](#behavioral-guarantees)
- [Inspection and maintenance](#inspection-and-maintenance)
- [Per-value expiry](#per-value-expiry)
- [Performance notes](#performance-notes)

# Method naming

Every synchronous cache operation has a short alias (`get`/`set`/`remove`/`clear`/`len`/...) and a
`cache_`-prefixed form (`cache_get`/`cache_set`/`cache_remove`/`cache_clear`/`cache_size`/...).
The short aliases are the preferred spelling. Use the `cache_`-prefixed names when a short alias
would collide with another in-scope trait's method of the same name (for example, your type also
implements a trait with its own `get`).

The `get`/`set`/`remove` short aliases for `Cached` stores live on `CachedExt`; those for
`ConcurrentCached` stores live on `ConcurrentCachedExt`. Both extension traits have blanket
implementations, so the short names are always available when the extension trait is in scope.
The simplest way to get them is `use cached::prelude::*;`, which re-exports both extension traits.
Alternatively, import them directly: `use cached::{Cached, CachedExt};`. Custom store
implementations only need to implement the `cache_`-prefixed required methods on the core trait;
the short aliases come for free via the blanket extension trait impl.

For `Cached` stores, `len`/`is_empty` are also on `CachedExt`. For `ConcurrentCached` stores,
size introspection is `cache_size` and `cache_is_empty` on `ConcurrentCacheBase` (the shared base
trait), not on `ConcurrentCachedExt`: bring `ConcurrentCacheBase` into scope to call them on a
generic bound. Note that `ConcurrentCacheBase::cache_size` returns `Result<Option<usize>, _>`:
fallible for IO-backed stores, and `None` when the backend cannot report an exact count. The sharded
stores keep their inherent infallible `len`/`is_empty` too, which take priority at the call site.

Both async traits use the `async_cache_*` spelling. `ConcurrentCachedAsync` mirrors the sync
`ConcurrentCached` surface (`async_cache_get`, `async_cache_set`, `async_cache_remove`, ...) for
concurrent stores that manage their own synchronization (the in-memory sharded stores, plus the
IO-backed redis and redb stores). `CachedGetOrSetAsync` is narrower: it
only memoizes an async closure over a synchronous in-memory `Cached` store, via the
`async_cache_get_or_set_with` family (`async_cache_get_or_set_with`,
`async_cache_try_get_or_set_with`, and their `_mut` variants).

`ConcurrentCachedAsync` has its own deduplicated aliases on `ConcurrentCachedAsyncExt`
(`async_get`, `async_set`, `async_remove`, `async_remove_entry`, `async_delete`,
`async_contains`, `async_clear`, `async_reset`, `async_get_or_set_with`,
`async_try_get_or_set_with`). They keep the `async_` prefix instead of being bare `get`/`set`,
because the stores implementing `ConcurrentCachedAsync` also implement the synchronous
`ConcurrentCached`: bare names would be a second applicable candidate alongside
`ConcurrentCachedExt` and make `store.get(&k)` ambiguous. Size and metric introspection is not
aliased there; it is `cache_size` / `cache_is_empty` and the metric readers on
`ConcurrentCacheBase`, which are callable on an async store with no extension trait imported.
`CachedGetOrSetAsync` has no alias trait; its `async_` prefix already prevents collisions with
the sync methods.

# Store comparison

| Store | Eviction policy | Size limit | TTL | Refresh on hit | `on_evict` | Concurrent | Async |
|---|---|---|---|---|---|---|---|
| [`UnboundCache`](https://docs.rs/cached/latest/cached/struct.UnboundCache.html) | None (unbounded) | No | No | N/A | On explicit remove | No | Yes |
| [`LruCache`](https://docs.rs/cached/latest/cached/struct.LruCache.html) | LRU | Yes | No | N/A | Yes | No | Yes |
| [`TtlCache`](https://docs.rs/cached/latest/cached/struct.TtlCache.html) | TTL (insert time) | No | Global | Optional | Yes | No | Yes |
| [`LruTtlCache`](https://docs.rs/cached/latest/cached/struct.LruTtlCache.html) | LRU + TTL | Yes | Global | Optional | Yes | No | Yes |
| [`TtlSortedCache`](https://docs.rs/cached/latest/cached/struct.TtlSortedCache.html) | TTL (expiry-ordered) | Optional | Global | No | Yes | No | Yes |
| [`ExpiringLruCache`](https://docs.rs/cached/latest/cached/struct.ExpiringLruCache.html) | LRU + value-defined | Yes | Per-value | N/A | Yes | No | Yes |
| [`ExpiringCache`](https://docs.rs/cached/latest/cached/struct.ExpiringCache.html) | Value-defined | No | Per-value | N/A | Yes | No | Yes |
| [`ShardedUnboundCache`](https://docs.rs/cached/latest/cached/struct.ShardedUnboundCache.html) | None (unbounded) | No | No | N/A | On explicit remove | Yes (`Arc`) | Yes |
| [`ShardedLruCache`](https://docs.rs/cached/latest/cached/struct.ShardedLruCache.html) | LRU | Yes | No | N/A | Yes | Yes (`Arc`) | Yes |
| [`ShardedTtlCache`](https://docs.rs/cached/latest/cached/struct.ShardedTtlCache.html) | TTL (insert time) | No | Global | Optional | Yes | Yes (`Arc`) | Yes |
| [`ShardedLruTtlCache`](https://docs.rs/cached/latest/cached/struct.ShardedLruTtlCache.html) | LRU + TTL | Yes | Global | Optional | Yes (†) | Yes (`Arc`) | Yes |
| [`ShardedExpiringCache`](https://docs.rs/cached/latest/cached/struct.ShardedExpiringCache.html) | Value-defined | No | Per-value | N/A | Yes | Yes (`Arc`) | Yes |
| [`ShardedExpiringLruCache`](https://docs.rs/cached/latest/cached/struct.ShardedExpiringLruCache.html) | LRU + value-defined | Yes | Per-value | N/A | Yes | Yes (`Arc`) | Yes |

> "On explicit remove" — `on_evict` fires only on `cache_remove`; there is no capacity eviction or TTL expiry trigger for these stores.
> † `ShardedLruTtlCacheBuilder::on_evict` requires `K: 'static + V: 'static`; see the builder docs for details.

`TtlCache`/`LruTtlCache`/`TtlSortedCache`/`ShardedTtlCache`/`ShardedLruTtlCache` require the `time_stores` feature.

# Sharded stores

## Shard count and capacity

`ShardedUnboundCache` and its variants are partitioned across power-of-two shards, each protected by a `parking_lot::RwLock`. The default shard count is derived in two steps:

- The host default is `available_parallelism() × 4`, clamped to 8–1024 and rounded up to a power of two. It is sampled once per process and reused by every cache built afterward.
- On the default path, the three LRU-bounded sharded stores (`ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache`) scale that down to match a total `max_size`: the count is `next_power_of_two(max_size / 16)`, clamped into `[1, host_default]`. This keeps each shard holding roughly 16 entries instead of preallocating an oversized shard array for a small cache — e.g. `ShardedLruCache::new(100)` builds 8 shards (`100 / 16 = 6` → `8`) with a total capacity of 128 (the 16-per-shard floor below), rather than one shard per host default.

Everything else keeps the plain host default: the unbounded stores (`ShardedUnboundCache`, and `ShardedTtlCache` / `ShardedExpiringCache` built without a `max_size`), the builder's `per_shard_max_size` path, and any explicit `shards = N` / `.shards(n)`. An explicit shard count is rounded up to a power of two but never clamped.

Shard structs are padded to 128-byte alignment (covering Intel adjacent-line prefetch and Apple Silicon 128-byte L1 lines) to eliminate false sharing; on a 64-shard deployment this amounts to ~8 KB of padding overhead per cache array. The outer type is an `Arc` — cloning is a reference share, not a deep copy (use `deep_clone()` for an independent copy; note that `deep_clone()` is an inherent method on each concrete sharded type, not part of any trait). They implement `ConcurrentCached`/`ConcurrentCachedAsync` and are the default store selected by `#[concurrent_cached]`.
For sharded LRU variants, eviction is enforced independently per shard. `max_size = N` is divided across shards with ceiling division. Use the builder's `per_shard_max_size` method for an exact per-shard cap (builder-only; `#[concurrent_cached]` does not expose a `per_shard_max_size` attribute — use `shards` to control parallelism and `max_size` for total capacity). **Capacity Fragmentation Warning**: To protect against premature evictions due to hash collisions in extremely small caches (where a shard capacity could drop to 1-2 entries), when sharding is active (`shards > 1`) we enforce a minimum capacity of `16` entries **per shard** (e.g., minimum total capacity of `128` on a single-core machine with 8 shards, or `256` on a 4-core machine with 16 shards). If you require smaller, strict limits under low capacities, configure `shards = 1` or specify `per_shard_max_size` directly (builder-only; not available via `#[concurrent_cached]`).
Because LRU caches require updating access recency, `ShardedLruCache`, `ShardedLruTtlCache`, and `ShardedExpiringLruCache` must acquire an exclusive **write lock** on accessed shards during read hits, which can lead to contention under highly concurrent read-heavy workloads. Unbounded `ShardedUnboundCache`, time-only `ShardedTtlCache` (when `refresh_on_hit` is disabled -- enabling it promotes read hits to exclusive write locks), and expiring `ShardedExpiringCache` require only a **shared read lock** on read hits, avoiding this contention. To mitigate contention on LRU variants, consider increasing the number of `shards` to distribute writes. Note: this write-lock-on-read behavior is a known limitation of the strict-LRU sharded stores. A future read-optimized variant that relaxes strict recency ordering will ship as a separate store type; the existing stores will not change semantics.

## Borrowed-key lookups and custom shard hashers

On the default hasher, the six sharded stores' inherent lookups accept any borrowed form of the
key with no allocation, so a `String`-keyed store reads with a plain `&str`:

```rust
use cached::ShardedLruCache;

let sharded: ShardedLruCache<String, u32> = ShardedLruCache::new(10);
sharded.set("a".to_string(), 1);
assert_eq!(sharded.get("a"), Some(1));
```

> **Custom shard hashers:** Every sharded store carries a third, defaulted type parameter for its [`ShardHasher`] — `ShardedUnboundCache<K, V, H = DefaultShardHasher>`, `ShardedLruCache<K, V, H = DefaultShardHasher>`, and so on — mirroring `std::collections::HashMap<K, V, S = RandomState>`. Writing `ShardedLruCache<K, V>` therefore gets the default hasher, which is what most users want; name the third parameter only when routing keys through a custom `ShardHasher`. Construct such a cache through the builder's `hasher` method: `ShardedLruCache::builder().hasher(my_hasher)` switches the builder's hasher type and `build` yields a `ShardedLruCache<K, V, H>` over `my_hasher`. `new`/`builder` are defined only on the default-hasher instantiation, so a custom hasher is always introduced through `hasher`, never a `ShardedLruCache::<_, _, H>` turbofish (which would otherwise silently drop the hasher).
>
> A hand-written `ShardHasher` that does not also implement `BuildHasher` keeps the six inherent `get`/`remove`/`remove_entry`/`delete`/`contains`/`peek` lookups at every key type it implements: each one is bounded on `H: ShardHasher<Q>` for the `Q` being looked up, so a single `impl ShardHasher<K> for MyRouter` covers the owned-key calls (`cache.get(&key)`). Borrowed forms are opt-in — writing a second `impl ShardHasher<str>` alongside `ShardHasher<String>` is what enables `cache.get("a")` on a `ShardedLruCache<String, V, MyRouter>`. Multiple impls on one router must agree on keys that compare equal (for `K: Borrow<Q>`, `shard_hash(&k)` must equal `shard_hash(k.borrow())`); the compiler cannot check that, and disagreement routes an owned insert and its equivalent borrowed lookup to different shards, producing a miss on an entry that is present. A `BuildHasher` that is also `Clone + Send + Sync + 'static` reaches every key type through the blanket `ShardHasher` impl, where `Borrow`'s own hash-agreement contract makes this automatic. How a missing impl is reported depends on how many candidates `Q` can collapse onto, counting both the router's own impls and any `ShardHasher` bounds in scope: with exactly one, inference collapses `Q` onto it and the failure is a call-site `E0308` type mismatch (`expected &UserId, found &u64`) rather than a missing-bound error. That covers a single-impl concrete router and, just as often, a generic helper bounded only on `H: ShardHasher<String>` calling `c.get("a")`. With two or more, `Q` has nothing to collapse onto and the failure is `E0277` with `ShardHasher`'s diagnostic notes (`NameRouter cannot route keys of type str to a shard`). The [`ShardHasher`] docs spell both out. The trait forms remain available for owned keys on any hasher: `ConcurrentCachedExt::get(&cache, &key).unwrap()` (and the matching `remove`/`remove_entry`/`delete`/`contains`), plus `ConcurrentCachePeek::peek(&cache, &key).unwrap()` for `peek` (`ConcurrentCachedExt` has no `peek` of its own). Both trait forms return `Result<_, Infallible>`, hence the `.unwrap()`.
>
> Naming that third type parameter in your own generic helpers has a sharp edge, but it is not the hasher bound: a helper written as `fn lookup<K, V, H: ShardHasher<K>>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V> { c.get(k) }` still fails to compile **at its own definition**, regardless of which hasher any call site uses, because the inherent `get` lives in an `impl` block that also requires `K: Hash + Eq + Clone` and `V: Clone`. rustc surfaces that as E0599 first, reported as "the method `get` exists for reference `&ShardedLruCache<K, V, H>`, but its trait bounds were not satisfied" and followed by the four bounds it is missing (`K: Hash`, `K: Eq`, `K: Clone`, `V: Clone`), so read the error as a bounds list rather than a missing method; `tests/ui/sharded_helper_missing_key_bounds.stderr` pins the exact text. The working signature is `fn lookup<K, V, H>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V> where K: Hash + Eq + Clone, V: Clone, H: ShardHasher<K> { c.get(k) }`: `H: ShardHasher<K>` is exactly the bound the owned-key `get` carries, and no additional marker trait is involved. A helper that looks keys up in a *borrowed* form names that form **in addition to** the store's key type, not instead of it: `fn lookup_borrowed<V, H>(c: &ShardedLruCache<String, V, H>, k: &str) -> Option<V> where V: Clone, H: ShardHasher<String> + ShardHasher<str> { c.get(k) }`, or `H: ShardHasher<K> + ShardHasher<Q>` with the borrowed form left generic (`K: Hash + Eq + Clone + Borrow<Q>`, `Q: Hash + Eq + ?Sized`). That `impl` block is itself bounded on `H: ShardHasher<K>`, so a borrowed call adds `ShardHasher<Q>` on top of the store's own hasher bound rather than replacing it. Leaving the borrowed half out is the confusing failure: with only `H: ShardHasher<String>`, `c.get("a")` fails as an E0308 argument mismatch (`expected &String, found &str`) rather than a missing-impl error, because `Q` collapses to the key type before any bound is checked.

## Double-reference lookups do not compile

The same inherent-method resolution
breaks argument inference for callers that used to pass a double-reference, and this is the shape
most likely to hit a user who never wrote a custom `ShardHasher` at all. Before this crate's
borrowed-key routing existed, `get` took `&K`, so `for k in &keys { cache.get(&k) }` on a
`ShardedLruCache<String, _>` compiled with `k: &String` through plain deref coercion of `&&String`
to `&String`. `get` is now generic over the looked-up form (`&Q` with `K: Borrow<Q>`), and
inference fills `Q` in from the argument type before any coercion can apply, so the same call
infers `Q = &String` and fails with "the trait bound `String: Borrow<&String>` is not satisfied":
a plain `E0277` that gives no "remove the extra `&`" hint, because the fix is not a missing bound
but an extra reference at the call site. Dropping that `&` fixes this shape (`cache.get(k)`
instead of `cache.get(&k)`). Any other indirection the old code let deref coercion paper over
fails the same way, but with no extra `&` to remove, so the deref has to be written out: with
`k: &Box<String>` (or `&Arc<String>`), `cache.get(k)` infers `Q = Box<String>` and fails on
`String: Borrow<Box<String>>`, and the call becomes `cache.get(&**k)` (or `cache.get(k.as_str())`).
In every case the goal is the same, that `Q` infers as a form the stored key actually borrows to
rather than a wrapper around it. The same shape applies to `remove`, `remove_entry`, `delete`,
`contains`, and `peek`.

## Inherent methods shadow the trait methods

The six sharded stores expose inherent `set` / `get` / `len` / `contains` / `peek` that return
unwrapped values (`Option<V>`, `usize`, `bool`), and inherent methods take call-site priority over
the `ConcurrentCached` / `ConcurrentCachedExt` trait methods of the same name, which return
`Result<_, Self::Error>`. That priority is deliberate — the sharded stores are infallible, so the
bare spelling is the useful one — but it has one sharp edge worth stating plainly:

`s.set(k, v).unwrap()` compiles. It resolves to `Option::unwrap` on the *displaced value*, not to
`Result::unwrap` on a store error, so it **panics on the first insert for a key**, where there is
no displaced value to return. The same trap applies to `s.get(&k).unwrap()` and
`s.peek(&k).unwrap()` on a miss.

Drop the `.unwrap()` to use the inherent method; use fully-qualified syntax to reach the trait
method:

```rust
use cached::{ConcurrentCachedExt, ShardedUnboundCache};

let s: ShardedUnboundCache<u32, u32> = ShardedUnboundCache::new();

// Inherent, infallible: returns the displaced value, so `None` on a first insert.
assert_eq!(s.set(1, 10), None);
assert_eq!(s.get(&1), Some(10));

// Trait, fallible: `Result<Option<V>, Infallible>` — the `unwrap` is on the Result.
assert_eq!(ConcurrentCachedExt::set(&s, 2, 20).unwrap(), None);
assert_eq!(ConcurrentCachedExt::get(&s, &2).unwrap(), Some(20));
```

# Behavioral guarantees

- Non-sharded in-memory stores (`UnboundCache`, `LruCache`, `TtlCache`, etc.) are not internally
  synchronized. Macro-generated `#[cached]`/`#[once]` functions wrap them in locks; users
  managing these stores directly must add their own synchronization when sharing across threads.
  `Sharded*` stores are internally synchronized (per-shard `parking_lot::RwLock`) and implement
  `ConcurrentCached`/`ConcurrentCachedAsync` — no external lock is needed.
  The synchronous `get` / `set` / `remove` short aliases come from the `ConcurrentCachedExt`
  extension trait (bring it into scope with `use cached::prelude::*;` or
  `use cached::{ConcurrentCached, ConcurrentCachedExt};`); the `cache_get` / `cache_set` /
  `cache_remove` spellings come from `ConcurrentCached` directly. For sharded stores, inherent
  methods with the same names take priority at the call site. The async trait operations are
  `async_`-prefixed, so they never collide (e.g., `STORE.async_cache_get(&key).await.expect("ShardedUnboundCache is infallible")`).
- `CachedExt::get` (and the `Cached::cache_get` required method it wraps) requires mutable access
  because some stores update recency, expiration timestamps, or metrics during reads.
- **`cache_get`/`cache_set` shapes differ per family, by design.** Single-owner: `Cached::cache_get`
  is `&mut self -> Option<&V>` and `Cached::cache_set` returns `Option<V>` (the displaced value).
  Concurrent: `ConcurrentCached::cache_get` is `&self -> Result<Option<V>, Error>` (owned value,
  fallible) and `ConcurrentCached::cache_set` returns a `#[must_use] Result<Option<V>, Error>`.
  The concurrent family returns owned values because its implementors include IO stores that
  serialize entries and cannot hand out a borrow into the store, and it is fallible because those
  stores can fail; the single-owner family stays infallible and borrow-returning. Lookup keys
  differ the same way on the trait: single-owner `Cached::cache_get` accepts any borrowed form of
  the key (`&Q where K: Borrow<Q>`, so `cache.get("a")` works on an `LruCache<String, _>`), while
  `ConcurrentCached::cache_get` takes `&K` exactly, because its IO-store implementors
  (`RedisCache`, `RedbCache`) serialize the full key and a generic `&Q` carries no serialization
  guarantee. The six sharded stores' own **inherent** `get`/`remove`/`remove_entry`/`delete`/
  `contains`/`peek` are the exception: they accept any borrowed form of the key too
  (`sharded.get("a")` works on a `ShardedLruCache<String, _>` with no allocation), bounded on
  [`H: ShardHasher<Q>`](https://docs.rs/cached/latest/cached/trait.ShardHasher.html) — the bound
  names the looked-up form, not the stored key. `ShardHasher` carries
  `Clone + Send + Sync + 'static` as supertraits, and every `BuildHasher` meeting those gets a
  blanket `ShardHasher<Q>` impl for every `Q: Hash`, so the default hasher and any such
  `BuildHasher`-based one reach all borrowed forms, with owned- and borrowed-key routing
  agreement guaranteed by the `Borrow` contract. A `BuildHasher` missing any of those
  supertraits (a non-`Clone` one, say) falls outside the blanket impl and is reported as a
  missing `ShardHasher` impl. A hand-written, non-`BuildHasher` `ShardHasher` keeps these six inherent
  methods at each key type it implements (`impl ShardHasher<K>` alone covers the owned-key
  calls) and opts into borrowed forms with a further `impl ShardHasher<Q>` that must agree with
  the first on keys that compare equal. Where a lookup form is unsupported there is **no
  method-resolution fallback**: the inherent method is selected by name first and then fails, so
  importing a trait does not rescue the call at the same call site. The replacement is the trait
  form, e.g. `ConcurrentCachedExt::get(&cache, &key).unwrap()` (and the matching
  `remove`/`remove_entry`/`delete`/`contains`), and
  `ConcurrentCachePeek::peek(&cache, &key).unwrap()` for `peek`, since `ConcurrentCachedExt` has
  no `peek`. `set` and `get_or_set_with` stay owned-key on every hasher, since they insert the
  key rather than look it up. A prelude glob can bring both families into scope without
  collision.
- **The inherent-method asymmetry between the two families is deliberate.** On a sharded store the
  short `set`/`get`/`len` calls resolve to *inherent* methods (infallible, `&self`), so
  `ShardedLruCache::new(100)` is usable bare. A single-owner `LruCache::new(100)` has no such
  inherent shims: `c.set`/`c.get`/`c.len` need the `CachedExt` extension trait in scope
  (`use cached::CachedExt;`, or the prelude). This mirrors each family's ownership model (sharded
  stores are self-synchronized and infallible; single-owner stores are `&mut self`) and is not an
  oversight. **Sharp edge:** because inherent methods win over trait methods at the call site, a
  `.unwrap()` you write on a sharded store does not mean what it means on the trait — see
  [Inherent methods shadow the trait methods](#inherent-methods-shadow-the-trait-methods).
- **`len` / `size` vs `iter` vs `evict` contract for timed and expiring stores:**
  `len()` (and `cache_size()`, `is_empty()`) return the raw stored entry count without
  scanning for expiry. On lazy-eviction stores (`TtlCache`, `LruTtlCache`,
  `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`, and their sharded equivalents)
  this count may include entries that have expired but not yet been swept, so
  `len()` can be greater than `iter().count()`. `iter()` (from [`CachedIter`]) omits
  expired entries from the yielded view but does not remove them from the store - it
  stays `&self`. Call `evict()` (via [`CacheEvict`] for single-owner stores or
  [`ConcurrentCacheEvict`] for sharded stores) to physically remove expired entries,
  reclaim memory, and obtain an accurate live count.
- Expired values can remain allocated until a mutating operation, `evict`, or
  store-specific cleanup removes them.
- `cache_remove` fires the `on_evict` callback (if set) and counts as an eviction for
  every successful removal, across all stores that track evictions. The unbounded
  non-expiring stores (`UnboundCache`, `ShardedUnboundCache`) are the exception: they have
  no evictions counter and always return `None` from
  `metrics().evictions`, though their `on_evict` callback still fires. The `on_evict` column
  above marks the unbounded stores where explicit removal is the *only* eviction trigger. For stores with
  expiry, removing a present-but-already-expired entry still evicts and fires `on_evict`,
  but `cache_remove` returns `None`; use `cache_delete` or `cache_remove_entry` when you
  need to know whether an entry was physically removed.
- `cache_clear()` is fast and side-effect-free: it does **not** fire `on_evict` and does
  not increment the evictions counter. Use `cache_clear_with_on_evict()` when you need the
  callback to fire for every removed entry (e.g., to release resources tracked via `on_evict`).
  Note: `cache_clear` is a required method on `ConcurrentCached` (and `async_cache_clear` on
  the async counterpart), with the short `clear()` alias on `ConcurrentCachedExt`, so generic
  code over `ConcurrentCached` can clear. `cache_clear_with_on_evict()` has its own trait per
  receiver family,
  [`CacheClearWithOnEvict`](https://docs.rs/cached/latest/cached/trait.CacheClearWithOnEvict.html)
  (`&mut self`) and
  [`ConcurrentCacheClearWithOnEvict`](https://docs.rs/cached/latest/cached/trait.ConcurrentCacheClearWithOnEvict.html)
  (`&self`), implemented by every in-memory store that has an
  `on_evict` callback: all seven single-owner stores and all six sharded ones. The concrete types
  keep the inherent method, which takes call-site priority; the traits only add the route through
  a generic bound. The Redis and redb stores have no `on_evict` mechanism and implement neither.
- Bounded caches enforce capacity on insertion. Time-bounded caches enforce freshness on lookup.
- Redis and disk stores serialize values and return owned values. Non-sharded in-memory stores
  return references from direct store APIs; sharded stores return owned `Option<V>` values
  (cloned under a shard lock). Macro-generated functions clone cached return values in all cases.
- Macro-generated `#[cached]` / `#[once]` cache statics use `RwLock` by default. Named cache
  statics for those macros should be inspected with `.read()` or `.write()`. `#[cached]` can
  switch to a `Mutex` with `sync_lock = "mutex"`; `#[once]` does not accept `sync_lock` and is
  always `RwLock`. Named `#[concurrent_cached]` statics hold a self-synchronizing
  store directly: sync functions use `LazyLock<Store>`, and async functions use
  `OnceCell<Store>`.
- `CachedPeek` provides non-mutating lookups that do not update recency, refresh TTLs, or record
  metrics. `CachedRead` is narrower and is only implemented where shared-lock lookups can preserve
  normal read-side semantics without recency or refresh mutation.
- [`CacheExpiry`](crate::CacheExpiry) is the per-key expiry read for single-owner stores: `cache_peek_expires_at`
  (alias `peek_expires_at`) returns `(Option<V>, Option<Instant>)`, the value plus the instant it
  expires at, so callers can refresh when the remaining TTL drops below a threshold.
  `cache_expires_at` (alias `expires_at`) is the value-free form, returning
  `(bool, Option<Instant>)`: presence plus the same instant, with no clone and no `V: Clone` bound,
  so it reads a deadline out of a cache whose value type is not `Clone`. The presence flag keeps an
  absent key (`(false, None)`) distinct from a present entry that never expires (`(true, None)`).
  Both carry the same no-side-effect contract as `cache_peek_with_expiry_status` and are
  implemented by the expiry-capable single-owner stores
  ([`TtlCache`], [`LruTtlCache`], [`TtlSortedCache`],
  [`ExpiringCache`], [`ExpiringLruCache`]). On the `Expires`-based stores the instant is advisory
  (it is `None` unless the value type overrides [`Expires::expires_at`], and `is_expired` stays the
  authority); the TTL stores report a real deadline. The Redis and redb stores do not implement it.
  The macro configurations that produce a supporting store are `ttl_secs` / `ttl` / `ttl_millis`
  (with or without `max_size`) and `expires = true`; on any other configuration the call fails to
  compile with an `E0599` that names the guard type rather than the missing trait. See the
  [`refresh_before_expiry`](https://github.com/jaemk/cached/blob/master/examples/refresh_before_expiry.rs)
  example for a runnable threshold-refresh recipe over both traits.
- [`CacheSetMaxSize`](https://docs.rs/cached/latest/cached/trait.CacheSetMaxSize.html) is the
  capacity resize for the bounded single-owner stores
  ([`LruCache`], [`LruTtlCache`], [`ExpiringLruCache`], [`TtlSortedCache`]): `set_max_size`
  returns the previous bound, `try_set_max_size` returns
  [`SetMaxSizeError::ZeroMaxSize`](https://docs.rs/cached/latest/cached/enum.SetMaxSizeError.html#variant.ZeroMaxSize)
  instead of panicking on a zero bound, and shrinking evicts eagerly, firing `on_evict` and
  counting an eviction per removed entry.
  [`ConcurrentCacheSetMaxSize`](https://docs.rs/cached/latest/cached/trait.ConcurrentCacheSetMaxSize.html)
  is the `&self` mirror on [`ShardedLruCache`], [`ShardedLruTtlCache`], and
  [`ShardedExpiringLruCache`], which add
  [`SetMaxSizeError::CapacityOverflow`](https://docs.rs/cached/latest/cached/enum.SetMaxSizeError.html#variant.CapacityOverflow)
  for a bound that overflows when split across shards, and which round the requested total up to a
  multiple of the shard count (further up to the 16-per-shard floor on a multi-shard store): the
  bound `set_max_size`/`try_set_max_size` installs, and the previous-bound value they return, are
  both rounded totals rather than the requested number. Nothing in the return value reports the
  rounding, so `try_set_max_size(4)` on a 16-shard cache returns `Ok(Some(previous))` having
  installed a 256-entry bound; read `cache_capacity` afterwards to see the bound actually in force.
  The unbounded and time-only stores ([`UnboundCache`], [`TtlCache`], [`ExpiringCache`], and
  their sharded forms) have no live bound and implement neither trait, so the bound is a compile
  error rather than a silent no-op. Reading the bound needs no extra trait on a concrete store;
  generic code that also reads the bound needs `T: Cached<K, V>` or `T: ConcurrentCacheBase`
  alongside `CacheSetMaxSize`/`ConcurrentCacheSetMaxSize`, since `CacheSetMaxSize` itself is
  un-parameterized and does not carry `cache_capacity`. `cache_capacity` is already a method on
  [`Cached`] and [`ConcurrentCacheBase`](crate::ConcurrentCacheBase) (defaulted to `None`, overridden by every bounded store).
- Sharded stores implement `ConcurrentCached`/`ConcurrentCachedAsync` instead of
  `Cached`/`CachedGetOrSetAsync`. Generic code parameterized over `Cached<K, V>` cannot accept sharded
  stores; use a `ConcurrentCached<K, V>` bound or a concrete type instead.
  Sharded stores do not implement the single-owner `CachedIter` or `CachedPeek` traits; code that
  is generic over `CachedIter<K, V>` or uses `.iter()` must use a non-sharded store. They do,
  however, provide a side-effect-free read via the [`ConcurrentCachePeek`](crate::ConcurrentCachePeek) trait (`cache_peek`,
  returning an owned `Option<V>`), with the inherent `peek` shim taking call-site priority on the
  concrete sharded types. `ConcurrentCachePeekAsync` is the async mirror of that trait
  (`async_cache_peek`, with an `async_peek` alias); it carries the identical no-recency,
  no-TTL-refresh, no-metrics, no-lazy-expiry contract and is deliberately not implemented by the
  IO stores.
  The four expiry-capable sharded stores ([`ShardedTtlCache`], [`ShardedLruTtlCache`],
  [`ShardedExpiringCache`], [`ShardedExpiringLruCache`]) implement [`ConcurrentCloneCached`](crate::ConcurrentCloneCached),
  which provides `cache_get_with_expiry_status` for reading stale entries without evicting them, and
  `cache_peek_with_expiry_status` as a side-effect-free counterpart (a read with no hit/miss
  counting, LRU promotion, or TTL renewal). The same four stores implement
  [`ConcurrentCacheExpiry`](crate::ConcurrentCacheExpiry), the concurrent counterpart of [`CacheExpiry`](crate::CacheExpiry), whose
  `cache_peek_expires_at` returns `(Option<V>, Option<Instant>)` and whose value-free
  `cache_expires_at` returns `(bool, Option<Instant>)`, both under the same no-side-effect
  contract (advisory instant on the two `Expires`-based stores, a real deadline on the two TTL
  stores). The Redis and redb stores implement neither. `#[concurrent_cached]` selects a supporting
  store under the same configurations as `#[cached]` (`ttl_secs` / `ttl` / `ttl_millis`, with or
  without `max_size`, and `expires = true`). Unlike `set` / `get` / `len` / `contains` / `peek`,
  neither expiry read has an inherent shim on the sharded types, so both need
  `use cached::ConcurrentCacheExpiry;` in scope.

`cache_get_or_set_with` returns a shared reference (`&V`); binding it as `&mut V`
does not compile. Use [`cache_get_or_set_with_mut`](crate::Cached::cache_get_or_set_with_mut)
when you need a mutable reference.

```compile_fail
// This does NOT compile: cache_get_or_set_with returns &V, not &mut V.
use cached::{Cached, UnboundCache};

let mut cache: UnboundCache<u32, u32> = UnboundCache::builder().build().unwrap();
let _: &mut u32 = cache.cache_get_or_set_with(1, || 2);
```

# Inspection and maintenance

`retain` filters a cache in place and returns the number of entries removed. It is inherent on all
seven single-owner stores (`&mut self`) and all six sharded stores (`&self`). On the expiry-aware
stores it also drops entries that are already expired, regardless of what the predicate returned,
so the count is information only the store has. Every removal fires `on_evict` and counts as an
eviction. On the sharded stores the sweep locks one shard at a time and is not atomic across
shards.

```rust
use cached::{CachedExt, LruCache};

let mut c: LruCache<u32, u32> = LruCache::new(10);
c.set(1, 10);
c.set(2, 21);
c.set(3, 30);
let removed = c.retain(|_k, v| v % 2 == 0); // keep even values
assert_eq!(removed, 1);
assert_eq!(c.len(), 2);
```

`contains` answers "is there a live entry for this key?" without cloning the value. It is
`Cached::cache_contains` / `CachedExt::contains` on the single-owner side (`&mut self`, accepts any
borrowed key form) and `ConcurrentCached::cache_contains` / `ConcurrentCachedExt::contains` on the
concurrent side (`&self`, `Result<bool, Self::Error>`). Neither carries a `V: Clone` bound, and the
concurrent one is object-safe, so it is callable through `dyn ConcurrentCached`. The built-in
stores implement it peek-based: no hit/miss metrics, no LRU promotion, no TTL refresh, and an
expired entry reports `false`.

```rust
use cached::{CachedExt, LruCache};

let mut c: LruCache<&str, u32> = LruCache::new(10);
c.set("a", 1);
assert!(c.contains("a"));
assert!(!c.contains("b"));
assert_eq!(c.metrics().hits, Some(0), "contains does not count a hit");
```

The LRU-family stores (`LruCache`, `LruTtlCache`, `ExpiringLruCache`) expose recency-ordered
snapshots: `key_order()`, `value_order()`, and `iter_order()`, each most-recently-used first.
The value-bearing two return [`CacheValue`]`<V, M>`, a wrapper that `Deref`s to `V` and carries
per-entry metadata — `M = ()` for `LruCache` / `ExpiringLruCache`, and `M = Option<Instant>` for
`LruTtlCache`, whose entries expose `expires_at()`. When you only want the values, the
[`IntoValues`] extension trait unwraps either shape in one call: `.into_values()` turns
`Vec<CacheValue<V, M>>` or `Vec<(K, CacheValue<V, M>)>` into a plain `Vec<V>`, order preserved.

```rust
use cached::{CacheValue, CachedExt, IntoValues, LruCache};

let mut c: LruCache<&str, u32> = LruCache::new(10);
c.set("a", 1);
c.set("b", 2);
let _ = c.get("a"); // promote "a"

assert_eq!(c.key_order(), vec!["a", "b"]);
// `CacheValue` derefs to `V` and compares against a bare value.
let values: Vec<CacheValue<u32>> = c.value_order();
assert_eq!(*values[0], 1);
assert_eq!(values[0], 1);
assert_eq!(c.iter_order().len(), 2);

// Bulk-unwrap either shape into plain values.
assert_eq!(c.value_order().into_values(), vec![1, 2]);
assert_eq!(c.iter_order().into_values(), vec![1, 2]);
```

[`TtlSortedCache::set_with`] starts a builder-style insert with a per-entry TTL override and an
opt-in expiry sweep, terminated by `.set()`. Plain `set` uses the cache's default TTL and never
runs the sweep (size-limit enforcement is unaffected by either).

```rust
# #[cfg(feature = "time_stores")]
# {
use cached::{CachedExt, TtlSortedCache};
use cached::time::Duration;

let mut c: TtlSortedCache<&str, u32> =
    TtlSortedCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
c.set("default-ttl", 1);
// Per-entry TTL override plus an expiry sweep on the way in.
let displaced = c.set_with("short", 2).ttl(Duration::from_millis(1)).evict().set();
assert_eq!(displaced, None);
assert_eq!(c.len(), 2);
# }
```

# Per-value expiry

While standard timed stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`) enforce a single, global Time-To-Live (TTL) duration applied to all entries in the cache, [`ExpiringLruCache`] and [`ExpiringCache`] let each individual value determine its own expiration. This is accomplished by storing values that implement the [`Expires`] trait.

This approach is highly useful when caching payloads like OAuth tokens, HTTP responses with varying `Cache-Control` headers, or database records that contain their own absolute expiration timestamps.

It is also the idiomatic way to give entries a **dynamic, per-entry TTL** — a lifetime computed at call time rather than the single uniform duration that `ttl = N` applies to every entry. Because the value carries its own expiry, each entry can be given a different lifetime derived from a function argument, runtime configuration, or a response header. (`expires = true` is mutually exclusive with `ttl`.) See the [`expires_per_key`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs) example for a runnable demonstration.

When using the `#[cached]` or `#[once]` proc macros, add `expires = true` to opt into per-value expiry automatically. For `#[cached]`, this selects `ExpiringCache` (unbounded) by default or `ExpiringLruCache` when `max_size` is also specified. For `#[once]`, this stores a single value whose expiry is polled on each call.

Implement [`Expires::expires_at`] too if the entry's deadline should be readable. It defaults to
`None`, and a value type that implements only `is_expired` makes the per-key expiry read
([`CacheExpiry`](crate::CacheExpiry) / [`ConcurrentCacheExpiry`](crate::ConcurrentCacheExpiry)) return `(Some(v), None)` (or `(true, None)` from the
value-free `cache_expires_at`) for live and expired entries alike, so a remaining-TTL policy built
on that read never fires and never reports an error. `is_expired` stays the liveness authority on
these stores either way.

The macro form below derives each entry's TTL from a function argument — `key`/`convert` keep the TTL out of the cache key so it influences only the entry's lifetime, not which slot it occupies (the [`expires_per_key`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs) example uses the same pattern):

```rust,no_run
# #[cfg(feature = "proc_macro")]
# #[allow(dead_code)]
# mod example {
use cached::macros::cached;
use cached::Expires;
use cached::time::{Duration, Instant};

#[derive(Clone)]
struct Token { value: String, expires_at: Instant }

impl Expires for Token {
    fn is_expired(&self) -> bool { Instant::now() >= self.expires_at }
}

// `ttl_secs` is a runtime argument — each user's token expires on its own schedule.
#[cached(expires = true, key = "u64", convert = { user_id })]
fn fetch_token(user_id: u64, ttl_secs: u64) -> Token {
    Token {
        value: format!("token-{user_id}"),
        expires_at: Instant::now() + Duration::from_secs(ttl_secs),
    }
}
# }
# fn main() {}
```

For concurrent (multi-thread, no external lock) use, the sharded equivalents [`ShardedExpiringCache`] and [`ShardedExpiringLruCache`] provide the same per-value expiry with internally-synchronized sharded storage. Use `#[concurrent_cached(expires = true)]` to select them automatically.

> **Memory note:** `ExpiringCache` and `ShardedExpiringCache` are unbounded and only remove
> expired entries when the same key is accessed again. `CachedIter::iter()` (implemented on the
> non-sharded `ExpiringCache` / `ExpiringLruCache` only, not on the sharded variants) filters
> expired entries from the iterator but does not remove them from the map. For high-cardinality workloads,
> call `evict()` periodically — on the single-owner `ExpiringCache` via [`CacheEvict`]
> (`use cached::CacheEvict;`, `&mut self`), and on the sharded `ShardedExpiringCache` via
> [`ConcurrentCacheEvict`] (`use cached::ConcurrentCacheEvict;`, `&self`) or its inherent
> `evict(&self)` method; note that `evict()` on sharded TTL and expiring stores requires
> `K: Clone`. Alternatively, prefer `ExpiringLruCache` / `ShardedExpiringLruCache` with a
> `max_size` bound.

```rust
use cached::{CachedExt, Expires, ExpiringCache, ExpiringLruCache};
use cached::time::{Duration, Instant};

#[derive(Clone)]
struct Response {
    payload: String,
    expires_at: Instant,
}

impl Expires for Response {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

let now = Instant::now();

// ExpiringCache — unbounded, default for `#[cached(expires = true)]`
let mut cache = ExpiringCache::builder().build().unwrap();
cache.set("key1", Response {
    payload: "a".to_string(),
    expires_at: now + Duration::from_secs(1),
});
cache.set("key2", Response {
    payload: "b".to_string(),
    expires_at: now + Duration::from_secs(3600),
});

// ExpiringLruCache — LRU-bounded, used with `#[cached(expires = true, max_size = N)]`
let mut lru = ExpiringLruCache::builder().max_size(10).build().unwrap();
lru.set("key1", Response {
    payload: "a".to_string(),
    expires_at: now + Duration::from_secs(1),
});
```

# Performance notes

v3 reworks the hot paths of the in-memory and sharded stores. Steady-state `O(1)` reads and
capacity-bounded inserts are unchanged; the wins concentrate in a few paths (figures are
hardware- and workload-dependent; see `benches/cache_benches.rs` for the paths measured):

- Overwriting an existing key with `cache_set` on the map-backed stores (`TtlCache`,
  `TtlSortedCache`, `ExpiringCache`) reuses the stored key instead of cloning the caller's key.
  The gain grows with key clone cost, so caches with expensive keys benefit most. The LRU-family
  stores instead rebind the slot to the caller's key (see the recency note below).
- Bulk eviction and retention (`evict`, `retain`, `retain_latest`) and the key/iteration order
  helpers on the single-owner in-memory and TTL stores now sweep in a single pass instead of
  collecting keys first, up to about 2x faster at 10k entries. TTL stores also sample the clock
  once per operation rather than once per entry.
- Single-threaded `cache_get` hits on the expiring-LRU and sharded LRU-TTL variants resolve in
  one hash lookup instead of two, about 5-15% faster. The plain sharded LRU releases the shard
  lock before updating its counters.

One deliberate tradeoff comes with the `cache_set` recency change (see the migration guide): an
overwrite now promotes the key to most-recently-used, which adds a small cost on stores with cheap
`Copy` keys where there is no key clone to save.
*/

use crate::{Cached, CachedIter, CachedPeek, CachedRead};
use std::cmp::Eq;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Default hash builder for non-sharded in-memory stores.
///
/// Resolves to `ahash::RandomState` when the `ahash` feature is enabled,
/// and to `std::collections::hash_map::RandomState` otherwise. This
/// matches the behavior prior to the introduction of the `S` type parameter,
/// so existing code that does not name the hasher is unaffected.
///
/// # Example
///
/// Use the default hasher (no change required for existing code):
///
/// ```rust
/// use cached::{Cached, UnboundCache};
///
/// let mut cache: UnboundCache<u32, u32> = UnboundCache::new();
/// cache.cache_set(1, 100);
/// assert_eq!(cache.cache_get(&1), Some(&100));
/// ```
///
/// Use a custom hasher via the builder's `.hasher()` method:
///
/// ```rust
/// use cached::{Cached, UnboundCache, DefaultHashBuilder};
/// use std::collections::hash_map::RandomState;
///
/// let mut cache = UnboundCache::<u32, u32>::builder()
///     .hasher(RandomState::new())
///     .build()
///     .unwrap();
/// cache.cache_set(1, 100);
/// assert_eq!(cache.cache_get(&1), Some(&100));
/// ```
///
/// ## Nightly caveat
///
/// On nightly Rust, `ahash::RandomState` specializes `BuildHasher::hash_one` under the
/// `specialize` cfg and dispatches on the static type of the argument, so an owned newtype key
/// and its borrowed primitive form can hash differently and a borrowed lookup can silently miss
/// a present entry. This affects the `HashMap`-backed stores that probe the hasher directly --
/// [`UnboundCache`], [`TtlCache`], [`ExpiringCache`], and [`TtlSortedCache`] -- but not
/// [`LruCache`], [`LruTtlCache`], or [`ExpiringLruCache`], which hand-build the `Hasher` to
/// avoid this. Work around it with `.hasher(std::collections::hash_map::RandomState::new())` in
/// place of the default.
#[cfg(feature = "ahash")]
pub type DefaultHashBuilder = ahash::RandomState;

/// Default hash builder for non-sharded in-memory stores.
///
/// Resolves to `ahash::RandomState` when the `ahash` feature is enabled,
/// and to `std::collections::hash_map::RandomState` otherwise. This
/// matches the behavior prior to the introduction of the `S` type parameter,
/// so existing code that does not name the hasher is unaffected.
///
/// ## Nightly caveat
///
/// On nightly Rust with the `ahash` feature enabled, `ahash::RandomState` specializes
/// `BuildHasher::hash_one` under the `specialize` cfg and dispatches on the static type of the
/// argument, so an owned newtype key and its borrowed primitive form can hash differently and a
/// borrowed lookup can silently miss a present entry. This affects the `HashMap`-backed stores
/// that probe the hasher directly -- [`UnboundCache`], [`TtlCache`], [`ExpiringCache`], and
/// [`TtlSortedCache`] -- but not [`LruCache`], [`LruTtlCache`], or [`ExpiringLruCache`], which
/// hand-build the `Hasher` to avoid this. Work around it with
/// `.hasher(std::collections::hash_map::RandomState::new())` in place of the default (or by
/// disabling the `ahash` feature, as this fallback already does).
#[cfg(not(feature = "ahash"))]
pub type DefaultHashBuilder = std::collections::hash_map::RandomState;

/// Construct a fresh [`DefaultHashBuilder`].
///
/// Abstracts over `ahash::RandomState::new()` vs `std::collections::hash_map::RandomState::new()`
/// since `ahash::RandomState` does not implement `Default`.
#[inline]
pub(super) fn new_default_hash_builder() -> DefaultHashBuilder {
    #[cfg(feature = "ahash")]
    {
        ahash::RandomState::new()
    }
    #[cfg(not(feature = "ahash"))]
    {
        std::collections::hash_map::RandomState::new()
    }
}

const STRIPE_COUNT: usize = 16;

#[repr(align(128))]
struct Slot(AtomicU64);

/// A hit/miss counter distributed across [`STRIPE_COUNT`] cache-line-padded
/// slots to reduce cross-core cache-line bouncing under concurrent increments.
///
/// Each thread is assigned a stable slot on first use via a thread-local index;
/// [`load`](StripedCounter::load) sums all slots for the aggregate value.
///
/// Used only by stores that implement [`CachedRead`], which allow concurrent
/// shared-lock reads. Stores that are always accessed under an exclusive write
/// lock use plain [`AtomicU64`] instead.
pub(super) struct StripedCounter {
    slots: Box<[Slot]>,
}

impl StripedCounter {
    pub(super) fn new() -> Self {
        let slots = (0..STRIPE_COUNT)
            .map(|_| Slot(AtomicU64::new(0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { slots }
    }

    /// Increment the current thread's stripe by one.
    #[inline]
    pub(super) fn increment(&self) {
        self.slots[thread_stripe()]
            .0
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Increment slot 0 by one, without a thread-local lookup or an atomic RMW.
    ///
    /// Only valid where the caller statically holds `&mut self` (exclusive access),
    /// e.g. behind an existing lock or an owning `&mut` receiver. `load()` sums all
    /// slots regardless of which slot was bumped, so the aggregate stays correct.
    #[inline]
    pub(super) fn increment_mut(&mut self) {
        *self.slots[0].0.get_mut() += 1;
    }

    /// Sum across all stripes.
    pub(super) fn load(&self) -> u64 {
        self.slots.iter().map(|s| s.0.load(Ordering::Relaxed)).sum()
    }

    /// Zero all stripes (used by `cache_reset`).
    pub(super) fn reset(&self) {
        for slot in self.slots.iter() {
            slot.0.store(0, Ordering::Relaxed);
        }
    }

    /// Return a new `StripedCounter` whose slot 0 holds the current aggregate.
    /// Used by manual `Clone` impls that carry counter state across the copy.
    pub(super) fn snapshot(&self) -> Self {
        let total = self.load();
        let new = Self::new();
        new.slots[0].0.store(total, Ordering::Relaxed);
        new
    }
}

#[inline]
fn thread_stripe() -> usize {
    thread_local! {
        static SLOT: usize = {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            NEXT.fetch_add(1, Ordering::Relaxed) % STRIPE_COUNT
        };
    }
    SLOT.with(|&s| s)
}

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

mod expiring;
mod expiring_lru;
mod lru;
#[cfg(feature = "time_stores")]
mod lru_ttl;
#[cfg(feature = "redb_store")]
mod redb;
#[cfg(feature = "redis_store")]
mod redis;
pub mod sharded;
#[cfg(feature = "time_stores")]
mod ttl;
#[cfg(feature = "time_stores")]
mod ttl_sorted;
mod unbound;

#[cfg(any(
    feature = "time_stores",
    feature = "redb_store",
    feature = "redis_store"
))]
use crate::time::Duration;
#[cfg(feature = "time_stores")]
use crate::time::Instant;

pub(super) type OnEvict<K, V> = std::sync::Arc<dyn Fn(&K, &V) + Send + Sync>;

/// Error returned by cache builder `build()` methods.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// A required field was not supplied to the builder.
    MissingRequired(&'static str),
    /// A field value is invalid.
    InvalidValue {
        /// The field whose value is invalid.
        field: &'static str,
        /// Human-readable reason.
        reason: &'static str,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::MissingRequired(field) => write!(f, "required field `{field}` was not set"),
            BuildError::InvalidValue { field, reason } => {
                write!(f, "invalid value for field `{field}`: {reason}")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Error returned by [`CacheSetMaxSize::try_set_max_size`](crate::CacheSetMaxSize::try_set_max_size)
/// and [`ConcurrentCacheSetMaxSize::try_set_max_size`](crate::ConcurrentCacheSetMaxSize::try_set_max_size),
/// and by the inherent `try_set_max_size` on the bounded stores.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetMaxSizeError {
    /// A max size of zero was supplied; max_size must be greater than zero.
    ZeroMaxSize,
    /// The requested max size cannot be represented as a per-shard capacity.
    ///
    /// Only the sharded LRU-bounded stores produce this. They divide the requested total
    /// across `n_shards` with `div_ceil` and a 16-entries-per-shard floor, then multiply back
    /// to get the effective total; for a `max_size` close to `usize::MAX` on a multi-shard
    /// cache that product overflows `usize` and no capacity can be assigned. Requesting a
    /// smaller total, or a single shard, is always representable.
    ///
    /// The infallible `set_max_size` panics in the same situation.
    CapacityOverflow,
}

impl std::fmt::Display for SetMaxSizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetMaxSizeError::ZeroMaxSize => write!(f, "max_size must be greater than zero"),
            SetMaxSizeError::CapacityOverflow => write!(
                f,
                "max_size is too large to divide across the shard count without overflowing usize"
            ),
        }
    }
}

impl std::error::Error for SetMaxSizeError {}

/// Error returned by [`CacheTtl::try_set_ttl`](crate::CacheTtl::try_set_ttl).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetTtlError {
    /// A TTL of zero was supplied; ttl must be greater than zero.
    ZeroTtl,
}

impl std::fmt::Display for SetTtlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetTtlError::ZeroTtl => write!(f, "ttl must be greater than zero"),
        }
    }
}

impl std::error::Error for SetTtlError {}

/// A cached value plus store-specific per-entry metadata, returned by the
/// LRU-family order methods ([`LruCache::iter_order`], [`LruCache::value_order`],
/// and their `LruTtlCache` / `ExpiringLruCache` counterparts).
///
/// `M` is the metadata carried alongside the value: `()` (the default) for stores
/// with no per-entry metadata (`LruCache`, `ExpiringLruCache`) and
/// `Option<Instant>` (the entry's expiry) for `LruTtlCache`. The wrapper
/// [`Deref`](std::ops::Deref)s to `V`, so read access is transparent; metadata is
/// exposed through typed accessors (e.g.
/// [`expires_at`](CacheValue::expires_at)) that exist only for the carrying `M`.
/// [`Display`](std::fmt::Display) forwards to the wrapped value, so `println!("{v}")`
/// works directly on an order-method result.
///
/// # Comparing against a bare value
///
/// `CacheValue` implements `PartialEq<V>`, so `wrapped == bare` compiles. The reverse,
/// `bare == wrapped`, does **not** compile: it would require
/// `impl<V, M> PartialEq<CacheValue<V, M>> for V`, a blanket impl over the foreign type
/// parameter `V` that Rust's coherence rules forbid. Put the `CacheValue` on the left
/// (`cv == bare`) or deref it explicitly (`*cv == bare`) instead.
///
/// # Bulk-unwrapping a collection
///
/// Use the [`IntoValues`] extension trait to turn `Vec<CacheValue<V, M>>` (from
/// `value_order()`) or `Vec<(K, CacheValue<V, M>)>` (from `iter_order()`) into a plain
/// `Vec<V>` without a manual `.into_iter().map(CacheValue::into_value).collect()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheValue<V, M = ()> {
    value: V,
    meta: M,
}

impl<V, M> CacheValue<V, M> {
    pub(crate) fn new(value: V, meta: M) -> Self {
        Self { value, meta }
    }

    /// Shared reference to the wrapped value.
    #[must_use]
    pub fn value(&self) -> &V {
        &self.value
    }

    /// Consume the wrapper and return the wrapped value.
    #[must_use]
    pub fn into_value(self) -> V {
        self.value
    }
}

impl<V> CacheValue<V, Option<crate::time::Instant>> {
    /// The entry's expiry instant. `None` means the entry never expires.
    #[must_use]
    pub fn expires_at(&self) -> Option<crate::time::Instant> {
        self.meta
    }
}

impl<V, M> std::ops::Deref for CacheValue<V, M> {
    type Target = V;
    fn deref(&self) -> &V {
        &self.value
    }
}

/// Compare directly against a bare value, ignoring metadata:
/// `CacheValue::new(1, meta) == 1`.
///
/// This impl is intentionally one-directional: `wrapped == bare` compiles, but
/// `bare == wrapped` does not. A blanket `impl<V, M> PartialEq<CacheValue<V, M>> for V`
/// would implement a foreign trait (`PartialEq`) for a foreign type parameter `V`,
/// which Rust's coherence rules forbid (`V` is not a local type and `CacheValue` does
/// not appear in `V`'s position). To compare the other way round, put the
/// [`CacheValue`] on the left (`cv == bare`) or deref it explicitly (`*cv == bare`).
impl<V: PartialEq, M> PartialEq<V> for CacheValue<V, M> {
    fn eq(&self, other: &V) -> bool {
        self.value == *other
    }
}

impl<V: std::fmt::Display, M> std::fmt::Display for CacheValue<V, M> {
    /// Forwards to the wrapped value's `Display` impl; metadata is not rendered.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.value, f)
    }
}

/// Bulk-unwrap the [`CacheValue`] wrapper out of the collections returned by the
/// LRU-family order methods ([`LruCache::iter_order`], [`LruCache::value_order`],
/// and their `LruTtlCache` / `ExpiringLruCache` counterparts), so collecting plain
/// `V`s does not require `.into_iter().map(CacheValue::into_value).collect()`.
///
/// Implemented for `Vec<CacheValue<V, M>>` (the `value_order()` shape) and for
/// `Vec<(K, CacheValue<V, M>)>` (the `iter_order()` shape; keys are discarded, since
/// the result is a plain `Vec<V>`). Order is preserved in both cases.
///
/// # Example
///
/// ```rust
/// use cached::stores::{IntoValues, LruCache};
/// use cached::Cached;
///
/// let mut cache: LruCache<u32, u32> = LruCache::new(10);
/// cache.cache_set(1, 100);
/// cache.cache_set(2, 200);
///
/// // `value_order()` shape: `Vec<CacheValue<V>>` -> `Vec<V>` (most-recently-used first).
/// let vals: Vec<u32> = cache.value_order().into_values();
/// assert_eq!(vals, vec![200, 100]);
///
/// // `iter_order()` shape: `Vec<(K, CacheValue<V>)>` -> `Vec<V>` (keys dropped).
/// let vals: Vec<u32> = cache.iter_order().into_values();
/// assert_eq!(vals, vec![200, 100]);
/// ```
pub trait IntoValues<V> {
    /// Consume `self`, unwrapping every [`CacheValue`] into its inner value while
    /// preserving order.
    fn into_values(self) -> Vec<V>;
}

impl<V, M> IntoValues<V> for Vec<CacheValue<V, M>> {
    fn into_values(self) -> Vec<V> {
        self.into_iter().map(CacheValue::into_value).collect()
    }
}

impl<K, V, M> IntoValues<V> for Vec<(K, CacheValue<V, M>)> {
    fn into_values(self) -> Vec<V> {
        self.into_iter().map(|(_, v)| v.into_value()).collect()
    }
}

/// Phase 1 of a two-phase sweep: run `doomed` over every entry in `store` and hand back the
/// entries it selected, removed from the map.
///
/// Shared by every `HashMap`-backed store, single-owner and sharded alike, so the ordering
/// assumption below lives in exactly one place.
///
/// # Why two passes
///
/// `doomed` wraps user code (a `retain` predicate, or a value's own
/// [`Expires::is_expired`](crate::Expires::is_expired)),
/// so it can panic. The selection pass only *reads* the map, so a panic leaves the cache
/// exactly as it was: nothing removed, nothing counted, nothing notified. The removal pass
/// replays the recorded decisions through a closure that cannot panic, so every entry it takes
/// out reaches the caller to be counted and notified.
///
/// A single `extract_if` pass driven directly by `doomed` would instead remove eagerly *while*
/// the user code runs, dropping every already-yielded entry during unwind: gone from the cache,
/// never notified, never counted.
///
/// # Ordering assumption
///
/// The two passes are tied together positionally: `flags[i]` is the decision for the `i`th
/// entry `iter` yielded, and `extract_if` consumes them in its own iteration order. This is
/// sound because nothing mutates the map between the passes -- the selection pass takes `&self`
/// on the map and the caller holds the only handle (a `&mut self` receiver, or a shard write
/// guard) for the whole call -- so `extract_if` walks the same untouched table `iter` just
/// walked, in the same order.
///
/// Recording decisions rather than keys is what keeps this off a `K: Clone` bound; the
/// alternative (collect doomed keys, then re-look-up each one) costs a clone plus a second hash
/// and probe per removed entry, and would force `K: Clone` on every caller's `retain`.
pub(crate) fn take_doomed<K, V, S, F>(store: &mut HashMap<K, V, S>, mut doomed: F) -> Vec<(K, V)>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
    F: FnMut(&K, &V) -> bool,
{
    let mut flags: Vec<bool> = Vec::with_capacity(store.len());
    flags.extend(store.iter().map(|(k, v)| doomed(k, v)));
    if !flags.contains(&true) {
        return Vec::new();
    }
    let mut flags = flags.into_iter();
    store
        .extract_if(|_, _| flags.next().unwrap_or(false))
        .collect()
}

/// Validate that `ttl` is non-zero; used by all TTL-capable store builders.
#[cfg(any(
    feature = "time_stores",
    feature = "redb_store",
    feature = "redis_store"
))]
pub(crate) fn validate_ttl(ttl: Duration) -> Result<(), BuildError> {
    if ttl.is_zero() {
        Err(BuildError::InvalidValue {
            field: "ttl",
            reason: "must be greater than zero",
        })
    } else {
        Ok(())
    }
}

/// A cached value paired with its per-entry expiry instant for TTL tracking.
///
/// Used internally by [`TtlCache`], [`LruTtlCache`], [`ShardedTtlCache`], and
/// [`ShardedLruTtlCache`]. The `expires_at` field holds the absolute instant at
/// which this entry expires, or `None` if the entry never expires (i.e. the TTL
/// was disabled at insert time via `set_ttl(Duration::ZERO)` / `unset_ttl()`).
///
/// Because expiry is fixed at insertion time, calling `set_ttl` after inserting an
/// entry does **not** retroactively change when existing entries expire. Only newly
/// inserted (or refreshed-on-hit) entries use the TTL current at that point.
#[cfg(feature = "time_stores")]
#[derive(Debug)]
pub(crate) struct TimedEntry<V> {
    /// The absolute instant at which this entry expires, or `None` for never.
    pub(crate) expires_at: Option<Instant>,
    /// The cached value.
    pub(crate) value: V,
}

#[cfg(feature = "time_stores")]
impl<V: Clone> Clone for TimedEntry<V> {
    fn clone(&self) -> Self {
        Self {
            expires_at: self.expires_at,
            value: self.value.clone(),
        }
    }
}

#[cfg(feature = "redb_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redb_store")))]
pub use crate::stores::redb::{RedbCache, RedbCacheBuildError, RedbCacheBuilder, RedbCacheError};
#[cfg(feature = "redis_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redis_store")))]
pub use crate::stores::redis::{
    ConnectionString, RedisCache, RedisCacheBuildError, RedisCacheBuilder, RedisCacheError,
};
pub use expiring::{ExpiringCache, ExpiringCacheBuilder};
pub use expiring_lru::{Expires, ExpiringLruCache, ExpiringLruCacheBuilder};
pub use lru::{LruCache, LruCacheBuilder};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use lru_ttl::{HasEvict, LruTtlCache, LruTtlCacheBuilder, NoEvict};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use ttl::{TtlCache, TtlCacheBuilder};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use ttl_sorted::{TtlSortedCache, TtlSortedCacheBuilder, TtlSortedSetBuilder};
pub use unbound::{UnboundCache, UnboundCacheBuilder};

pub use sharded::{
    DefaultShardHasher, ShardHasher, ShardedExpiringCache, ShardedExpiringCacheBuilder,
    ShardedExpiringLruCache, ShardedExpiringLruCacheBuilder, ShardedLruCache,
    ShardedLruCacheBuilder, ShardedUnboundCache, ShardedUnboundCacheBuilder,
};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use sharded::{
    ShardedLruTtlCache, ShardedLruTtlCacheBuilder, ShardedTtlCache, ShardedTtlCacheBuilder,
};

// Canonical `AsyncRedisCache` availability gate (kept in sync with src/lib.rs and
// src/stores/redis.rs): a redis async runtime feature must be enabled. The six runtime features
// each imply `redis_store` + `async`; the capability-only features are excluded (no runtime).
#[cfg(any(
    feature = "redis_smol",
    feature = "redis_smol_native_tls",
    feature = "redis_smol_rustls",
    feature = "redis_tokio",
    feature = "redis_tokio_native_tls",
    feature = "redis_tokio_rustls",
))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
    )))
)]
pub use crate::stores::redis::{AsyncRedisCache, AsyncRedisCacheBuilder};

impl<K, V, S> Cached<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::get(self, k)
    }
    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::get_mut(self, k)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        HashMap::insert(self, k, v)
    }
    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.entry(key).or_insert_with(f)
    }
    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let v = match self.entry(key) {
            Entry::Occupied(occupied) => occupied.into_mut(),
            Entry::Vacant(vacant) => vacant.insert(f()?),
        };

        Ok(v)
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::remove(self, k)
    }
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::remove_entry(self, k)
    }
    fn cache_clear(&mut self) {
        HashMap::clear(self);
    }
    fn cache_reset(&mut self) {
        // Clear and release capacity without requiring `S: Default`, so stores built with a
        // non-`Default` `BuildHasher` (e.g. a seeded hasher, or ahash on wasm where
        // `RandomState: Default` is gated off) still implement `Cached`. The existing hasher
        // instance is preserved, matching the "configuration preserved across reset" contract.
        HashMap::clear(self);
        self.shrink_to_fit();
        self.cache_reset_metrics();
    }
    fn cache_size(&self) -> usize {
        HashMap::len(self)
    }
}

impl<K, V, S> CachedIter<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.iter()
    }
}

impl<K, V, S> CachedPeek<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        HashMap::get(self, k)
    }
}

impl<K, V, S> CachedRead<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq,
    S: std::hash::BuildHasher,
{
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, S> CachedGetOrSetAsync<K, V> for HashMap<K, V, S>
where
    K: Hash + Eq + Clone + Send,
    S: std::hash::BuildHasher + Send,
{
    fn async_cache_get_or_set_with_mut<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move {
            match self.entry(k) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => v.insert(f().await),
            }
        }
    }

    fn async_cache_try_get_or_set_with_mut<'a, F, Fut, E>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<&'a mut V, E>> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        E: 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = Result<V, E>> + Send + 'a,
    {
        async move {
            let v = match self.entry(k) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => v.insert(f().await?),
            };
            Ok(v)
        }
    }
}

/// Trait for cache stores that support explicit eviction of expired entries.
///
/// Implementors remove all entries that are past their expiry from the store and
/// invoke the `on_evict` callback (if configured) for each removed entry.
///
/// `evict()` is the explicit way to physically remove expired entries, reclaim
/// memory, and obtain an accurate live count. After calling `evict()`, `len()`
/// reflects only live entries. Without it, `len()` may count expired-but-not-yet-swept
/// entries while `iter().count()` omits them - the two can differ on any lazy-eviction
/// store.
///
/// This trait is for in-memory stores with infallible expiration checks. IO-backed
/// stores expose their own APIs because sweeping can fail: `RedbCache` uses
/// `remove_expired_entries`, while Redis relies on server-side key expiry.
pub trait CacheEvict {
    /// Physically remove all expired entries from the cache and return the count removed.
    ///
    /// After this call, `len()` reflects only live entries. Fires the `on_evict` callback
    /// and increments `cache_evictions()` for each removed entry. Hit/miss metrics are not
    /// affected; call [`cache_reset_metrics`](crate::Cached::cache_reset_metrics) separately
    /// if needed.
    ///
    /// **Note for sharded in-memory stores**: these are internally synchronized and normally held
    /// behind an `Arc`/`static`, so they cannot offer `&mut self`. They implement
    /// [`ConcurrentCacheEvict`] (a `&self` counterpart of this trait) instead, and also expose an
    /// inherent `evict(&self)` method.
    #[must_use]
    fn evict(&mut self) -> usize;
}

/// Concurrent counterpart of [`CacheEvict`] for internally-synchronized stores.
///
/// Sharded in-memory stores are normally held behind an `Arc`/`static`, so they
/// cannot offer the `&mut self` [`CacheEvict::evict`]. This trait provides the same
/// operation through a shared reference.
///
/// `evict()` is the explicit way to physically remove expired entries, reclaim
/// memory, and obtain an accurate live count on the sharded expiry-capable stores
/// (`ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`,
/// `ShardedExpiringLruCache`). After calling `evict()`, `len()` (the inherent method)
/// reflects only live entries.
pub trait ConcurrentCacheEvict {
    /// Physically remove all expired entries across all shards and return the count removed.
    ///
    /// After this call, `len()` (the inherent method on sharded stores) reflects only live
    /// entries. Fires `on_evict` and increments `cache_evictions()` for each removed entry.
    #[must_use]
    fn evict(&self) -> usize;
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use super::*;

    /// Each `SetMaxSizeError` variant renders a distinct, non-empty message naming the
    /// constraint it stands for, so a caller matching on `Display` output can tell an
    /// out-of-range total apart from a zero one.
    #[test]
    fn set_max_size_error_variants_have_distinct_messages() {
        let zero = SetMaxSizeError::ZeroMaxSize.to_string();
        let overflow = SetMaxSizeError::CapacityOverflow.to_string();
        assert_ne!(zero, overflow);
        assert!(zero.contains("greater than zero"), "{zero}");
        assert!(overflow.contains("overflow"), "{overflow}");
        assert!(overflow.contains("shard"), "{overflow}");
    }

    // `increment_mut` (the exclusive-access, non-atomic fast path used by `&mut self`
    // call sites) and `increment` (the striped, thread-local-indexed atomic path used by
    // `CachedRead`'s shared-reference call sites) must both feed the same aggregate: a mix
    // of the two, in either order, sums to the total call count under `load()`.
    #[test]
    fn increment_mut_and_increment_agree_on_aggregate() {
        let mut counter = StripedCounter::new();
        assert_eq!(counter.load(), 0);

        // Mix `increment_mut` and `increment` calls; every call, regardless of which
        // method, must contribute exactly 1 to the aggregate.
        counter.increment_mut();
        counter.increment();
        counter.increment_mut();
        counter.increment();
        counter.increment();
        counter.increment_mut();

        assert_eq!(counter.load(), 6);

        // `reset()` still zeroes everything, including whatever `increment_mut` wrote to
        // slot 0 and whatever `increment` scattered across the striped slots.
        counter.reset();
        assert_eq!(counter.load(), 0);

        // After reset, further mixed increments still land correctly.
        for _ in 0..10 {
            counter.increment_mut();
        }
        for _ in 0..10 {
            counter.increment();
        }
        assert_eq!(counter.load(), 20);

        // `snapshot()` carries the aggregate (from a mix of both increment paths) across
        // a clone, collapsed into a fresh counter's slot 0.
        let snap = counter.snapshot();
        assert_eq!(snap.load(), 20);
        // The snapshot is independent: incrementing it must not affect the original.
        let mut snap = snap;
        snap.increment_mut();
        assert_eq!(snap.load(), 21);
        assert_eq!(counter.load(), 20);
    }

    #[test]
    fn hashmap() {
        let mut c = std::collections::HashMap::new();
        assert!(c.cache_get(&1).is_none());
        assert_eq!(c.cache_misses(), None);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(c.cache_hits(), None);
        assert_eq!(c.cache_misses(), None);
    }

    #[test]
    fn build_error_display() {
        let err1 = BuildError::MissingRequired("ttl");
        assert_eq!(err1.to_string(), "required field `ttl` was not set");

        let err2 = BuildError::InvalidValue {
            field: "max_size",
            reason: "must be greater than zero",
        };
        assert_eq!(
            err2.to_string(),
            "invalid value for field `max_size`: must be greater than zero"
        );
    }

    // Compile-time assertion: BuildError must implement Clone + PartialEq + Eq.
    fn _assert_build_error_bounds<T: Clone + PartialEq + Eq>() {}
    fn _check_build_error() {
        _assert_build_error_bounds::<BuildError>();
    }

    #[test]
    fn build_error_clone_partial_eq_eq() {
        // Two equal MissingRequired values compare equal and survive a clone round-trip.
        let a = BuildError::MissingRequired("ttl");
        let b = a.clone();
        assert_eq!(a, b);

        // Two equal InvalidValue values compare equal and survive a clone round-trip.
        let c = BuildError::InvalidValue {
            field: "max_size",
            reason: "must be greater than zero",
        };
        let d = c.clone();
        assert_eq!(c, d);

        // Different variants are not equal.
        assert_ne!(
            BuildError::MissingRequired("ttl"),
            BuildError::InvalidValue {
                field: "ttl",
                reason: "must be greater than zero",
            },
        );

        // Different field names inside the same variant are not equal.
        assert_ne!(
            BuildError::MissingRequired("ttl"),
            BuildError::MissingRequired("max_size"),
        );
    }

    // The author's `build_error_clone_partial_eq_eq` exercises clone/eq on both
    // variants, a cross-variant `assert_ne!`, and differing fields *within*
    // `MissingRequired`. The remaining derived-PartialEq paths for the struct
    // variant `InvalidValue` were not checked: equality must depend on BOTH the
    // `field` and the `reason` field. These would not fail to compile if the
    // derive were reverted (the wrapper enums prove `BuildError` already had a
    // hand-rolled `Debug`), so each comparison below is value-level and bites if
    // the `PartialEq`/`Eq`/`Clone` derive is removed.
    #[test]
    fn build_error_invalid_value_field_discriminates() {
        // Same `reason`, different `field` => not equal.
        assert_ne!(
            BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            },
            BuildError::InvalidValue {
                field: "ttl",
                reason: "must be greater than zero",
            },
        );

        // Same `field`, different `reason` => not equal.
        assert_ne!(
            BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            },
            BuildError::InvalidValue {
                field: "max_size",
                reason: "allocation failed",
            },
        );

        // Fully equal struct variants compare equal (and the clone matches).
        let a = BuildError::InvalidValue {
            field: "max_size",
            reason: "must be greater than zero",
        };
        assert_eq!(a, a.clone());
    }

    // The consumer-facing point of the derive: a real builder failure can be
    // compared with `assert_eq!`/matched, and cloned. `LruCacheBuilder::build`
    // is feature-free and produces both `BuildError` variants directly.
    #[test]
    fn lru_build_error_is_comparable_and_cloneable() {
        // Missing required field.
        let missing = LruCache::<i32, i32>::builder().build().unwrap_err();
        assert_eq!(missing, BuildError::MissingRequired("max_size"));
        // assert_eq! over a clone of a real builder error.
        assert_eq!(missing, missing.clone());

        // Invalid value (zero capacity).
        let invalid = LruCache::<i32, i32>::builder()
            .max_size(0)
            .build()
            .unwrap_err();
        assert_eq!(
            invalid,
            BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            }
        );

        // The two real failures are distinct.
        assert_ne!(missing, invalid);
    }

    // Wrapper enums (RedisCacheBuildError / RedbCacheBuildError) INTENTIONALLY do
    // NOT derive Clone/PartialEq/Eq (their other variants wrap non-Clone/Eq
    // errors). Assert only that the config path still surfaces the embedded
    // `BuildError` and that Debug/Display work. These tests need a live service
    // only past the field validation, which the builders run first, so no
    // network/disk is touched.
    #[cfg(feature = "redis_store")]
    #[test]
    fn redis_build_error_wraps_build_error_without_clone_eq() {
        // No prefix set => Build(BuildError::MissingRequired("prefix")) before any IO.
        let err = RedisCacheBuilder::<String, u32>::new().build().unwrap_err();
        assert!(
            matches!(
                err,
                RedisCacheBuildError::Build(BuildError::MissingRequired("prefix"))
            ),
            "expected Build(MissingRequired(\"prefix\")), got {err:?}"
        );
        // Debug and Display are available on the wrapper (transparent to BuildError).
        assert!(!format!("{err:?}").is_empty());
        assert_eq!(
            err.to_string(),
            BuildError::MissingRequired("prefix").to_string()
        );
    }

    #[cfg(feature = "redb_store")]
    #[test]
    fn redb_build_error_wraps_build_error_without_clone_eq() {
        // No name set => Build(BuildError::MissingRequired("name")) before any IO.
        let err = RedbCacheBuilder::<String, u32>::new().build().unwrap_err();
        assert!(
            matches!(
                err,
                RedbCacheBuildError::Build(BuildError::MissingRequired("name"))
            ),
            "expected Build(MissingRequired(\"name\")), got {err:?}"
        );
        assert!(!format!("{err:?}").is_empty());
        assert_eq!(
            err.to_string(),
            BuildError::MissingRequired("name").to_string()
        );
    }
}
