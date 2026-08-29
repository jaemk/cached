# Changelog

## [Unreleased]

### Breaking Changes

- A sharded store (`ShardedLruCache`, `ShardedUnboundCache`, `ShardedTtlCache`,
  `ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`) built over a
  hand-written `ShardHasher` that does not also implement `BuildHasher` loses its inherent `get`,
  `remove`, `remove_entry`, `delete`, `contains`, and `peek` methods. Those six methods are now
  generic over `Borrow<Q>` and bounded on `H: BuildHasher` (named `BorrowedKeyRouting` in the
  trait bound so the compile error explains itself), because owned-key and borrowed-key shard
  routing are only provably equal for the blanket `ShardHasher` impl every `BuildHasher` receives;
  a hand-rolled `ShardHasher` carries no such guarantee. There is no method-resolution fallback:
  the inherent method is selected by name first and then fails its bound, so importing a trait
  does not rescue the call at the same call site.
  Migration is mechanical: replace the inherent call with the trait's owned-key form, which still
  takes `&K`. `cache.get(&k)` becomes `ConcurrentCachedExt::get(&cache, &k).unwrap()`; same for
  `remove`, `remove_entry`, and `delete` (`ConcurrentCachedExt::remove(&cache, &k).unwrap()`, and
  so on) and for `contains` (`ConcurrentCachedExt::contains(&cache, &k).unwrap()`). `peek` moves to
  `ConcurrentCachePeek::peek(&cache, &k).unwrap()` instead, since its alias lives on
  `ConcurrentCachePeek`, not `ConcurrentCachedExt`.
  Accepted deliberately in a MINOR release: 3.0.0 shipped 2026-08-22, so adoption of a custom
  `ShardHasher` router is assumed to be effectively zero, and the alternative (relaxing
  `ShardHasher<K>` to `K: ?Sized` and bounding the new methods on `H: ShardHasher<Q>` instead of
  `H: BuildHasher`) would have introduced an unchecked cross-impl consistency contract
  (`shard_hash(&k) == shard_hash(k.borrow())`) that the type system cannot verify, whose failure
  mode is a silent phantom miss on an entry that is actually present.
- Argument-inference breakage: `cache.get(&k)` where `k: &K` (for example, `for k in &keys {
  cache.get(&k) }`) previously compiled through deref coercion. It no longer does: `Q` unifies to
  `&K` first, and the call fails with ``the trait bound `String: Borrow<&String>` is not
  satisfied``; same for `k: &Box<K>` and `k: &Arc<K>`. The `BorrowedKeyRouting` diagnostic above
  does not fire for this case, since the `H` bound is satisfied and only the `Borrow` impl is
  missing, so the error is opaque. Migration: drop the extra `&` (`cache.get(k)`), or deref
  explicitly (`cache.get(&*boxed)`).
- Generic-helper breakage: a downstream helper bounded only on the hasher type, for example
  `fn lookup<K, V, H: ShardHasher<K>>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V> {
  c.get(k) }`, now fails to compile at its own definition, regardless of which hasher its call
  sites use: `ShardHasher<K>` alone no longer implies the bound the inherent methods need. The
  inherent `get` lives in `impl<K, V, H: ShardHasher<K>> ShardedLruCache<K, V, H> where K: Hash +
  Eq + Clone, V: Clone`, so the helper also needs the key/value bounds too, not just a hasher
  bound; rustc reports those key/value bounds first (E0599) and never names `BorrowedKeyRouting`.
  Migration: add the full bound set to the helper's own definition:
  ```rust
  fn lookup<K, V, H>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V>
  where K: Hash + Eq + Clone, V: Clone, H: ShardHasher<K> + BorrowedKeyRouting
  { c.get(k) }
  ```
- Shard routing no longer consults `BuildHasher::hash_one`. The blanket `ShardHasher` impl for a
  `BuildHasher` and every store's borrowed-key routing build a `Hasher` with `build_hasher()`,
  hash the key and finish it, and `DefaultShardHasher`'s `hash_one` override was removed so it
  uses the provided default too. An overridden `hash_one` is no longer consulted for routing, and
  the upper-32-bit distribution contract for a custom hasher applies to the `Hasher` returned by
  `build_hasher()`. This matters for a `BuildHasher` whose `hash_one` dispatches on the static
  type of its argument (`ahash::RandomState` does, on any nightly compiler): an owned newtype key
  and its borrowed primitive form would otherwise route to different shards.

### Added

- `cached::claim::{ClaimRegistry, Claim}`: a single-flight claim on a key, for collapsing
  concurrent refreshes of one key onto a single caller. `ClaimRegistry::claim(key)` returns
  `Some(Claim<K>)` to the first caller and `None` to every later caller until that `Claim` is
  dropped; the key is released from `Drop`, so completion, a panic, and cancellation (an
  aborted async task) all release it, unlike a hand-rolled guard released at the end of the
  body. This is not background refresh: the registry spawns nothing and awaits nothing, and
  spawning the refresh stays with the caller, same as today. Additive, ungated (no new
  dependency, no feature flag), and reachable via `cached::claim::` or `cached::prelude`, not
  the crate root, to keep `Claim` and `ClaimRegistry` out of rustc's nearest-match suggestions
  for mistyped imports ([design/0053](specs/design/0053-refresh-claim-guard.md)).
- `CacheSetMaxSize` and `ConcurrentCacheSetMaxSize` traits, reaching `set_max_size` /
  `try_set_max_size` from generic code holding a `T: CacheSetMaxSize` (or
  `ConcurrentCacheSetMaxSize`) bound instead of a concrete store type. No new capability: both
  methods already existed as inherent-only methods and already evicted eagerly on shrink, firing
  `on_evict` per removed entry; this only adds a route through a generic bound. Implemented by
  `LruCache`, `LruTtlCache`, `ExpiringLruCache`, `TtlSortedCache` (single-owner) and
  `ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache` (sharded). Not implemented by
  the unbounded stores (`UnboundCache`, `TtlCache`, `ExpiringCache` and their sharded forms), which
  have no live capacity to resize, or by `RedisCache` / `AsyncRedisCache` / `RedbCache`, which have
  no client-side capacity.
- `CacheClearWithOnEvict` and `ConcurrentCacheClearWithOnEvict` traits, reaching
  `cache_clear_with_on_evict` from generic code the same way. Implemented by all 7 single-owner
  in-memory stores and all 6 sharded stores (every in-memory store that has an `on_evict`
  callback); not implemented by the three IO stores, which have no `on_evict` mechanism. The crate
  doc previously described this method as inherent-only and unreachable from generic code; that
  gap is now closed.
- The six sharded stores' inherent `get`, `remove`, `remove_entry`, `delete`, `contains`, and
  `peek` now accept any borrowed form of the key (`K: Borrow<Q>`), matching the single-owner
  stores: `sharded_cache.get("a")` now works on a `ShardedLruCache<String, _>` without allocating
  a `String` first. Bounded on `H: BuildHasher`, surfaced in a failed bound as
  `BorrowedKeyRouting` rather than a bare `BuildHasher` error. `set` and `get_or_set_with` are
  unchanged: they take the key by value because they insert it. See "Breaking Changes" above for
  the three cases this is not additive for.
- `cached::BorrowedKeyRouting` (`use cached::BorrowedKeyRouting;`): a new public export at the
  crate root, deliberately not in the prelude. Previously it was only mentioned in
  breaking-change prose above, so a user with `use cached::prelude::*;` who follows that
  migration advice hit E0405 (unresolved name) and had to guess the path.
- `cached::prelude` now also exports `CacheSetMaxSize`, `CacheClearWithOnEvict`,
  `ConcurrentCacheSetMaxSize`, and `ConcurrentCacheClearWithOnEvict`. Note that a downstream
  extension trait offering `set_max_size` or `cache_clear_with_on_evict` for the same stores,
  combined with `use cached::prelude::*`, now yields E0034 (multiple applicable items in scope);
  fixable with UFCS.

### Fixed

- `ShardedUnboundCache`, `ShardedTtlCache`, and `ShardedExpiringCache` backed their shards with
  `HashMap<K, V, ahash::RandomState>`, whose intra-shard probe goes through `BuildHasher::hash_one`.
  On a nightly compiler, where ahash enables its `specialize` cfg, an entry inserted under an
  owned newtype key was unreachable through its borrowed primitive form: `get`, `contains`, and
  `peek` reported a miss, and `remove` and `delete` silently no-opped. They now use
  `DefaultShardHasher`, which has no `hash_one` override, so the probe depends only on `Hash`.
- `TtlSortedCache::cache_clear_with_on_evict` drained every entry into a `Vec` even with no
  `on_evict` callback configured, where only the removed count is observable. It now clears in
  place in that case.
- `TtlSortedCache::cache_clear_with_on_evict` now counts an eviction per removed entry even when
  no `on_evict` callback is configured, matching the trait contract and the other implementors.
  Previously it took an early-return fast path that cleared the store without counting anything.
  Observable to anyone metering evictions on that store.

### Documentation

- Gate the per-entry-expiry macro example in the crate doc on the `proc_macro` feature. The fence
  used `cached::macros`, which does not exist without that feature, so
  `cargo test --no-default-features --doc` failed to compile it; CI's no-default-features row
  runs `--tests` only, so this went uncaught there. Gated rather than marked `ignore` (like its
  two neighboring macro fences), so it still compiles when the feature is on; the rendered
  README is unchanged, since cargo-readme strips the hidden lines.

## [3.1.1] - 2026-08-25

Documentation and tests only. There is no API or behavior change.

### Documentation

- Document that a custom `ty` on `#[concurrent_cached]` requires the cached function to
  return `Result<T, E>`, with a compiling example on the `ConcurrentCached` trait. The macro
  cannot see the store's `Error` type at expansion time, so it always emits the fallible
  path. Previously this was only discoverable by hitting the compile error, which is now
  also pinned by a `tests/ui` case.
- New example `examples/moka_custom_store.rs`: adapting a third-party cache (`moka`) to
  `ConcurrentCached`. The orphan rule makes a local newtype mandatory for any foreign store,
  and the example records what an API that does not line up method-for-method costs to
  adapt, including returning the replaced value from `cache_set` without a get-then-set
  race. `moka` is a dev-dependency for the example only; it is not a dependency of the
  crate ([#220](https://github.com/jaemk/cached/issues/220)).

## [3.1.0] - 2026-08-24

### Added

- `CacheExpiry` and `ConcurrentCacheExpiry` traits, providing `cache_peek_expires_at()` /
  `peek_expires_at()`: a side-effect-free per-key read returning `(Option<V>, Option<Instant>)`
  instead of the `bool` `cache_peek_with_expiry_status` returns, so callers can implement a
  threshold-based refresh (refresh when the remaining TTL drops below N) directly against the
  deadline ([#91](https://github.com/jaemk/cached/issues/91)). Additive and non-breaking:
  standalone traits, not new required methods on `CloneCached` / `ConcurrentCloneCached`.
  Implemented by `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`,
  `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`, and `ShardedExpiringLruCache`.
  Both traits also provide a value-free `cache_expires_at()` / `expires_at()`, returning `(bool,
  Option<Instant>)` (presence, deadline) instead of cloning the value, for callers who only need
  the remaining time; it needs no `V: Clone` bound, since that bound moved off the impl blocks and
  onto the value-returning methods (`cache_peek_expires_at` / `peek_expires_at`).
  On `ExpiringCache`, `ExpiringLruCache`, `ShardedExpiringCache`, and `ShardedExpiringLruCache`
  the deadline comes from `Expires::expires_at()`, whose default body returns `None`: for an
  `Expires` impl that only implements `is_expired` (the crate's own documented recipe), the read
  reports `None` for both live and expired entries, and a threshold-refresh policy built on it
  silently never fires.

### Documentation

- New runnable example `examples/refresh_before_expiry.rs`: recompute an entry once its
  remaining ttl drops below a threshold, using `cache_peek_expires_at` (`peek_expires_at`) to
  read the deadline and `{fn}_prime_cache` to refresh outside the cache write lock, so the
  stored value is replaced while still live and no caller reads an expired entry. Companion to
  `examples/stale_while_revalidate.rs`, which handles the already-expired case. Covers the sync
  `#[cached]`, async `#[cached]`, and async `#[concurrent_cached]` static shapes. No API change.

- New runnable example `examples/stale_while_revalidate.rs`: serve an expired value
  immediately and refresh it off the critical path, composed from
  `cache_peek_with_expiry_status` (which returns an expired entry as
  `(Some(value), true)` without removing it) and `{fn}_prime_cache` (which runs the
  function body outside the cache write lock). Covers the sync `#[cached]`, async
  `#[cached]`, and async `#[concurrent_cached]` static shapes, and an in-flight guard
  that collapses concurrent refreshes for the same key, and a single-flight section
  adding `sync_writes = "by_key"` so the cold path deduplicates while stale reads still
  never block. No API change.

### Fixed

- `examples/stale_while_revalidate.rs` released its in-flight refresh claim only when the
  refresh returned normally, so a panicking or aborted refresh left the key claimed for the
  rest of the process and pinned it to a stale value with no way back. The claim is now
  released from `Drop`. Example code only; no API change.

## [3.0.0 / cached_proc_macro 3.0.0 / cached_proc_macro_types 3.0.0] - 2026-08-22

This entry describes the complete 2.0.2 -> 3.0.0 delta. The ten release candidates
(`3.0.0-rc.1` through `3.0.0-rc.10`) are folded in here, and API that was introduced and
then changed again across the candidates is recorded only in its final shipped form; the rc
git tags remain. The upgrade is documented step by step in the
[migration guide](docs/migrations/2.0-to-3.0-human.md), with the mechanical breaking-change
list in the [agent-oriented guide](docs/migrations/2.0-to-3.0.md).

### Breaking Changes

#### Minimum supported Rust version

- MSRV raised from 1.85 to 1.92. `redb` 4.x set the 1.89 floor, and the `async_core` feature
  (enabled by `async`) does not compile before 1.92: the two `CachedGetOrSetAsync` RPIT
  default bodies hit a rustc borrowck limitation ([rust-lang/rust#100013]). Verified by
  bisection (fails on 1.89.0, 1.90.0, 1.91.0; clean on 1.92.0). Non-async feature sets built
  on 1.89, but `rust-version` is a single crate-level value, so the floor moves for every
  feature set.
- `cached_proc_macro_types` moved to edition 2024, and its version now tracks `cached` in
  lockstep rather than a standalone `1.0`.

#### Store renames and the disk backend ([#237])

- `DiskCache` is renamed `RedbCache` (naming the backend, like `RedisCache`) and is backed by
  [`redb`](https://crates.io/crates/redb) 4.x instead of the unmaintained `sled`, dropping the
  RustSec-flagged `fxhash` transitive dependency. Still pure-Rust (no C toolchain). There are
  no `DiskCache*` aliases: rename `DiskCache` / `DiskCacheBuilder` / `DiskCacheError` /
  `DiskCacheBuildError` to `RedbCache*` at the call site. The on-disk format changed and
  `DISK_FILE_VERSION` was bumped, so existing caches are not read and entries are recomputed.
- `RedbCache::connection()` / `connection_mut()`, `RedbCacheBuilder::connection_config`, and
  the `connection_config` macro attribute are removed; the backend handle is not exposed.
- `DiskCacheBuilder::sync_to_disk_on_cache_change` is renamed `durable` and the default flipped
  from `false` to `true` (fsync per write), so a disk cache persists by default. `durable(false)`
  uses `Durability::None`, which can lose writes on process exit or crash; call
  `RedbCache::flush()` / `async_flush()` to force a durable commit.
- `RedbCacheBuilder::disk_directory` is renamed `disk_dir`, matching the `disk_dir` attribute on
  `#[concurrent_cached]`.
- `ShardedCache` is renamed `ShardedUnboundCache` (with `ShardedCacheBuilder` ->
  `ShardedUnboundCacheBuilder`); the old name read as the umbrella for the whole sharded family
  while naming only the unbounded variant. No deprecated alias.
- The six sharded stores are single types carrying a defaulted hasher parameter,
  `ShardedX<K, V, H = DefaultShardHasher>`, mirroring `HashMap<K, V, S = RandomState>`. The 2.x
  `ShardedCacheBase` pattern is gone: there is no `ShardedUnboundCacheBase`, `ShardedLruCacheBase`,
  `ShardedTtlCacheBase`, `ShardedLruTtlCacheBase`, `ShardedExpiringCacheBase`, or
  `ShardedExpiringLruCacheBase`. Migration is a mechanical rename dropping `Base`.
- `cached::TimedEntry` is now `pub(crate)`, and the `store()` accessors on `UnboundCache`,
  `TtlCache`, `LruTtlCache`, and `ExpiringLruCache` are removed. They exposed the internal
  backing map and leaked the internal entry wrapper; use the public `Cached` API instead.

#### Trait surface

- The short method aliases (`get`, `set`, `remove`, `remove_entry`, `clear`, `len`, `is_empty`,
  `delete`, `try_set`, `contains`, `hits`, `misses`, `metrics`, and the short `get_or_set_with`
  family) moved off `Cached` / `ConcurrentCached` onto the blanket extension traits `CachedExt` /
  `ConcurrentCachedExt`. The core traits keep only the `cache_`-prefixed methods, so a custom
  store implements a smaller surface. Callers using `cached::prelude::*` need no change; others
  add `use cached::CachedExt;` / `use cached::ConcurrentCachedExt;`, or use the `cache_` names.
  Custom `impl Cached` / `impl ConcurrentCached` blocks must drop any short-alias methods.
- `ConcurrentCachedAsync`'s cache operations carry an `async_` prefix (`async_cache_get`,
  `async_cache_set`, `async_cache_remove`, `async_cache_remove_entry`, `async_cache_delete`),
  removing the `E0034` "multiple applicable items" error when both concurrent traits are in scope.
- The concurrent trait surface is split. Introspection (`type Error`, `cache_size`,
  `cache_is_empty`) lives on `ConcurrentCacheBase`, the supertrait of both concurrent traits;
  the global-TTL controls (`ttl`, `set_ttl`, `try_set_ttl`, `unset_ttl`) live on
  `ConcurrentCacheTtl`, implemented only by the TTL-capable concurrent stores. `len` is removed
  from the base trait as a duplicate of `cache_size`. Custom impls must move `type Error` (and
  any size override) into an `impl ConcurrentCacheBase` block and TTL behavior into
  `impl ConcurrentCacheTtl`.
- `refresh_on_hit` / `set_refresh_on_hit` live on their own `CacheRefreshOnHit` and
  `ConcurrentCacheRefreshOnHit` traits rather than on `CacheTtl` / `ConcurrentCacheTtl`.
  `CacheRefreshOnHit` is implemented by `TtlCache` and `LruTtlCache`;
  `ConcurrentCacheRefreshOnHit` by `RedisCache`, `AsyncRedisCache`, `RedbCache`,
  `ShardedTtlCache`, and `ShardedLruTtlCache`. `TtlSortedCache` implements neither: its
  deadline-ordered index cannot refresh an entry's expiry on read, and its 2.x
  `set_refresh_on_hit` was a no-op that discarded its argument. Both new traits are in the
  prelude. The inherent `refresh_on_hit` / `set_refresh_on_hit` on `TtlCache` and `LruTtlCache`
  are removed (they shadowed the trait methods and the setter returned `()`), as are the
  inherent TTL controls on the sharded TTL stores and `TtlSortedCache::set_ttl`: runtime TTL
  control is trait-only.
- `CacheTtl` and `CacheEvict` are single-owner (`&mut self`) traits only, since `&mut self` is
  unusable on a store held through `Arc`/`static`. Concurrent stores set TTL through
  `ConcurrentCacheTtl::set_ttl` (`&self`) and evict through the new `ConcurrentCacheEvict`
  (`fn evict(&self) -> usize`).
- `Cached` and `ConcurrentCacheBase` gained an associated `type Error`, bounded by
  `std::error::Error + Send + Sync + 'static`. Every built-in in-memory store (`UnboundCache`,
  `LruCache`, `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`,
  and the six sharded stores) is infallible: `type Error = std::convert::Infallible`. A TTL that
  would overflow `Instant` bounds stores the entry with no expiry instead of failing, so
  `cache_try_set` no longer has a dedicated error type; the 2.x `TtlSortedCacheError` and the
  boxed `Box<dyn std::error::Error>` return are both gone.
- `Cached::cache_get_or_set_with` / `cache_try_get_or_set_with` (and their aliases) return
  `&V` / `Result<&V, E>` instead of `&mut V` ([#179]). The new `*_mut` variants
  (`cache_get_or_set_with_mut`, `cache_try_get_or_set_with_mut`, and the async spellings)
  preserve the mutable-reference behavior. External impls must update their signatures and
  implement the new required `*_mut` methods.
- The 2.x `CachedAsync` trait is renamed `CachedGetOrSetAsync`, naming the job it actually does
  (memoizing an async closure over a synchronous in-memory `Cached` store). Its four sync
  passthroughs (`async_cache_get` / `async_cache_set` / `async_cache_remove` /
  `async_cache_clear`) and the misleading `Self: Cached` bound are removed, and its get-or-set
  methods use the `async_cache_*` namespace (`async_cache_get_or_set_with`,
  `async_cache_try_get_or_set_with`, and their `_mut` variants).
- New required methods on custom impls: `cache_clear` / `cache_reset` on `ConcurrentCached`
  (and the async counterparts), whose 2.x no-op `Ok(())` defaults silently did nothing;
  `cache_peek_with_expiry_status` on `CloneCached` / `ConcurrentCloneCached`, whose defaults
  returned a wrong result that silently broke `force_refresh` + `result_fallback`; and
  `cache_contains` / `async_cache_contains` on `ConcurrentCached` / `ConcurrentCachedAsync`,
  which carry no `V: Clone` bound so `contains` works for non-`Clone` values.
  `ConcurrentCached::cache_contains` has no `where Self: Sized` bound and is dyn-callable.
- `SerializeCached::cache_set_ref` and `SerializeCachedAsync::async_cache_set_ref` return
  `Result<(), Self::Error>` instead of `Result<Option<V>, Self::Error>`, removing a per-write
  read-and-decode round trip on the IO stores. Call `cache_get` first if you need the prior value.
- `ShardHasher` requires `Clone` as a supertrait, and any thread-safe `std::hash::BuildHasher`
  now implements it through a blanket impl, so `std::hash::RandomState` and `ahash::RandomState`
  are accepted directly by the sharded builders' `.hasher(...)`. `DefaultShardHasher` implements
  `BuildHasher` and reaches `ShardHasher` through that one blanket path, which also makes it
  usable with `HashMap::with_hasher` and `LruCacheBuilder::hasher`. A type cannot implement both
  `BuildHasher` and a hand-written `ShardHasher` (coherence rejects the pair), so a custom
  shard-routing hasher must not implement `BuildHasher`.
- `Expires::expires_at` returns `crate::time::Instant` (web-time backed, correct under wasm)
  instead of `std::time::Instant`, and `CloneCached::cache_get_with_expiry_status` requires
  `V: Clone`, matching its peek sibling.

#### Store behavior

- `cache_set` over an existing key promotes that key to most-recently-used on `LruCache`,
  `LruTtlCache`, `ExpiringLruCache`, `ShardedLruCache`, `ShardedLruTtlCache`, and
  `ShardedExpiringLruCache`. In 2.x the value was replaced in place and the entry kept its
  position, so this changes which entry a capacity eviction selects in overwrite-heavy
  workloads. It also resolves a divergence where configuring an `on_evict` callback changed
  eviction order on two sharded stores. `cache_peek`, `cache_peek_with_expiry_status`, and
  `cache_contains` remain non-promoting; inserting a new key is unchanged. No public API writes
  a value without touching recency.
- `on_evict` receives the displaced entry's own stored key rather than the caller's `Eq`-equal
  instance, on every store and every removal path, matching `HashMap::insert`. Observable only
  for key types whose `Eq`/`Hash` ignore part of the payload.
- Every store counts an eviction before firing `on_evict`, on every removal path (`evict`,
  `retain`, `cache_remove` / `cache_remove_entry`, lazy expiry sweeps, `cache_set` over an
  expired entry, capacity evictions, and the get-or-set families), so a panicking callback can
  no longer remove an entry without counting it.
- `retain` returns `usize` (the number of entries removed) instead of `()` on all 13 stores that
  have it. The count includes entries the predicate rejected and, on the expiry-aware stores,
  entries removed for having expired regardless of the predicate. This diverges from
  `HashMap::retain` deliberately, because this `retain` does strictly more than filter, and it
  matches `TtlSortedCache::retain_latest`. There is no `#[must_use]`, so existing
  `cache.retain(...);` statements keep compiling; only call sites binding the result as `()`
  need a discard.
- `set_max_size` returns `Option<usize>` (the previous bound) on `LruCache`, `LruTtlCache`, and
  `ExpiringLruCache`, unifying the return type with `TtlSortedCache`.
- The default shard count of the LRU-bounded sharded stores (`ShardedLruCache`,
  `ShardedLruTtlCache`, `ShardedExpiringLruCache`) is capped by the requested `max_size`
  instead of derived solely from `available_parallelism()`: on the default path the count is
  `next_power_of_two(max_size / 16).clamp(1, default_shard_count())`. This changes the
  observable `capacity()`, `shards()`, and `shard_sizes()` for small caches on high-core-count
  hosts (`ShardedLruCache::new(100)` resolves to 8 shards / capacity 128, where a 64-core box
  previously produced 256 shards / capacity 4096). An explicit `.shards(n)` is authoritative;
  the `per_shard_max_size` path and the unbounded stores keep `default_shard_count()`.
- `ShardedTtlCache` and `ShardedLruTtlCache` decide expiry against a clock sample taken before
  the shard lock is acquired, so an entry that crosses its expiry while the caller queues for
  the lock is judged live. This stays within the documented lazy-expiry contract, which makes
  no promise of prompt removal.
- TTL stores track per-entry expiry, so `set_ttl` applies to future inserts only; existing
  entries keep their computed expiry, and `refresh_on_hit` recomputes expiry from the current
  TTL at access time. A zero `Duration` passed to any `set_ttl` surface means "expiry disabled",
  exactly equivalent to `unset_ttl()`: it no longer panics on the sharded stores and no longer
  means "expire immediately" on `TtlSortedCache`. `build()` still rejects a zero TTL, and
  `try_set_ttl(0)` still returns `SetTtlError::ZeroTtl`. For the redis stores a disabled TTL
  writes keys without expiry (a plain `SET`) and the refresh path issues no `EXPIRE`.
- `TtlSortedCache` gains a `set` family in place of `insert` / `insert_ttl` / `insert_evict` /
  `insert_ttl_evict`: `set(k, v)` plus the `set_with(k, v)` entry-setter builder, which chains
  `.ttl(Duration)` / `.ttl_secs(n)` / `.ttl_millis(n)` for a per-entry override and `.evict()`
  for the post-insertion sweep before the terminal `.set() -> Option<V>`. `TtlSortedSetBuilder`
  is re-exported from the crate root.
- `TtlSortedCache`'s get-or-set family no longer removes an expired entry before running the
  initializer, so a cancelled or panicking initializer leaves the expired entry in place and
  fires no `on_evict`; on success `on_evict` fires after the initializer. All four variants now
  agree with each other and with `TtlCache` / `LruTtlCache`.
- `iter_order` / `value_order` on `LruCache`, `LruTtlCache`, and `ExpiringLruCache` return
  `CacheValue`-wrapped values (`Vec<(K, CacheValue<V, M>)>` and `Vec<CacheValue<V, M>>`), one
  shape across the LRU family. `M` is per-entry metadata: `()` for `LruCache` /
  `ExpiringLruCache`, `Option<Instant>` for `LruTtlCache` (read through `CacheValue::expires_at`).
  `LruTtlCache` no longer leaks bare `(Option<Instant>, V)` tuples. `key_order` is unchanged.
- `cache_reset` (and the concurrent counterparts) no longer preserves the preallocated backing
  capacity: it clears and shrinks to `initial_capacity`, so later inserts may reallocate.
  Recreate the cache instead of resetting it to retain the allocation.
- Sharded `copy_from` returns `Result<_, BuildError>` instead of panicking on invalid
  configuration, and the `Eq` marker impls for `UnboundCache` and `LruCache` require `V: Eq`.
- The six sharded types expose inherent `get` / `set` / `remove` / `remove_entry` / `delete` /
  `reset` / `contains` / `peek` returning unwrapped values, so `store.get(&k)` is `Option<V>`
  rather than `Result<Option<V>, Infallible>`. These take call-site priority over the
  `ConcurrentCached*` trait methods, which return `Result<_, Self::Error>`; note that
  `s.set(k, v).unwrap()` therefore compiles as `Option::unwrap` and panics on a first insert.
  Use the `cache_`-prefixed trait methods, or UFCS, for the `Result` shape.

#### Builders

- `capacity(n)` is renamed `initial_capacity(n)` on `UnboundCacheBuilder`, `TtlCacheBuilder`,
  `TtlSortedCacheBuilder`, and `ExpiringCacheBuilder`, where it pre-allocates without bounding
  entry count. The name was ambiguous next to `max_size(n)` on the LRU builders.
- `RedbCache::builder(name)`, `RedisCache::builder(prefix)`, and `AsyncRedisCache::builder(prefix)`
  take the primary required field as a positional argument. The 2.x `::new(` entry points on
  these three types are removed (they returned a builder, conflicting with the convention that
  `new()` returns a ready store); the in-memory and sharded stores gained real
  `Type::new()` / `Type::new(required_field)` constructors.
- `LruTtlCacheBuilder` and `ShardedLruTtlCacheBuilder` take the hasher in the third generic slot
  and the eviction typestate marker last: `LruTtlCacheBuilder<K, V, S = DefaultHashBuilder,
  E = NoEvict>` and `ShardedLruTtlCacheBuilder<K, V, H = DefaultShardHasher, E = NoEvict>`.
  `LruTtlCacheBuilder` had no hasher parameter in 2.x, so a 2.x annotation of
  `LruTtlCacheBuilder<K, V, HasEvict>` names the hasher slot in 3.0 and must gain the hasher as
  the third argument. Code naming only `<K, V>`, or reaching the hasher through `.hasher(..)`,
  is unaffected.
- The redis TTL is optional: omitting `.ttl(...)` stores keys without expiry. A TTL that is set
  must be greater than zero, and `RedisCacheBuildError::MissingRequired("ttl")` is no longer
  returned.
- `RedisCacheBuilder::build()` / `AsyncRedisCacheBuilder::build()` reject an empty prefix with
  `Build(BuildError::InvalidValue { field: "prefix", .. })`. The prefix is what scopes
  `cache_clear` to one logical cache; with an empty prefix, `cache_clear` matched
  `<namespace>:*` and deleted the entries of every cache sharing the namespace.
- `RedbCacheBuilder::build()` validates `cache_name` as a filename component: empty, path
  separators, path-traversal components, and any character invalid in a cross-platform filename
  (`:` `<` `>` `"` `|` `?` `*`, or an ASCII control byte) are rejected rather than silently
  creating subdirectories or escaping the cache directory.
- Builder refresh naming is unified on `refresh_on_hit`: the `refresh()` alias is removed from
  the in-memory TTL builders, and the redis/redb builders' `refresh` is renamed. The
  `#[cached(refresh = true)]` attribute is unchanged.
- `BuildError::InvalidTtl { ttl }` is removed; a zero TTL at build time yields
  `BuildError::InvalidValue { field: "ttl", reason: "must be greater than zero" }`.
  `RedisCacheBuildError::InvalidTtl` and `RedbCacheBuildError::InvalidTtl` become
  `Build(BuildError)`, wrapping the inner error instead of duplicating it.

#### Error and metrics types

- Error enum variants dropped their redundant `Error` suffix:
  `RedbCacheError::{StorageError, CacheDeserializationError, CacheSerializationError}` became
  `{Storage, CacheDeserialization, CacheSerialization}`; `RedbCacheBuildError::ConnectionError`
  became `Storage`; `RedisCacheError::{RedisCacheError, PoolError, CacheDeserializationError,
  CacheSerializationError}` became `{Redis, Pool, CacheDeserialization, CacheSerialization}`.
  `RedbCacheError` / `RedbCacheBuildError` are struct variants (named fields) matching the redis
  enums, and `CacheDeserialization` carries a `cached_value: Vec<u8>` field.
- The public store error enums (`RedbCacheError`, `RedbCacheBuildError`, `RedisCacheError`,
  `RedisCacheBuildError`, `BuildError`, `SetTtlError`, `SetMaxSizeError`) are `#[non_exhaustive]`,
  so external matches need a wildcard arm.
- The redis and redb error types no longer expose `redis::`, `r2d2::`, or `redb::` types through
  public fields or blanket `From` impls. Foreign causes are boxed behind
  `Box<dyn std::error::Error + Send + Sync>` and read through `source()`, so a backing-crate
  version bump is no longer a breaking change to these enums.
- `Return<T>::value` and `Return<T>::was_cached` are private fields
  (`cached_proc_macro_types`). Use `*r` / `r.into_inner()` for the value and `r.was_cached()`
  for the flag; struct pattern matches must switch to the accessors.
- `CacheMetrics.size` is renamed `entry_count` and is now `Option<usize>`, reporting `None` for
  stores whose size is unknown (redis/redb) instead of a false `0`. `CacheMetrics` is
  `#[non_exhaustive]` and derives `Default`, so construct it by mutating
  `CacheMetrics::default()` rather than with a struct literal.
- `RedbCache::remove_expired_entries` returns `Result<usize, RedbCacheError>` (the number
  removed) instead of `Result<(), RedbCacheError>`, matching the `evict` traits.

#### Macros

- The `ttl` attribute takes three mutually exclusive forms: `ttl_secs = N` (whole seconds,
  replacing the 2.x bare-integer `ttl = N`), `ttl_millis = N` (milliseconds, new), and
  `ttl = "<Duration expr>"` (a string-literal Duration expression). The old bare-integer form
  produces an error directing you to `ttl_secs`. Builders gained matching `.ttl_secs(n)` /
  `.ttl_millis(n)` methods ([#149]).
- The deprecated `size` attribute is removed from `#[cached]` / `#[concurrent_cached]` (use
  `max_size = N`; the macros detect `size` and emit a directed error), and the `unbound`
  attribute is removed from `#[cached]` (a bare `#[cached]` already builds an `UnboundCache`).
- `#[cached(refresh = true)]` without a TTL is a compile error; it was previously ignored.
  `#[cached]` also rejects `result_fallback` combined with `with_cached_flag`, and rejects an
  explicit `sync_writes_buckets` when `sync_writes` is not `"by_key"` (the value was accepted
  and silently ignored).
- `#[cached]` / `#[once]` reject the concurrent-store-only attributes (`disk`, `redis`,
  `map_error`, `shards`, `durable`, `disk_dir`, `cache_prefix_block`) with a targeted redirect
  to `#[concurrent_cached]`, and `#[once]` rejects the `#[cached]`-only attributes
  (`result_fallback`, `refresh`, `max_size`, `ty`, `create`, `key`, `convert`, `sync_lock`,
  `unsync_reads`, `sync_writes_buckets`). `#[concurrent_cached]` rejects a custom `ty` without a
  `create` block on the redis and disk paths, an `async` closure for `map_error`, and
  `cache_prefix_block` on the disk path (it is redis-only).
- All three macros reject a `name` starting with `__cached` (the prefix reserved for generated
  bindings) and validate `name` as a Rust identifier.
- `#[concurrent_cached]`'s `refresh` attribute is a plain `bool` (was `Option<bool>`), so
  `refresh = false` no longer conflicts with `expires` or a `create` block.

#### Features and runtime

- Redis TLS is a separate axis ([#231]): `redis_tokio` and `redis_smol` enable the TLS-agnostic
  connection path, so add `redis_tokio_native_tls` / `redis_tokio_rustls` (or the `redis_smol`
  equivalents) to restore TLS. `redis_connection_manager` and `redis_async_cache` are
  capability features depending only on `redis/aio`, so they are runtime-agnostic and must be
  paired with a runtime feature; the connection manager is a per-cache `.connection_manager(true)`
  opt-in rather than a feature that cfg-swapped every cache's connection type.
- The `disk_store` feature is renamed `redb_store`. The `wasm` feature is removed (it gated
  nothing; `web-time` provides wasm-compatible time types transparently), as are `redis_ahash`
  and `async_tokio_rt_multi_thread`.
- The `async` feature no longer implies `tokio`; it pulls only `async-lock`, and
  `cached::async_sync::{Mutex, RwLock, OnceCell}` re-export from `async-lock` instead of
  `tokio::sync` (`OnceCell` there has no `const_new()`). Async `RedbCache` runs blocking redb
  work on the `blocking` crate's thread pool instead of `tokio::spawn_blocking`, and
  `RedbCacheError::BackgroundTaskFailed` is removed. `blocking` is pulled by `redb_store` rather
  than `async`, so redis-only and in-memory async builds do not pay for it.
- Optional dependencies are gated with Cargo's `dep:` syntax, so an optional dependency's name
  is no longer silently usable as a feature; enable the named crate feature instead.
- Redis values are serialized with MessagePack (`rmp-serde`) instead of JSON. Old 2.x JSON
  entries are read transparently and rewritten as MessagePack on their next write. Redis TTLs
  use `PSETEX` / `PEXPIRE`, so sub-second TTLs are honored to the millisecond (requires
  Redis 2.6+).
- Redis key segments are percent-escaped (`:` -> `%3A`, `%` -> `%25`) and the key always has
  three fields (`{namespace}:{prefix}:{key}`), so distinct triples always map to distinct keys;
  an unescaped join previously let `namespace="a:b", prefix=""` collide with `namespace="a",
  prefix="b"`. An empty prefix keeps its separator: `("ns", "", "k")` encodes as `ns::k`. This
  changes the on-wire key for any segment containing `:` or `%`; the value envelope's `version`
  field does not cover key layout, so an old entry is not found after upgrading, is recomputed
  and rewritten at the new key, and the old entry expires on its original TTL.
- `RedisCache::connection_string()` / `AsyncRedisCache::connection_string()` return a
  `ConnectionString` newtype whose `Display` and `Debug` redact credentials; call `.reveal()`
  for the raw URL.
- The `ahash` feature enables `ahash/runtime-rng` on non-wasm targets, seeding hash maps from
  the OS RNG instead of a compile-time seed (hash-flood resistance). wasm32 keeps the
  compile-time seed. No source change required.

### Added

- `cached::prelude` re-exports the common traits plus the `CacheMetrics` struct for a single
  glob import.
- Custom hashers on the non-sharded in-memory stores: `UnboundCache`, `LruCache`, `TtlCache`,
  `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, and `ExpiringLruCache` gained a hasher type
  parameter defaulted to `DefaultHashBuilder` and a `.hasher(s)` builder method, mirroring the
  sharded stores. `DefaultHashBuilder` is re-exported from the crate root.
- `Builder::new()` on all 13 in-memory and sharded builders, matching the IO builders'
  public constructors.
- `CacheValue<V, M = ()>`: the value-plus-metadata wrapper returned by the LRU-family order
  methods, re-exported from the crate root. `Deref<Target = V>`, `PartialEq<V>` against bare
  values, `Display` where `V: Display`, `value()` / `into_value()`, and `expires_at()` when
  `M = Option<Instant>`. `IntoValues::into_values()` bulk-unwraps an `iter_order()` /
  `value_order()` result back into a plain `Vec<V>`. The reverse comparison
  `bare_value == wrapped` cannot be implemented: coherence forbids the blanket impl.
- `retain(keep)` across every in-memory store: `UnboundCache`, `LruCache`, `TtlCache`,
  `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`, and the six sharded
  stores. Every removed entry fires `on_evict`; on the expiry-aware stores expired entries are
  removed regardless of the predicate and every removal counts an eviction. The sharded form
  locks one shard at a time (not atomic across shards), runs the predicate under the shard write
  lock (so it must not re-enter the cache), fires `on_evict` after the lock is released, and
  requires no `K: Clone` bound.
- `ConcurrentCachePeek` and `ConcurrentCachePeekAsync`: side-effect-free `cache_peek` /
  `async_cache_peek` (plus `peek` / `async_peek` aliases) for concurrent stores, with no
  recency promotion, no TTL refresh, no hit/miss metrics, and no lazy removal of expired
  entries. Implemented by the six sharded stores, which also expose an inherent
  `peek(&self, &K) -> Option<V>`. `RedisCache`, `RedbCache`, and `AsyncRedisCache` implement
  neither: peek is an in-memory concept, and for an IO-backed store there is no client-side
  state to skip while the operation remains a full round trip. Both traits are in the prelude.
- `ConcurrentCachedAsyncExt`, a blanket extension trait over `ConcurrentCachedAsync` with ten
  `async_`-prefixed aliases (`async_get`, `async_set`, `async_remove`, `async_remove_entry`,
  `async_delete`, `async_contains`, `async_clear`, `async_reset`, `async_get_or_set_with`,
  `async_try_get_or_set_with`), mirroring `ConcurrentCachedExt`. In the prelude.
- `ConcurrentCached::cache_try_get_or_set_with` and its async counterpart (both provided):
  fallible-init get-or-set returning `Result<Result<V, E>, Self::Error>`, store error outer,
  closure error inner; nothing is stored on a closure `Err`.
  `ConcurrentCachedExt::try_get_or_set_with` is the short alias. `ConcurrentCached` /
  `ConcurrentCachedAsync` also gained defaulted `cache_get_or_set_with` (get-then-set,
  non-atomic) and no-op-default `cache_reset_metrics`.
- Metric and introspection parity: `ConcurrentCacheBase` gained `cache_hits` / `cache_misses` /
  `cache_capacity` / `cache_evictions` and a default `metrics()`, so a generic bound can read a
  sharded store's metrics; `CachedExt` gained `capacity` / `evictions` / `reset`;
  `ConcurrentCachedExt` gained `len` / `is_empty` / `hits` / `misses` / `capacity` /
  `evictions` / `clear` / `reset`; `CachedPeek::peek`, `CloneCached::peek_with_expiry_status`,
  and `ConcurrentCloneCached::{get_with_expiry_status, peek_with_expiry_status}` fill in the
  remaining aliases.
- `Cached::cache_contains` (defaulted, get-based, overridden peek-based by the built-ins) and
  inherent `contains` on the six sharded stores, giving `contains` both spellings on both trait
  families. `CachedExt::contains` delegates to it, so `contains` no longer counts a hit/miss,
  promotes recency, or refreshes TTL, and reports expired entries as absent.
- `ExpiringLruCache::iter_order` / `key_order` / `value_order`, completing LRU-family
  introspection parity, and `TtlSortedCache::capacity() -> Option<usize>`.
- Runtime capacity resizing on the sharded LRU stores: `set_max_size(&self, usize) ->
  Option<usize>` and `try_set_max_size(&self, usize) -> Result<Option<usize>, SetMaxSizeError>`.
  Shrinking evicts LRU-excess entries per shard strictly by recency, fires `on_evict`, and
  counts evictions; resize is not atomic across shards. `LruCache`, `LruTtlCache`, and
  `ExpiringLruCache` gained the same pair ([#180]), and `SetMaxSizeError` (variants
  `ZeroMaxSize` and `CapacityOverflow`) replaces the mix of `BuildError` and `std::io::Error`
  the 2.x resize paths returned. `CacheTtl::try_set_ttl` is the matching strict TTL setter,
  returning `SetTtlError::ZeroTtl`.
- `per_shard_initial_capacity` on the three unbounded sharded builders, the sharded counterpart
  of `initial_capacity`.
- `SerializeCached` / `SerializeCachedAsync` with `cache_set_ref` / `async_cache_set_ref`,
  implemented by `RedisCache` / `AsyncRedisCache` / `RedbCache`, letting serialize-backed stores
  set an entry without taking ownership. `#[concurrent_cached]` calls the borrowed setter for
  any store implementing them, avoiding a value clone per set ([#196], [#195]).
- `RedisCache` / `AsyncRedisCache` implement `cache_clear` / `async_cache_clear` through a
  namespace-scoped `SCAN` + batched `DEL` (O(n), scoped to the cache's prefix, not a server
  flush), with glob metacharacters in the namespace/prefix escaped so they match literally
  ([#200]). `RedisCache` and `AsyncRedisCache` also implement `Clone`; `RedbCache` does not.
- `RedbCache::flush` / `async_flush` force a durable commit, `RedbCache::disk_path()` returns
  the backing file path, and `RedbCache::async_remove_expired_entries` runs the sweep on the
  `blocking` thread pool so it is usable from async contexts.
- `RedisCacheError` / `RedbCacheError` and their build-error siblings expose
  `is_deserialization() -> bool`, so callers can distinguish a codec failure from a storage or
  network error without a full match. `Debug` is implemented for `RedisCache`,
  `AsyncRedisCache`, and `RedbCache`, redacted to namespace/prefix/path/ttl/refresh.
- `PartialEq` / `Eq` for `ExpiringCache` and `ExpiringLruCache`, and `PartialEq` / `Eq` / `Hash`
  for `ConnectionString`. `NoEvict` / `HasEvict` derive `Clone`, `Copy`, `Debug`, `Default` and
  are documented at the crate root.
- `Expires::expires_at(&self) -> Option<Instant>` as a default method returning the value's
  expiry instant when tracked. Advisory only: `is_expired()` remains the authoritative liveness
  check, and existing `impl Expires` blocks get the default for free.
- Macro attributes: `force_refresh` (a block expression over the arguments that bypasses the
  cached value, [#146]), `in_impl = true` for methods inside `impl` blocks including `self`
  receivers ([#16], [#140]), `companions_vis = "<vis>"` to set the generated companions'
  visibility, `companions = false` to suppress the `{fn}_no_cache` / `{fn}_prime_cache`
  companions entirely, and `ttl_millis` (above). `convert`, `create`, `force_refresh`,
  `map_error`, and `cache_prefix_block` accept unquoted Rust in addition to the quoted-string
  form. `map_error` is optional on the disk and redis paths (the generated code uses
  `.map_err(Into::into)?`, so `E: From<RedbCacheError>` / `From<RedisCacheError>`).
  `#[concurrent_cached]` accepts `result_fallback` together with `expires`.
- Macro ergonomics: `#[cached]` / `#[concurrent_cached]` accept reference arguments (`&T`,
  `Option<&T>`) on the default-key path, deriving an owned key without a `convert` ([#202],
  [#203]); the crate root is resolved via `proc-macro-crate`, so a renamed or re-exported
  `cached` dependency works ([#157]); macro-introduced bindings are hygienically named
  `__cached_*`, so arguments named `key`, `cache`, or `result` no longer collide ([#230],
  [#114]); and a generic function without `key` + `convert` produces a clear error ([#80]).
- Compile-time missing-feature guards: `#[cached]` / `#[once]` / `#[concurrent_cached]` on an
  `async fn` without the `async` feature, a TTL attribute without `time_stores`, and
  `#[concurrent_cached(disk = true)]` / `(redis = true)` without `redb_store` / a redis feature
  all name the missing feature instead of surfacing errors from generated internals. A return
  type that does not implement `Clone` produces exactly one error, spanned at the return type.
- `#[doc(alias)]` entries mapping the 2.x store names to their 3.0 types (`SizedCache` ->
  `LruCache`, `TimedCache` -> `TtlCache`, `TimedSizedCache` -> `LruTtlCache`) for docs.rs search.
- The release workflow creates a git tag and GitHub release for each workspace crate that is
  newly published ([#245]), and refuses to publish a half-bumped workspace: `bin/check-versions.sh`
  fails the release when a `cached_proc_macro*` dependency pin disagrees with that subcrate's
  version, or when a stable `cached` would depend on a pre-release subcrate.

### Security

- `sync_writes = "by_key"` bucket selection seeds from a per-static `RandomState` instead of a
  fixed-seed hasher, so an attacker who knows the key space cannot collapse the lock buckets to
  force whole-cache serialization.
- Corrupt or undecodable cached values on the redis/redb `cache_get` path self-heal by default:
  the entry is deleted and the call returns a miss so the cached function recomputes. Opt into
  fail-closed behavior with `.strict_deserialization(true)`.
- Redis credential handling is structural: `resolve_connection_string()` returns a redacting
  `ConnectionString`, the build path constructs sanitized synthetic errors (including the
  `r2d2` pool-build failure and the `NotUnicode` env-var value, which is the connection string
  itself), and `RedisCacheBuilder::connection_pool_connection_timeout` bounds how long `build`
  waits for a connection. The legacy-JSON backward read requires the exact version field value,
  and client-side caching rejects a URL pinning RESP2 (which cannot deliver invalidation
  messages, so accepting it would silently serve stale data).
- redb disk hardening on Unix: the cache directory is created `0700` and the database file
  forced to `0600` on every open (not only at creation); a symlink at the resolved db path or
  at a configured cache directory is rejected before opening; symlink and permission validation
  runs for the XDG default candidates, not only the temp fallback; and a read-only or
  group/world-writable candidate falls back to the temp directory.
- `RedbCacheError::CacheDeserialization` / `RedisCacheError::CacheDeserialization` render their
  `cached_value` bytes as `<N bytes redacted>` in `Debug`, and are documented as potentially
  sensitive.

### Fixed

- `{fn}_prime_cache` no longer deadlocks or blocks readers: it ran the function body while
  holding the cache write lock, so a recursive prime re-locked the same static on the same
  thread (parking_lot is non-reentrant) and any prime blocked every reader for the full
  recompute. The body now runs before the lock is taken.
- `#[cached(result_fallback = true)]` no longer overwrites a newer cached value with a stale
  one. The fallback was captured before the function body ran and written back unconditionally
  on `Err`, so a slow failing call could clobber a value a concurrent call had refreshed, and on
  a TTL store refresh its deadline. The fallback is now read under the same lock the write takes;
  `result_fallback` rejects a non-disabled `sync_writes`, so no caller could serialize the
  window themselves.
- TTL expiry is anchored after the value factory resolves on every get-or-set path across
  `TtlCache`, `LruTtlCache`, and `TtlSortedCache`; several paths anchored before the factory, so
  a factory slower than the TTL produced an already-stale entry. Refreshing an entry under an
  overflowing TTL now clears the deadline, as a fresh insert already did.
- Eviction accounting: the try-path get-or-set no longer fires `on_evict` or counts an eviction
  until the replacement factory succeeds; overwriting an expired entry fires `on_evict` and
  counts uniformly across the timed and sharded stores; a panicking `on_evict` during capacity
  eviction can no longer leave the cache over capacity; `cache_clear_with_on_evict` counts every
  removed entry rather than degrading to a silent `cache_clear` without a callback; and
  `cache_remove` samples expiry once, at removal, so a slow callback cannot turn a live entry
  into a `None` return.
- Sweeps are panic-safe and two-phase (select, remove, count, then notify) across the in-memory
  and sharded stores. `retain` / `evict` previously fired `on_evict` inside the selection scan
  or removed entries eagerly while the user predicate ran, so a panicking predicate could remove
  nothing while having already run cleanup callbacks, or silently drop every entry already
  yielded. The sharded implementation records a `Vec<bool>` of decisions rather than cloned keys,
  so no `K: Clone` bound is added.
- `TtlSortedCache::set_and_get_mut` no longer orphans a map row when the size trim it triggers
  unwinds: the stamp was unlinked from the deadline index and re-inserted after the trim, so a
  panic in between left the entry in the map but invisible to every index-driven sweep while
  still counted by `cache_size()`. `TtlSortedCache::set_with(..).evict()` also performs the
  expiry sweep when `max_size` is configured and the map is under the bound, where the opt-in
  was previously discarded, and `build` reserves with `try_reserve` so a capacity-overflowing
  `max_size` returns `Err(BuildError)` instead of aborting.
- `RedisCache::cache_clear` / `async_cache_clear` decode `SCAN` replies as bytes rather than
  `String`. Redis keys are binary-safe, so a single non-UTF-8 key anywhere in the cache's scope
  aborted the clear permanently: the offending key was never removed, so every retry failed
  identically.
- `RedbCache::cache_set` no longer returns a displaced value that had already expired, and
  `RedbCacheBuilder::build()` returns `RedbCacheBuildError::Storage` instead of panicking when
  the backing file is damaged. A truncated tail is the ordinary result of a full disk or a
  killed process, and the file is a disposable cache, so it must not take the application down.
  (This cannot help under `panic = "abort"`.)
- Read-then-write races closed on both IO stores: redb refresh-on-hit, expiry eviction, and
  `remove_expired_entries` re-read and re-check inside the write transaction, and use a single
  time snapshot for the scan and write passes; redb self-heal re-reads before deleting; and the
  redis self-heal delete is conditional through a Lua script comparing stored bytes, so a
  concurrent valid write racing the read is not discarded.
- `RedbCache` default-directory resolution self-heals a pre-existing cache directory created
  with legacy permissions by an earlier version, which permanently failed the security
  validation. The chmod only succeeds for the owner, so an attacker-owned or symlinked
  directory still falls through to the next candidate.
- Sharded expiry evaluation happens once, under the shard write lock: `ShardedTtlCache`,
  `ShardedLruTtlCache`, `ShardedExpiringCache`, and `ShardedExpiringLruCache` previously
  evaluated a displaced entry's expiry outside the lock or twice, so a value crossing the
  threshold in that window fired `on_evict` without counting the eviction or produced a wrong
  return value. `deep_clone` on the expiring sharded stores reads the hit/miss counters under
  the shard read lock, so cloned metrics match cloned entries.
- `LruCache::cache_reset` uses a fallible allocation path (a grown `max_size` could request a
  `HashMap` capacity past the allocation limit and panic), and internal LRU list pre-allocation
  saturates instead of overflowing.
- Macro correctness: the `#[once]` generic-value-type guard compares whole idents, so
  `fn f<S: Into<String>>(..) -> String` is no longer falsely rejected; a raw-identifier cache
  `name` (e.g. `r#type`) builds a working static instead of panicking; attributes written
  between the macro and the `fn` forward to every generated item, so `#[cfg]` gating stays in
  lockstep; user lint attributes reach the generated `*_prime_cache` companion; and no generated
  `use` places a name in a scope enclosing user code, so a user item named `Cached` or
  `CloneCached` is no longer shadowed.
- Macro attribute errors span the offending attribute rather than the function name, and
  malformed `key` / `convert` / `force_refresh` values produce contextual errors explaining the
  expected syntax instead of a bare `syn` "unexpected token".
- `RedbCacheError`, `RedbCacheBuildError`, `RedisCacheError`, and `RedisCacheBuildError`
  `Display` output includes the underlying cause, which was previously reachable only through
  `Debug` while the source type is documented as not public API.
- `ConcurrentCacheTtl::refresh_on_hit` reflects the configured flag: the concurrent stores
  overrode only `set_refresh_on_hit`, so the getter always reported `false` through trait
  dispatch.
- `Cached for HashMap` no longer requires `S: Default`, so `HashMap<K, V, DefaultHashBuilder>`
  implements `Cached` on wasm.
- docs.rs feature annotations (`doc(cfg)`) on the `async_core`-gated impls and on
  `AsyncRedisCacheBuilder::client_side_caching`, which previously rendered as unconditionally
  available.

### Changed

- The in-memory and sharded stores are faster, with no contract change beyond the behavior
  changes listed above. The sharded stores resolve a read hit in one hash lookup instead of two,
  count evictions per shard rather than through a shared striped counter, and cache the host's
  CPU topology (sampled once per process in a `OnceLock`) instead of probing it on every
  construction. The LRU-family and expiry-aware stores sweep in one pass instead of collecting
  keys first, the TTL stores sample the clock once per operation instead of once per entry
  examined, and `ExpiringCache` is smaller per instance.
- Sharded stores gained an inherent `get_or_set_with` returning `V` directly, so the common case
  needs no trait import or `.unwrap()`.
- `#[must_use]` is applied across the pure-query trait methods (`cache_size` / `len` /
  `is_empty` / `metrics` / `hits` / `misses` / `ttl` / `refresh_on_hit` / ...), the removal
  methods on the concurrent traits, `CacheEvict::evict` / `ConcurrentCacheEvict::evict`,
  `CacheMetrics::hit_ratio`, the order accessors, and the sharded builders. The short
  `remove` / `remove_entry` aliases and the inherent sharded `set` / `remove` are deliberately
  left un-annotated: on the inherent methods the attribute cannot fire on `.unwrap()` (which
  consumes the value) and would fire on correct fire-and-forget calls.
- `Return::set_was_cached` is `#[doc(hidden)]` (macro plumbing); it remains `pub` and callable.
- `KeyedCache` moved under a `#[doc(hidden)] pub mod __private`, so it no longer appears as a
  suggested import when a user references a removed legacy store name.
- `hashbrown` updated to 0.17 (internal). Dev-only: `criterion` 0.8, `googletest` 0.14.
- The published crate manifests no longer carry a `[lints]` table, so a future-toolchain warning
  firing in `cached` cannot break downstream builds, and `specs/`, `local/`, `.cursorrules`, and
  `Makefile` are excluded from the published package.

### Documentation

- The `len` / `cache_size` / `iter` / `evict` contract on lazy-eviction stores is documented in
  one place: `len` returns the stored count without an expiry scan (so it may include expired
  entries), `iter` omits expired entries from the view without removing them, and `evict()`
  reclaims them and yields an accurate live count.
- The sharded inherent-vs-trait return-shape split is documented on all six sharded store types,
  including the UFCS disambiguation and the `.unwrap()` sharp edge.
- The redis on-wire format (positional MessagePack array, `REDIS_VALUE_VERSION`) and the redb
  on-disk format (versioned file name, table name) are documented as stable for the 3.x series
  on the store struct docs; changes bump the embedded version and are reserved for a major
  release.
- The `Arc<T>` return pattern for expensive-to-clone values is documented on the macros: the
  cache stores the `Arc`, and hits clone only the pointer ([#64]).
- New runnable example `examples/resilience.rs` covering `sync_writes = "by_key"`,
  `result_fallback`, and `force_refresh`, plus cache-invalidation ([#21]) and struct-method
  ([#236]) examples.

[#16]: https://github.com/jaemk/cached/issues/16
[#21]: https://github.com/jaemk/cached/issues/21
[#64]: https://github.com/jaemk/cached/issues/64
[#80]: https://github.com/jaemk/cached/issues/80
[#114]: https://github.com/jaemk/cached/issues/114
[#140]: https://github.com/jaemk/cached/issues/140
[#146]: https://github.com/jaemk/cached/issues/146
[#149]: https://github.com/jaemk/cached/issues/149
[#157]: https://github.com/jaemk/cached/issues/157
[#179]: https://github.com/jaemk/cached/issues/179
[#180]: https://github.com/jaemk/cached/issues/180
[#195]: https://github.com/jaemk/cached/issues/195
[#196]: https://github.com/jaemk/cached/issues/196
[#200]: https://github.com/jaemk/cached/issues/200
[#202]: https://github.com/jaemk/cached/issues/202
[#203]: https://github.com/jaemk/cached/issues/203
[#230]: https://github.com/jaemk/cached/issues/230
[#231]: https://github.com/jaemk/cached/issues/231
[#236]: https://github.com/jaemk/cached/pull/236
[#237]: https://github.com/jaemk/cached/issues/237
[#245]: https://github.com/jaemk/cached/issues/245
[rust-lang/rust#100013]: https://github.com/rust-lang/rust/issues/100013

## [2.0.2]
- Docs/tests only (no API change): document the `Expires` trait / `expires = true` as the idiomatic way to set a dynamic, per-entry TTL (a lifetime computed at call time rather than the uniform `ttl = N`), with a runnable example reference, and add a regression test for the runtime-argument-driven TTL case ([#246](https://github.com/jaemk/cached/issues/246)).

## [2.0.1]
- Fix `TtlSortedCacheBuilder`: an explicit `.capacity(n)` is now honored even when `.max_size(m)` is also set. Previously the `max_size`-derived `m + 1` preallocation ran first, and because `HashMap::reserve` never shrinks, a smaller `.capacity(n)` had no effect. The explicit capacity now takes precedence as the preallocation hint while `max_size` continues to bound entry count ([#266](https://github.com/jaemk/cached/issues/266)).

## [2.0.0 / cached_proc_macro 2.0.0]
> **Upgrading from 1.1?** See the [2.0 migration guide](docs/migrations/1.1-to-2.0-human.md).

### Breaking Changes

#### Minimum supported Rust version & edition
- **MSRV raised from 1.80 to 1.85, and the crates moved to the 2024 edition.** Edition 2024 was stabilized in Rust 1.85, so this is the new minimum a downstream project needs to build `cached`. Consumers already on Rust ≥ 1.85 are unaffected; those on 1.80–1.84 must update their toolchain. (The repository's `rust-toolchain.toml` pins the latest stable for local development and CI only — that pin does not propagate to consumers.)

#### Trait API changes
- `Cached::cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>`: new required method on the `Cached` trait that removes an entry and returns the stored key and value. Unlike `cache_remove`, this returns `Some` even when the deleted entry was already expired, making it possible to distinguish "key absent" from "key present but expired". Always fires the store's `on_evict` callback (if set).
- `ConcurrentCached::cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error>`: same semantics on the concurrent trait; implemented for all nine concurrent stores (six sharded plus `DiskCache` / `RedisCache` / `AsyncRedisCache`). The seven non-sharded stores (`UnboundCache`, `LruCache`, etc.) gain `cache_remove_entry` via the `Cached` trait above.
- `Cached::cache_delete<Q>(&mut self, k: &Q) -> bool`: new default method on `Cached` that deletes an entry without returning it; returns `true` if an entry was physically removed (including expired entries), `false` if the key was absent. Implemented via `cache_remove_entry`.
- `DiskCache` and `RedisCache` / `AsyncRedisCache` now require `K: Clone` (in addition to existing bounds) for their `ConcurrentCached` / `ConcurrentCachedAsync` impls, which is needed to return the stored key from `cache_remove_entry`.
- **`ConcurrentCached` / `ConcurrentCachedAsync` mutators now take `&self`** instead of `&mut self`: `set_refresh_on_hit`, `set_ttl`, and `unset_ttl` are defined with a shared receiver, matching the internally-synchronized `&self` contract of the rest of these traits (`cache_set`, `cache_remove`, …). This lets you flip the refresh flag or change the TTL on a shared store (e.g. one behind an `Arc` or a `static`) without exclusive access. Implementors must update their method signatures (`fn set_ttl(&self, …)` etc.); the bundled `DiskCache` / `RedisCache` / `AsyncRedisCache` stores do this via interior mutability (`parking_lot::Mutex` + `AtomicBool`). The single-owner `Cached` and `CacheTtl` traits are unaffected and keep their `&mut self` mutators.
- **`ConcurrentCached::cache_size` / `ConcurrentCachedAsync::cache_size`**: new method `fn cache_size(&self) -> Result<Option<usize>, Self::Error>` reporting the number of entries, with a default of `Ok(None)`. The default makes it non-breaking for existing external implementors and honest for stores that cannot cheaply produce a count: the six sharded stores override it to return `Ok(Some(len))`, while the external-store impls (`DiskCache`, `RedisCache`, `AsyncRedisCache`) keep the `Ok(None)` default because their backends (redb, Redis) expose no O(1) size. Sharded stores also retain their inherent `len()` / `is_empty()` for a non-`Result` count.

#### Macro attribute changes (`#[cached]`, `#[once]`, `#[concurrent_cached]`)
- **`result = true` removed from `#[cached]` and `#[once]`**: All `Result<T, E>` return types now automatically skip caching `Err` values. Remove `result = true` from all `#[cached]` and `#[once]` annotations — the behavior is now the default. To force-cache `Err` values, use the new `cache_err = true` opt-in.
- **`option = true` removed from `#[cached]` and `#[once]`**: All `Option<T>` return types now automatically skip caching `None` values. Remove `option = true` from all `#[cached]` and `#[once]` annotations — the behavior is now the default. To force-cache `None` values, use the new `cache_none = true` opt-in.
- **`#[concurrent_cached]` now supports `Option<T>` returns**: previously only `Result<T, E>` was accepted; `Option<T>` and plain `T: Clone` returns are now natively supported on the default in-memory sharded path. Note: `option = true` was never a recognized attribute on `#[concurrent_cached]` (it was silently ignored in 1.x); the new `cache_none = true` is the explicit opt-in to cache `None` values.
- **`#[cached]` / `#[once]` on `fn() -> Option<T>` without attributes**: previously cached `None` as-is; now skips caching `None`. Add `cache_none = true` to preserve the old behavior.
- **`#[cached]` / `#[once]` on `fn() -> Result<T,E>` without attributes**: previously cached the full `Result`; now skips caching `Err`. Add `cache_err = true` to preserve the old behavior.
- **`result_fallback = true` no longer requires `result = true`**: the explicit `result = true` companion is dropped; `result_fallback` now auto-detects `Result<T,E>` return types.
- **Custom-`ty` users storing `Option<T>` or `Result<T,E>` directly**: if your cache store type holds `Option<T>` or `Result<T,E>` as the value, you must now add `cache_none = true` or `cache_err = true` respectively so the macro uses the full wrapper type rather than extracting the inner `T`.
- **`map_error` on the default in-memory sharded path is now a compile error**: previously `map_error = "…"` was silently accepted and ignored when the store was the infallible default. If you had `map_error` on a `#[concurrent_cached]` that uses no `redis`/`disk`/`ty`/`create`, remove it. If you still need `map_error` (because you are switching to a `redis` or `disk` backend), add the corresponding backend attribute.
- **`result_fallback = true` and `with_cached_flag = true` are mutually exclusive** on `#[concurrent_cached]`: using both together is now a compile error. The combination was never valid — `result_fallback` stores the inner `Ok(T)` value while `with_cached_flag` wraps it in `Return<T>` — but the error was previously inscrutable. Remove one of the two attributes.
- **`cache_none = true` and `with_cached_flag = true` are mutually exclusive** on `#[cached]`, `#[once]`, and `#[concurrent_cached]`: using both together is now a compile error. The combination was never valid — `cache_none = true` stores `Option<T>` as the cached value type while `with_cached_flag = true` stores the inner `T` — but the error was previously a confusing downstream type mismatch. Remove one of the two attributes.

#### Store behavior changes
- **`cache_remove` on expiring stores** now returns `None` for expired-but-present entries. Previously `ExpiringCache`, `ExpiringLruCache`, and expiry-aware sharded stores returned `Some(value)` for an already-expired entry; now returns `None`. The entry is still removed and `on_evict` still fires.
- **`ConcurrentCached::cache_delete`** (and its `ConcurrentCachedAsync` equivalent) now returns `true` for expired-but-physically-present entries. In 1.x the method returned `false` for such entries. Use `cache_remove` if you need to distinguish a live removal from an expired one.
- **`LruCache::retain`** now fires `on_evict` and increments `cache_evictions()` for each removed entry, matching the semantics of `cache_remove`. Previously `retain` was side-effect-free. Internal TTL and expiring wrapper stores (`LruTtlCache`, `ExpiringLruCache`) use a new crate-internal `retain_silent` for their eviction sweeps, so those stores continue to count evictions exactly once.
- **`DiskCacheBuildError` gains a new `InvalidTtl(BuildError)` variant**: any exhaustive `match` on `DiskCacheBuildError` must add an arm for `InvalidTtl`. This variant is returned when a `DiskCacheBuilder` is given a zero-duration TTL.
- **`RedisCacheBuildError` gains a new `InvalidTtl(BuildError)` variant**: same as above for `RedisCacheBuildError`. Returned when a `RedisCacheBuilder` is given a zero-duration TTL.

#### Builder-only construction — `build()` returns `Result`, all store constructors removed
- **Every store is now built exactly one way: `X::builder().…setters….build()?`.** All direct, store-returning constructors are removed — `new`, `with_capacity`, `with_max_size`, `with_ttl`, `with_ttl_and_capacity`, `with_ttl_and_refresh`, `with_max_size_and_ttl`, `with_max_size_and_ttl_and_refresh`, every `try_with_*`, and the sharded `new` / `with_shards` / `with_max_size[_and_shards]` / `with_ttl[_and_shards]` / `with_max_size_and_ttl[_and_shards]` variants — across `UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`, and all six sharded stores. (`DiskCache` / `RedisCache` / `AsyncRedisCache` are unchanged: their `new(...)` / `builder(...)` already return a builder.) This removes the second, panic-prone construction path that duplicated the builder.
- **`Builder::build` now returns `Result<Store, BuildError>` for every in-memory and sharded store.** It previously returned the store directly and panicked on invalid configuration. Add `?` or `.unwrap()`. (Disk/Redis `build()` already returned `Result`; unchanged.)
- **`try_build()` is removed from all builders.** Now that `build()` is the single fallible constructor the alias is redundant — replace every `.try_build()` with `.build()`.
- **`TtlSortedCacheBuilder` gains `.capacity(n)`** — the preallocation hint formerly supplied via `TtlSortedCache::with_ttl_and_capacity`. It is distinct from `.max_size(n)`, which is the eviction bound.
- **Zero TTL is now always rejected.** Because every store is built through its (validating) builder, a zero `Duration` yields `BuildError::InvalidTtl`. The previously-permissive direct constructors (e.g. `TtlCache::with_ttl(Duration::ZERO)`) that accepted a zero TTL no longer exist.

#### `size` → `max_size` naming (builder setter, macro attribute, runtime setters)
- Builder setter `.size(n)` → `.max_size(n)` (LRU-family stores and `TtlSortedCache`). The sharded builders' per-shard cap setter is `per_shard_max_size`.
- The `#[cached]` / `#[concurrent_cached]` **macro attribute `size = N` → `max_size = N`**. The old `size = N` spelling keeps working as a **deprecated alias** that emits a deprecation warning (anchored at the `size` token). Setting both on one annotation is a compile error. See "New macro attributes" under Added below.
- **`TtlSortedCache` runtime max-size setters**: `size_limit(n)` → `set_max_size(n)` and `try_size_limit(n)` → `try_set_max_size(n)` (matching the `set_ttl` runtime-mutator convention). The error type also changed: `try_set_max_size` now returns `Result<Option<usize>, cached::SetMaxSizeError>` instead of `std::io::Result<Option<usize>>`; if you propagate the error with `?` into an `io::Error` context, update the enclosing function's error type or convert explicitly.

### Added

#### New macro attributes
- `max_size = N` attribute for `#[cached]` and `#[concurrent_cached]`: the preferred spelling of the LRU-bound attribute, mirroring the renamed `max_size` builder setter. The original `size = N` attribute continues to work as a **deprecated alias** — using it emits a deprecation warning (anchored at the `size` token) steering you to `max_size`. Specifying both `size` and `max_size` on the same annotation is a compile error.
- `cache_err = true` attribute for `#[cached]`, `#[once]`, and `#[concurrent_cached]`: opt-in to also cache `Err` values from `Result<T, E>` returns (requires a `Result<T, E>` return type; mutually exclusive with `result_fallback`).
- `cache_none = true` attribute for `#[cached]`, `#[once]`, and `#[concurrent_cached]`: opt-in to also cache `None` values from `Option<T>` returns (requires an `Option<T>` return type).
- `result_fallback = true` support for `#[concurrent_cached]`: on an `Err` return, the last cached `Ok` value for the same key is returned instead. The stale value is kept in the primary cache slot (via `ConcurrentCloneCached::cache_get_with_expiry_status`) and re-cached with a fresh TTL window on `Err`; no separate fallback store is created. Requires a TTL (`ttl`/`ttl_secs`/`ttl_millis`) (a compile error is emitted otherwise). Restricted to the default in-memory sharded path (not redis/disk). Mutually exclusive with `cache_err` and `with_cached_flag`.

#### New sharded in-memory cache stores
- Add six fully-concurrent, sharded in-memory cache stores: `ShardedCache<K,V>` (unbounded), `ShardedLruCache<K,V>` (LRU), `ShardedTtlCache<K,V>` (TTL, requires `time_stores`), `ShardedLruTtlCache<K,V>` (LRU + TTL, requires `time_stores`), `ShardedExpiringCache<K,V>` (per-value expiry, unbounded), and `ShardedExpiringLruCache<K,V>` (per-value expiry, LRU-bounded). All six wrap an `Arc` (cheap clone, `Send + Sync`), use power-of-two per-shard `parking_lot::RwLock`s with cache-line-padded shard structs to eliminate false sharing, and support builder APIs with `on_evict` callbacks, `copy_from` for live resharding, and `metrics()` / `shard_sizes()` for observability. Shard routing uses the `ShardHasher<K>` trait (default: `DefaultShardHasher` backed by ahash) as a zero-overhead type parameter, allowing custom partition logic without runtime overhead.
- `#[concurrent_cached]` now defaults to an in-memory sharded store when `redis = true` and `disk = true` are both absent and no custom `ty`/`create` is provided. Macro attributes `max_size = N`, `ttl = T`, `shards = S`, and `expires = true` select the matching variant. `map_error` must not be specified on this path — the stores are `Infallible` and have no errors to map (supply `redis = true`, `disk = true`, or a custom `ty`/`create` to use a fallible store).
- `#[concurrent_cached]` on the default in-memory sharded stores now accepts plain return types — any `T: Clone`, `Option<T>`, or `Result<T, E>`. `redis`, `disk`, and custom `ty`/`create` stores still require `Result<T, E>`.
- Add `expires = true` attribute support to `#[concurrent_cached]` macro to automatically select `ShardedExpiringCache` (unbounded) or `ShardedExpiringLruCache` (LRU-bounded when `max_size` is also set).
- `ShardedExpiringCache` and `ShardedExpiringLruCache` require cached values to implement the `Expires` trait; `copy_from` skips entries already reporting `is_expired() == true`. Both expose `deep_clone` for snapshot copies.

#### Other additions
- Add `cache_clear_with_on_evict()` to all six sharded stores (`ShardedCache`, `ShardedLruCache`, `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`): fires the `on_evict` callback for every removed entry when a callback is configured, and (where applicable) increments the evictions counter (`ShardedCache` is unbounded and has no evictions counter). The plain `clear()` inherent method remains fast and side-effect-free; `cache_clear_with_on_evict()` is the opt-in alternative.
- Add `cache_clear_with_on_evict()` to all seven non-sharded stores (`UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`, `ExpiringCache`, `ExpiringLruCache`, `TtlSortedCache`): fires the `on_evict` callback for every removed entry and (where applicable) increments the evictions counter. The plain `cache_clear()` method remains fast and side-effect-free; `cache_clear_with_on_evict()` is the opt-in alternative.
- Add `StripedCounter` — a 16-slot cache-line-padded atomic counter — for hit/miss metrics on `UnboundCache` and `TtlSortedCache` to reduce false sharing under concurrent `cache_get_read`. All other stores continue to use plain `AtomicU64`.
- Add `ConcurrentCloneCached<K, V>` trait: concurrent analogue of `CloneCached` for the four expiry-capable sharded stores (`ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`). Provides `cache_get_with_expiry_status(&self, key: &K) -> (Option<V>, bool)` — returns the value without removing expired entries, enabling `result_fallback` to fall back to stale values in-place. Takes `&self` (not `&mut self`) since sharded stores are internally synchronized.
- Add API consistency aliases: `Cached::{get,set,remove,remove_entry,delete}` and `ConcurrentCached::{get,set,remove,remove_entry,delete}` delegate to the existing `cache_*` methods (the sync `Cached` trait gains `remove_entry` / `delete` to match `ConcurrentCached`); both the sharded and non-sharded TTL builders expose `.refresh_on_hit(...)` as the primary setter with `.refresh(...)` retained as an alias; `DiskCache`, `RedisCache`, and `AsyncRedisCache` expose `::builder(...)` aliases (alongside their existing `::new(...)` builder entry points). Note: `DiskCache::new(...)` / `RedisCache::new(...)` / `AsyncRedisCache::new(...)` are **builder** entry points -- they return a builder, not a ready-to-use store -- and are intentionally retained; only the in-memory and sharded store constructors that returned stores directly were removed.
- Add an inherent `capacity()` getter to `LruCache`, `LruTtlCache`, and `ExpiringLruCache` — and to their sharded counterparts `ShardedLruCache`, `ShardedLruTtlCache`, and `ShardedExpiringLruCache` — that returns the configured max-entry bound (distinct from `cache_size()`, which returns the current live entry count).
- Add `BuildError::InvalidTtl { ttl }` variant for a single consistently-worded zero-TTL rejection path across all builders.
- Document on `ConcurrentCachedAsync` that `get`/`set`/`remove`/`delete` short aliases are intentionally absent to avoid worsening method-resolution ambiguity.

### Fixed
- Unify zero-TTL validation across all TTL-capable store builders: `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ShardedTtlCache`, `ShardedLruTtlCache`, `DiskCache`, `RedisCache`, and `AsyncRedisCache` builders now all call the shared `validate_ttl` helper and return `BuildError::InvalidTtl { ttl }`. With construction now builder-only, a zero TTL is uniformly rejected at build time (there is no longer a permissive direct-constructor path).
- Make the generated `#[concurrent_cached]` in-memory `Infallible` error shim map into the function's declared `Result<_, E>` error type, reject invalid store-selection attributes, and use UFCS for generated `ConcurrentCached` calls so sync functions compile even when both concurrent traits are in scope.
- Implement `CacheEvict` for `ShardedTtlCacheBase` and `ShardedLruTtlCacheBase`, make sharded builders return `BuildError` instead of panicking on capacity/shard overflows, avoid unnecessary `'static` bounds when building `ShardedLruTtlCache` without `on_evict`, optimize `ShardedTtlCacheBase` hits under `refresh_on_hit` by bypassing read-locks, and correct the sharded LRU capacity documentation.
- Fix timed-store eviction sweeps to use the crate's configured `Instant` type.
- Optimize `TtlSortedCache::cache_get` and `cache_get_mut` live hits to use a single hash-map lookup.
- Unify `cache_remove` semantics: removing any present entry now fires the store's `on_evict` callback (if set) and increments `evictions`.
- Tighten `#[concurrent_cached]` return-type classification so generic plain return types like `HashMap<K, V>` are not mistaken for `Result` aliases.
- Tighten `Result`-return detection in all three macros to require the exact identifier `Result` rather than matching any identifier that ends with `"Result"`. Type aliases such as `type MyResult<T> = Result<T, E>` are now treated as plain values (their `Err` variant is cached). Only the literal `Result<T, E>` and its fully-qualified forms (e.g. `std::result::Result<T, E>`) continue to trigger skip-on-`Err` / `result_fallback` semantics. This aligns with the existing `Option`-detection behavior and makes the macro surface consistent.
- Pass the stored key (via `remove_entry`) rather than the lookup key to `on_evict` in `ShardedTtlCache::cache_remove` and `ShardedExpiringCache::cache_get` / `cache_remove`.
- `#[concurrent_cached]` now rejects `map_error` on the default in-memory sharded path with a compile error — the stores are `Infallible` and accepting `map_error` while silently ignoring it was misleading. Previously `map_error` on this path was accepted and the infallible path emitted `.expect(…)` regardless.
- Remove redundant `.clone()` on the `#[concurrent_cached]` cache-hit return path for all three return-type variants.
- Fix `#[concurrent_cached(with_cached_flag = true)]` on the default in-memory path for plain `cached::Return<T>` returns.
- Extend `build()` panic messages on all sharded stores to include the underlying `BuildError` detail.
- Fix `ShardedLruTtlCacheBase::evict()` to remove expired inner entries without calling `cache_remove`, preventing double-counting of evictions and double-firing of `on_evict`.
- Fix `Cached::cache_delete` (now on `Cached` via `cache_remove_entry`) correctly returns `true` for entries that were present but already expired; previously `cache_delete` on `ConcurrentCached` returned `false` for expired entries.

## [1.1.0 / cached_proc_macro 1.1.0]

### Added
- Add `ExpiringCache` (and `ExpiringCacheBuilder`) as a size-unbounded store where each value implements the `Expires` trait and determines its own expiration.
- Add `expires = true` attribute to the `#[cached]` procedural macro: automatically selects `ExpiringCache` (unbounded) or `ExpiringLruCache` (LRU-bounded when `size` is also set), so the return type controls its own expiry via `Expires`. Compatible with `result`, `option`, `result_fallback`, `sync_writes`, `key`/`convert`, and `size`. Mutually exclusive with `ttl`, `ty`, `create`, `with_cached_flag`, `unsync_reads`, `refresh`, and `unbound`.
- Add support for the `expires = true` attribute in the `#[once]` procedural macro to allow single-value functions to utilize value-defined expiration (`Expires` trait).
- Add comprehensive unit tests in `src/stores/expiring_lru.rs` covering the `Expires` trait and `ExpiringLruCache`'s `CachedIter::iter` expired-filtering, `Clone`, `std::fmt::Debug`, `cache_remove`, and `cache_clear`.
- Implement `std::fmt::Debug` and `Clone` for `TtlSortedCache` (and its internal `Entry` type) and `ExpiringCache` to ensure full `Debug`/`Clone` trait parity across all 7 core in-memory store types.
- Add robust unit tests across all remaining core cache stores (`UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`, `TtlSortedCache`) verifying `Debug` and `Clone` trait behaviors; `UnboundCache` and `LruCache` also verify `PartialEq` and `Eq`.
- Add comprehensive validation unit tests for each store builder's fallible `try_build()` methods (asserting expected `BuildError` outcomes for invalid capacities, sizes, or missing required attributes like `ttl`).
- Add unit tests validating the `std::fmt::Display` representation for all `BuildError` variants in `src/stores/mod.rs`.
- Add standardized micro-benchmarks (`benches/cache_benches.rs`) for cache hits across all 7 core in-memory stores (`UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`, `ExpiringLruCache`, `ExpiringCache`, `TtlSortedCache`), cache misses & inserts, eviction capacity overhead, and `RwLock` lock-synchronization (with and without `CachedRead::cache_get_read` unsynchronized reads).
- Add new `bench` target to the `Makefile` to run the benchmark suite.
- Add standard, runnable example `examples/expires_per_key.rs` demonstrating how to use the `Expires` trait with `ExpiringLruCache` and `ExpiringCache` for per-value expiration, including keyed caching via `#[cached(expires = true)]` and single-value caching via `#[once(expires = true)]`.
- Add detailed library-level documentation and quickstart example for `Expires`, `ExpiringCache`, and `ExpiringLruCache` to `src/lib.rs` (automatically synced to `README.md`).

## [1.0.0 / cached_proc_macro 1.0.0 / cached_proc_macro_types 1.0.0]
> **Upgrading from 0.x?** See the [1.0 migration guide](docs/migrations/0.x-to-1.0-human.md)
> for a complete walkthrough of every breaking change (and an
> [agent-oriented version](docs/migrations/0.x-to-1.0.md) for automated tooling).
## Added
- Add comprehensive async integration tests in `tests/cached.rs` for `CachedAsync` methods on `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringLruCache`, and `UnboundCache` to assert correct `on_evict` invocation on expired lookups.
- Add `make help` and `make check/help` targets for documenting and validating
  supported Makefile commands.
- Add fallible `try_build` methods to `TtlCacheBuilder` and `ExpiringLruCacheBuilder`.
- Re-export `TtlSortedCacheError` at the crate root (and via `cached::stores`) so users can
  name and match on the error returned by `TtlSortedCache::cache_try_set`.
- `ExpiringLruCache::store()` accessor (mirroring `LruTtlCache::store()`) for advanced
  introspection of the inner `LruCache`.
- Add `ConcurrentCached::cache_delete` and `ConcurrentCachedAsync::cache_delete` for deleting
  entries without decoding or returning the previous value.
- `CachedPeek` trait: non-mutating cache lookups that skip recency updates, TTL refresh, and hit/miss metrics
- `CachedRead` trait: shared-reference reads for stores with no read-side mutation; used by `unsync_reads`
- `CacheEvict` trait: explicit `evict()` method to sweep expired entries from all timed/expiring stores
- `unsync_reads = true` option for `#[cached]`: uses a read lock on the cache-hit path instead of a write lock; requires the store to implement `CachedRead` (supported by `UnboundCache`, `TtlSortedCache`, `HashMap`, and custom stores that implement `CachedRead`)
- `on_evict(|k, v| { ... })` eviction callbacks on all in-memory stores (`LruCache`, `TtlCache`, `LruTtlCache`, `ExpiringLruCache`, `TtlSortedCache`)
- `::builder()` constructor APIs for all in-memory stores
- `cache_evictions()` metric on all stores that support eviction
- `ConcurrentCachedAsync` is now implemented for `DiskCache`; `#[concurrent_cached(disk = true)]`
  on an `async fn` runs all `sled` I/O on `tokio`'s blocking pool via `spawn_blocking` instead
  of blocking the async runtime. Adds the `DiskCacheError::BackgroundTaskFailed` variant
  returned if that blocking task is cancelled or panics.
- `#[cached]`, `#[once]`, and `#[concurrent_cached]` are now re-exported at the crate root
  (`use cached::cached;` works), alongside the existing `cached::macros::*` path.
- `DiskCacheBuildError`, `DiskCacheBuilder`, `RedisCacheBuildError`, `RedisCacheBuilder`, and
  `AsyncRedisCacheBuilder` are now re-exported at the crate root, matching the in-memory
  `*Builder` re-exports — the error type returned by `DiskCache`/`RedisCache` `build()` is now
  nameable via the same path the cache type came from.
## Changed
- Make LRU-backed `try_build` paths consistently use fallible allocation helpers
  instead of panicking constructors.
- Optimize `TtlCache`, `LruTtlCache`, and `ExpiringLruCache` to perform exactly one lookup (O(1)) on hit paths for `cache_get`, `cache_get_mut`, and `cache_get_with_expiry_status` by inlining expiration status checks.
- **Breaking:** `LruCache::try_with_size` and `LruTtlCache::try_with_size_and_ttl` now return `Result<_, BuildError>` directly instead of `std::io::Result` as a hard breaking change, aligning them with modern Builder pattern construction.
- `TtlSortedCache::set_ttl` now returns `Option<Duration>` (previously `Duration`) to match
  `CacheTtl::set_ttl` and the `set_ttl` of every other timed store.
- `LruCache`, `LruTtlCache`, and `ExpiringLruCache` `cache_reset` implementations now
  rebuild their backing stores instead of only clearing entries.
- `DiskCache::cache_get` now returns deserialization errors for corrupted entries instead of
  treating them as cache misses.
- `DiskCache::remove_expired_entries` now reports storage and deserialization errors encountered
  while sweeping instead of ignoring them.
- Fix timed `#[once]` caches so TTL starts after the function body finishes executing.
- Improve macro diagnostics for `result_fallback` without `result = true` and for
  `with_cached_flag` return types whose names merely contain `Return`.
- Fix `ExpiringLruCache::cache_capacity` to report `Some(capacity)` (was falling
  through to the `Cached` default `None`, so `metrics().capacity` was inaccurate
  for the only size-bounded store that didn't override it).
- `RedisCache`, `RedisCacheBuilder`, `AsyncRedisCache`, and `AsyncRedisCacheBuilder`
  now use a fn-pointer `PhantomData<fn() -> (K, V)>` so the cache type is
  unconditionally `Send + Sync` regardless of whether `K`/`V` are. Dropped the
  `V: Sync` bound from `impl AsyncRedisCache` and `impl ConcurrentCachedAsync
  for AsyncRedisCache` (values cross the async boundary by value, never by
  shared reference). A value that is `Send` but `!Sync` (e.g. one containing a
  `Cell`) — previously rejected because the macro-emitted
  `LazyLock<RedisCache<_, V>>` / `OnceCell<AsyncRedisCache<_, V>>` static
  required the cache type to be `Sync` (`PhantomData<(K, V)>` propagated
  `V: Sync`), and the async path additionally had `V: Send + Sync` on the
  trait/inherent impls — is now accepted. Mirrors the async `DiskCache`
  relaxation.
- `#[concurrent_cached]` now structurally requires the function return to be a
  `Result` (last path segment named `Result`). Previously `Option<T>` / `Vec<T>`
  / bare `T` returns passed the attribute check and produced a confusing error
  inside the generated body; they now fail with a clean spanned diagnostic
  pointing at the return type. Proc-macro token-only limitation: a `Result`
  *type alias* renamed away from `Result` is not recognized (same as
  `with_cached_flag`/`Return`).
- **Breaking:** `#[concurrent_cached]` now rejects every store-builder attribute
  (`ttl`, `refresh`, `cache_prefix_block`, `disk_dir`, `connection_config`,
  `sync_to_disk_on_cache_change`) when a `create` block is supplied, with a
  single unified message naming each offender. Previously only `ttl`/`refresh`
  (and `cache_prefix_block` for the redis/custom branches) were rejected, so
  `disk_dir`/`connection_config`/`sync_to_disk_on_cache_change` paired with
  `create` were silently ignored — a real footgun (the user thought their disk
  path / durability was applied when it was not). Move the dropped attrs into
  your `create` block, or remove them.
- **Breaking:** `#[cached]` likewise rejects its store-builder attributes
  (`ttl`, `ttl_millis`, `max_size`, `unbound`, `refresh`) when a `create` block
  is supplied, with the same unified message, mirroring `#[concurrent_cached]`.
  Previously `refresh` paired with `create` was silently ignored. Move the
  dropped attrs into your `create` block, or remove them.
- `CacheEvict::evict` now returns the number of expired entries removed, matching the existing
  `TtlSortedCache` behavior.
- Fix `DiskCache::cache_get` refreshes to return serialization errors instead of panicking when
  refreshed values cannot be serialized.
- Fix `DiskCache::cache_set` to return the raw previous value at a key, matching the
  `ConcurrentCached` trait contract and Redis behavior.
- Fix `LruTtlCache` expired lookups so they do not promote expired entries or inflate the
  inner `LruCache` hit/miss metrics.
- Fix `ExpiringLruCache::cache_get` and `cache_get_mut` to use `peek_by_key` +
  `move_to_front_by_key` instead of routing through `LruCache::cache_get`, which was
  inflating the inner store's hit counter on every successful lookup.
- Fix `ExpiringLruCache::cache_get_mut` to fire `on_evict` callbacks and increment eviction
  metrics when an expired entry is removed.
- Redis TTL handling now rejects only zero durations, rounds sub-second non-zero TTLs up to one
  second, and avoids overflowing refresh expirations.
- **Breaking:** Redis cache key format changed from raw concatenation (`{namespace}{prefix}{key}`)
  to colon-delimited joining with empty-segment skipping (`{namespace}:{prefix}:{key}`).
  Existing Redis caches built against pre-1.0 versions will see cache misses on upgrade because
  stored keys will no longer match. The default namespace (`cached-redis-store:`) is trimmed of
  its trailing colon and re-joined, so the effective change for default-namespace users is that
  the prefix and key are now separated by `:` (e.g. `cached-redis-store:my_prefixmy_key` →
  `cached-redis-store:my_prefix:my_key`).
- `LruTtlCache` validation errors now use `ErrorKind::InvalidInput` instead of raw OS error
  codes.
- Improve `#[cached(unsync_reads = true)]` diagnostics for generated sized/timed stores and
  convert several `#[concurrent_cached]` macro panics into spanned compile errors.
- Fix `LruTtlCache` and `ExpiringLruCache`: `on_evict` callbacks and eviction counts now correctly fire when `cache_get_or_set_with` replaces an expired entry (previously the displaced value was silently discarded)
- Fix `ExpiringLruCache::cache_get`: expired entries are now removed on access instead of being promoted to most-recent in the LRU, which was causing live entries to be evicted ahead of expired ones
- Fix `TtlSortedCache`: size-limit validation now returns `ErrorKind::InvalidInput` instead of `from_raw_os_error(22)`
- Fix `HashMap` `CachedPeek`/`CachedRead` impls: removed spurious `S: Default` bound (only the `Cached` impl requires it)
- Expanded `make tests` matrix with explicit `no-default`, `proc_macro`-only, `time_stores`, `async`, `disk_store`, and `redis` feature combinations
- **Breaking:** `redis_connection_manager` no longer implies `redis_tokio`. It now implies `async`
  and `redis_store` plus the `redis/tokio-comp` and `redis/connection-manager` redis features —
  giving you the Tokio async runtime and the connection manager without pulling in TLS. Users who
  need TLS should add `redis_tokio` (native-tls) or configure TLS via the `redis` crate directly.
## Removed
- **Breaking:** Completely removed the unused internal `Status` enum from `cached::stores` (it was previously returned by an internal helper which has been inlined/eliminated).
- **Breaking:** Removed declarative macros (`cached!`, `cached_key!`, `cached_result!`,
  `cached_key_result!`, `cached_control!`) and the `macros` module that contained them.
  Use the `#[cached]`, `#[once]`, and `#[concurrent_cached]` procedural macros instead.
- **Breaking:** The procedural macro re-export module has been renamed from `proc_macro` to
  `macros`. Update `use cached::proc_macro::cached` to `use cached::macros::cached`
  (and similarly for `once`; the `io_cached` macro was additionally renamed — see below).
- **Breaking:** Renamed the `IOCached`/`IOCachedAsync` traits to
  `ConcurrentCached`/`ConcurrentCachedAsync`, and the `#[io_cached]` proc macro to
  `#[concurrent_cached]` (`cached::macros::io_cached` → `cached::macros::concurrent_cached`).
  The contract is unchanged — the names no longer imply "IO", since a self-synchronizing
  in-memory store is equally valid. Update `impl IOCached for`/`use cached::IOCached` and
  every `#[io_cached(...)]` attribute accordingly.
- **Breaking:** Removed `InMemoryAdapter<K, V, C>`. It only wrapped a `Cached` store in a
  single `parking_lot::Mutex`, which is strictly worse than `#[cached]` for the macro path
  (double locking) and trivially hand-rolled for the rare generic-bridge case. Use
  `#[cached]`/`#[once]` for in-memory memoization, or implement `ConcurrentCached` directly.
- The example files `basic_proc_macro` and `kitchen_sink_proc_macro` have been renamed to
  `basic` and `kitchen_sink` respectively.
- **Breaking:** Renamed `CanExpire` trait to `Expires`. Update `use cached::CanExpire` to
  `use cached::Expires` and all `V: CanExpire` bounds to `V: Expires`.
- **Breaking:** IO store builder methods drop the `set_` prefix to match in-memory builder style:
  - `DiskCacheBuilder`: `set_ttl` → `ttl`, `set_refresh` → `refresh`,
    `set_disk_directory` → `disk_directory`,
    `set_sync_to_disk_on_cache_change` → `sync_to_disk_on_cache_change`,
    `set_connection_config` → `connection_config`
  - `RedisCacheBuilder` / `AsyncRedisCacheBuilder`: `set_lifespan` → `ttl`,
    `set_refresh` → `refresh`, `set_namespace` → `namespace`, `set_prefix` → `prefix`,
    `set_connection_string` → `connection_string`,
    `set_connection_pool_max_size` → `connection_pool_max_size`,
    `set_connection_pool_min_idle` → `connection_pool_min_idle`,
    `set_connection_pool_max_lifetime` → `connection_pool_max_lifetime`,
    `set_connection_pool_idle_timeout` → `connection_pool_idle_timeout`,
    `set_client_side_caching` → `client_side_caching` (async only);
    the internal resolver `connection_string` → `resolve_connection_string`
    (the setter now owns the bare name).
- **Breaking:** Removed all `#[deprecated]` shim methods: `LruCache::with_capacity`,
  `TtlSortedCache::ttl_millis`, `DiskCacheBuilder::set_lifespan`.
- **Breaking:** Removed `cache_ttl`, `cache_set_ttl`, and `cache_unset_ttl` from
  the `Cached` trait. Use `CacheTtl::ttl`, `set_ttl`, and `unset_ttl` on timed
  stores instead.
- **Breaking:** Renamed IO-backed TTL/refresh methods to match `CacheTtl`:
  `cache_ttl` → `ttl`, `cache_set_ttl` → `set_ttl`, `cache_unset_ttl` → `unset_ttl`,
  `cache_set_refresh` → `set_refresh_on_hit`.
- **Breaking:** Renamed inherent timed-store refresh accessors:
  `TtlCache::refresh` → `refresh_on_hit`, `TtlCache::set_refresh` → `set_refresh_on_hit`,
  `LruTtlCache::refresh` → `refresh_on_hit`, `LruTtlCache::set_refresh` → `set_refresh_on_hit`.
- **Breaking:** `get_store()` → `store()` on `TtlCache`, `LruTtlCache`, and `UnboundCache`
  (follows Rust API Guidelines C-GETTER).
- **Breaking:** `TtlSortedCache::get_borrowed` removed; `get` is now generic
  (`get<Q>(&self, key: &Q) where K: Borrow<Q>`) so `cache.get("key")` and
  `cache.get(slice)` work directly.
- **Breaking:** `TtlSortedCache`'s inherent `remove(&K)` / `clear()` / `len()`
  / `is_empty()` / `get<Q>(&self, ...)` methods removed — they shadowed the
  same-named `Cached` short aliases without adding behavior. Bring `Cached`
  into scope and use the trait short aliases (`cache.remove(&k)` etc.) or the
  canonical `cache_*` forms. The inherent `get` was the only one with a
  semantic difference: it was `&self` and **did not** evict expired entries on
  access (the trait `Cached::get` requires `&mut self` and *does* — it
  delegates to `cache_get`, which removes expired entries on access in this
  store). To preserve the previous `&self` non-evicting read behavior, use
  [`CachedRead::cache_get_read`](https://docs.rs/cached/latest/cached/trait.CachedRead.html)
  or `CachedPeek::cache_peek`. Both already implemented by `TtlSortedCache`.
- **Breaking:** Renamed `CachedAsync::get_or_set_with` → `async_get_or_set_with` and
  `CachedAsync::try_get_or_set_with` → `async_try_get_or_set_with`. The old names collided
  with the same-named `Cached` convenience methods (the in-memory stores implement both
  traits), so any call with both traits in scope (e.g. `use cached::*;`) failed to compile
  with `E0034`. The `#[cached]`/`#[once]` macros are unaffected — they call the canonical
  `cache_*` methods.
- Fix rustdoc links so documentation builds cleanly with warnings denied across
  feature combinations.

## [0.59.0 / [cached_proc_macro[0.27.0]]]
## Added
## Changed
- Fix `examples/wasm` build: add `time_stores` feature to the `cached` dependency (required when using `default-features = false` with `TimedCache`)
## Removed

## [0.58.0]
## Added
- Add `redis_async_cache` feature for Redis client-side caching support via the RESP3 protocol
## Changed
- Update `redis` to 1.0
## Removed

## [0.57.0 / [cached_proc_macro[0.26.0]]]
## Added
- Add `parking_lot` dependency
## Changed
- Switch to `parking_lot`'s `Mutex` and `RwLock` in all macros.
- Remove `unwrap()` calls from lock operations.
## Removed

## [0.56.0 / [cached_proc_macro[0.25.0]]]
## Added
## Changed
- *BREAKING* All timed/expiring caches now use std::time::Duration values instead of raw seconds/millis.
- Update `redis` to 0.32
- Update `hashbrown` to 0.15
## Removed

## [0.55.1 / [cached_proc_macro[0.24.0]]]
## Added
- Add `sync_writes = "by_key"` support to `#[cached]`
## Changed
- Update `redis` to 0.29.0
- Update `directories` to 6.0
- Update `thiserror` to 2.0
- With the `sync_writes = "by_key"` addition, the argument values changed from a boolean
  to strings. The equivalent of `sync_writes = true` is now `sync_writes = "default"`
## Removed

## [0.54.0]
## Added
- Add `Cached::cache_try_get_or_set_with` for parity with async trait
## Changed
- Remove unnecessary string clones in redis cache store
- Update cargo default features manifest key
## Removed

## [0.53.1 / [cached_proc_macro[0.23.0]]]
## Added
## Changed
- Replace `instant` with `web_time` in proc macro, update cached_proc_macro version
## Removed

## [0.53.0]
## Added
## Changed
- Replace unmaintained `instant` crate with `web_time`
## Removed

## [0.52.0 / [cached_proc_macro[0.22.0]] ]
## Added
## Changed
- Propagate function generics to generated inner cache function 
## Removed


## [0.51.4]
## Added
## Changed
- Update `DiskCache` to require `ToString` instead of `Display`
## Removed

## [0.51.3]
## Added
- `ExpiringSizedCache`: Allow specifying explicit TTL when inserting
## Changed
- Refactor `ExpiringSizedCache` internals to not require tombstones
- `ExpiringSizedCache` keys must impl `Ord`
- `ExpiringSizedCache` `remove` and `insert` updated to return only unexpired values
## Removed

## [0.51.2]
## Added
- Add `get_borrowed` methods to `ExpiringSizedCache` to support cache retrieval using `&str` / `&[T]`
  when the key types are `String` / `Vec<T>`. This is a workaround for issues implementing `Borrow`
  for a generic wrapper type.
## Changed
## Removed

## [0.51.1]
## Added
- Update documentation and add missing methods to `ExpiringSizedCache` (clear, configuration methods)
## Changed
- `ExpiringSizedCache`: When allocating using `with_capacity`, allocate enough space to account for
  the default max number of tombstone entries
## Removed

## [0.51.0]
## Added
- Add `ExpiringSizedCache` intended for high read scenarios. Currently incompatible with the cached trait and macros.
## Changed
## Removed

## [0.50.0 / [cached_proc_macro[0.21.0]] ]
## Added
- Add `DiskCacheBuilder::set_sync_to_disk_on_cache_change` to specify that the cache changes should be written to disk on every cache change.
- Add `sync_to_disk_on_cache_change` to `#[io_cached]` to allow setting `DiskCacheBuilder::set_sync_to_disk_on_cache_change` from the proc macro.
- Add `DiskCacheBuilder::set_connection_config` to give more control over the sled connection.
- Add `connection_config` to `#[io_cached]` to allow setting `DiskCacheBuilder::set_connection_config` from the proc macro.
- Add `DiskCache::connection()` and `DiskCache::connection_mut()` to give access to the underlying sled connection.
- Add `cache_unset_lifespan` to cached traits for un-setting expiration on types that support it
## Changed
- [Breaking] `type` attribute is now `ty`
- Upgrade to syn2 
- Corrected a typo in DiskCacheError (de)serialization variants
- Signature or `DiskCache::remove_expired_entries`: this now returns `Result<(), DiskCacheError>` instead of `()`, returning an `Err(sled::Error)` on removing and flushing from the connection.
## Removed

## [0.49.3]
## Added
## Changed
- Fix `DiskCache` expired value logic
## Removed

## [0.49.2]
## Added
## Changed
- While handling cache refreshes in `DiskCache::cache_get`, treat deserialization failures as non-existent values
## Removed

## [0.49.1]
## Added
## Changed
- Fix `DiskCache::remove_expired_entries` signature
## Removed

## [0.49.0 / [cached_proc_macro[0.20.0]] ]
## Added
- Add DiskCache store
- Add `disk=true` (and company) flags to `#[io_cached]`
## Changed
## Removed

## [0.48.1 / [cached_proc_macro[0.19.1]] / [cached_proc_macro_types[0.1.1]]]
## Added
- Include LICENSE file in `cached_proc_macro` and `cached_proc_macro_types`
## Changed
## Removed

## [0.48.0 / [cached_proc_macro[0.19.0]]]
## Added
- Add `CloneCached` trait with additional methods when the cache value type implements `Clone`
- Add `result_fallback` option to `cached` proc_macro to support re-using expired cache values
  when utilizing an expiring cache store and a fallible function.
## Changed
## Removed

## [0.47.0]
## Added
## Changed
- Update redis `0.23.0` -> `0.24.0`
## Removed

## [0.46.1 / [cached_proc_macro[0.18.1]]
## Added
## Changed
- Fix #once sync_writes bug causing a deadlock after ttl expiry, https://github.com/jaemk/cached/issues/174
## Removed

## [0.46.0]
## Added
- Add `ahash` feature to use the faster [ahash](https://github.com/tkaitchuck/aHash) algorithm.
- Set `ahash` as a default feature.
- Update hashbrown `0.13.0` -> `0.14.0`
## Changed
## Removed

## [0.45.1] / [cached_proc_macro[0.18.0]]
## Added
## Changed
- Release `*_no_cache` changes from `0.45.0`. The change is in the proc macro crate which
  I forgot to release a new version of.
## Removed

## [0.45.0]
## Added
- Generate `*_no_cache` function for every cached function to allow calling the original function
  without caching. **This is backwards incompatible if you have a function with the same name**.
## Changed
- `tokio` dependency has been removed from `proc_macro` feature (originally unecessarily included).
- `async` feature has been removed from the `default` feature. **This is a backwards incompatible change.**
  If you want to use `async` features, you need to enable `async` explicitly.
- remove accidental `#[doc(hidden)]` on the `stores` module
## Removed

## [0.44.0] / [cached_proc_macro[0.17.0]]
## Added
- Option to enable redis multiplex-connection manager on `AsyncRedisCache`
## Changed
- Show proc-macro documentation on docs.rs
- Document needed feature flags
- Hide implementation details in documentation
- Relax `Cached` trait's `cache_get`, `cache_get_mut` and `cache_remove` key parameter. Allow `K: Borrow<Q>`
  like `std::collections::HashMap` and friends. Avoids copies particularly on `Cached<String, _>` where now
  you can do `cache.cache_get("key")` and before you had to `cache.cache_get("key".to_string())`.

  Note: This is a minor breaking change for anyone manually implementing the `Cached` trait.
  The signatures of `cache_get`, `cache_get_mut`, and `cache_remove` must be updated to include the
  additional trait bound on the `key` type:
  ```rust
    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
  ```
## Removed
- Dependency to `lazy_static` and `async_once` are removed.

## [0.43.0]
## Added
## Changed
- Update redis `0.22.0` -> `0.23.0`
- Update serial_test `0.10.0` -> `2.0.0`
## Removed

## [0.42.0] / [cached_proc_macro[0.16.0]]
## Added
## Changed
- Better code generation for `#[cached]` when the `sync_writes` flag is true.
## Removed

## [0.41.0]
## Added
## Changed
- Fix "sized" cache types (`SizedCache`, `TimedSizedCache`) to check capacity and evict members after insertion.
- Fixes bug where continuously inserting a key present in the cache would incorrectly evict the oldest cache member
  even though the cache size was not increasing.
## Removed

## [0.40.0]
## Added
- Add optional feature flag `redis_ahash` to enable `redis`'s optional `ahash` feature
## Changed
- Update `redis` to `0.22.0`
- Move `tokio`'s `rt-multi-thread` feature from being a default to being optionally enabled by `async_tokio_rt_multi_thread`
- Fix makefile's doc target to match documentation, changed from `make sync` to `make docs`
## Removed

## [0.39.0]
## Added
- Add flush method to ExpiringValueCache
## Changed
## Removed

## [0.38.0] / [cached_proc_macro[0.15.0]]
## Added
## Changed
- Fix proc macro argument documentation
- Disable futures `default-features`
- Add cache-remove to redis example
## Removed

## [0.37.0] / [cached_proc_macro[0.14.0]]
## Added
## Changed
- Mark the auto-generated "priming" functions with `#[allow(dead_code)]`
- Fix documentation typos
- Replace dev/build scripts with a Makefile
## Removed

## [0.36.0] / [cached_proc_macro[0.13.0]]
## Added
- wasm support for non-io macros and stores
## Changed
- Use `instant` crate for wasm compatible time
## Removed

## [0.35.0]
## Added
- Added `ExpiringValueCache` for caching values that can themselves expire.
- Added COPYRIGHT file
## Changed
## Removed

## [0.34.1]
## Added
- Make sure `AsyncRedisCacheBuilder`, `RedisCacheBuilder`, and `RedisCacheBuildError` publicly visible
## Changed
## Removed

## [0.34.0] / [cached_proc_macro[0.12.0]]
## Added
## Changed
- Replace `async-mutex` and `async-rwlock` used by proc-macros with `tokio::sync` versions
- Add optional `version` field to `CachedRedisValue` struct
- Cleanup feature flags so async redis features include `redis_store` and `async` features automatically
## Removed

## [0.33.0]
## Added
- Allow specifying the namespace added to cache keys generated by redis stores
## Changed
- Bump hashbrown 0.11.2 -> 0.12: https://github.com/rust-lang/hashbrown/blob/master/CHANGELOG.md#v0120---2022-01-17
- Bump smartstring 0.2 -> 1: https://github.com/bodil/smartstring/blob/master/CHANGELOG.md#100---2022-02-24
## Removed

## [0.32.1]
## Added
## Changed
- Fix redis features so `redis/aio` is only included when async redis
  features (`redis_tokio` / `redis_async_std`) are enabled
## Removed

## [0.32.0] / [cached_proc_macro[0.11.0]]
## Added
- Fix how doc strings are handled by proc-macros. Capture all documentation on the
  cached function definitions and add them to the function definitions generated
  by the proc-macros. Add doc strings to generated static caches. Link to relevant static
  caches in generated function definitions. Add documentation to the generated
  cache-priming function.
## Changed
## Removed

## [0.31.0] / [cached_proc_macro[0.10.0]]
## Added
- `IOCached` and `IOCachedAsync` traits
- `RedisCache` and `AsyncRedisCache` store types
- Add `#[io_cached]` proc macro for defining cached functions backed
  by stores that implement `IOCached`/`IOCachedAsync`
## Changed
- Convert from travis-ci to github actions
- Update build status badge to link to github actions
## Removed

## [0.30.0]
## Added
- Add flush method to TimedSize and TimedSized caches
## Changed
- Fix timed/timed-sized cache-get/insert/remove to remove and not
  return expired values
## Removed

## [0.29.0] / [cached_proc_macro[0.9.0]]
## Added
- proc-macro: support arguments of the wrapped function being prefixed with `mut`
## Changed
## Removed

## [0.28.0]
## Added
- Add failable TimedSize and SizeCached constructors
## Changed
## Removed

## [0.27.0] / [cached_proc_macro[0.8.0]]
## Added
- Add `time_refresh` option to `#[cached]` to refresh TTLs on cache hits
- Generate `*_prime_cache` functions for every `#[cached]` and `#[once]` function
  to allow priming caches.
## Changed
## Removed

## [0.26.1] / [cached_proc_macro[0.7.1]]
## Added
- Add `sync_writes` option to `#[cached]` macro to synchronize
  concurrent function calls of duplicate arguments. For ex, if
  a long running `#[cached(sync_writes = true)]` function is called
  several times concurrently, the actual function is only executed
  once while all other calls block and return the newly cached value.
## Changed
## Removed

## [0.26.0] / [cached_proc_macro[0.7.0]]
## Added
- Add `#[once]` macro for create a `RwLock` cache wrapping a single value
- For all caches, add a function to get an immutable reference to their
  contents. This makes it possible to manually dump a cache, so its contents
  can be saved and restored later.
## Changed
## Removed

## [0.25.1]
## Added
## Changed
- Update deps hashbrown and darling, remove async-mutex from cached-proc-macro crate
## Removed

## [0.25.0]
## Added
- Add option to "timed" caches to refresh the ttl of entries on cache hits
## Changed
## Removed

## [0.24.1] / [cached_proc_macro[0.6.1]]
## Added
- Add docs strings to the items generated by the `#cached` proc macro
## Changed
## Removed

## [0.24.0]
## Added
- `cache_reset_metrics` trait method to reset hits/misses
## Changed
## Removed

## [0.23.0]
## Added
## Changed
- Refactor cache store types to separate modules
## Removed

## cached[0.22.0] / cached_proc_macro[0.6.0] / cached_proc_macro_types[0.1.0]
## Added
- Add support for returning a `cached::Return` wrapper type that
  indicates whether the result came from the function's cache.
## Changed
## Removed

## [0.21.1] / [0.5.0]
## Added
- Support mutual `size` & `time` args in the cached proc macro.
  Added when TimedSizedCache was added, but forgot to release
  the cached_proc_macro crate update.
## Changed
## Removed

## [0.21.0]
## Added
- Add a TimedSizedCache combining LRU and timed/ttl logic
## Changed
## Removed

## [0.20.0]
## Added
- Add new CachedAsync trait. Only present with async feature. Adds two async function in the entry API style of HashMap
## Changed
## Removed

## [0.19.0] / [0.4.0]
## Added
## Changed
- Add type hint `_result!` macros
- remove unnecessary transmute in cache reset
- remove unnecessary clones in proc macro
## Removed

## [0.18.0] / [0.3.0]
## Added
## Changed
- use `async-mutex` instead of full `async-std`
## Removed

## [0.17.0]
## Added
## Changed
- Store inner values when `result=true` or `option=true`. The `Error` type in the
`Result` now no longer needs to implement `Clone`.
## Removed

## [0.16.0]
## Added
- add `cache_set_lifespan` to change the cache lifespace, old value returned.
## Changed
## Removed

## [0.15.1]
## Added
## Changed
- fix proc macro when result=true, regression from changing `cache_set` to return the previous value
## Removed

## [0.15.0]
## Added
- add `Cached` implementation for std `HashMap`
## Changed
- trait `Cached` has a new method `cache_get_or_set_with`
- `cache_set` now returns the previous value if any
## Removed

## [0.14.0]
## Added
- add Clone, Debug trait derives on pub types

## Changed

## Removed

## [0.13.1]
## Added

## Changed
- fix proc macro documentation

## Removed

## [0.13.0]
## Added
- proc macro version
- async support when using the new proc macro version

## Changed

## Removed

## [0.12.0]
## Added
- Add `cache_get_mut` to `Cached` trait, to allow mutable access for values in the cache.
- Change the type of `hits` and `misses` to be `u64`.

## Changed

## Removed

## [0.11.0]
## Added
- Add `value_order` method to SizedCache, similar to `key_order`

## Changed

## Removed

## [0.10.0]
## Added
- add `cache_reset` trait method for resetting cache collections to
  their initial state

## Changed
- Update `once_cell` to 1.x

## Removed

## [0.9.0]
## Added

## Changed
- Replace SizedCache implementation to avoid O(n) lookup on cache-get
- Update to Rust-2018 edition
- cargo fmt everything

## Removed


## [0.8.1]
## Added

## Changed
- Replace inner cache when "clearing" unbounded cache

## Removed


## [0.8.0]
## Added

## Changed
- Switch to `once_cell`. Library users no longer need to import `lazy_static`

## Removed

## [0.7.0]
## Added
- Add `cache_clear` and `cache_result` to `Cached` trait
  - Allows for defeating cache entries if desired

## Changed

## Removed

## [0.6.2]
## Added

## Changed
- Update documentation
  - Note the in-memory nature of cache stores
  - Note the behavior of memoized functions under concurrent access

## Removed

## [0.6.1]
## Added

## Changed
- Fixed duplicate key eviction in `SizedCache::cache_set`. This would manifest when
  `cached` functions called with duplicate keys would race set an uncached key,
  or if `SizedCache` was used directly.

## Removed

## [0.6.0]
## Added
- Add `cached_result` and `cached_key_result` to allow the caching of success for a function that returns `Result`.
- Add `cached_control` macro to allow specifying functionality
  at key points of the macro

## [0.5.0]
## Added
- Add `cached_key` macro to allow defining the caching key

## Changed
- Tweak `cached` macro syntax
- Update readme

## Removed


## [0.4.4]
## Added

## Changed
- Update trait docs

## Removed


## [0.4.3]
## Added

## Changed
- Update readme
- Update examples
- Update crate documentation and examples

## Removed
