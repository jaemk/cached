# Changelog

## [Unreleased]
- Update `hashbrown` to 0.17 (internal only; not part of the public API). Dev-only: bump `criterion` to 0.8 and `googletest` to 0.14. No API or behavior change.

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
- **`ConcurrentCached::cache_size` / `ConcurrentCachedAsync::cache_size`**: new method `fn cache_size(&self) -> Result<Option<usize>, Self::Error>` reporting the number of entries, with a default of `Ok(None)`. The default makes it non-breaking for existing external implementors and honest for stores that cannot cheaply produce a count: the six sharded stores override it to return `Ok(Some(len))`, while the external-store impls (`DiskCache`, `RedisCache`, `AsyncRedisCache`) keep the `Ok(None)` default because their backends (sled, Redis) expose no O(1) size. Sharded stores also retain their inherent `len()` / `is_empty()` for a non-`Result` count.

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
- **`TtlSortedCache` runtime max-size setters**: `size_limit(n)` → `set_max_size(n)` and `try_size_limit(n)` → `try_set_max_size(n)` (matching the `set_ttl` runtime-mutator convention).

### Added

#### New macro attributes
- `max_size = N` attribute for `#[cached]` and `#[concurrent_cached]`: the preferred spelling of the LRU-bound attribute, mirroring the renamed `max_size` builder setter. The original `size = N` attribute continues to work as a **deprecated alias** — using it emits a deprecation warning (anchored at the `size` token) steering you to `max_size`. Specifying both `size` and `max_size` on the same annotation is a compile error.
- `cache_err = true` attribute for `#[cached]`, `#[once]`, and `#[concurrent_cached]`: opt-in to also cache `Err` values from `Result<T, E>` returns (requires a `Result<T, E>` return type; mutually exclusive with `result_fallback`).
- `cache_none = true` attribute for `#[cached]`, `#[once]`, and `#[concurrent_cached]`: opt-in to also cache `None` values from `Option<T>` returns (requires an `Option<T>` return type).
- `result_fallback = true` support for `#[concurrent_cached]`: on an `Err` return, the last cached `Ok` value for the same key is returned instead. The stale value is kept in the primary cache slot (via `ConcurrentCloneCached::cache_get_with_expiry_status`) and re-cached with a fresh TTL window on `Err`; no separate fallback store is created. Requires `ttl` (a compile error is emitted otherwise). Restricted to the default in-memory sharded path (not redis/disk). Mutually exclusive with `cache_err` and `with_cached_flag`.

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
- Add API consistency aliases: `Cached::{get,set,remove,remove_entry,delete}` and `ConcurrentCached::{get,set,remove,remove_entry,delete}` delegate to the existing `cache_*` methods (the sync `Cached` trait gains `remove_entry` / `delete` to match `ConcurrentCached`); both the sharded and non-sharded TTL builders expose `.refresh_on_hit(...)` as the primary setter with `.refresh(...)` retained as an alias; `DiskCache`, `RedisCache`, and `AsyncRedisCache` expose `::builder(...)` aliases (alongside their existing `::new(...)` builder entry points). Note: `DiskCache::new(...)` / `RedisCache::new(...)` / `AsyncRedisCache::new(...)` are **builder** entry points — they return a builder, not a ready-to-use store — and are intentionally retained; only the in-memory and sharded store constructors that returned stores directly were removed.
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
