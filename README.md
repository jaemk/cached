# cached

[![Build Status](https://github.com/jaemk/cached/actions/workflows/build.yml/badge.svg)](https://github.com/jaemk/cached/actions/workflows/build.yml)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Caching structures and simplified function memoization

`cached` provides implementations of several caching structures as well as macros
for defining memoized functions.

When using the store types directly, start with `use cached::prelude::*;`. The short
method names (`get`/`set`/`len`/...) live on blanket extension traits, and the prelude
brings every trait in with one import (see Method naming below).

Requires Rust >= 1.92. (The `async_core` / `async` features do not compile before 1.92: the
`CachedGetOrSetAsync` default bodies hit a borrowck limitation rustc attributes to
[rust-lang/rust#100013](https://github.com/rust-lang/rust/issues/100013). `rust-version` is a
single crate-level value, so the floor applies to every feature set.)

Memoized functions defined using `#[cached]`/`#[once]` macros are thread-safe with the backing
function-cache wrapped in a mutex/rwlock. `#[concurrent_cached]` functions are thread-safe via the
store's own internal synchronization: sharded stores use per-shard `parking_lot::RwLock`; Redis and
disk stores rely on their respective server/file-system concurrency.
By default, `#[cached]` uses no write synchronization: concurrent uncached calls for the same key may
each compute independently and overwrite each other, matching the 2.x behavior and Python's
`functools.lru_cache`. Set `sync_writes = "by_key"` to deduplicate concurrent first calls for the same
key through bucketed per-key locks. Set `sync_writes = true` (or `"default"`) to hold the whole-cache
lock for the duration of each miss. Note: `"by_key"` holds the per-key bucket lock across the entire
function body, so it must not be used on recursive or re-entrant memoized functions (deadlock risk when
keys in the active call chain share a bucket). `#[once]` defaults to no synchronization (add
`sync_writes = true` to serialize concurrent first-calls); `#[concurrent_cached]` does not support
`sync_writes`. The number of per-key lock buckets for `"by_key"` is tunable with
`sync_writes_buckets = N` (default 64).

- See [`cached::stores` docs](https://docs.rs/cached/latest/cached/stores/index.html) for the available cache stores.
- See [`macros` docs](https://docs.rs/cached/latest/cached/macros/index.html) for more macro examples.

> **Upgrading from 2.x?** See the
> [migration guide](https://github.com/jaemk/cached/blob/master/docs/migrations/2.0-to-3.0-human.md)
> for a step-by-step walkthrough, or the
> [agent-oriented guide](https://github.com/jaemk/cached/blob/master/docs/migrations/2.0-to-3.0.md)
> for the complete breaking-change list.
>
> **Upgrading from 1.x?** 2.0 contains breaking changes (new `cache_remove_entry` required method,
> `Result`/`Option` caching behavior flipped to smart-by-default, `result`/`option` attributes
> removed, and more). See the
> [2.0 migration guide](https://github.com/jaemk/cached/blob/master/docs/migrations/1.1-to-2.0-human.md)
> for a step-by-step walkthrough.
>
> **Upgrading from a pre-1.0 release?** 1.0 contains breaking changes (store
> renames, removed declarative macros, renamed macro/builder attributes, and a
> changed Redis key format). See the
> [1.0 migration guide](https://github.com/jaemk/cached/blob/master/docs/migrations/0.x-to-1.0-human.md)
> for a step-by-step walkthrough, or the
> [agent-oriented guide](https://github.com/jaemk/cached/blob/master/docs/migrations/0.x-to-1.0.md)
> for automated migration tooling.

**Method naming**

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

**Features**

- `default`: Include `proc_macro`, `ahash`, and `time_stores` features
- `proc_macro`: Include proc macros
- `ahash`: Enable the optional `ahash` hasher as default hashing algorithm.
- `async_core`: Async trait definitions (the runtime-agnostic async cache traits) without the `async-lock` dependency. Enabled by `async`.
- `async`: Include support for async functions and async cache stores (runtime-agnostic; no tokio dependency; uses `async-lock`)
- `redis_store`: Include Redis cache store
- `redis_smol`: Include async Redis support using `smol` (no TLS); implies `redis_store` and `async`
- `redis_smol_native_tls`: `redis_smol` + TLS via `native-tls` (system TLS library)
- `redis_smol_rustls`: `redis_smol` + TLS via `rustls` (pure-Rust TLS)
- `redis_tokio`: Include async Redis support using `tokio` (no TLS); implies `redis_store` and `async`
- `redis_tokio_native_tls`: `redis_tokio` + TLS via `native-tls` (system TLS library)
- `redis_tokio_rustls`: `redis_tokio` + TLS via `rustls` (pure-Rust TLS)
- `redis_connection_manager`: Enable the optional `connection-manager` capability of `redis`. Additive: async redis
  caches keep using a `MultiplexedConnection` by default; opt a specific cache into the auto-reconnecting connection
  manager with `.connection_manager(true)` on its builder. This capability feature pulls in `redis_store` and `async`
  itself, but it is runtime-agnostic (`redis/connection-manager` needs only `redis/aio`), so it carries no runtime:
  pair it with a runtime feature (`redis_tokio*` or `redis_smol*`) to actually connect; enabling it alone leaves you
  without a runtime. Does **not** enable TLS.
- `redis_async_cache`: Enable Redis client-side caching over RESP3 for async Redis caches.
  Implies `async` and `redis_store`, but is runtime-agnostic (`redis/cache-aio` needs only `redis/aio`): pair it with a
  runtime feature (`redis_tokio*` or `redis_smol*`) or the build has no runtime to connect with. Does not enable TLS.
- `redb_store`: Include disk cache store
- `time_stores`: Include time-based cache stores ([`TtlCache`](https://docs.rs/cached/latest/cached/struct.TtlCache.html), [`LruTtlCache`](https://docs.rs/cached/latest/cached/struct.LruTtlCache.html), [`TtlSortedCache`](https://docs.rs/cached/latest/cached/struct.TtlSortedCache.html), [`ShardedTtlCache`](https://docs.rs/cached/latest/cached/struct.ShardedTtlCache.html), and [`ShardedLruTtlCache`](https://docs.rs/cached/latest/cached/struct.ShardedLruTtlCache.html)).
  Also required when using `#[cached(ttl_secs = ...)]`, `#[cached(ttl = ...)]`, `#[cached(ttl_millis = ...)]`, `#[concurrent_cached(ttl_secs = ...)]`, `#[concurrent_cached(ttl = ...)]`, or `#[concurrent_cached(ttl_millis = ...)]` on the default in-memory path. (`#[once]` has its own ungated timer, so `#[once(ttl_secs = ...)]` does NOT require this feature.)
  Disable this feature when targeting environments without system time support (e.g. `wasm32-unknown-unknown` without WASI or JS).

The procedural macros (`#[cached]`, `#[once]`, `#[concurrent_cached]`) offer a number of features, including async support.
See the [`macros`](https://docs.rs/cached/latest/cached/macros/index.html) module for more samples, and the
[`examples`](https://github.com/jaemk/cached/tree/master/examples) directory for runnable snippets.

Any custom cache that implements `cached::Cached` can be used with the `#[cached]`/`#[once]` macros in place of the built-ins (`cached::CachedGetOrSetAsync` additionally memoizes an async closure over such a store).
Any custom cache that implements `cached::ConcurrentCached`/`cached::ConcurrentCachedAsync` can be used with the `#[concurrent_cached]` macro.

**Macro quick reference**

| Use case | Annotated signature |
|---|---|
| **`#[cached]`** | |
| Unbounded memoize (default; concurrent misses each compute independently) | `#[cached] fn fib(n: u64) -> u64` |
| Unbounded memoize, explicit no-sync (same as default) | `#[cached(sync_writes = false)] fn fib(n: u64) -> u64` |
| LRU-bounded — evict past N entries | `#[cached(max_size = 1_000)] fn lookup(id: u32) -> Row` |
| TTL — expire results after N whole seconds | `#[cached(ttl_secs = 60)] fn config() -> Config` |
| TTL as a Duration expression (inlined verbatim, so `Duration` must be in scope; see note below) | `#[cached(ttl = "Duration::from_secs(60)")] fn config() -> Config` |
| TTL in milliseconds (sub-second capable; Redis honors millisecond TTL via PSETEX/PEXPIRE) | `#[cached(ttl_millis = 500)] fn poll(id: u64) -> Status` |
| LRU + TTL | `#[cached(max_size = 500, ttl_secs = 300)] fn search(q: String) -> Vec<Hit>` |
| Don't cache `None` returns (implicit for `Option<T>`) | `#[cached] fn find(id: u64) -> Option<User>` |
| Don't cache `Err` returns (implicit for `Result<T, E>`) | `#[cached] fn load(id: u64) -> Result<Data, E>` |
| Force-cache `None` returns | `#[cached(cache_none = true)] fn find(id: u64) -> Option<User>` |
| Force-cache `Err` returns | `#[cached(cache_err = true)] fn load(id: u64) -> Result<Data, E>` |
| Serve stale value when function returns `Err` | `#[cached(result_fallback = true, ttl_secs = 60)] fn fetch(id: u64) -> Result<Data, E>` |
| Per-value / dynamic per-entry TTL (value carries its own expiry) | `#[cached(expires = true)] fn token(scope: String) -> Token` |
| Deduplicate concurrent first calls per key (opt-in; do not use on recursive functions) | `#[cached(ttl_secs = 30, sync_writes = "by_key")] fn expensive(id: u64) -> Payload` |
| Recompute when an expression over the args is true | `#[cached(force_refresh = { id == 0 })] fn fetch(id: u64) -> Data` |
| Force-refresh via a dedicated flag (exclude it from the key) | `#[cached(key = "u64", convert = { id }, force_refresh = { refresh })] fn fetch(id: u64, refresh: bool) -> Data { let _ = refresh; … }` — the generated guard reads `refresh` to decide whether to bypass the cache; the function body still receives `refresh` as a normal parameter, so if your body does not otherwise use it, add `let _ = refresh;` (or `#[allow(unused_variables)]`) to silence the unused-variable warning |
| Cache a method inside an `impl` block (one cache shared across all instances) | `#[cached(in_impl = true)] fn load(&self, id: u64) -> Data` |
| Control visibility of generated `_no_cache` / `_prime_cache` companions | `#[cached(companions_vis = "pub(crate)")] pub fn compute(x: u64) -> u64` |
| Async | `#[cached(max_size = 100)] async fn remote(id: u64) -> Data` |
| **`#[once]`** | |
| Compute and cache a global value forever | `#[once] fn app_config() -> Config` |
| Refresh a global value periodically | `#[once(ttl_secs = 300, sync_writes = true)] fn pubkey() -> Key` |
| TTL in milliseconds (sub-second capable) | `#[once(ttl_millis = 500)] fn pubkey() -> Key` |
| Optional global — skip caching if `None` (implicit) | `#[once] fn feature_flag() -> Option<Flag>` |
| Recompute when an expression is true | `#[once(force_refresh = { flag })] fn config(flag: bool) -> Config` |
| Cache a method inside an `impl` block (one value shared across all instances) | `#[once(in_impl = true)] fn config(&self) -> Config` |
| **`#[concurrent_cached]`** | |
| Thread-safe sharded memoize (no global lock per call) | `#[concurrent_cached] fn compute(x: u64) -> u64` |
| Sharded with LRU | `#[concurrent_cached(max_size = 1_000)] fn lookup(id: u64) -> Row` |
| Sharded with TTL | `#[concurrent_cached(ttl_secs = 60)] fn fetch(url: String) -> Body` |
| Sharded LRU + TTL with custom shard count | `#[concurrent_cached(max_size = 1_000, ttl_secs = 60, shards = 32)] fn query(id: u64) -> Row` |
| TTL in milliseconds (sub-second; Redis honors millisecond TTL via PSETEX/PEXPIRE) | `#[concurrent_cached(ttl_millis = 500)] fn poll(id: u64) -> Status` |
| Per-value expiry, thread-safe | `#[concurrent_cached(expires = true)] fn session(id: u32) -> Token` |
| Per-value expiry with LRU bound | `#[concurrent_cached(expires = true, max_size = 1_000)] fn session(id: u32) -> Token` |
| Cache only successful results (implicit for `Result<T, E>`) | `#[concurrent_cached] fn load(id: u64) -> Result<Row, DbError>` |
| Don't cache `None` returns (implicit for `Option<T>`) | `#[concurrent_cached] fn find(id: u64) -> Option<Row>` |
| Serve stale value when function returns `Err` | `#[concurrent_cached(result_fallback = true, ttl_secs = 60)] fn fetch(id: u64) -> Result<Data, E>` |
| Recompute when an expression over the args is true | `#[concurrent_cached(force_refresh = { id == 0 })] fn fetch(id: u64) -> Data` |
| Force-refresh via a dedicated flag (exclude it from the key) | `#[concurrent_cached(key = "u64", convert = { id }, force_refresh = { refresh })] fn fetch(id: u64, refresh: bool) -> Data { let _ = refresh; … }` — the generated guard reads `refresh` to decide whether to bypass the cache; the body still receives it as a normal parameter, so add `let _ = refresh;` (or `#[allow(unused_variables)]`) if your body does not otherwise use it |
| Cache a method inside an `impl` block (one cache shared across all instances) | `#[concurrent_cached(in_impl = true)] fn load(&self, id: u64) -> Data` |
| Persist results to disk (with `map_error`; or omit when `E: From<RedbCacheError>`) | `#[concurrent_cached(disk = true, map_error = \|e\| MyErr(e))] fn crunch(n: u64) -> Result<Data, MyErr>` |
| Redis-backed async cache (shorthand; uses the default connection/builder) | `#[concurrent_cached(redis = true, ttl_secs = 30, map_error = \|e\| MyErr(e))] async fn api(id: u64) -> Result<Resp, MyErr>` |
| Redis-backed async cache (quoted or unquoted `create`/`map_error`) | `#[concurrent_cached(ty = "AsyncRedisCache<u64, String>", create = { ... }, map_error = \|e\| MyErr(e))] async fn api(id: u64) -> Result<Resp, MyErr>` |

On `#[cached]` and `#[concurrent_cached]`, the LRU bound is set with `max_size = N` (mirroring the `max_size` builder/constructor methods on the stores). The `size = N` spelling — a deprecated alias in 2.x — has been removed; only `max_size = N` is accepted.

The `ttl` attribute accepts a Duration expression as a quoted string: `ttl = "Duration::from_secs(60)"`. The expression is inlined verbatim, so `Duration` must be in scope at the call site (e.g. `use cached::time::Duration;`); the `ttl_secs` / `ttl_millis` forms need no import. For whole seconds, the shorter `ttl_secs = N` form is preferred. `ttl_millis = N` sets a TTL in milliseconds. The three attributes `ttl`, `ttl_secs`, and `ttl_millis` are mutually exclusive; using more than one is a compile error. All three are mutually exclusive with `expires`. Sub-second precision for `ttl_millis` is honored by the in-memory, disk (redb), and Redis stores; Redis applies the TTL with millisecond precision via PSETEX/PEXPIRE.

For the default in-memory sharded stores, `#[concurrent_cached]` accepts any return type — plain values, `Option<T>`, or `Result<T, E>`.
Plain values are always cached as-is. `Option<T>` returns skip caching `None` by default; use `cache_none = true` to also cache `None` values. `Result<T, E>` only caches `Ok` values; `Err` is returned without being stored. Use `cache_err = true` to also cache `Err` values.
The macro detects `Result<T, E>` by matching the exact identifier `Result` (including fully-qualified paths such as `std::result::Result<T, E>`). Type aliases are not resolved at macro-expansion time, so any alias — even one whose name ends with `Result` (e.g. `type MyResult<T> = Result<T, E>`) — is treated as a plain value and its `Err` variant is cached. Use `Result<T, E>` directly when you need Ok-only caching behavior.
The same applies to `Option<T>` detection: a type alias such as `type MaybeRow<T> = Option<T>` is treated as a plain value and its `None` variant is cached. Use `Option<T>` directly when you need `None`-skipping behavior.
On the default in-memory path, do not specify `map_error` -- the sharded stores are infallible and supplying it is a compile error.
For `disk` and `redis` stores, `Result<T, E>` is required. `map_error` is optional: when supplied it converts the store error into your `E`; when omitted the generated code uses `.map_err(Into::into)?`, so `E` must implement `From<RedbCacheError>` (disk) or `From<RedisCacheError>` (Redis). Both quoted-string and unquoted forms are accepted: `map_error = |e| MyErr(e)` and `map_error = "|e| MyErr(e)"` are equivalent.

**Store comparison**

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

`ShardedUnboundCache` and its variants are partitioned across power-of-two shards, each protected by a `parking_lot::RwLock`. The default shard count is derived in two steps:

- The host default is `available_parallelism() × 4`, clamped to 8–1024 and rounded up to a power of two. It is sampled once per process and reused by every cache built afterward.
- On the default path, the three LRU-bounded sharded stores (`ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache`) scale that down to match a total `max_size`: the count is `next_power_of_two(max_size / 16)`, clamped into `[1, host_default]`. This keeps each shard holding roughly 16 entries instead of preallocating an oversized shard array for a small cache — e.g. `ShardedLruCache::new(100)` builds 8 shards (`100 / 16 = 6` → `8`) with a total capacity of 128 (the 16-per-shard floor below), rather than one shard per host default.

Everything else keeps the plain host default: the unbounded stores (`ShardedUnboundCache`, and `ShardedTtlCache` / `ShardedExpiringCache` built without a `max_size`), the builder's `per_shard_max_size` path, and any explicit `shards = N` / `.shards(n)`. An explicit shard count is rounded up to a power of two but never clamped.

Shard structs are padded to 128-byte alignment (covering Intel adjacent-line prefetch and Apple Silicon 128-byte L1 lines) to eliminate false sharing; on a 64-shard deployment this amounts to ~8 KB of padding overhead per cache array. The outer type is an `Arc` — cloning is a reference share, not a deep copy (use `deep_clone()` for an independent copy; note that `deep_clone()` is an inherent method on each concrete sharded type, not part of any trait). They implement `ConcurrentCached`/`ConcurrentCachedAsync` and are the default store selected by `#[concurrent_cached]`.
For sharded LRU variants, eviction is enforced independently per shard. `max_size = N` is divided across shards with ceiling division. Use the builder's `per_shard_max_size` method for an exact per-shard cap (builder-only; `#[concurrent_cached]` does not expose a `per_shard_max_size` attribute — use `shards` to control parallelism and `max_size` for total capacity). **Capacity Fragmentation Warning**: To protect against premature evictions due to hash collisions in extremely small caches (where a shard capacity could drop to 1-2 entries), when sharding is active (`shards > 1`) we enforce a minimum capacity of `16` entries **per shard** (e.g., minimum total capacity of `128` on a single-core machine with 8 shards, or `256` on a 4-core machine with 16 shards). If you require smaller, strict limits under low capacities, configure `shards = 1` or specify `per_shard_max_size` directly (builder-only; not available via `#[concurrent_cached]`).
Because LRU caches require updating access recency, `ShardedLruCache`, `ShardedLruTtlCache`, and `ShardedExpiringLruCache` must acquire an exclusive **write lock** on accessed shards during read hits, which can lead to contention under highly concurrent read-heavy workloads. Unbounded `ShardedUnboundCache`, time-only `ShardedTtlCache` (when `refresh_on_hit` is disabled -- enabling it promotes read hits to exclusive write locks), and expiring `ShardedExpiringCache` require only a **shared read lock** on read hits, avoiding this contention. To mitigate contention on LRU variants, consider increasing the number of `shards` to distribute writes. Note: this write-lock-on-read behavior is a known limitation of the strict-LRU sharded stores. A future read-optimized variant that relaxes strict recency ordering will ship as a separate store type; the existing stores will not change semantics.

> **Custom shard hashers:** Every sharded store carries a third, defaulted type parameter for its [`ShardHasher`] — `ShardedUnboundCache<K, V, H = DefaultShardHasher>`, `ShardedLruCache<K, V, H = DefaultShardHasher>`, and so on — mirroring `std::collections::HashMap<K, V, S = RandomState>`. Writing `ShardedLruCache<K, V>` therefore gets the default hasher, which is what most users want; name the third parameter only when routing keys through a custom `ShardHasher`. Construct such a cache through the builder's `hasher` method: `ShardedLruCache::builder().hasher(my_hasher)` switches the builder's hasher type and `build` yields a `ShardedLruCache<K, V, H>` over `my_hasher`. `new`/`builder` are defined only on the default-hasher instantiation, so a custom hasher is always introduced through `hasher`, never a `ShardedLruCache::<_, _, H>` turbofish (which would otherwise silently drop the hasher).
>
> A hand-written `ShardHasher` that does not also implement `BuildHasher` keeps the six inherent `get`/`remove`/`remove_entry`/`delete`/`contains`/`peek` lookups at every key type it implements: each one is bounded on `H: ShardHasher<Q>` for the `Q` being looked up, so a single `impl ShardHasher<K> for MyRouter` covers the owned-key calls (`cache.get(&key)`). Borrowed forms are opt-in — writing a second `impl ShardHasher<str>` alongside `ShardHasher<String>` is what enables `cache.get("a")` on a `ShardedLruCache<String, V, MyRouter>`. Multiple impls on one router must agree on keys that compare equal (for `K: Borrow<Q>`, `shard_hash(&k)` must equal `shard_hash(k.borrow())`); the compiler cannot check that, and disagreement routes an owned insert and its equivalent borrowed lookup to different shards, producing a miss on an entry that is present. A `BuildHasher` that is also `Clone + Send + Sync + 'static` reaches every key type through the blanket `ShardHasher` impl, where `Borrow`'s own hash-agreement contract makes this automatic. How a missing impl is reported depends on how many the router carries: with exactly one `ShardHasher` impl, inference collapses `Q` onto it and the failure is a call-site `E0308` type mismatch (`expected &UserId, found &u64`) rather than a missing-bound error, while a router with two or more impls gives `Q` nothing to collapse onto and fails as `E0277` with `ShardHasher`'s diagnostic notes (`NameRouter cannot route keys of type str to a shard`). The [`ShardHasher`] docs spell both out. The trait forms remain available for owned keys on any hasher: `ConcurrentCachedExt::get(&cache, &key).unwrap()` (and the matching `remove`/`remove_entry`/`delete`/`contains`), plus `ConcurrentCachePeek::peek(&cache, &key).unwrap()` for `peek` (`ConcurrentCachedExt` has no `peek` of its own). Both trait forms return `Result<_, Infallible>`, hence the `.unwrap()`.
>
> Naming that third type parameter in your own generic helpers has a sharp edge, but it is not the hasher bound: a helper written as `fn lookup<K, V, H: ShardHasher<K>>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V> { c.get(k) }` still fails to compile **at its own definition**, regardless of which hasher any call site uses, because the inherent `get` lives in an `impl` block that also requires `K: Hash + Eq + Clone` and `V: Clone`. rustc surfaces that as E0599 first, reported as "the method `get` exists for reference `&ShardedLruCache<K, V, H>`, but its trait bounds were not satisfied" and followed by the four bounds it is missing (`K: Hash`, `K: Eq`, `K: Clone`, `V: Clone`), so read the error as a bounds list rather than a missing method; `tests/ui/sharded_helper_missing_key_bounds.stderr` pins the exact text. The working signature is `fn lookup<K, V, H>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V> where K: Hash + Eq + Clone, V: Clone, H: ShardHasher<K> { c.get(k) }`: `H: ShardHasher<K>` is exactly the bound the owned-key `get` carries, and no additional marker trait is involved. A helper that looks keys up in a *borrowed* form names that form **in addition to** the store's key type, not instead of it: `fn lookup_borrowed<V, H>(c: &ShardedLruCache<String, V, H>, k: &str) -> Option<V> where V: Clone, H: ShardHasher<String> + ShardHasher<str> { c.get(k) }`, or `H: ShardHasher<K> + ShardHasher<Q>` with the borrowed form left generic (`K: Hash + Eq + Clone + Borrow<Q>`, `Q: Hash + Eq + ?Sized`). That `impl` block is itself bounded on `H: ShardHasher<K>`, so a borrowed call adds `ShardHasher<Q>` on top of the store's own hasher bound rather than replacing it. Leaving the borrowed half out is the confusing failure: with only `H: ShardHasher<String>`, `c.get("a")` fails as an E0308 argument mismatch (`expected &String, found &str`) rather than a missing-impl error, because `Q` collapses to the key type before any bound is checked.
>
> The same inherent-method resolution also breaks argument inference for callers that used to pass a double-reference. Before this crate's borrowed-key routing existed, `get` took `&K`, so `for k in &keys { cache.get(&k) }` on a `ShardedLruCache<String, _>` compiled with `k: &String` through plain deref coercion of `&&String` to `&String`. `get` is now generic over the looked-up form (`&Q` with `K: Borrow<Q>`), and inference fills `Q` in from the argument type before any coercion can apply, so the same call infers `Q = &String` and fails with "the trait bound `String: Borrow<&String>` is not satisfied": a plain `E0277` that gives no "remove the extra `&`" hint, because the fix is not a missing bound but an extra reference at the call site. Dropping that `&` fixes this shape (`cache.get(k)` instead of `cache.get(&k)`). Any other indirection the old code let deref coercion paper over fails the same way, but with no extra `&` to remove, so the deref has to be written out: with `k: &Box<String>` (or `&Arc<String>`), `cache.get(k)` infers `Q = Box<String>` and fails on `String: Borrow<Box<String>>`, and the call becomes `cache.get(&**k)` (or `cache.get(k.as_str())`). In every case the goal is the same, that `Q` infers as a form the stored key actually borrows to rather than a wrapper around it.

**Behavioral guarantees**

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
  blanket `ShardHasher<Q>` impl for every `Q`, so the default hasher and any such
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
  `.unwrap()` you write on a sharded store does not mean what it means on the trait — see the
  warning below this list.
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
- [`CacheExpiry`] is the per-key expiry read for single-owner stores: `cache_peek_expires_at`
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
  [`Cached`] and [`ConcurrentCacheBase`] (defaulted to `None`, overridden by every bounded store).
- Sharded stores implement `ConcurrentCached`/`ConcurrentCachedAsync` instead of
  `Cached`/`CachedGetOrSetAsync`. Generic code parameterized over `Cached<K, V>` cannot accept sharded
  stores; use a `ConcurrentCached<K, V>` bound or a concrete type instead.
  Sharded stores do not implement the single-owner `CachedIter` or `CachedPeek` traits; code that
  is generic over `CachedIter<K, V>` or uses `.iter()` must use a non-sharded store. They do,
  however, provide a side-effect-free read via the [`ConcurrentCachePeek`] trait (`cache_peek`,
  returning an owned `Option<V>`), with the inherent `peek` shim taking call-site priority on the
  concrete sharded types. `ConcurrentCachePeekAsync` is the async mirror of that trait
  (`async_cache_peek`, with an `async_peek` alias); it carries the identical no-recency,
  no-TTL-refresh, no-metrics, no-lazy-expiry contract and is deliberately not implemented by the
  IO stores.
  The four expiry-capable sharded stores ([`ShardedTtlCache`], [`ShardedLruTtlCache`],
  [`ShardedExpiringCache`], [`ShardedExpiringLruCache`]) implement [`ConcurrentCloneCached`],
  which provides `cache_get_with_expiry_status` for reading stale entries without evicting them, and
  `cache_peek_with_expiry_status` as a side-effect-free counterpart (a read with no hit/miss
  counting, LRU promotion, or TTL renewal). The same four stores implement
  [`ConcurrentCacheExpiry`], the concurrent counterpart of [`CacheExpiry`], whose
  `cache_peek_expires_at` returns `(Option<V>, Option<Instant>)` and whose value-free
  `cache_expires_at` returns `(bool, Option<Instant>)`, both under the same no-side-effect
  contract (advisory instant on the two `Expires`-based stores, a real deadline on the two TTL
  stores). The Redis and redb stores implement neither. `#[concurrent_cached]` selects a supporting
  store under the same configurations as `#[cached]` (`ttl_secs` / `ttl` / `ttl_millis`, with or
  without `max_size`, and `expires = true`). Unlike `set` / `get` / `len` / `contains` / `peek`,
  neither expiry read has an inherent shim on the sharded types, so both need
  `use cached::ConcurrentCacheExpiry;` in scope.

**Sharded stores: inherent methods shadow the trait methods**

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

**Inspection and maintenance APIs**

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
use cached::{CachedExt, TtlSortedCache};
use cached::time::Duration;

let mut c: TtlSortedCache<&str, u32> =
    TtlSortedCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
c.set("default-ttl", 1);
// Per-entry TTL override plus an expiry sweep on the way in.
let displaced = c.set_with("short", 2).ttl(Duration::from_millis(1)).evict().set();
assert_eq!(displaced, None);
assert_eq!(c.len(), 2);
```

**Single-flight refresh claims**

[`claim::ClaimRegistry`] collapses concurrent refreshes of one key onto a single caller:
`claim(key)` hands the first caller a [`claim::Claim`] and every later caller `None` until that
`Claim` is dropped, which happens on normal completion, on a panic, and on cancellation (a
dropped async task) alike, so a claim can never wedge a key the way a hand-released guard can. It
is independent of any store and is not background refresh: the registry spawns nothing and awaits
nothing, so it composes with the stale-while-revalidate recipe (`examples/stale_while_revalidate.rs`,
`examples/refresh_before_expiry.rs`) without taking over where the refresh runs. See the
[`claim` module docs](https://docs.rs/cached/latest/cached/claim/index.html) for the full contract, including why it is reachable through
`cached::claim::` and the prelude rather than the crate root.

```rust
use cached::claim::ClaimRegistry;

let registry: ClaimRegistry<String> = ClaimRegistry::new();
let claim = registry.claim("user:1".to_string()).expect("first caller wins");
assert!(registry.claim("user:1".to_string()).is_none(), "already in flight");
drop(claim);
assert!(registry.claim("user:1".to_string()).is_some(), "released, so claimable again");
```

**Performance**

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

**Per-Value Expiry via the `Expires` Trait**

While standard timed stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`) enforce a single, global Time-To-Live (TTL) duration applied to all entries in the cache, [`ExpiringLruCache`] and [`ExpiringCache`] let each individual value determine its own expiration. This is accomplished by storing values that implement the [`Expires`] trait.

This approach is highly useful when caching payloads like OAuth tokens, HTTP responses with varying `Cache-Control` headers, or database records that contain their own absolute expiration timestamps.

It is also the idiomatic way to give entries a **dynamic, per-entry TTL** — a lifetime computed at call time rather than the single uniform duration that `ttl = N` applies to every entry. Because the value carries its own expiry, each entry can be given a different lifetime derived from a function argument, runtime configuration, or a response header. (`expires = true` is mutually exclusive with `ttl`.) See the [`expires_per_key`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs) example for a runnable demonstration.

When using the `#[cached]` or `#[once]` proc macros, add `expires = true` to opt into per-value expiry automatically. For `#[cached]`, this selects `ExpiringCache` (unbounded) by default or `ExpiringLruCache` when `max_size` is also specified. For `#[once]`, this stores a single value whose expiry is polled on each call.

Implement [`Expires::expires_at`] too if the entry's deadline should be readable. It defaults to
`None`, and a value type that implements only `is_expired` makes the per-key expiry read
([`CacheExpiry`] / [`ConcurrentCacheExpiry`]) return `(Some(v), None)` (or `(true, None)` from the
value-free `cache_expires_at`) for live and expired entries alike, so a remaining-TTL policy built
on that read never fires and never reports an error. `is_expired` stays the liveness authority on
these stores either way.

The macro form below derives each entry's TTL from a function argument — `key`/`convert` keep the TTL out of the cache key so it influences only the entry's lifetime, not which slot it occupies (the [`expires_per_key`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs) example uses the same pattern):

```rust
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

----

The basic usage looks like:

```rust
use cached::macros::cached;

/// Defines a function named `fib` that uses a cache implicitly named `FIB`.
/// By default, the cache will be the function's name in all caps.
/// The following line is equivalent to #[cached(name = "FIB")]
#[cached]
fn fib(n: u64) -> u64 {
    if n == 0 || n == 1 { return n }
    fib(n-1) + fib(n-2)
}
```

----

```rust
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;
use cached::LruCache;

/// Use an explicit cache-type with a custom creation block and custom cache-key generating block
#[cached(
    ty = "LruCache<String, usize>",
    create = LruCache::builder().max_size(100).build().unwrap(),
    convert = { format!("{}{}", a, b) }
)]
fn keyed(a: &str, b: &str) -> usize {
    let size = a.len() + b.len();
    sleep(Duration::new(size as u64, 0));
    size
}
```

----

```rust
use cached::macros::once;

/// Only cache the initial function call.
/// Function will be re-executed after the cache
/// expires (according to `ttl_secs`).
/// When no (or expired) cache, concurrent calls
/// will synchronize (`sync_writes`) so the function
/// is only executed once.
#[once(ttl_secs=10, sync_writes = true)]
fn keyed(a: String) -> Option<usize> {
    if a == "a" {
        Some(a.len())
    } else {
        None
    }
}
```

----

```rust
use cached::macros::cached;

/// Cannot use sync_writes and result_fallback together
#[cached(
    ttl_secs = 1,
    sync_writes = "default",
    result_fallback = true
)]
fn doesnt_compile() -> Result<String, ()> {
    Ok("a".to_string())
}
```
----

`cache_get_or_set_with` returns a shared reference (`&V`); binding it as `&mut V`
no longer compiles. Use [`cache_get_or_set_with_mut`](crate::Cached::cache_get_or_set_with_mut)
when you need a mutable reference.

```rust
// This does NOT compile: cache_get_or_set_with returns &V, not &mut V.
use cached::{Cached, UnboundCache};

let mut cache: UnboundCache<u32, u32> = UnboundCache::builder().build().unwrap();
let _: &mut u32 = cache.cache_get_or_set_with(1, || 2);
```
----

```rust
use cached::macros::concurrent_cached;
use cached::AsyncRedisCache;
use cached::time::Duration;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ExampleError {
    #[error("error with redis cache `{0}`")]
    RedisError(String),
}

/// Cache the results of an async function in redis. Redis keys are laid out as
/// `{namespace}:{prefix}:{key}`, where `namespace` defaults to `cached-redis-store:`
/// and `prefix` is required (here `cached_redis_prefix`). The prefix is what scopes
/// `cache_clear` to this logical cache, so give each cache a distinct prefix.
/// Redis and disk stores require `Result<T, E>`; supply a `map_error` closure
/// to convert store errors into your error type.
#[concurrent_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    ty = "AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::builder("cached_redis_prefix")
            .ttl(Duration::from_secs(1))
            .refresh_on_hit(true)
            .build()
            .await
            .expect("error building example redis cache")
    } "##
)]
async fn async_cached_sleep_secs(secs: u64) -> Result<String, ExampleError> {
    std::thread::sleep(cached::time::Duration::from_secs(secs));
    Ok(secs.to_string())
}
```

----

```rust
use cached::macros::concurrent_cached;
use cached::RedbCache;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ExampleError {
    #[error("error with disk cache `{0}`")]
    DiskError(String),
}

/// Cache the results of a function on disk.
/// Cache files will be stored under the system cache dir
/// unless otherwise specified with `disk_dir` or the `create` argument.
/// Disk stores require `Result<T, E>`; supply a `map_error` closure
/// to convert store errors into your error type.
#[concurrent_cached(
    map_error = r##"|e| ExampleError::DiskError(format!("{:?}", e))"##,
    disk = true
)]
fn cached_sleep_secs(secs: u64) -> Result<String, ExampleError> {
    std::thread::sleep(cached::time::Duration::from_secs(secs));
    Ok(secs.to_string())
}
```

----

```rust
use cached::macros::concurrent_cached;

/// Memoize with the default in-memory sharded store — no `map_error`, `ty`,
/// or `create` needed. Add `max_size` for LRU eviction or `ttl` for time-based
/// expiry (requires the `time_stores` feature).
///
/// `#[concurrent_cached]` does **not** support `sync_writes`.
/// For `Option<T>` returns, `None` is skipped by default (use `cache_none = true` to cache it).
/// For `Result<T, E>` returns, only `Ok` values are cached by default (use `cache_err = true`
/// to also cache `Err`). `result_fallback = true` is supported: on an `Err` return, the last
/// cached `Ok` value for the same key is returned instead. The stale value is held in the
/// primary cache slot and re-cached on `Err`; no secondary store is created. It requires
/// expiring entries, so set either a TTL (`ttl_secs`, `ttl_millis`, or
/// `ttl = "<Duration expr>"`, which re-caches with a fresh TTL window) or `expires = true`
/// (per-value expiry, where the re-cached value stays expired and the next call recomputes).
#[concurrent_cached]
fn slow_double(x: u64) -> u64 {
    std::thread::sleep(cached::time::Duration::from_millis(10));
    x * 2
}

/// LRU capacity of 1 000 entries spread across shards.
#[concurrent_cached(max_size = 1000)]
fn slow_triple(x: u64) -> u64 {
    x * 3
}

/// Only cache successful lookups — `Err` is returned but not stored.
#[concurrent_cached]
fn load_user(id: u64) -> Result<String, std::io::Error> {
    Ok(format!("user_{id}"))
}
```


Functions defined via macros will have their results cached using the
function's arguments as a key, or a `convert` expression specified on the macro.

When a macro-defined function is called, the function's cache is first checked for an already
computed (and still valid) value before evaluating the function body.

Due to the requirements of storing arguments and return values in a global cache:

- Function return types:
  - For in-memory stores (`#[cached]` / `#[once]`), must be owned and implement `Clone`
  - For in-memory `#[concurrent_cached]` (sharded stores — the default), must implement `Clone`.
    Any return type is accepted: plain `T`, `Option<T>`, or `Result<T, E>`. `Option<T>` skips
    caching `None` by default; use `cache_none = true` to also cache `None`. When the
    return type is `Result<T, E>`, only `Ok(v)` is stored — `Err` values are returned but not cached.
    Use `cache_err = true` to also cache `Err` values.
  - For I/O-backed stores used by `#[concurrent_cached]` (Redis and disk), must be `Result<T, E>`
    where `T: Clone + serde::Serialize + serde::DeserializeOwned` (the store serializes it).
    `map_error` is optional: supply it to convert the store's error into `E`, or omit it when
    `E: From<RedisCacheError>` (Redis) or `E: From<RedbCacheError>` (disk).
- Function arguments:
  - For in-memory stores (`#[cached]` / `#[once]`), must either be owned and implement `Hash + Eq + Clone`,
    or a `convert` expression must be specified on the macro to produce a key of a `Hash + Eq + Clone` type.
  - For in-memory `#[concurrent_cached]` (sharded stores), must implement `Hash + Eq + Clone`. The
    macro's default key construction always clones function arguments, so `K: Clone` is required on
    every in-memory path. (When using `convert` to supply an already-owned key, only the store's
    own bounds apply: `K: Hash + Eq` for unbounded/TTL-only variants, `K: Hash + Eq + Clone` for LRU
    variants — except when `result_fallback = true` is also set, which always requires `K: Clone`
    regardless of store variant because the generated code clones the key into the fallback store.)
  - For I/O-backed stores used by `#[concurrent_cached]` (Redis and disk), must either be owned and
    implement `Display + Clone`, or a `convert` expression must be used to produce a key of a
    `Display + Clone` type. `Clone` is needed so removal APIs can return the stored key.
  - Floats (`f32` / `f64`), and any type containing them (e.g. a struct with float fields), do not
    implement `Hash` / `Eq`, so they are the canonical case that requires a `convert` expression to
    produce a hashable key. For example `key = "String", convert = r#"{ format!("{:.6}", x) }"#`, or
    wrap the value with a crate such as `ordered-float`.
- Arguments and return values will be `cloned` in the process of insertion and retrieval. For Redis and
  disk stores, keys are additionally formatted into `String`s and values are de/serialized. When the
  return value is expensive to clone, return `Arc<T>` from the cached function: the cache stores the
  `Arc` and every hit clones only the pointer, not `T`.
- Macro-defined functions should not be used to produce side-effectual results!
- Macro-defined functions live at module scope by default (the macro expands to a static plus
  one or more functions). To cache a method inside an `impl` block, set `in_impl = true`, which
  emits the cache static inside the generated method body instead. A `{fn}_no_cache` sibling
  method is generated at the same visibility, calling the original body directly and bypassing
  the cache. The `_prime_cache` companion is not generated for `in_impl` methods (a
  function-local static cannot be shared between two sibling methods, so priming would silently
  do nothing; calling a non-existent prime function is a clear compile error instead).
- Macro-defined methods may take a `self` receiver only when `in_impl = true`; `self` is excluded
  from the default cache key. Otherwise `self`-receiver methods are rejected with a compile error
  (a `convert` block alone does not make them valid: off the `in_impl` path the cache static is
  emitted at `impl` scope, where a `static` is not a legal item).
  **Footgun:** because `self` is excluded, two instances with different internal state but identical
  arguments share one cache entry, so `a.load(5)` and `b.load(5)` return the same cached value even
  when `a` and `b` differ. The cache is process-global, not per-instance. If a method's result
  depends on `self`'s fields, fold them into the key with a `convert` expression (e.g.
  `convert = r#"{ format!("{}:{}", self.id, id) }"#`), or keep the logic in a free function keyed on
  those fields.
- Macro-defined functions can be generic over type parameters only when a `key` + `convert` is
  supplied to produce a concrete key type. On the default-key path (no `convert`), `#[cached]` /
  `#[concurrent_cached]` reject generic functions, since each monomorphization would need its own
  static cache: write a concrete monomorphic wrapper per type instead. (`#[once]` caches a single
  concrete value and is unaffected.)



License: MIT
