# Changelog

## [Unreleased]

## [3.0.0-rc.8 / cached_proc_macro 3.0.0-rc.8 / cached_proc_macro_types 3.0.0-rc.8] - 2026-07-17

### Breaking Changes

- `#[cached]` rejects an explicit `sync_writes_buckets` when `sync_writes` is not `"by_key"` with a pointed compile error. The value was previously accepted and silently ignored (buckets only exist on the `by_key` path); `#[once]` already rejected the same inert combination.
- `RedbCacheBuilder::disk_directory` renamed to `disk_dir`, matching the `disk_dir` attribute on `#[concurrent_cached]`. The 2.x builder method was `DiskCacheBuilder::disk_directory`; see the [migration guide](docs/migrations/2.0-to-3.0.md#69-disk_directory-builder-method-renamed-to-disk_dir).
- `SetMaxSizeError::ZeroSize` is renamed to `SetMaxSizeError::ZeroMaxSize`, matching `SetTtlError::ZeroTtl` (the variant names the argument it validates; its message was already "max_size must be greater than zero"). The variant only ever shipped in 3.0.0 release candidates.
- The inherent `ttl` / `set_ttl` / `unset_ttl` / `refresh_on_hit` / `set_refresh_on_hit` methods on `ShardedTtlCacheBase` / `ShardedLruTtlCacheBase` are removed; the runtime TTL controls live only on `ConcurrentCacheTtl` (same names and signatures, still `&self`). This makes the concurrent family uniform: `RedisCache`, `AsyncRedisCache`, and `RedbCache` already exposed them only through the trait, and the single-owner stores went through the same change for `CacheTtl` in rc.1. Builder setters are unaffected. Import the trait (or `cached::prelude::*`) at call sites; see the [migration guide](docs/migrations/2.0-to-3.0.md#70-sharded-ttl-stores-inherent-ttlset_ttlunset_ttlrefresh_on_hitset_refresh_on_hit-removed-use-concurrentcachettl).

### Added

- `ShardedLruCacheBase`, `ShardedLruTtlCacheBase`, and `ShardedExpiringLruCacheBase` (and their default-hasher aliases) now support runtime capacity resizing via `set_max_size(&self, usize) -> Option<usize>` and `try_set_max_size(&self, usize) -> Result<Option<usize>, SetMaxSizeError>`. Shrinking evicts LRU-excess entries per shard strictly by recency (expired-but-recent entries survive a shrink; only `evict()` sweeps by TTL/expiry), fires `on_evict`, and increments the eviction counter; the same ceiling-division-plus-16-per-shard-floor policy the builders use is applied; resize is not atomic across shards (shards are locked and resized one at a time, so concurrent readers may briefly observe mixed per-shard capacities).
- `per_shard_initial_capacity` on `ShardedUnboundCacheBuilder`, `ShardedTtlCacheBuilder`, and `ShardedExpiringCacheBuilder`: a per-shard preallocation hint, the sharded counterpart of the single-owner builders' `initial_capacity` (total preallocation is `shards x per_shard_initial_capacity`).
- `ExpiringLruCache::retain(keep)`: removes entries that are expired or fail the predicate, firing `on_evict` and counting evictions, matching `LruTtlCache::retain`. The `retain` docs on all three LRU stores now spell out the shared contract: iteration is over the LRU entries, expired entries are removed without consulting the predicate (expiry-aware stores only), and survivor recency order is unchanged.
- New runnable example `examples/resilience.rs` covering `sync_writes = "by_key"`, `result_fallback`, and `force_refresh`.

### Changed

- `ExpiringCache`, `ExpiringLruCache`, and their builders (including the sharded `ShardedExpiringCacheBuilder` / `ShardedExpiringLruCacheBuilder`) drop the `K: Hash + Eq` / `V: Expires` bounds from the type definitions, matching every other store (bounds live on the impls). Purely a relaxation: all previously-valid code still compiles, and the types can now be named in generic contexts without carrying the bounds.
- `ConnectionString` derives `PartialEq`, `Eq`, and `Hash` (comparing the raw unredacted URL).
- `NoEvict` / `HasEvict` derive `Clone`, `Copy`, `Debug`, and `Default`, and are documented at the crate root (previously `#[doc(hidden)]` there but documented at `cached::stores`). They appear in `LruTtlCacheBuilder` / `ShardedLruTtlCacheBuilder` signatures, so they are findable on docs.rs now.
- `cached::prelude` re-exports `CacheTtl` unconditionally, matching `ConcurrentCacheTtl` (the trait was never feature-gated; only the built-in stores implementing it require `time_stores`).
- `Return::set_was_cached` is `#[doc(hidden)]` (macro plumbing, not supported public API). The method remains `pub` and callable (no compile breakage); it is de-documented because only macro-generated code should set the flag -- use `Return::new` to construct a value and `was_cached()` to read the flag.
- `#[must_use]` added to `CacheMetrics::hit_ratio` and the `iter_order` / `key_order` / `value_order` accessors on `LruCache` / `LruTtlCache`.
- `metrics().capacity` on the sharded LRU stores (`ShardedLruCacheBase` / `ShardedLruTtlCacheBase` / `ShardedExpiringLruCacheBase`) loads the total with `Acquire` ordering, matching `capacity()`: after a same-thread `set_max_size`, `metrics().capacity` and `capacity()` now always agree.

### Documentation

- The redis on-wire format (msgpack `value`/`version` fields, `REDIS_VALUE_VERSION`) and the redb on-disk format (versioned file name, table name, msgpack fields) are documented as stable for the 3.x series, in "Format stability" sections on the `RedisCache` / `AsyncRedisCache` and `RedbCache` struct docs; changes bump the embedded version and are reserved for a major release.
- `AsyncRedisCacheBuilder` explains why it has no `connection_pool_*` methods (multiplexed connection, no r2d2 pool; pool sizing does not apply).
- The expiring stores document the `cache_remove` / `cache_remove_entry` asymmetry: `cache_remove` filters expired values (`None`), `cache_remove_entry` returns the entry regardless of expiry.
- `ShardedLruTtlCacheBuilder::on_evict` enumerates the `cache_set`-over-expired-entry trigger, matching the other sharded TTL/expiring builders.
- `#[concurrent_cached]`'s `expires` doc lists `result_fallback` in its mutual-exclusion set (the combination was already a compile error).
- `#[cached]`'s `unsync_reads` doc names the built-in `CachedRead` stores (`UnboundCache`, `TtlSortedCache`); `result_fallback` distinguishes TTL-store refresh semantics from `expires`-store behavior; `#[once]`'s `in_impl` doc notes `companions_vis` also overrides the `_no_cache` sibling's visibility.
- The crate-doc custom-store example uses the unquoted `create` / `convert` forms.
- The error-source downcast tests are labeled as pinning non-contract behavior (the concrete source types stay out of the semver contract).
- The `#[concurrent_cached(redis = true)]` shorthand now appears in the macro quick-reference table.
- `Cached::cache_capacity` / `ConcurrentCacheBase::cache_capacity` docs clarify that capacity means the eviction bound (`max_size`), not pre-allocated memory.
- `set_max_size` on the sharded LRU stores documents concurrent-caller semantics: overlapping resizes interleave per-shard writes (no data race or lost entries, but the resulting bound blends the two targets and `capacity()` reports whichever total was published last); serialize resizes externally, or re-issue the resize, when a single consistent target matters.

## [3.0.0-rc.7 / cached_proc_macro 3.0.0-rc.7] - 2026-07-12

### Breaking Changes

- `RedisCacheBuilder::build()` / `AsyncRedisCacheBuilder::build()` reject an empty prefix with `Build(BuildError::InvalidValue { field: "prefix", .. })`. The prefix is what scopes `cache_clear` to one logical cache; with an empty prefix, `cache_clear` matches `<namespace>:*` and deletes the entries of every cache sharing the namespace (all of them, under the shared default namespace). The `RedisCacheBuildError::EmptyScope` variant from rc.3 (which fired only when namespace and prefix were both empty) is removed; the empty-prefix rejection subsumes it. See the [migration guide](docs/migrations/2.0-to-3.0.md#9-rediscachebuilderbuild--asyncrediscachebuilderbuild-reject-an-empty-prefix).
- `#[concurrent_cached]` rejects an `async` closure for `map_error` at the macro with a pointed message. It previously passed macro validation and failed downstream at the `Result::map_err` `FnOnce` bound with an opaque type error.

### Fixed

- `RedbCache` default-directory resolution self-heals a pre-existing cache directory with legacy permissions. A directory created by an earlier `cached` version was created with the process umask (0775 under the umask-002 user-private-group default of Debian/Ubuntu) and permanently failed the security validation with "I/O error preparing the disk cache directory"; the app-derived candidate is now tightened to 0700 and re-validated (the chmod only succeeds for the owner, so an attacker-owned or symlinked directory still falls through to the next candidate instead of aborting).
- `ShardedExpiringLruCache::cache_set` evaluates the displaced entry's `is_expired()` exactly once, under the shard write lock. It previously evaluated twice (once inside the lock for the eviction counter, once outside for `on_evict` and the return value), so a value crossing the expiry threshold between the two calls fired `on_evict` without counting the eviction. The other sharded expiring stores already evaluated once.
- `RedisCacheBuildError::MissingConnectionString` redacts the env-var value carried by `std::env::VarError::NotUnicode`. The raw value is the connection string itself (credentials included) and was printed by both `Display` and `Debug`.
- `RedisCacheBuildError` uses a manual `Debug` impl, matching `RedisCacheError` / `RedbCacheError` (`RedbCacheBuildError` keeps its derived `Debug`; it carries no redactable value).
- `make examples` actually runs the registered examples again: the per-example targets are `.PHONY`, and make skips implicit-rule search for phony targets, so the `examples/basic/%` / `examples/redis/%` pattern rules silently expanded to nothing and only the two explicitly-ruled examples (`wasm`, `redis-async-async-std`) ran. The rules are now static pattern rules, `expires_per_key` and `struct_method` are registered, and the expansion guard checks every registered example expands to its own run command.

### Documentation

- `Cached`'s trait-object recipe is now the compiling form `dyn ConcurrentCached<K, V, Error = E>` (the previous `dyn ConcurrentCached<K, V> + ConcurrentCacheBase<Error = E>` spelling fails E0225: only one non-auto trait is allowed in `dyn`).
- The sharded stores' inherent `get` docs name `ConcurrentCachedExt::get` as the trait-qualified call; the documented `ConcurrentCached::get(&store, k)` does not compile (the `get` alias lives on the extension trait).
- The evictions-counter exception in the store comparison covers both unbounded non-expiring stores (`UnboundCache` and `ShardedUnboundCache` return `None` from `metrics().evictions`), not just the sharded one.
- The migration guide's feature-name section no longer claims `serde` is a private `dep:` name: `serde` is a public feature (since rc.5) enabling `SerializeCached` support for custom stores; the feature table and error-message index now list it.
- `RedisCache` / `AsyncRedisCache` struct docs describe the TTL as optional (entries built without one persist until removed) instead of always applied.
- `strict_deserialization` docs (redis and redb) state that the previous value displaced by `cache_set` is discarded in both modes when it cannot be decoded, and that a strict-mode `remove_expired_entries` sweep aborts atomically (evictions from earlier in the pass are rolled back; covered by a new test).
- `RedbCache::remove_expired_entries` no longer links `CacheEvict::evict` (a trait `RedbCache` does not implement); it explains the naming and `Result` return instead.
- `ShardedLruCache`'s `on_evict` builder doc enumerates `cache_clear_with_on_evict` as a firing site, matching the other sharded stores; `cache_prefix_block` docs show the unquoted expression form.

## [3.0.0-rc.6 / cached_proc_macro 3.0.0-rc.6] - 2026-07-09

> Fixes from the 3.0.0 pre-release review. The 2.x -> 3.0 upgrade is documented in the [migration guide](docs/migrations/2.0-to-3.0.md).

### Breaking Changes

- `RedisCacheBuildError::Resp2DowngradeWithClientSideCaching` is declared only with the `redis_async_cache` feature (it was never constructible without it). A `match` arm naming the variant in a build without the feature no longer compiles; gate the arm or rely on the `_` arm the `#[non_exhaustive]` enum already requires.

### Fixed

- `LruTtlCache::cache_set` over an existing entry now passes the stored key to `on_evict` instead of the caller's key. Key types where equal instances are non-identical previously received the wrong key instance (same class of bug fixed for the `*_mut` paths in rc.5).
- `ShardedTtlCache`, `ShardedLruTtlCache`, and `ShardedExpiringCache` `cache_set` now evaluate the displaced entry's expiry while still holding the shard write lock. Previously the check ran after the lock was released, so an entry crossing the expiry boundary in that window could be misclassified (wrong return value and `on_evict` decision).
- `ShardedExpiringCache` / `ShardedExpiringLruCache` `deep_clone` reads the hit/miss counters while still holding the shard read lock, so the cloned metrics are consistent with the cloned entries.
- `RedbCache::remove_expired_entries` uses a single time snapshot for its scan and write passes; an entry can no longer be judged live in the scan and expired in the write (or vice versa).

### Added

- `cached::prelude` re-exports `CacheMetrics`, so `metrics()` call sites need no second import.
- `#[must_use]` on `ConcurrentCached::{cache_get, cache_set}` and `ConcurrentCachedAsync::{async_cache_get, async_cache_set}`, matching the documented contract.
- Explicit `#[source]` on `RedisCacheError::Redis` and `RedisCacheError::Pool`, with source-downcast tests.
- The `force_refresh` parse error for a bare literal (e.g. `force_refresh = true`) now suggests the unquoted `{ ... }` block form; the `#[once]` rejection error for `create` points at the attribute instead of the function.

### Documentation

- Migration guides updated to the final rc surface: positional `builder(arg)` forms, `CachedGetOrSetAsync` / `async_cache_get_or_set_with*` names, and the `ConcurrentCacheBase` `cache_size` / `cache_is_empty` method set.
- `on_evict` builder docs list the displaced-expired-entry trigger and the `cache_clear_with_on_evict` callback timing; `ConcurrentCached` documents why lookups take `&K` rather than `Q: Borrow`; the redb docs state the actual redb/redis TTL difference; `result_fallback` docs note only non-disabled `sync_writes` values conflict; `force_refresh` docs prefer the unquoted block form.

## [3.0.0-rc.5 / cached_proc_macro 3.0.0-rc.5] - 2026-07-07

### Breaking Changes

- `SerializeCached::cache_set_ref` and `SerializeCachedAsync::async_cache_set_ref` return `Result<(), Self::Error>` instead of `Result<Option<V>, Self::Error>`. The previous value is no longer fetched on the IO-backed stores (removes a per-write read+decode round-trip on redis). Call `cache_get` first if you need the prior value. Custom `SerializeCached` impls must update their return type.
- `ConcurrentCacheBase::len` is removed (it duplicated `cache_size`); `ConcurrentCacheBase::is_empty` is renamed to `cache_is_empty`. The inherent `len()` and `is_empty()` on the six sharded concrete types are unchanged. **Migration:** replace `<T: ConcurrentCacheBase>::len(...)` with `cache_size(...)` and `is_empty(...)` with `cache_is_empty(...)`.
- `RedbCache::builder`, `RedisCache::builder`, and `AsyncRedisCache::builder` now take the primary required field as a positional argument: `RedbCache::builder(name)`, `RedisCache::builder(prefix)`, `AsyncRedisCache::builder(prefix)`. The no-arg `RedbCacheBuilder::new()` / `RedisCacheBuilder::new()` / `AsyncRedisCacheBuilder::new()` entry points on the builder structs are unchanged.
- The redis TTL is now optional: omitting `.ttl(...)` before `build()` stores keys without expiry (equivalent to `unset_ttl()`). A TTL that is set must be greater than zero. `RedisCacheBuildError::MissingRequired("ttl")` is no longer returned. **Migration:** if your build path detected the absent-ttl error, set the TTL explicitly or rely on the no-expiry default; a zero TTL still returns `InvalidValue`.
- `#[concurrent_cached(disk = true, cache_prefix_block = ...)]` is a compile error. `cache_prefix_block` is a redis-only attribute; the redb table name derives from the `name` attribute. Remove `cache_prefix_block` from disk-backed `#[concurrent_cached]` uses.

### Fixed

- On expired-entry replacement via `cache_get_or_set_with_mut` / `cache_try_get_or_set_with_mut`, the `on_evict` callback on `LruCache`, `LruTtlCache`, and `ExpiringLruCache` now receives the stored key of the evicted entry rather than the lookup key. Key types where equal instances may be non-identical (case-insensitive keys, interned strings) previously received the wrong key instance.
- `TtlSortedCache::cache_get_or_set_with_mut` and `cache_try_get_or_set_with_mut` now leave the expired entry in place when the factory returns `Err` or panics, matching `TtlCache` and `LruTtlCache`. Previously the expired entry was removed before the factory ran, so a factory failure left the key absent. `TtlSortedCache::set_max_size` now evicts eagerly down to the new bound (matching `LruCache`), instead of deferring eviction to the next insert.
- `ExpiringCache::cache_get_or_set_with_mut` now fires `on_evict` while the old entry is still present in the store, then inserts the new value, matching `TtlCache`. Previously the new value was inserted first, so a callback that read the store observed the new entry.
- redb self-heal (`strict_deserialization = false`): the entry is re-read inside the write transaction before deletion, so a concurrent valid `cache_set` that commits between the corrupt-bytes read and the self-heal write is no longer discarded.
- redis self-heal: the delete is now conditional via a Lua script that compares stored bytes before deleting, so a concurrent `PSETEX` of a valid value that races the GET is not overwritten.

### Added

- `ConcurrentCloneCached::get_with_expiry_status` provided-method alias for `cache_get_with_expiry_status`, matching the `CloneCached::get_with_expiry_status` alias on the single-owner trait.
- `RedbCache::async_remove_expired_entries` runs the expired-entry sweep on the `blocking` thread pool, making it usable from async contexts without blocking the runtime.
- `serde` cargo feature enables `serde` and `rmp-serde` without requiring `redis_store` or `redb_store`. Use it to implement `SerializeCached` on a custom store type.
- `RedisCacheError` and `RedbCacheError` implement a custom `Debug` that redacts the `cached_value` bytes in `CacheDeserialization` variants (rendered as `<N bytes redacted>`), preventing raw application data from appearing in debug output.
- `#[source]` on `RedisCacheBuildError::Connection` and `RedisCacheBuildError::Pool`, aligning them with the `#[source]`-annotated variants on `RedbCacheBuildError`.

## [3.0.0-rc.4 / cached_proc_macro 3.0.0-rc.4 / cached_proc_macro_types 3.0.0-rc.4] - 2026-07-05

> Changes since rc.3, all non-breaking. The 2.x -> 3.0 upgrade is documented in the [migration guide](docs/migrations/2.0-to-3.0.md).

### Fixed
- `TtlCache::cache_get_or_set_with_mut` and its async twin now run the value factory before firing the `on_evict` callback and counting the eviction on the expired-entry path. A panicking factory (sync) or a dropped/cancelled future (async) no longer leaves the expired entry in place with the callback already fired, which previously double-fired on the next access.
- `RedisCache::cache_remove` / `AsyncRedisCache::async_cache_remove` honor `strict_deserialization`. An undecodable displaced value is discarded and the call returns `Ok(None)` in the default mode (the entry is still removed); it returns `Err(CacheDeserialization)` only under `strict_deserialization(true)`, matching `cache_get` and `RedbCache::cache_remove`.
- `#[once]` and `#[concurrent_cached]` forward user lint attributes (for example `#[allow(...)]`) to the generated `*_prime_cache` companion, matching `#[cached]`.

### Added
- `#[cached]` / `#[once]` / `#[concurrent_cached]` on an `async fn` built without the `async` feature now fail with an error naming the missing feature instead of an error pointing at an internal module.
- `#[doc(alias)]` entries mapping the 2.x store names to their 3.0 types (`SizedCache` -> `LruCache`, `TimedCache` -> `TtlCache`, `TimedSizedCache` -> `LruTtlCache`) for docs.rs search.

### Packaging
- The published crate manifests no longer carry a `[lints]` table. A future-toolchain warning firing in `cached` can no longer break downstream builds; warning enforcement moved to the workspace dev tooling.
- `specs/`, `local/`, `.cursorrules`, and `Makefile` are excluded from the published package.

### Documentation
- Migration guide: describe the boxed `Box<dyn std::error::Error + Send + Sync>` error sources and how to inspect them, rewrite the redis capability/runtime feature notes, add the `ShardedLruTtlCacheBuilder` type-parameter reorder, and correct the no-arg builder examples and stale `DiskCache` references.
- Correct the `async` feature docs (`blocking` moved to `redb_store`), the `ConcurrentCachedAsync` provided-method list, the sharded `set_ttl` `refresh_on_hit` caveat, and several changelog and AGENTS.md notes.

## [3.0.0-rc.3 / cached_proc_macro 3.0.0-rc.3 / cached_proc_macro_types 3.0.0-rc.3] - 2026-07-05

> Changes since rc.2. The 2.x -> 3.0 upgrade is documented in the [migration guide](docs/migrations/2.0-to-3.0.md); the rc.1 and rc.2 sections below record the earlier 3.0 candidates. Note the `sync_writes` default reverted since the release candidates: see "`sync_writes` default reverted to no synchronization" below.

### Breaking Changes

#### `sync_writes` default reverted to no synchronization
- rc.1 and rc.2 defaulted a bare `#[cached]` to `sync_writes = "by_key"`. That default held a
  per-key bucket lock across the function body, which deadlocks recursive memoized functions
  whenever two keys in the active call chain share a bucket, and serialized hot readers of the
  same key. The default is again no synchronization, matching 2.x and `functools.lru_cache`.
- `sync_writes = "by_key"` remains available as an explicit opt-in, documented with the
  recursion and hit-path caveats. `sync_writes = "disabled"` is accepted as a spelling of the
  default.
- **Migration:** no change from 2.x. Callers who relied on the rc-era `by_key` default must set
  `sync_writes = "by_key"` explicitly.

#### `CachedAsync` renamed to `CachedGetOrSetAsync`; sync passthroughs removed
- The trait that memoizes an async closure over a synchronous in-memory `Cached` store is
  renamed to name that job. Its four sync passthroughs (`async_cache_get` / `async_cache_set` /
  `async_cache_remove` / `async_cache_clear`), which only forwarded to the sync `Cached`
  methods, are removed along with the misleading `Self: Cached` bound. The get-or-set family is
  unchanged.
- **Migration:** import `cached::CachedGetOrSetAsync` instead of `cached::CachedAsync`; call the
  sync `cache_*` methods on an in-memory store instead of the removed `async_cache_*`
  passthroughs.

#### `CacheMetrics` fields
- `CacheMetrics::entry_count` is now `Option<usize>`; `metrics()` reports `None` for stores
  whose size is unknown (redis/redb) instead of a false `0`. `CacheMetrics` is also
  `#[non_exhaustive]` and derives `Default`, so future counters can be added without breaking
  construction.
- **Migration:** handle the `Option` on `entry_count`; construct `CacheMetrics` by mutating a
  `CacheMetrics::default()` rather than with a struct literal.

#### Fallible and total store APIs
- Sharded `copy_from` returns `Result<_, BuildError>` instead of panicking on invalid
  configuration.
- `Expires::expires_at` returns `crate::time::Instant` (web-time backed, correct under wasm)
  instead of `std::time::Instant`.
- `CloneCached::cache_get_with_expiry_status` requires `V: Clone`, matching its
  `cache_peek_with_expiry_status` sibling.
- The `Eq` marker impls for `UnboundCache` and `LruCache` now require `V: Eq` (the `PartialEq`
  impls keep `V: PartialEq`).
- `ShardedLruTtlCacheBuilder`'s type parameters are reordered so the hash builder is last,
  matching the other sharded builders.

#### redis/redb error types decoupled from their backing crates
- `RedisCacheError` / `RedisCacheBuildError` / `RedbCacheError` / `RedbCacheBuildError` no longer
  expose `redis::`, `r2d2::`, or `redb::` types through public fields or blanket `From` impls.
  Foreign error causes are boxed behind `Box<dyn std::error::Error + Send + Sync>`, so a redis or
  redb version bump is no longer a breaking change to these enums.
- **Migration:** match on the variant (e.g. `Connection { .. }`, `Pool { .. }`, `Storage { .. }`)
  and read `source()` for the cause instead of pattern-matching the foreign error directly.

#### Feature and dependency changes
- Optional dependencies are gated with Cargo's `dep:` syntax, so an optional dependency's name is
  no longer silently usable as a feature. Enable the named crate feature (`redis_store`,
  `redb_store`, `proc_macro`, ...) rather than a bare dependency name.
- `blocking` moved from the base `async` feature to `redb_store` (it only offloads synchronous
  redb work). Redis-only and in-memory async builds no longer pull it.
- `redis_connection_manager` and `redis_async_cache` are additive and runtime-agnostic: both
  depend only on `redis/aio`, so the async runtime is a separate axis. Pair a capability with
  `redis_tokio*` or `redis_smol*`. The connection manager is now a per-cache
  `.connection_manager(true)` opt-in rather than a feature that cfg-swapped every cache's
  connection type.
- `RedbCacheBuilder` rejects a `cache_name` containing any character invalid in a cross-platform
  filename (`:` `<` `>` `"` `|` `?` `*`, a path separator, or an ASCII control byte). `:`-bearing
  module-path-style names no longer build.

### Security

#### Seeded per-key lock bucket hasher
- `sync_writes = "by_key"` bucket selection seeds from a per-static `RandomState` instead of a
  fixed-seed hasher, so an attacker who knows the key space can no longer collapse the lock
  buckets to force whole-cache serialization.

#### Self-healing deserialization is the default for redis/redb
- A corrupt or undecodable cached value on the `cache_get` path is self-healed by default: the
  offending entry is deleted and the call returns `Ok(None)` (a miss) so the cached function
  recomputes. Opt into the previous fail-closed behavior with `.strict_deserialization(true)`,
  which returns `Err(CacheDeserialization { .. })` instead.

#### redis credential and error hardening
- Connection-string redaction is structural: `resolve_connection_string()` returns a redacting
  `ConnectionString`, and the build path constructs sanitized synthetic errors, so "no
  credentials in the error `Display`/`Debug`" is a compile-time property rather than a
  convention.
- The `r2d2` pool-build failure is sanitized like the connection path, closing the last
  build-path error that could surface the connection URL.
- `RedisCacheBuilder::connection_pool_connection_timeout` bounds how long `build` waits to
  establish a connection.

#### redb disk hardening (Unix)
- A symlink at the resolved db path or an explicitly configured cache directory is rejected
  before opening, so writes cannot be redirected through a planted symlink. The
  symlink-and-permissions validation now runs for the XDG default candidates, not only the temp
  fallback.
- The db file is forced to mode `0600` on every open, not only at creation, so a file created
  `0644` by an earlier version is no longer readable by group or other.
- A default candidate on a read-only filesystem falls back to the temp directory, not only on
  `PermissionDenied`.

### Fixed

#### `#[cached]` / `#[once]` prime companion no longer deadlocks or blocks readers
- The `{fn}_prime_cache` companion ran the function body while holding the cache write lock. A
  recursive prime re-locked the same static on the same thread and deadlocked (parking_lot is
  non-reentrant), and any prime blocked every reader for the full recompute. The body now runs
  before the lock is taken, mirroring the main path.

#### ttl expiry anchored after the factory
- `TtlCache`, `LruTtlCache`, and `TtlSortedCache` compute an entry's expiry after the value
  factory resolves on every get-or-set path (several async and `LruTtlCache` sync paths anchored
  before the factory, so a factory slower than the ttl produced an already-stale entry).

#### eviction accounting corrections
- `TtlCache`'s try-path get-or-set no longer fires `on_evict` or counts an eviction until the
  replacement factory succeeds; on `Err` the expired entry is left in place, so the next lookup
  evicts it exactly once instead of double-firing.
- Overwriting an expired entry via `cache_set` fires `on_evict` and counts the eviction
  uniformly across the timed and sharded stores.
- A panicking `on_evict` during `LruCache` capacity eviction can no longer leave the cache over
  capacity: the victim is removed before the callback runs, and the check loops until the bound
  holds.

#### `TtlSortedCache` allocation is fallible
- `build` reserves with `try_reserve`, returning `Err(BuildError)` on a capacity-overflowing
  `max_size` or `initial_capacity` instead of aborting; `set_max_size` grows on demand, so
  `try_set_max_size` is genuinely panic-free.

#### redb read-then-write races
- `disk_cache_get` refresh-on-hit and expiry eviction, and `remove_expired_entries`, re-read and
  re-check the entry inside the write transaction before mutating, so a concurrent writer in the
  read-to-write gap is no longer clobbered.

#### macro correctness
- The `#[once]` generic-value-type guard compares whole idents, so `fn f<S: Into<String>>(..) ->
  String` is no longer falsely rejected because `"String"` contains `"S"`.
- A raw-identifier cache `name` (e.g. `r#type`) builds a working `static` instead of panicking.
- Attributes written between the macro and the `fn` (`#[cfg]`, lint attrs) forward to every
  generated item, so cfg-gating stays in lockstep and `#[allow(..)]` reaches the generated body.
- `#[concurrent_cached]` rejects a custom `ty` without a `create` block on the redis and disk
  paths (previously it declared the cache as `ty` but built the default store).
- `#[cached]` rejects `result_fallback` combined with `with_cached_flag` (their `Return`-vs-raw
  value shapes are incompatible).

### Changed

- Sharded stores gain an inherent `get_or_set_with` returning `V` directly, so the common case
  needs no trait import or `.unwrap()`.
- `ConcurrentCachedExt` gains `clear` / `reset` aliases for parity with `CachedExt`.
- The `CacheTtl` trait is no longer feature-gated (its built-in impls remain gated on
  `time_stores`).
- `Cached for HashMap` no longer requires `S: Default`, so `HashMap<K, V, DefaultHashBuilder>`
  implements `Cached` on wasm; `cache_reset` clears and shrinks in place.
- `ConcurrentCached::cache_get_or_set_with` is dyn-compatible.
- `TtlSortedCache::ttl()` resolves a zero configured ttl to `None`, and `cache_set` on a ttl that
  overflows `Instant` stores the value with no expiry instead of dropping it.

## [3.0.0-rc.2 / cached_proc_macro 3.0.0-rc.2 / cached_proc_macro_types 3.0.0-rc.2] - 2026-07-02
> Second 3.0 release candidate. The 3.0 API is not final and may change before the 3.0.0 release. See the [migration guide](docs/migrations/2.0-to-3.0.md). Note: this candidate defaulted `#[cached]` to `sync_writes = "by_key"`; that default was reverted before 3.0.0 (see the "`sync_writes` default reverted to no synchronization" entry in the 3.0.0-rc.3 notes).

### Breaking Changes

#### `capacity()` builder method renamed to `initial_capacity()` on allocation-hint builders
- `UnboundCacheBuilder`, `TtlCacheBuilder`, `TtlSortedCacheBuilder`, and `ExpiringCacheBuilder`
  had a `.capacity(n)` method that pre-allocates the backing `HashMap` without bounding entry
  count. The name was ambiguous next to `.max_size(n)` (the eviction bound on the LRU builders).
  It is now `.initial_capacity(n)`.
- **Migration:** rename `.capacity(n)` to `.initial_capacity(n)` on those four builders.
  `max_size` on the LRU builders is unchanged.

#### `Return<T>` fields are now private (`cached_proc_macro_types`)
- `Return<T>::value` and `Return<T>::was_cached` are now private fields. Code that accessed
  them directly no longer compiles.
- **Migration:** use `*r` / `r.into_inner()` to get the inner value and `r.was_cached()` for
  the flag. Pattern-matching on the struct fields (`Return { value, was_cached }`) must switch
  to the accessor methods.

#### `set_max_size` returns `Option<usize>` on LRU stores
- `LruCache::set_max_size`, `LruTtlCache::set_max_size`, and `ExpiringLruCache::set_max_size`
  now return `Option<usize>` (the previous bound) instead of `usize`, matching
  `TtlSortedCache::set_max_size` and unifying the return type across all four stores.
- **Migration:** the returned value is now wrapped in `Some(..)`; update any pattern or type
  annotation that expected a bare `usize`.

#### ahash default hasher enables `runtime-rng` on non-wasm targets
- The `ahash` feature now enables the `ahash/runtime-rng` sub-feature on non-wasm targets,
  seeding hash maps from the OS RNG at startup. Previously ahash used a compile-time seed,
  which left hash maps vulnerable to hash-flood denial-of-service attacks.
- No source change is required. On wasm32 targets `runtime-rng` is not enabled (it would
  require a `getrandom` backend); the compile-time seed is kept there.
- **Migration:** transparent to code. No rename or API change. wasm targets using `ahash`
  retain compile-time seeding; non-wasm targets get OS-seeded hashing automatically.

### Security

#### redb: cache directory and file permissions hardened (Unix)
- The cache directory is now created with mode `0700` and the redb database file with mode
  `0600` on Unix, preventing other local users from reading cached data.
- The system-temp-dir fallback path is rejected if it resolves to a symlink or is
  group/world-writable, closing a symlink-attack and shared-temp-dir interception vector.

#### Redis: credential and error hardening
- Connection-string parse errors no longer include the URL or embedded password in the error
  message, preventing accidental credential leakage in logs.
- The legacy-JSON backward-read fallback now requires the exact version field value; a JSON
  object without the precise version marker is not treated as a cached entry.
- Client-side caching (`redis_async_cache`) rejects a connection URL that pins RESP2 (via the
  `?protocol=resp2` query parameter); RESP2 does not support client-side invalidation messages,
  so accepting it would silently serve stale data.
- `RedbCacheError::CacheDeserialization` and `RedisCacheError::CacheDeserialization` are
  documented as potentially sensitive (the `cached_value` bytes field may contain raw
  application data); callers should not log the full error in production.

### Fixed

#### `LruCache::cache_reset` no longer risks allocation panic after `set_max_size` grows the bound
- After `set_max_size` increased the capacity, a subsequent `cache_reset` could request a
  `HashMap` capacity larger than `isize::MAX / mem::size_of::<Entry>()`, causing an allocation
  panic. `cache_reset` now uses a fallible allocation path.

#### `lru_list::with_capacity` uses saturating arithmetic
- Internal LRU list pre-allocation used unchecked arithmetic that could overflow on extreme
  capacity values. The computation now saturates.

### Changed

#### `#[once]` emits a clear compile error when the value type names a function generic parameter
- Previously using `#[once]` with a value type that referenced a generic parameter of the
  annotated function produced a confusing downstream error. The macro now emits a clear
  compile-time diagnostic explaining that `#[once]` requires a concrete value type (generics
  are supported only when the value type itself does not reference a generic parameter).
- The docs are corrected to state that generics are supported only with a concrete value type.

#### Reserved name prefix `__cached` enforced across all three macros
- `#[cached]`, `#[once]`, and `#[concurrent_cached]` now reject a `name` attribute that starts
  with `__cached` (the prefix reserved for generated bindings).

#### `#[once]` rejects `sync_writes_buckets`
- `sync_writes_buckets` is inert on `#[once]` (which has no per-key lock). The macro now
  emits a clear error instead of silently ignoring the attribute.

#### Generated by-key lock bindings renamed to the `__cached_` hygiene convention
- Internal generated bindings used by the per-key (`by_key`) lock path are renamed to follow
  the `__cached_` prefix, consistent with the rest of the generated hygiene convention
  introduced in the main rc.1 batch.

#### Documentation corrections
- `#[cached]` `sync_writes` default is documented as `"by_key"` (was incorrectly documented
  as `false`).
- `refresh = true` requiring a TTL is noted at the attribute documentation.
- `ty` / `expires` interaction is documented.
- Sharded store `len` / `metrics` are documented as approximate (may include expired entries).
- Several method-doc and inline comment fixes.
- `#[must_use]` added to `CacheEvict::evict`.
- Macro error messages aligned for consistency.

## [3.0.0-rc.1 / cached_proc_macro 3.0.0-rc.1 / cached_proc_macro_types 3.0.0-rc.1] - 2026-06-21
> First 3.0 release candidate. The 3.0 API is not final and may change before the 3.0.0 release. See the [migration guide](docs/migrations/2.0-to-3.0.md).

### Breaking Changes

#### Redis TLS features split ([#231](https://github.com/jaemk/cached/issues/231))
- `redis_tokio` no longer implies native-tls. It now enables the TLS-agnostic `redis/tokio-comp`
  connection path. Add `redis_tokio_native_tls` (system TLS) or `redis_tokio_rustls` (pure-Rust
  TLS) alongside to restore TLS.
- `redis_smol` no longer implies native-tls. Add `redis_smol_native_tls` or `redis_smol_rustls`
  alongside if TLS is required.
- `redis_async_cache` is now also TLS-agnostic: it pulls `redis_tokio` + `redis/cache-aio`.
  Add `redis_tokio_native_tls` or `redis_tokio_rustls` alongside if TLS is required.
  _Updated before 3.0.0: `redis_async_cache` depends only on `redis/aio` and no longer implies `redis_tokio`; pair it with a runtime feature separately. See "Feature and dependency changes" in the 3.0.0-rc.3 notes._
- **Migration:** if you were relying on `redis_tokio`, `redis_smol`, or `redis_async_cache`
  for TLS connectivity, add the appropriate TLS backend feature (`redis_tokio_native_tls` /
  `redis_tokio_rustls` for Tokio; `redis_smol_native_tls` / `redis_smol_rustls` for smol)
  to your `Cargo.toml` features list.

#### Minimum supported Rust version
- MSRV raised from 1.85 to 1.89 (required by `redb` 4.x).

#### `DiskCache` backend: sled → redb ([#237](https://github.com/jaemk/cached/issues/237))
- Renamed `DiskCache` -> `RedbCache` (names the backend explicitly, like `RedisCache`); `DiskCache`, `DiskCacheBuilder`, `DiskCacheError`, and `DiskCacheBuildError` remain as type aliases, so existing code keeps compiling.
  _Superseded before 3.0.0: the type aliases were removed later in this same rc.1 entry (see "API audit follow-ups" below). Rename `DiskCache*` to `RedbCache*` directly._
- `DiskCache` is now backed by [`redb`](https://crates.io/crates/redb) 4.x instead of the unmaintained `sled`, dropping the RustSec-flagged `fxhash` transitive dependency. Still pure-Rust (no C toolchain).
- On-disk format changed: existing caches are not read (entries are recomputed); `DISK_FILE_VERSION` was bumped.
- `RedbCacheError::Storage` and `RedbCacheBuildError::Storage` now wrap `redb::Error` instead of `sled::Error`; `RedbCacheBuildError` gained an `Io` variant and dropped the never-constructed `MissingDiskPath` variant.
- Removed `DiskCache::connection()` / `connection_mut()`, `DiskCacheBuilder::connection_config`, and the `connection_config` macro attribute. The backend handle is no longer exposed.
- `durable` maps to redb durability and defaults to `true` (durable, fsync per write), so a disk cache persists by default. Set `false` to trade durability for write throughput: writes then use `Durability::None`, which is not persisted until a later durable commit, so they can be lost on process exit or crash; call `RedbCache::flush()` / `async_flush()` to force one.
- `DiskCacheBuilder::sync_to_disk_on_cache_change` is renamed to `durable`; the default flipped from `false` (no fsync) to `true` (durable). **Migration:** replace `.sync_to_disk_on_cache_change(false)` with `.durable(false)` to keep the no-fsync behavior; callers that set `.sync_to_disk_on_cache_change(true)` can drop the call entirely since `true` is now the default.

#### `new()` constructor consistency for stores
- In-memory stores (`UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`, and all six sharded variants) gained `Type::new()` / `Type::new(required_field)` constructors that return a ready-to-use cache. `builder()` is still available for non-default configuration.
- `RedbCache::new` (and its `DiskCache` alias), `RedisCache::new`, and `AsyncRedisCache::new` are removed. These previously returned a *builder*, conflicting with the convention that `new()` returns a ready store. Replace `::new(` with `::builder(` on these three types; the rest of the builder chain is unchanged.
  _Updated before 3.0.0: the builders now take the required field as a positional argument: `RedbCache::builder(name)`, `RedisCache::builder(prefix)`, `AsyncRedisCache::builder(prefix)`. See "Store builder API uniformity" below._

#### Macro `ttl` attribute replaced by `ttl_secs`, `ttl_millis`, and Duration expression
- `ttl = <integer>` (bare whole-second integer) is removed from `#[cached]` / `#[once]` / `#[concurrent_cached]`. The macro now accepts three mutually exclusive forms: `ttl_secs = N` (whole seconds, replaces the old integer form), `ttl_millis = N` (milliseconds, new in this release), or `ttl = "<Duration expr>"` (a string-literal Duration expression, e.g. `ttl = "Duration::from_secs(60)"`). Using the old bare-integer form produces an error directing you to `ttl_secs`.
- Builders gained `.ttl_secs(n)` and `.ttl_millis(n)` convenience methods alongside the existing `.ttl(Duration)`. All three target the same underlying field; the last call wins. Builder-level calls do not enforce mutual exclusion.

#### Short method aliases moved to `CachedExt` / `ConcurrentCachedExt`
- The short method aliases (`get`, `set`, `remove`, `remove_entry`, `clear`, `len`, `is_empty`, `delete`, `try_set`, `contains`, `hits`, `misses`, `metrics`, and the short `get_or_set_with` family) moved off `Cached` / `ConcurrentCached` onto blanket extension traits `CachedExt` / `ConcurrentCachedExt`, implemented for every `Cached` / `ConcurrentCached` type. The core traits keep only the `cache_`-prefixed methods, so a custom store implements a smaller surface. Both extension traits are re-exported from the crate root and the prelude. **Migration:** callers using `cached::prelude::*` need no change; others add `use cached::CachedExt;` / `use cached::ConcurrentCachedExt;` where they call a short alias, or use the `cache_`-prefixed form. Custom `impl Cached` / `impl ConcurrentCached` blocks must drop any short-alias methods (now provided by the blanket impl).

#### Macro attribute changes
- Removed the deprecated `size` attribute from `#[cached]` / `#[concurrent_cached]` (deprecated since 2.0). Use `max_size = N`; the macros still detect `size` and emit a compile error directing you to `max_size`.

#### Trait API changes
- `ConcurrentCachedAsync` cache operations are renamed with an `async_` prefix (`async_cache_get`, `async_cache_set`, `async_cache_remove`, `async_cache_remove_entry`, `async_cache_delete`), removing the `E0034` "multiple applicable items" error when both concurrent traits are imported.
- Split the concurrent cache trait surface to eliminate the remaining `E0034` "multiple applicable items in scope" error. `ConcurrentCached` and `ConcurrentCachedAsync` previously each declared identical synchronous helpers (`cache_size`, `len`, `is_empty`, `ttl`, `set_ttl`, `unset_ttl`, `refresh_on_hit`, `set_refresh_on_hit`); on a store implementing both traits (`RedbCache`, every `Sharded*` store) calling one of those with both traits in scope failed to compile. Those helpers now live on two new shared traits: introspection (`type Error`, `cache_size`, `len`, `is_empty`) on `ConcurrentCacheBase` (the supertrait of both concurrent traits, mirroring the single-owner `Cached` core), and the global-TTL controls (`ttl`, `set_ttl`, `unset_ttl`, `refresh_on_hit`, `set_refresh_on_hit`, plus a new validated `try_set_ttl` that rejects a zero `Duration` with `SetTtlError::ZeroTtl`) on `ConcurrentCacheTtl`. Only the TTL-capable concurrent stores (`ShardedTtlCache`, `ShardedLruTtlCache`, `RedisCache`, `AsyncRedisCache`, `RedbCache`) implement `ConcurrentCacheTtl`; the non-TTL sharded stores no longer expose `set_ttl`/`ttl`/etc. Both new traits are re-exported from the crate root and the prelude. **Migration:** custom `impl ConcurrentCached`/`ConcurrentCachedAsync` blocks must move their `type Error` (and any `cache_size`/`len`/`is_empty` override) into a single `impl ConcurrentCacheBase for X` block, and any TTL behavior into `impl ConcurrentCacheTtl for X`. Callers using `cached::prelude::*` need no change (both traits are imported); callers importing the concurrent traits individually should add `ConcurrentCacheBase` / `ConcurrentCacheTtl` where they call those helpers.
- `CacheTtl` and `CacheEvict` are now single-owner (`&mut self`) traits only, since `&mut self` was unusable on stores held through `Arc`/`static`. `CacheTtl` was removed from `DiskCache`, `RedisCache`, `AsyncRedisCache`, `ShardedTtlCache`, and `ShardedLruTtlCache`; `CacheEvict` from the four TTL/expiring sharded stores. Set TTL on concurrent stores via `ConcurrentCacheTtl::set_ttl` (`&self`), and evict via the new `ConcurrentCacheEvict` trait (`fn evict(&self) -> usize`, implemented by `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`). Single-owner in-memory stores are unchanged.
- `Cached::cache_get_or_set_with` / `cache_try_get_or_set_with` (and their `get_or_set_with` / `try_get_or_set_with` aliases) and `CachedAsync::async_get_or_set_with` / `async_try_get_or_set_with` now return `&V` / `Result<&V, E>` instead of `&mut V` / `Result<&mut V, E>` ([#179](https://github.com/jaemk/cached/issues/179)). New `*_mut` variants (`cache_get_or_set_with_mut`, `cache_try_get_or_set_with_mut`, `get_or_set_with_mut`, `try_get_or_set_with_mut`, `async_get_or_set_with_mut`, `async_try_get_or_set_with_mut`) preserve the mutable-reference behavior. External `impl`s of these traits must update their method signatures and implement the new `*_mut` required methods.
  _Note: the `CachedAsync` async method names cited above (`async_get_or_set_with`, `async_try_get_or_set_with`) were renamed to `async_cache_get_or_set_with` / `async_cache_try_get_or_set_with` by the same rc.1 entry (see "CachedAsync method renames" below), then `CachedAsync` was renamed to `CachedGetOrSetAsync` in rc.3. These intermediate names never appeared in any shipped release._
- `refresh_on_hit` and `set_refresh_on_hit` are now **required** methods on `CacheTtl` and `ConcurrentCacheTtl` (the trait-default bodies that returned `false` were removed). This fixes a latent bug: the concurrent stores (`ShardedTtlCache`, `ShardedLruTtlCache`, `RedisCache`, `AsyncRedisCache`, `RedbCache`) overrode only `set_refresh_on_hit`, so `ConcurrentCacheTtl::refresh_on_hit` always reported `false` through trait dispatch even after `set_refresh_on_hit(true)`; it now correctly reflects the configured flag. **Migration:** custom `impl CacheTtl`/`impl ConcurrentCacheTtl` blocks must now provide both methods explicitly (a non-refreshing store can return `false` and treat the setter as a no-op).

#### Other breaking changes
- Error enum variants dropped their redundant `Error` suffix: `RedbCacheError::{StorageError, CacheDeserializationError, CacheSerializationError}` became `{Storage, CacheDeserialization, CacheSerialization}`; `RedbCacheBuildError::ConnectionError` became `Storage` (names the backend, matching `RedbCacheError::Storage`); `RedisCacheError::{RedisCacheError, PoolError, CacheDeserializationError, CacheSerializationError}` became `{Redis, Pool, CacheDeserialization, CacheSerialization}`.
- The public store error enums (`RedbCacheError`, `RedbCacheBuildError`, `RedisCacheError`, `RedisCacheBuildError`, `BuildError`, and the `TtlSortedCache` error) are now `#[non_exhaustive]`, so external matches must include a wildcard arm.
- `RedbCache::remove_expired_entries` now returns `Result<usize, RedbCacheError>` (the number of entries removed) instead of `Result<(), RedbCacheError>`, matching `CacheEvict::evict` / `ConcurrentCacheEvict::evict`.
- `CacheMetrics.size` renamed to `entry_count` (the only field not matching its `cache_*` accessor).
- Builder refresh naming unified on `refresh_on_hit`: the `refresh()` alias was removed from the in-memory TTL builders, and `DiskCacheBuilder` / `RedisCacheBuilder` / `AsyncRedisCacheBuilder` `refresh` was renamed to `refresh_on_hit`. The `#[cached(refresh = true)]` attribute is unchanged.
- `cache_reset` (and `ConcurrentCached::cache_reset` / `ConcurrentCachedAsync::async_cache_reset`) no longer preserves the preallocated backing capacity. It now calls `clear()` + `shrink_to(initial_capacity)`, which the allocator may satisfy with a smaller allocation, so subsequent inserts up to the initial capacity may reallocate. To retain the allocation, recreate the cache instead of resetting it.

#### Redis and disk store changes
- Redis values are now serialized with MessagePack (`rmp-serde`) instead of JSON; the `redis_store` feature pulls `rmp-serde` instead of `serde_json`. Old (2.x) JSON-format entries are read transparently: the store tries MessagePack first, then falls back to `serde_json` for entries that carry a `version` key in the JSON object, and serves the value without recompute. New writes use MessagePack; old entries are rewritten as MessagePack on their next write. `RedisCacheError`'s serialize/deserialize variants carry `rmp_serde::encode::Error` / `rmp_serde::decode::Error` instead of `serde_json::Error`.
  _Updated before 3.0.0: the serialize/deserialize error sources are boxed as `Box<dyn std::error::Error + Send + Sync>` (see "redis/redb error types decoupled from their backing crates" in the 3.0.0-rc.3 notes); `rmp_serde::*::Error` is not directly pattern-matchable._
- Redis TTL now uses the millisecond commands `PSETEX` / `PEXPIRE`; sub-second TTLs are honored to the millisecond instead of rounded up to the next whole second. Whole-second TTLs are unchanged. Requires Redis 2.6+.
- `RedisCache::connection_string()` / `AsyncRedisCache::connection_string()` now return a `ConnectionString` newtype whose `Display` and `Debug` both redact credentials. Call `.reveal()` on the returned value to get the raw URL string.
- `RedbCacheError` and `RedbCacheBuildError` are now struct variants (named fields) matching the redis enums; `RedbCacheBuildError::Connection` is renamed `Storage`. The serialize/deserialize variants on both backends carry MessagePack error types, and `CacheDeserialization` gains a `cached_value: Vec<u8>` field holding the bytes that failed to decode. **Migration:** tuple patterns like `CacheSerialization(e)` become `CacheSerialization { source }`.
  _Updated before 3.0.0: the serialize/deserialize error sources on both backends are boxed (see "redis/redb error types decoupled from their backing crates" in the 3.0.0-rc.3 notes)._

#### Sharded store and error-type renames
- `ShardedCache` renamed to `ShardedUnboundCache` (along with `ShardedCacheBase` -> `ShardedUnboundCacheBase` and `ShardedCacheBuilder` -> `ShardedUnboundCacheBuilder`). The old name read as the umbrella for the whole sharded family while it only named the unbounded variant; the new name is parallel with `ShardedLruCache`, `ShardedTtlCache`, and the rest. No deprecated alias - rename at the call site.
- `ttl_sorted`'s dedicated error type is removed; `TtlSortedCache` now uses the shared `CacheSetError` (variant `TimeBounds`), the same type as `TtlCache` / `LruTtlCache`, so all three TTL stores report one error type. The previous `TtlSortedCacheError` name (and the `ttl_sorted::Error` it aliased) no longer exists; rename it to `CacheSetError`. The unused `From<ttl_sorted::Error> for std::io::Error` impl is removed; the store never surfaced errors through `io::Error`.

#### Required trait methods (custom `ConcurrentCached` / `CloneCached` impls)
- `cache_clear` and `cache_reset` are now required on `ConcurrentCached` (and `async_cache_clear` / `async_cache_reset` on `ConcurrentCachedAsync`). Their previous no-op `Ok(())` defaults silently did nothing; every built-in store already overrides both. A custom impl must now provide them. `cache_reset_metrics` keeps its no-op default.
- `cache_peek_with_expiry_status` is now required on `CloneCached` and `ConcurrentCloneCached`. The old provided defaults returned a wrong result (`(None, false)` / a side-effecting delegate) that silently broke `force_refresh` + `result_fallback` for custom stores. Every built-in store already overrides it; a custom expiry-capable store must provide a genuinely side-effect-free read.

#### Macro attribute and store-method removals
- The `unbound` attribute is removed from `#[cached]`. The default store (no `max_size`, `ttl`, or `expires`) is already an `UnboundCache`, so `#[cached(unbound)]` built an identical store to a bare `#[cached]`. The attribute is intercepted with a migration error; drop it.
- `#[concurrent_cached]`'s `refresh` attribute is now a plain `bool` (was `Option<bool>`), matching `#[cached]`. `refresh = false` is the default and no longer conflicts with `expires` or a `create` block - only `refresh = true` does. No change needed unless you relied on `refresh = false` erroring next to `expires`/`create`.
- The inherent `refresh_on_hit(&self) -> bool` and `set_refresh_on_hit(&mut self, bool)` methods on `TtlCache` and `LruTtlCache` are removed; they shadowed the `CacheTtl` trait methods, and the inherent setter returned `()` instead of the previous value. Bring `CacheTtl` into scope to call them (the trait setter returns the previous `bool`). The builder `refresh_on_hit(self, bool) -> Self` is unchanged.

#### Feature and toolchain
- The `wasm` cargo feature is removed. It gated nothing - `web-time` provides wasm-compatible time types transparently with no opt-in feature. Drop it from your feature list; wasm targets need nothing extra.
- The `disk_store` cargo feature is renamed to `redb_store`, naming the backend (`redb`) explicitly, parallel to the `redis_*` features. No backwards-compatible alias; rename it in your `Cargo.toml`.
- The `redis_ahash` cargo feature is removed. It enabled the `redis` crate's optional `ahash` feature and gated no `cached` code; enable `ahash` on your own `redis` dependency if needed.
- `cached_proc_macro_types` moved to edition 2024 and raised its `rust-version` to 1.89, matching the workspace; its version tracks `cached` in lockstep (3.0.0-rc.x), not a standalone `1.0`. `cached_proc_macro`'s `rust-version` is likewise raised to 1.89.

#### API audit follow-ups
- `LruTtlCache::iter_order` now returns `Vec<(K, (Option<Instant>, V))>` and `LruTtlCache::value_order` returns `Vec<(Option<Instant>, V)>`. The expiry instant is wrapped in `Option` (`None` means the entry never expires) to align with the per-entry expiry model introduced in this release. Previously both methods exposed a bare `Instant`. **Migration:** unwrap or pattern-match the `Option<Instant>` at call sites.
- `Cached::cache_try_set` (and its `try_set` alias) now return `Result<Option<V>, CacheSetError>` instead of `Result<Option<V>, Box<dyn std::error::Error>>`. `CacheSetError` is a new `#[non_exhaustive]` enum (variant `TimeBounds`) re-exported from the crate root, so callers can match on the failure instead of handling an opaque boxed error. Custom `Cached` impls that override `cache_try_set` must update the return type.
- The `DiskCache` / `DiskCacheBuilder` / `DiskCacheError` / `DiskCacheBuildError` aliases for the `Redb*` types are removed (the rename to `RedbCache` happened earlier in this release; the aliases are not carried forward). Rename `DiskCache*` to `RedbCache*`.
- The `store()` accessors on `UnboundCache`, `TtlCache`, `LruTtlCache`, and `ExpiringLruCache` are removed. They exposed the internal backing map (and leaked the internal `TimedEntry<V>` wrapper) and existed on only some stores. Use the public `Cached` API (`cache_get`, `cache_size`, iteration helpers) instead.
- `ShardHasher` now requires `Clone` as a supertrait (the `deep_clone` / `copy_from` methods already required it de facto). Custom `ShardHasher` impls must be `Clone`; `DefaultShardHasher` already is.
- `#[must_use]` was added to the pure-query trait methods (`cache_size`/`len`/`is_empty`/`metrics`/`hits`/`misses`/`ttl`/`refresh_on_hit`/...) and to `cache_remove`/`cache_remove_entry`. Code that discards these results under `-D warnings` will need `let _ = ...`. The short `remove`/`remove_entry` aliases are intentionally left un-annotated for for-effect removal.
- The `Expires` trait gained a default method `expires_at(&self) -> Option<Instant>` returning the value's expiry instant when tracked (`None` by default / when unknown). It is advisory/observability only; `is_expired()` remains the authoritative liveness check. Existing `impl Expires` blocks (which provide only `is_expired`) get the default for free.

#### Store builder API uniformity (C1)
> Updated before 3.0.0: the builders take the primary required field as a positional argument -- `RedbCache::builder(name)`, `RedisCache::builder(prefix)`, `AsyncRedisCache::builder(prefix)`. The no-arg `::builder()` entry point described below was superseded; redis `ttl` is optional (no set ttl stores keys without expiry).
- `RedbCache::builder(name)`, `RedisCache::builder(prefix, ttl)`, and `AsyncRedisCache::builder(prefix, ttl)` now take no arguments: `::builder()`. Required fields (`name`, `prefix`, `ttl`) are set via dedicated setters and validated in `build()`, which returns `BuildError::MissingRequired(field_name)` if a required field is absent. All store builders now share a uniform no-arg `::builder()` entry point.

#### `CachedAsync` method renames (I1)
> Superseded before 3.0.0: `CachedAsync` was renamed to `CachedGetOrSetAsync` and its four sync passthroughs (`async_cache_get` / `async_cache_set` / `async_cache_remove` / `async_cache_clear`) removed. See the "`CachedAsync` renamed to `CachedGetOrSetAsync`; sync passthroughs removed" entry in the 3.0.0-rc.3 notes.
- The four `async_get_or_set_with*` methods on the `CachedAsync` trait are renamed with the `cache_` namespace infix, matching the `Cached` trait convention: `async_get_or_set_with` -> `async_cache_get_or_set_with`, `async_get_or_set_with_mut` -> `async_cache_get_or_set_with_mut`, `async_try_get_or_set_with` -> `async_cache_try_get_or_set_with`, `async_try_get_or_set_with_mut` -> `async_cache_try_get_or_set_with_mut`. The four shorthand methods are likewise renamed: `get_async` -> `async_cache_get`, `set_async` -> `async_cache_set`, `remove_async` -> `async_cache_remove`, `clear_async` -> `async_cache_clear`. Every `CachedAsync` method now uses the `async_cache_*` namespace. The `ConcurrentCachedAsync` trait is unchanged.

#### Error vocabulary for TTL validation (I4+I5)
- `BuildError::InvalidTtl { ttl }` is removed. A zero TTL at build time now yields `BuildError::InvalidValue { field: "ttl", reason: "must be greater than zero" }`, which is more general (the variant can represent other field-validation failures) and more descriptive.
- `RedisCacheBuildError::InvalidTtl` and `RedbCacheBuildError::InvalidTtl` are renamed to `RedisCacheBuildError::Build(BuildError)` and `RedbCacheBuildError::Build(BuildError)` respectively, wrapping the inner `BuildError` instead of duplicating its content. Exhaustive matches on these enums must be updated.

#### `set_ttl(Duration::ZERO)` now disables expiry for future inserts only (I2)
- A zero `Duration` passed to any `set_ttl` surface now means "expiry disabled" -- exactly equivalent to `unset_ttl()`, with future-inserted entries never expiring. It no longer panics (sharded stores) and no longer means "expire immediately". This is uniform across the sharded `ShardedTtlCache` / `ShardedLruTtlCache`, the single-owner `TtlCache` / `LruTtlCache`, and `RedisCache` / `AsyncRedisCache`. For the Redis stores a disabled TTL writes keys WITHOUT any expiry (a plain `SET` instead of `SETEX`), and the refresh-on-hit path issues no `EXPIRE`. `build()` still rejects a zero TTL, and `CacheTtl::try_set_ttl(0)` still returns `SetTtlError::ZeroTtl` -- those are the strict "give me a real ttl" paths; to disable expiry, call `set_ttl(0)` or `unset_ttl()`. (`TtlSortedCache` now matches the other TTL stores: a zero TTL disables expiry for future inserts, where it previously meant immediate expiry. Its per-entry expiry is now `Option<Instant>` (`None` = never expires), ordered so never-expiring entries are evicted last under a size cap.) Because TTL stores now track per-entry expiry, `set_ttl` affects future inserts only; existing entries keep their computed expiry.

#### `#[cached(refresh = true)]` without a TTL is now a compile error (I7)
- Using `refresh = true` on `#[cached]` without also specifying a TTL (`ttl_secs`, `ttl_millis`, or `ttl`) is now a compile error. Previously the attribute was silently ignored in this configuration. This matches the existing behavior of `#[concurrent_cached]`, which has always required a TTL alongside `refresh = true`.

#### `sync_writes` default on `#[cached]` changed to `"by_key"`
> Reverted before 3.0.0: the default is again no synchronization. See the "`sync_writes` default reverted to no synchronization" entry in the 3.0.0-rc.3 notes.
- A bare `#[cached]` now uses `sync_writes = "by_key"`: concurrent first calls for the same key are deduplicated through bucketed per-key locks. Previously the default was no synchronization, mirroring Python's `functools.lru_cache`. Opt out with `sync_writes = false`. `result_fallback` with no explicit `sync_writes` implicitly uses `Disabled` (not `"by_key"`). `#[once]` and `#[concurrent_cached]` defaults are unchanged.

#### `Cached` trait: `type Error` associated type; `cache_try_set` / `try_set` return `Result<Option<V>, Self::Error>`
- `Cached` gained `type Error`. Built-in infallible stores (`UnboundCache`, `LruCache`, sharded stores, `ExpiringCache`, `ExpiringLruCache`) set `type Error = std::convert::Infallible`. `TtlCache` / `LruTtlCache` / `TtlSortedCache` set `type Error = CacheSetError`. Custom `impl Cached` blocks must add the associated type; call sites that bound the error as `CacheSetError` for an infallible store must update to `Infallible` or drop the annotation.

#### Sharded stores: inherent `get`/`set`/`remove`/`remove_entry`/`delete`/`reset` return unwrapped values
- The six concrete sharded types now expose inherent methods returning `Option<V>`, `()`, and `bool` directly, so `store.get(&k)` is `Option<V>` rather than `Result<Option<V>, Infallible>`. To use the `Result`-returning trait methods, call through `ConcurrentCached` or use the `cache_`-prefixed names.

#### `TimedEntry` is no longer public
- `cached::TimedEntry` is now `pub(crate)`. Any `use cached::TimedEntry;` import fails. The type was only reachable after the `store()` accessors were removed.

#### Runtime decoupling: `async_tokio_rt_multi_thread` removed; `async` no longer pulls tokio; `async_sync` re-exports changed
- `async_tokio_rt_multi_thread` cargo feature removed. Users who need `tokio/rt-multi-thread` (e.g. for `#[tokio::test]`) must add `tokio` with `rt-multi-thread` directly to their own dev-dependencies.
- The `async` feature no longer implies `tokio`. It now pulls only `async-lock` and `blocking` (runtime-agnostic). smol/async-std async users no longer compile tokio.
  _Updated before 3.0.0: `blocking` moved from `async` to `redb_store` (see "Feature and dependency changes" in the 3.0.0-rc.3 notes); `async` depends only on `dep:async-lock` at HEAD._
- `cached::async_sync::{Mutex, RwLock, OnceCell}` now re-export from `async-lock` instead of `tokio::sync`. `OnceCell` from `async-lock` has no `const_new()`; replace with `OnceCell::new()`.
- Async `RedbCache` runs blocking redb work on the `blocking` crate's thread pool instead of `tokio::spawn_blocking`, making it runtime-agnostic. `RedbCacheError::BackgroundTaskFailed` variant removed.

#### Macro attributes `convert`, `create`, `force_refresh`, `map_error`, `cache_prefix_block` accept unquoted Rust
- These attributes now accept unquoted Rust (e.g. `convert = { format!("{a}") }`, `map_error = |e| MyErr(e)`, `force_refresh = { id == 0 }`). The quoted-string form still works. `force_refresh = true` (a bare bool) is now also valid. `ty` and `key` remain quoted strings.

#### `map_error` optional on disk/Redis `#[concurrent_cached]`
- When omitted, the generated code uses `.map_err(Into::into)?`. The function's error type must implement `From<RedbCacheError>` (disk) or `From<RedisCacheError>` (Redis). Supplying `map_error` still works and requires no change.

#### `companions_vis` macro attribute
- `#[cached]`, `#[once]`, and `#[concurrent_cached]` accept `companions_vis = "<vis>"` to set the visibility of the generated `{fn}_no_cache` and `{fn}_prime_cache` companions independently of the cached function's own visibility. Defaults to the cached function's visibility (no change for existing code).

### Additive / non-breaking
- `cached::prelude` re-exports the common traits for a single glob import.
- Custom hasher on the non-sharded in-memory stores: `UnboundCache`, `LruCache`, `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, and `ExpiringLruCache` gained a hasher type parameter defaulted to `DefaultHashBuilder` (e.g. `UnboundCache<K, V, S = DefaultHashBuilder>`) and a `.hasher(s)` builder method, mirroring the sharded stores. `DefaultHashBuilder` (ahash under the `ahash` feature, else std `RandomState`) is re-exported from the crate root. Naming a store as `UnboundCache<K, V>` is unchanged.
- Concurrent metrics through a trait: `ConcurrentCacheBase` gained `cache_hits` / `cache_misses` / `cache_capacity` / `cache_evictions` and a default `metrics() -> CacheMetrics`, so a `ConcurrentCached` / `ConcurrentCachedAsync` bound can read a sharded store's metrics generically (the inherent `metrics()` is retained), mirroring the accessors on `Cached`.
- `#[cached]` / `#[once]` now reject the concurrent-store-only attributes `disk`, `redis`, and `map_error` with a clear error pointing to `#[concurrent_cached]`, instead of a generic unknown-field message.
- The `len` / `cache_size` / `iter` / `evict` contract on lazy-eviction stores is documented in one place: `len` / `cache_size` returns the stored count without an expiry scan (may include expired entries), `iter` omits expired entries from the view without removing them, and `evict()` reclaims expired entries and yields an accurate live count. Behavior unchanged.
- `ConcurrentCached` / `ConcurrentCachedAsync` gained a no-op-default `cache_reset_metrics` / `async_cache_reset_metrics` (`&self`). The sharded stores override it to zero their per-shard counters; `RedbCache` and `RedisCache` / `AsyncRedisCache` keep the no-op default (they track no in-memory metrics). `cache_clear` / `cache_reset` (and their async counterparts) are required methods, not defaults - see the breaking-changes entry above.
- `CacheTtl::try_set_ttl` - the strict "give me a real ttl" variant of `set_ttl` that returns the new `SetTtlError` (variant `ZeroTtl`) when passed a zero TTL instead of interpreting it as "disable expiry". Use it when a zero TTL is a caller error rather than a request to disable expiry (which `set_ttl(0)` / `unset_ttl()` do). Provided default, so existing `CacheTtl` impls get it for free.
- `ConcurrentCached` / `ConcurrentCachedAsync` gained ergonomic `len` / `is_empty` aliases over `cache_size`, mirroring the sync `Cached` trait.
- `Debug` is implemented for `RedisCache`, `AsyncRedisCache`, and `RedbCache` (redacted: prints only namespace/prefix/path/ttl/refresh, never connection strings or credentials).
- `PartialEq` / `Eq` are implemented for `ExpiringCache` and `ExpiringLruCache` (equal when their stored entries are equal).
- `#[must_use]` parity across the sharded builders, and the `with_hasher` doc alias is spread to every sharded builder's `hasher` method for discoverability.
- Malformed `key` / `convert` macro attributes now produce a contextual error explaining what the attribute expects, with an example, instead of a bare `syn` "unexpected token".
- `redis_connection_manager` now builds on the `redis_tokio` feature instead of re-listing redis sub-features (resolved feature set unchanged).
  _Updated before 3.0.0: `redis_connection_manager` is runtime-agnostic and no longer implies `redis_tokio` (see "Feature and dependency changes" in the 3.0.0-rc.3 notes)._
- `ConcurrentCached` / `ConcurrentCachedAsync` gained a defaulted `cache_get_or_set_with` / `async_cache_get_or_set_with` (with a `get_or_set_with` alias), mirroring the single-owner traits. The default is a get-then-set (non-atomic; a concurrent miss may run the factory more than once).
- `ConcurrentCached` / `ConcurrentCachedAsync` gained a defaulted `refresh_on_hit()` getter, and `set_refresh_on_hit` is now defaulted (`{ false }`) so custom impls no longer need to write it.
  _Superseded before 3.0.0: `refresh_on_hit` and `set_refresh_on_hit` are required methods on `ConcurrentCacheTtl` (see the "Trait API changes" entry above in this rc.1 section)._
- `RedisCache` and `AsyncRedisCache` now implement `Clone` (Arc-backed pool / cloneable connection). `RedbCache` stays non-`Clone`.
- The `name` macro attribute is validated as a Rust identifier: an invalid `name` now produces a spanned "`name` must be a valid Rust identifier" error instead of a macro panic.
- `#[once]` and `#[concurrent_cached]` now reject the `#[cached]`-only sync attributes (`sync_lock`, `unsync_reads`, and `sync_writes_buckets` on `#[concurrent_cached]`) with a clear "not supported on this macro" message instead of a generic unknown-field error.
- `RedisCacheBuildError::MissingConnectionString` and the redis (de)serialization errors now expose their wrapped cause via `Error::source()` and render it through `Display` (cleaner than the previous debug formatting).
- `ConcurrentCacheEvict::evict` is now `#[must_use]`.
- `RedbCache::flush` and `RedbCache::async_flush` force a durable (fsync) commit, so you can run with `durable(false)` for cheap writes and flush at chosen points (periodically or before shutdown) to persist them.
- `RedbCache::disk_path()` returns the path of the on-disk redb database file backing the cache.
- New `SerializeCached` / `SerializeCachedAsync` traits with `cache_set_ref(&self, &K, &V)` / `async_cache_set_ref`, implemented by `RedisCache` / `AsyncRedisCache` / `RedbCache`, let serialize-backed stores set an entry without taking ownership of the key/value. The `#[concurrent_cached]` macro now calls the borrowed setter for any store implementing these traits (the built-in `redis`/`disk` stores and custom `ty`/`create` stores alike), avoiding an extra value clone on the set ([#196](https://github.com/jaemk/cached/issues/196), [#195](https://github.com/jaemk/cached/issues/195)).
- `RedisCache` / `AsyncRedisCache` now implement `cache_clear` / `async_cache_clear` via a namespace-scoped `SCAN` + batched `DEL` (O(n), scoped to the cache's prefix, not a server flush), and `cache_reset` / `async_cache_reset` delegate to them (redis tracks no in-memory metrics, matching `RedbCache`). Glob metacharacters (`*`, `?`, `[`, `]`, `\`) in the namespace/prefix are escaped in the `SCAN` pattern so they match literally ([#200](https://github.com/jaemk/cached/issues/200)). `RedisCacheBuilder` / `AsyncRedisCacheBuilder` `build()` now returns `RedisCacheBuildError::EmptyScope` when both the namespace (after trimming trailing `:`) and the prefix are empty, since that would make `cache_clear` run `SCAN MATCH *` and delete every key in the database. (This is technically a breaking behavior change for any caller that explicitly set the namespace to empty and left the prefix empty; the default namespace `"cached-redis-store:"` is non-empty so normal usage is unaffected. See the [migration guide](docs/migrations/2.0-to-3.0.md#9-rediscachebuilderbuild--asyncrediscachebuilderbuild-return-emptyscope-when-namespace-and-prefix-are-both-empty) for details.)
- `LruCache::set_max_size` / `try_set_max_size` resize a live cache, eagerly evicting LRU entries when shrinking (paralleling `TtlSortedCache`'s existing `set_max_size` / `try_set_max_size`, which set the new bound but evict lazily on the next insert rather than eagerly); `LruTtlCache` and `ExpiringLruCache` gained the same two methods (delegating to their inner LRU) for parity ([#180](https://github.com/jaemk/cached/issues/180)). All four `try_set_max_size` methods now return a single dedicated `SetMaxSizeError` (variant `ZeroSize`) instead of the builder `BuildError` (LRU family) or a `std::io::Error` (`TtlSortedCache`), so the runtime-resize error is self-describing and consistent across stores.
- `RedbCacheBuilder::build()` now validates `cache_name` (used as a filename component) and returns `RedbCacheBuildError::InvalidCacheName` if it is empty, contains a path separator (`/` or `\`), or is a path-traversal component (`.` or `..`), which would otherwise silently create subdirectories, escape the cache directory, or produce a meaningless filename.
- `#[cached]` / `#[concurrent_cached]` / `#[once]` gained a `ttl_millis = N` attribute for sub-second TTLs (milliseconds); mutually exclusive with `ttl`, `ttl_secs`, and `expires`, with a compile error if any are combined ([#149](https://github.com/jaemk/cached/issues/149)).
- `#[cached]` / `#[concurrent_cached]` / `#[once]` gained a `force_refresh = "{ <bool expr> }"` attribute (a curly-brace expression block over the function's arguments, like `convert`) that bypasses the cached value and recomputes when the expression is true. On `#[once]` it overwrites the single shared value (there is no per-call key, so unlike `#[cached]` there is no "exclude the flag from the key" caveat). When combined with `result_fallback = true`, a force-refreshed call that returns `Err` still serves the previously cached `Ok` value (the fallback wins over the bypass), and capturing that fallback value leaves no read side effects on the bypassed entry (no TTL renewal, recency update, or hit-counter change) on both `#[cached]` and `#[concurrent_cached]` ([#146](https://github.com/jaemk/cached/issues/146)).
- `#[cached]` / `#[concurrent_cached]` / `#[once]` gained an `in_impl = true` attribute, allowing them on methods inside `impl` blocks (the generated cache static lives in the function body); `self`-receiver methods are accepted only under `in_impl` (a `convert` block alone cannot rescue them, since the cache static cannot live at `impl` scope) ([#16](https://github.com/jaemk/cached/issues/16), [#140](https://github.com/jaemk/cached/issues/140)).
- `#[cached]` / `#[concurrent_cached]` accept reference arguments (`&T`, `Option<&T>`) on the default-key path, deriving an owned key (`<T as ToOwned>::Owned`) without requiring a `convert` ([#202](https://github.com/jaemk/cached/issues/202), [#203](https://github.com/jaemk/cached/issues/203)).
- The macros resolve the crate root via `proc-macro-crate`, so a renamed or re-exported `cached` dependency works ([#157](https://github.com/jaemk/cached/issues/157)).
- Macro-introduced bindings are now hygienically named (`__cached_*`), so function arguments named `key`, `cache`, or `result` no longer collide with generated code ([#230](https://github.com/jaemk/cached/issues/230), [#114](https://github.com/jaemk/cached/issues/114)).
- Applying `#[cached]` / `#[concurrent_cached]` to a generic function without a `key` + `convert` now produces a clear compile error (each monomorphization would need its own static); generics are supported when `key` + `convert` pin a concrete key type ([#80](https://github.com/jaemk/cached/issues/80)).
- The release workflow now creates a git tag and GitHub release for each workspace crate that is newly published, via `bin/tag-release.sh`. The root crate keeps the bare `vX.Y.Z` tag; subcrates are tagged `<crate-name>-vX.Y.Z` ([#245](https://github.com/jaemk/cached/issues/245)).
- Doc fixes: corrected the "sharded stores expose inherent helpers" note, added a `Cached::get` mutability note, documented the sharded-LRU minimum-per-shard capacity, named floats as the canonical `convert` case ([#78](https://github.com/jaemk/cached/issues/78)), and added a cache-invalidation example ([#21](https://github.com/jaemk/cached/issues/21)) and a struct-method example ([#236](https://github.com/jaemk/cached/pull/236)).
- `hashbrown` updated to 0.17 (internal). Dev-only: `criterion` 0.8, `googletest` 0.14.
- `#[once]` now rejects the `#[cached]`-only attributes (`result_fallback`, `refresh`, `max_size`, `ty`, `create`, `key`, `convert`) with clear "not supported on `#[once]`" messages instead of a generic unknown-field error (I6).
- `#[must_use]` added to `CacheEvict::evict` and the single-owner inherent `evict` methods (I3).
- A non-string `force_refresh` value (e.g. `force_refresh = true` instead of the required block form `force_refresh = "{ ... }"`) now produces a helpful error message explaining the expected syntax (8b).
- TTL stores (`TtlCache`, `LruTtlCache`, `ShardedTtlCache`, `ShardedLruTtlCache`) now store per-entry expiry timestamps; `set_ttl` applies to future inserts only; `refresh_on_hit` recomputes expiry from the current TTL at access time.
- Per-entry expiry on the sharded TTL stores removes the need to re-read the global TTL on every lookup and eliminates a class of time-skew bugs where entries inserted before a `set_ttl` change could expire at unexpected times.
- `async_sync::{Mutex, RwLock, OnceCell}` now re-export from `async-lock`; async `RedbCache` uses the `blocking` crate thread pool (runtime-agnostic). Async smol/async-std users no longer pull tokio.
- Inherent `get`/`set`/`remove`/`remove_entry`/`delete`/`reset` on the six sharded types return unwrapped values directly (no `Result` wrapper).
- Macro attributes `convert`, `create`, `force_refresh`, `map_error`, `cache_prefix_block` accept unquoted Rust in addition to the existing quoted-string form. `force_refresh = true` (bare bool) is now valid.
- `map_error` is optional on `#[concurrent_cached(disk = true)]` and Redis-backed `#[concurrent_cached]`; when omitted the generated code uses `.map_err(Into::into)?`.
- `companions_vis = "<vis>"` attribute on all three macros controls the visibility of generated `{fn}_no_cache` and `{fn}_prime_cache` companions.
- `RedisCacheError`, `RedbCacheError`, and their build-error siblings expose `is_deserialization() -> bool`, a predicate returning `true` for `CacheDeserialization` variants, so callers can distinguish a codec failure from a storage or network error without a full match.
- The `async_core` cargo feature enables the async trait definitions (`CachedAsync` / `SerializeCachedAsync` / `ConcurrentCachedAsync` and their supertrait machinery) without pulling the `async-lock` runtime dependency. Use it when you need the async trait bounds but supply your own synchronization. For most users the `async` feature is the right choice; it also enables `async-lock`.

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
