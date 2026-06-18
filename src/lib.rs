/*!
[![Build Status](https://github.com/jaemk/cached/actions/workflows/build.yml/badge.svg)](https://github.com/jaemk/cached/actions/workflows/build.yml)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Caching structures and simplified function memoization

`cached` provides implementations of several caching structures as well as macros
for defining memoized functions.

Memoized functions defined using `#[cached]`/`#[once]` macros are thread-safe with the backing
function-cache wrapped in a mutex/rwlock. `#[concurrent_cached]` functions are thread-safe via the
store's own internal synchronization: sharded stores use per-shard `parking_lot::RwLock`; Redis and
disk stores rely on their respective server/file-system concurrency.
By default, the function-cache is **not** locked for the duration of the function's execution, so initial (on an empty cache)
concurrent calls of long-running functions with the same arguments will each execute fully and each overwrite
the memoized value as they complete. This mirrors the behavior of Python's `functools.lru_cache`. To synchronize the execution and caching
of un-cached arguments, specify `#[cached(sync_writes = true)]` / `#[once(sync_writes = true)]`; for
`#[cached]`, use `sync_writes = "by_key"` to synchronize duplicate keys through bucketed per-key locks
(not supported by `#[once]` or `#[concurrent_cached]`).

- See [`cached::stores` docs](https://docs.rs/cached/latest/cached/stores/index.html) cache stores available.
- See [`macros` docs](https://docs.rs/cached/latest/cached/macros/index.html) for more macro examples.

> **Upgrading from 2.x?** The next major contains breaking changes (the disk cache backend
> changed from sled to redb, async Redis TLS support split into explicit features,
> `ConcurrentCachedAsync` cache methods renamed with an `async_` prefix, MSRV raised to 1.89,
> and more). See the
> [migration guide](https://github.com/jaemk/cached/blob/master/docs/migrations/2.0-to-unreleased.md)
> for a step-by-step walkthrough.
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
`ConcurrentCachedAsync` keeps the `async_cache_*` spelling (`async_cache_get`, `async_cache_set`,
`async_cache_remove`, …). `CachedAsync` uses the `async_`-prefixed `get_or_set_with` family
(`async_get_or_set_with`, `async_try_get_or_set_with`, and their `_mut` variants); it has no
`async_cache_*` methods. Neither trait has a short alias; the `async_` prefix already prevents
collisions with the sync methods.

**Features**

- `default`: Include `proc_macro`, `ahash`, and `time_stores` features
- `proc_macro`: Include proc macros
- `ahash`: Enable the optional `ahash` hasher as default hashing algorithm.
- `async_core`: Include runtime-agnostic async traits used by async cache stores
- `async`: Include support for async functions and async cache stores using Tokio synchronization
- `async_tokio_rt_multi_thread`: Enable `tokio`'s optional `rt-multi-thread` feature.
- `redis_store`: Include Redis cache store
- `redis_smol`: Include async Redis support using `smol` (no TLS); implies `redis_store` and `async`
- `redis_smol_native_tls`: `redis_smol` + TLS via `native-tls` (system TLS library)
- `redis_smol_rustls`: `redis_smol` + TLS via `rustls` (pure-Rust TLS)
- `redis_tokio`: Include async Redis support using `tokio` (no TLS); implies `redis_store` and `async`
- `redis_tokio_native_tls`: `redis_tokio` + TLS via `native-tls` (system TLS library)
- `redis_tokio_rustls`: `redis_tokio` + TLS via `rustls` (pure-Rust TLS)
- `redis_connection_manager`: Enable the optional `connection-manager` feature of `redis`. Any async redis caches created
  will use a connection manager instead of a `MultiplexedConnection`. Implies `async` (Tokio runtime) and `redis_store`,
  but does **not** enable TLS. Add `redis_tokio_native_tls` or `redis_tokio_rustls` alongside if TLS is required.
- `redis_async_cache`: Enable Redis client-side caching over RESP3 for async Redis caches.
  Implies `redis_tokio`, `async`, and `redis_store`, but does **not** enable TLS. Add `redis_tokio_native_tls` or `redis_tokio_rustls` alongside if TLS is required.
- `redis_ahash`: Enable the optional `ahash` feature of `redis`
- `disk_store`: Include disk cache store
- `wasm`: Enable WASM support. Note that this feature is incompatible with `tokio`'s multi-thread
  runtime (`async_tokio_rt_multi_thread`) and all Redis features (`redis_store`, `redis_smol`, `redis_smol_native_tls`, `redis_smol_rustls`, `redis_tokio`, `redis_tokio_native_tls`, `redis_tokio_rustls`, `redis_connection_manager`, `redis_async_cache`, `redis_ahash`)
- `time_stores`: Include time-based cache stores ([`TtlCache`](https://docs.rs/cached/latest/cached/struct.TtlCache.html), [`LruTtlCache`](https://docs.rs/cached/latest/cached/struct.LruTtlCache.html), [`TtlSortedCache`](https://docs.rs/cached/latest/cached/struct.TtlSortedCache.html), [`ShardedTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedTtlCache.html), and [`ShardedLruTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedLruTtlCache.html)).
  Also required when using `#[cached(ttl_secs = ...)]`, `#[cached(ttl = ...)]`, `#[cached(ttl_millis = ...)]`, `#[concurrent_cached(ttl_secs = ...)]`, `#[concurrent_cached(ttl = ...)]`, `#[concurrent_cached(ttl_millis = ...)]`, `#[once(ttl_secs = ...)]`, `#[once(ttl = ...)]`, or `#[once(ttl_millis = ...)]` on the default in-memory path.
  Disable this feature when targeting environments without system time support (e.g. `wasm32-unknown-unknown` without WASI or JS).

The procedural macros (`#[cached]`, `#[once]`, `#[concurrent_cached]`) offer a number of features, including async support.
See the [`macros`](https://docs.rs/cached/latest/cached/macros/index.html) module for more samples, and the
[`examples`](https://github.com/jaemk/cached/tree/master/examples) directory for runnable snippets.
Project automation targets are documented by `make help`, and `make check/help` verifies that the
help output stays in sync with supported Makefile targets.

Any custom cache that implements `cached::Cached`/`cached::CachedAsync` can be used with the `#[cached]`/`#[once]` macros in place of the built-ins.
Any custom cache that implements `cached::ConcurrentCached`/`cached::ConcurrentCachedAsync` can be used with the `#[concurrent_cached]` macro.

**Macro quick reference**

| Use case | Annotated signature |
|---|---|
| **`#[cached]`** | |
| Unbounded memoize (default) | `#[cached] fn fib(n: u64) -> u64` |
| LRU-bounded — evict past N entries | `#[cached(max_size = 1_000)] fn lookup(id: u32) -> Row` |
| TTL — expire results after N whole seconds | `#[cached(ttl_secs = 60)] fn config() -> Config` |
| TTL as a Duration expression (inlined verbatim, so `Duration` must be in scope; see note below) | `#[cached(ttl = "Duration::from_secs(60)")] fn config() -> Config` |
| TTL in milliseconds (sub-second capable; Redis rounds up to whole seconds) | `#[cached(ttl_millis = 500)] fn poll(id: u64) -> Status` |
| LRU + TTL | `#[cached(max_size = 500, ttl_secs = 300)] fn search(q: String) -> Vec<Hit>` |
| Don't cache `None` returns (implicit for `Option<T>`) | `#[cached] fn find(id: u64) -> Option<User>` |
| Don't cache `Err` returns (implicit for `Result<T, E>`) | `#[cached] fn load(id: u64) -> Result<Data, E>` |
| Force-cache `None` returns | `#[cached(cache_none = true)] fn find(id: u64) -> Option<User>` |
| Force-cache `Err` returns | `#[cached(cache_err = true)] fn load(id: u64) -> Result<Data, E>` |
| Serve stale value when function returns `Err` | `#[cached(result_fallback = true, ttl_secs = 60)] fn fetch(id: u64) -> Result<Data, E>` |
| Per-value / dynamic per-entry TTL (value carries its own expiry) | `#[cached(expires = true)] fn token(scope: String) -> Token` |
| Deduplicate concurrent first calls for same key | `#[cached(ttl_secs = 30, sync_writes = "by_key")] fn expensive(id: u64) -> Payload` |
| Recompute when an expression over the args is true | `#[cached(force_refresh = "{ id == 0 }")] fn fetch(id: u64) -> Data` |
| Force-refresh via a dedicated flag (exclude it from the key) | `#[cached(key = "u64", convert = "{ id }", force_refresh = "{ refresh }")] fn fetch(id: u64, refresh: bool) -> Data { let _ = refresh; … }` — the generated guard reads `refresh` to decide whether to bypass the cache; the function body still receives `refresh` as a normal parameter, so if your body does not otherwise use it, add `let _ = refresh;` (or `#[allow(unused_variables)]`) to silence the unused-variable warning |
| Cache a method inside an `impl` block (one cache shared across all instances) | `#[cached(in_impl = true)] fn load(&self, id: u64) -> Data` |
| Async | `#[cached(max_size = 100)] async fn remote(id: u64) -> Data` |
| **`#[once]`** | |
| Compute and cache a global value forever | `#[once] fn app_config() -> Config` |
| Refresh a global value periodically | `#[once(ttl_secs = 300, sync_writes = true)] fn pubkey() -> Key` |
| TTL in milliseconds (sub-second capable) | `#[once(ttl_millis = 500)] fn pubkey() -> Key` |
| Optional global — skip caching if `None` (implicit) | `#[once] fn feature_flag() -> Option<Flag>` |
| Recompute when an expression is true | `#[once(force_refresh = "{ flag }")] fn config(flag: bool) -> Config` |
| Cache a method inside an `impl` block (one value shared across all instances) | `#[once(in_impl = true)] fn config(&self) -> Config` |
| **`#[concurrent_cached]`** | |
| Thread-safe sharded memoize (no global lock per call) | `#[concurrent_cached] fn compute(x: u64) -> u64` |
| Sharded with LRU | `#[concurrent_cached(max_size = 1_000)] fn lookup(id: u64) -> Row` |
| Sharded with TTL | `#[concurrent_cached(ttl_secs = 60)] fn fetch(url: String) -> Body` |
| Sharded LRU + TTL with custom shard count | `#[concurrent_cached(max_size = 1_000, ttl_secs = 60, shards = 32)] fn query(id: u64) -> Row` |
| TTL in milliseconds (sub-second; Redis rounds up to whole seconds) | `#[concurrent_cached(ttl_millis = 500)] fn poll(id: u64) -> Status` |
| Per-value expiry, thread-safe | `#[concurrent_cached(expires = true)] fn session(id: u32) -> Token` |
| Per-value expiry with LRU bound | `#[concurrent_cached(expires = true, max_size = 1_000)] fn session(id: u32) -> Token` |
| Cache only successful results (implicit for `Result<T, E>`) | `#[concurrent_cached] fn load(id: u64) -> Result<Row, DbError>` |
| Don't cache `None` returns (implicit for `Option<T>`) | `#[concurrent_cached] fn find(id: u64) -> Option<Row>` |
| Serve stale value when function returns `Err` | `#[concurrent_cached(result_fallback = true, ttl_secs = 60)] fn fetch(id: u64) -> Result<Data, E>` |
| Recompute when an expression over the args is true | `#[concurrent_cached(force_refresh = "{ id == 0 }")] fn fetch(id: u64) -> Data` |
| Force-refresh via a dedicated flag (exclude it from the key) | `#[concurrent_cached(key = "u64", convert = "{ id }", force_refresh = "{ refresh }")] fn fetch(id: u64, refresh: bool) -> Data { let _ = refresh; … }` — the generated guard reads `refresh` to decide whether to bypass the cache; the body still receives it as a normal parameter, so add `let _ = refresh;` (or `#[allow(unused_variables)]`) if your body does not otherwise use it |
| Cache a method inside an `impl` block (one cache shared across all instances) | `#[concurrent_cached(in_impl = true)] fn load(&self, id: u64) -> Data` |
| Persist results to disk | `#[concurrent_cached(disk = true, map_error = \|e\| MyErr(e))] fn crunch(n: u64) -> Result<Data, MyErr>` |
| Redis-backed async cache | `#[concurrent_cached(ty = "AsyncRedisCache<u64, String>", create = r#"{ ... }"#, map_error = \|e\| MyErr(e))] async fn api(id: u64) -> Result<Resp, MyErr>` |

On `#[cached]` and `#[concurrent_cached]`, the LRU bound is set with `max_size = N` (mirroring the `max_size` builder/constructor methods on the stores). The `size = N` spelling — a deprecated alias in 2.x — has been removed; only `max_size = N` is accepted.

The `ttl` attribute accepts a Duration expression as a quoted string: `ttl = "Duration::from_secs(60)"`. The expression is inlined verbatim, so `Duration` must be in scope at the call site (e.g. `use cached::time::Duration;`); the `ttl_secs` / `ttl_millis` forms need no import. For whole seconds, the shorter `ttl_secs = N` form is preferred. `ttl_millis = N` sets a TTL in milliseconds. The three attributes `ttl`, `ttl_secs`, and `ttl_millis` are mutually exclusive; using more than one is a compile error. All three are mutually exclusive with `expires`. Sub-second precision for `ttl_millis` is honored by the in-memory and disk (redb) stores; Redis applies TTL at whole-second granularity, so `ttl_millis` is rounded up to the next whole second on a Redis-backed store (500ms becomes 1s, 1500ms becomes 2s).

For the default in-memory sharded stores, `#[concurrent_cached]` accepts any return type — plain values, `Option<T>`, or `Result<T, E>`.
Plain values are always cached as-is. `Option<T>` returns skip caching `None` by default; use `cache_none = true` to also cache `None` values. `Result<T, E>` only caches `Ok` values; `Err` is returned without being stored. Use `cache_err = true` to also cache `Err` values.
The macro detects `Result<T, E>` by matching the exact identifier `Result` (including fully-qualified paths such as `std::result::Result<T, E>`). Type aliases are not resolved at macro-expansion time, so any alias — even one whose name ends with `Result` (e.g. `type MyResult<T> = Result<T, E>`) — is treated as a plain value and its `Err` variant is cached. Use `Result<T, E>` directly when you need Ok-only caching behavior.
The same applies to `Option<T>` detection: a type alias such as `type MaybeRow<T> = Option<T>` is treated as a plain value and its `None` variant is cached. Use `Option<T>` directly when you need `None`-skipping behavior.
On the default in-memory path, do **not** specify `map_error` — the sharded stores are infallible and supplying it is a compile error.
For `disk` and `redis` stores, `Result<T, E>` is required and `map_error` must convert the store's error into your `E`.

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
| [`ShardedUnboundCache`](https://docs.rs/cached/latest/cached/type.ShardedUnboundCache.html) | None (unbounded) | No | No | N/A | On explicit remove | Yes (`Arc`) | Yes |
| [`ShardedLruCache`](https://docs.rs/cached/latest/cached/type.ShardedLruCache.html) | LRU | Yes | No | N/A | Yes | Yes (`Arc`) | Yes |
| [`ShardedTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedTtlCache.html) | TTL (insert time) | No | Global | Optional | Yes | Yes (`Arc`) | Yes |
| [`ShardedLruTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedLruTtlCache.html) | LRU + TTL | Yes | Global | Optional | Yes (†) | Yes (`Arc`) | Yes |
| [`ShardedExpiringCache`](https://docs.rs/cached/latest/cached/type.ShardedExpiringCache.html) | Value-defined | No | Per-value | N/A | Yes | Yes (`Arc`) | Yes |
| [`ShardedExpiringLruCache`](https://docs.rs/cached/latest/cached/type.ShardedExpiringLruCache.html) | LRU + value-defined | Yes | Per-value | N/A | Yes | Yes (`Arc`) | Yes |

> "On explicit remove" — `on_evict` fires only on `cache_remove`; there is no capacity eviction or TTL expiry trigger for these stores.
> † `ShardedLruTtlCacheBuilder::on_evict` requires `K: 'static + V: 'static`; see the builder docs for details.

`TtlCache`/`LruTtlCache`/`TtlSortedCache`/`ShardedTtlCache`/`ShardedLruTtlCache` require the `time_stores` feature.

`ShardedUnboundCache` and its variants are partitioned across power-of-two shards (default: `available_parallelism() × 4`, clamped to 8–1024; the 8–1024 clamp applies only to this computed default — an explicit `shards = N` is rounded up to a power of two but never clamped) each protected by a `parking_lot::RwLock`. Shard structs are padded to 128-byte alignment (covering Intel adjacent-line prefetch and Apple Silicon 128-byte L1 lines) to eliminate false sharing; on a 64-shard deployment this amounts to ~8 KB of padding overhead per cache array. The outer type is an `Arc` — cloning is a reference share, not a deep copy (use `deep_clone()` for an independent copy; note that `deep_clone()` is an inherent method on each concrete sharded type, not part of any trait). They implement `ConcurrentCached`/`ConcurrentCachedAsync` and are the default store selected by `#[concurrent_cached]`.
For sharded LRU variants, eviction is enforced independently per shard. `max_size = N` is divided across shards with ceiling division. Use the builder's `per_shard_max_size` method for an exact per-shard cap (builder-only; `#[concurrent_cached]` does not expose a `per_shard_max_size` attribute — use `shards` to control parallelism and `max_size` for total capacity). **Capacity Fragmentation Warning**: To protect against premature evictions due to hash collisions in extremely small caches (where a shard capacity could drop to 1-2 entries), when sharding is active (`shards > 1`) we enforce a minimum capacity of `16` entries **per shard** (e.g., minimum total capacity of `128` on a single-core machine with 8 shards, or `256` on a 4-core machine with 16 shards). If you require smaller, strict limits under low capacities, configure `shards = 1` or specify `per_shard_max_size` directly (builder-only; not available via `#[concurrent_cached]`).
Because LRU caches require updating access recency, `ShardedLruCache`, `ShardedLruTtlCache`, and `ShardedExpiringLruCache` must acquire an exclusive **write lock** on accessed shards during read hits, which can lead to contention under highly concurrent read-heavy workloads. Unbounded `ShardedUnboundCache`, time-only `ShardedTtlCache` (when `refresh_on_hit` is disabled — enabling it promotes read hits to exclusive write locks), and expiring `ShardedExpiringCache` require only a **shared read lock** on read hits, avoiding this contention. To mitigate contention on LRU variants, consider increasing the number of `shards` to distribute writes.

> **`*Base` types:** Each sharded store has a corresponding `*Base` generic (`ShardedUnboundCacheBase<K, V, H>`, `ShardedLruCacheBase<K, V, H>`, etc.) parameterized on a custom [`ShardHasher`]. The named aliases (`ShardedUnboundCache`, `ShardedLruCache`, …) use the default hasher and are what most users should reach for. Use the `*Base` types only when implementing a custom `ShardHasher` for non-standard shard routing. Construct a custom-hasher cache through the alias builder and its `hasher` method: `ShardedLruCache::builder().hasher(my_hasher)` switches the builder's hasher type and `build` yields a `*Base<K, V, H>` over `my_hasher`. `new`/`builder` are defined only on the default-hasher alias, so a custom hasher is always introduced through `hasher`, never a `*Base::<_, _, H>` turbofish (which would otherwise silently drop the hasher).

**Behavioral guarantees**

- Non-sharded in-memory stores (`UnboundCache`, `LruCache`, `TtlCache`, etc.) are not internally
  synchronized. Macro-generated `#[cached]`/`#[once]` functions wrap them in locks; users
  managing these stores directly must add their own synchronization when sharing across threads.
  `Sharded*` stores are internally synchronized (per-shard `parking_lot::RwLock`) and implement
  `ConcurrentCached`/`ConcurrentCachedAsync` — no external lock is needed.
  The synchronous `get` / `set` / `remove` (and their `cache_get` / `cache_set` / `cache_remove`
  aliases) come from the `ConcurrentCached` trait (it must be in scope — `use cached::ConcurrentCached;` or
  `use cached::prelude::*;`), not from inherent methods. The async trait operations are
  `async_`-prefixed, so they never collide (e.g., `STORE.async_cache_get(&key).await.expect("ShardedUnboundCache is infallible")`).
- `Cached::get` (and its `cache_get` alias) requires mutable access because some
  stores update recency, expiration timestamps, or metrics during reads.
- Expired values can remain allocated until a mutating operation, `evict`, or
  store-specific cleanup removes them. Methods such as `len` may include expired values
  unless a store documents otherwise.
- `cache_remove` fires the `on_evict` callback (if set) and counts as an eviction for
  every successful removal, across all stores that track evictions. `ShardedUnboundCache` is the
  exception: it has no evictions counter and always returns `None` from
  `metrics().evictions`, though its `on_evict` callback still fires. The `on_evict` column
  above marks the unbounded stores where explicit removal is the *only* eviction trigger. For stores with
  expiry, removing a present-but-already-expired entry still evicts and fires `on_evict`,
  but `cache_remove` returns `None`; use `cache_delete` or `cache_remove_entry` when you
  need to know whether an entry was physically removed.
- `cache_clear()` is fast and side-effect-free: it does **not** fire `on_evict` and does
  not increment the evictions counter. Use `cache_clear_with_on_evict()` when you need the
  callback to fire for every removed entry (e.g., to release resources tracked via `on_evict`).
  Note: neither `clear()` nor `cache_clear_with_on_evict()` is part of `ConcurrentCached`
  or its async counterpart — `clear()` is exposed as an inherent method on each concrete
  sharded store type, and `cache_clear_with_on_evict()` is inherent-only as well; generic code
  parameterized over `ConcurrentCached` cannot call either.
- Bounded caches enforce capacity on insertion. Time-bounded caches enforce freshness on lookup.
- Redis and disk stores serialize values and return owned values. Non-sharded in-memory stores
  return references from direct store APIs; sharded stores return owned `Option<V>` values
  (cloned under a shard lock). Macro-generated functions clone cached return values in all cases.
- Macro-generated `#[cached]` / `#[once]` cache statics use `RwLock` by default. Named cache
  statics for those macros should be inspected with `.read()` or `.write()` unless
  `sync_lock = "mutex"` is set. Named `#[concurrent_cached]` statics hold a self-synchronizing
  store directly: sync functions use `LazyLock<Store>`, and async functions use
  `OnceCell<Store>`.
- `CachedPeek` provides non-mutating lookups that do not update recency, refresh TTLs, or record
  metrics. `CachedRead` is narrower and is only implemented where shared-lock lookups can preserve
  normal read-side semantics without recency or refresh mutation.
- Sharded stores implement `ConcurrentCached`/`ConcurrentCachedAsync` instead of
  `Cached`/`CachedAsync`. Generic code parameterized over `Cached<K, V>` cannot accept sharded
  stores; use a `ConcurrentCached<K, V>` bound or a concrete type instead.
  Sharded stores also do not implement `CachedIter` or `CachedPeek`. Code that is generic over
  `CachedIter<K, V>` or uses `.iter()` / `cache_peek` must use non-sharded stores instead.
  The four expiry-capable sharded stores ([`ShardedTtlCache`], [`ShardedLruTtlCache`],
  [`ShardedExpiringCache`], [`ShardedExpiringLruCache`]) implement [`ConcurrentCloneCached`],
  which provides `cache_get_with_expiry_status` for reading stale entries without evicting them, and
  `cache_peek_with_expiry_status` as a side-effect-free counterpart (the built-in sharded stores
  override the default, which delegates to the renewing read).

**Per-Value Expiry via the `Expires` Trait**

While standard timed stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`) enforce a single, global Time-To-Live (TTL) duration applied to all entries in the cache, [`ExpiringLruCache`] and [`ExpiringCache`] let each individual value determine its own expiration. This is accomplished by storing values that implement the [`Expires`] trait.

This approach is highly useful when caching payloads like OAuth tokens, HTTP responses with varying `Cache-Control` headers, or database records that contain their own absolute expiration timestamps.

It is also the idiomatic way to give entries a **dynamic, per-entry TTL** — a lifetime computed at call time rather than the single uniform duration that `ttl = N` applies to every entry. Because the value carries its own expiry, each entry can be given a different lifetime derived from a function argument, runtime configuration, or a response header. (`expires = true` is mutually exclusive with `ttl`.) See the [`expires_per_key`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs) example for a runnable demonstration.

When using the `#[cached]` or `#[once]` proc macros, add `expires = true` to opt into per-value expiry automatically. For `#[cached]`, this selects `ExpiringCache` (unbounded) by default or `ExpiringLruCache` when `max_size` is also specified. For `#[once]`, this stores a single value whose expiry is polled on each call.

The macro form below derives each entry's TTL from a function argument — `key`/`convert` keep the TTL out of the cache key so it influences only the entry's lifetime, not which slot it occupies (`ignore`d as a doctest because it requires the default `proc_macro` feature; the same code runs in the [`expires_per_key`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs) example):

```rust,ignore
use cached::macros::cached;
use cached::Expires;
use cached::time::{Duration, Instant};

#[derive(Clone)]
struct Token { value: String, expires_at: Instant }

impl Expires for Token {
    fn is_expired(&self) -> bool { Instant::now() >= self.expires_at }
}

// `ttl_secs` is a runtime argument — each user's token expires on its own schedule.
#[cached(expires = true, key = "u64", convert = "{ user_id }")]
fn fetch_token(user_id: u64, ttl_secs: u64) -> Token {
    Token {
        value: format!("token-{user_id}"),
        expires_at: Instant::now() + Duration::from_secs(ttl_secs),
    }
}
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
use cached::{Cached, Expires, ExpiringCache, ExpiringLruCache};
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

```rust,no_run,ignore
use cached::macros::cached;

/// Defines a function named `fib` that uses a cache implicitly named `FIB`.
/// By default, the cache will be the function's name in all caps.
/// The following line is equivalent to #[cached(name = "FIB", unbound)]
#[cached]
fn fib(n: u64) -> u64 {
    if n == 0 || n == 1 { return n }
    fib(n-1) + fib(n-2)
}
# pub fn main() { }
```

----

```rust,no_run,ignore
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;
use cached::LruCache;

/// Use an explicit cache-type with a custom creation block and custom cache-key generating block
#[cached(
    ty = "LruCache<String, usize>",
    create = "{ LruCache::builder().max_size(100).build().unwrap() }",
    convert = r#"{ format!("{}{}", a, b) }"#
)]
fn keyed(a: &str, b: &str) -> usize {
    let size = a.len() + b.len();
    sleep(Duration::new(size as u64, 0));
    size
}
# pub fn main() { }
```

----

```rust,no_run,ignore
use cached::macros::once;

/// Only cache the initial function call.
/// Function will be re-executed after the cache
/// expires (according to `ttl_secs`).
/// When no (or expired) cache, concurrent calls
/// will synchronize (`sync_writes`) so the function
/// is only executed once.
# #[cfg(feature = "time_stores")]
#[once(ttl_secs=10, sync_writes = true)]
fn keyed(a: String) -> Option<usize> {
    if a == "a" {
        Some(a.len())
    } else {
        None
    }
}
# pub fn main() { }
```

----

```compile_fail
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

```compile_fail
use cached::{Cached, UnboundCache};

let mut cache: UnboundCache<u32, u32> = UnboundCache::builder().build().unwrap();
let _: &mut u32 = cache.cache_get_or_set_with(1, || 2);
```
----

```rust,no_run,ignore
use cached::macros::concurrent_cached;
use cached::AsyncRedisCache;
use cached::time::Duration;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ExampleError {
    #[error("error with redis cache `{0}`")]
    RedisError(String),
}

/// Cache the results of an async function in redis. Cache
/// keys will be prefixed with `cache_redis_prefix`.
/// Redis and disk stores require `Result<T, E>`; supply a `map_error` closure
/// to convert store errors into your error type.
#[concurrent_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    ty = "AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::builder("cached_redis_prefix", Duration::from_secs(1))
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

```rust,no_run,ignore
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

```rust,no_run,ignore
use cached::macros::concurrent_cached;

/// Memoize with the default in-memory sharded store — no `map_error`, `ty`,
/// or `create` needed. Add `max_size` for LRU eviction or `ttl` for time-based
/// expiry (requires the `time_stores` feature).
///
/// `#[concurrent_cached]` does **not** support `sync_writes`.
/// For `Option<T>` returns, `None` is skipped by default (use `cache_none = true` to cache it).
/// For `Result<T, E>` returns, only `Ok` values are cached by default (use `cache_err = true`
/// to also cache `Err`). `result_fallback = true` is supported (requires `ttl_secs`, `ttl_millis`, or `ttl = "<Duration expr>"`): on an `Err`
/// return, the last cached `Ok` value for the same key is returned instead. The stale value
/// is held in the primary cache slot and re-cached with a fresh TTL window on `Err`; no
/// secondary store is created.
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
    `map_error` must be supplied to convert the store's error into `E`.
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
  disk stores, keys are additionally formatted into `String`s and values are de/serialized.
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


*/

#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(clippy::manual_async_fn)]

use crate::time::Duration;
#[cfg(feature = "proc_macro")]
#[cfg_attr(docsrs, doc(cfg(feature = "proc_macro")))]
pub use macros::{Return, cached, concurrent_cached, once};
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
use std::future::Future;
#[cfg(any(
    feature = "redis_smol",
    feature = "redis_smol_native_tls",
    feature = "redis_smol_rustls",
    feature = "redis_tokio",
    feature = "redis_tokio_native_tls",
    feature = "redis_tokio_rustls",
    feature = "redis_async_cache",
    feature = "redis_connection_manager"
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
        feature = "redis_async_cache",
        feature = "redis_connection_manager"
    )))
)]
pub use stores::{AsyncRedisCache, AsyncRedisCacheBuilder};
pub use stores::{
    BuildError, CacheEvict, ConcurrentCacheEvict, DefaultShardHasher, Expires, ExpiringCache,
    ExpiringCacheBuilder, ExpiringLruCache, ExpiringLruCacheBuilder, LruCache, LruCacheBuilder,
    SetMaxSizeError, SetTtlError, ShardHasher, ShardedExpiringCache, ShardedExpiringCacheBase,
    ShardedExpiringCacheBuilder, ShardedExpiringLruCache, ShardedExpiringLruCacheBase,
    ShardedExpiringLruCacheBuilder, ShardedLruCache, ShardedLruCacheBase, ShardedLruCacheBuilder,
    ShardedUnboundCache, ShardedUnboundCacheBase, ShardedUnboundCacheBuilder, UnboundCache,
    UnboundCacheBuilder,
};
#[cfg(feature = "disk_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "disk_store")))]
pub use stores::{
    DiskCache, DiskCacheBuildError, DiskCacheBuilder, DiskCacheError, RedbCache,
    RedbCacheBuildError, RedbCacheBuilder, RedbCacheError,
};
#[cfg(feature = "time_stores")]
#[doc(hidden)]
pub use stores::{HasEvict, NoEvict};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use stores::{
    LruTtlCache, LruTtlCacheBuilder, ShardedLruTtlCache, ShardedLruTtlCacheBase,
    ShardedLruTtlCacheBuilder, ShardedTtlCache, ShardedTtlCacheBase, ShardedTtlCacheBuilder,
    TtlCache, TtlCacheBuilder, TtlSortedCache, TtlSortedCacheBuilder, TtlSortedCacheError,
};
#[cfg(feature = "redis_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redis_store")))]
pub use stores::{RedisCache, RedisCacheBuildError, RedisCacheBuilder, RedisCacheError};

mod lru_list;
#[cfg(feature = "proc_macro")]
#[cfg_attr(docsrs, doc(cfg(feature = "proc_macro")))]
pub mod macros;
pub mod stores;
/// Re-export of the [`web_time`](https://docs.rs/web_time) crate,
/// which provides time types compatible with both native and WebAssembly targets.
pub use web_time as time;

#[cfg(feature = "async")]
#[doc(hidden)]
pub mod async_sync {
    pub use tokio::sync::Mutex;
    pub use tokio::sync::OnceCell;
    pub use tokio::sync::RwLock;
}

#[doc(hidden)]
pub mod sync_sync {
    pub use parking_lot::Mutex;
    pub use parking_lot::RwLock;
}

/// Convenience re-exports of the commonly-needed cache traits.
///
/// Glob-import this module to bring the public cache traits into scope in one line —
/// `use cached::prelude::*;` — instead of importing each trait individually. This is
/// especially handy for the `ConcurrentCached` family, whose methods (`cache_get`,
/// `cache_set`, …) are trait methods and require the trait to be in scope to call.
///
/// Only traits are re-exported here; concrete store types are intentionally omitted to
/// avoid name clashes. Import those directly (e.g. `use cached::ShardedUnboundCache;`).
pub mod prelude {
    pub use crate::{
        CacheEvict, Cached, CachedIter, CachedPeek, CachedRead, CloneCached, ConcurrentCacheEvict,
        ConcurrentCached, ConcurrentCloneCached, Expires, SerializeCached,
    };

    #[cfg(feature = "time_stores")]
    #[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
    pub use crate::CacheTtl;

    #[cfg(feature = "async_core")]
    #[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
    pub use crate::{CachedAsync, ConcurrentCachedAsync, SerializeCachedAsync};
}

/// Cache operations
///
/// Every synchronous operation has a short alias (`get`/`set`/`remove`/`clear`/`len`/...) and a
/// `cache_`-prefixed form (`cache_get`/`cache_set`/`cache_remove`/`cache_clear`/`cache_size`/...).
/// The short aliases are the preferred spelling. Use the `cache_`-prefixed names when a short
/// alias would collide with another in-scope trait's method of the same name (for example, your
/// type also implements a trait with its own `get`).
/// `ConcurrentCachedAsync` keeps the `async_cache_*` spelling (`async_cache_get`,
/// `async_cache_set`, `async_cache_remove`, …). `CachedAsync` uses the `async_`-prefixed
/// `get_or_set_with` family (`async_get_or_set_with`, `async_try_get_or_set_with`, and their
/// `_mut` variants); it has no `async_cache_*` methods. Neither trait has a short alias; the
/// `async_` prefix already prevents collisions with the sync methods.
///
/// ```rust
/// use cached::{Cached, UnboundCache};
///
/// let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
///
/// // Preferred short alias:
/// cache.set("key".to_string(), "owned value".to_string());
/// let borrowed_cache_value = cache.get("key");
/// assert_eq!(borrowed_cache_value, Some(&"owned value".to_string()));
///
/// // Full cache_*-prefixed form (use when a short alias would collide):
/// cache.cache_set("key2".to_string(), "another value".to_string());
/// let v2 = cache.cache_get("key2");
/// assert_eq!(v2, Some(&"another value".to_string()));
/// ```
pub trait Cached<K, V> {
    // ── Core required methods (stores implement these) ────────────────────

    /// Attempt to retrieve a cached value.
    ///
    /// Takes `&mut self` because some stores update recency, expiration timestamps,
    /// or metrics during reads.
    ///
    /// ```rust
    /// # use cached::{Cached, UnboundCache};
    /// # let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
    /// # cache.set("key".to_string(), "owned value".to_string());
    /// let v1 = cache.get("key").map(String::clone);
    /// let v2 = cache.get(&"key".to_string()).map(String::clone);
    /// # assert_eq!(v1, v2);
    /// ```
    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Attempt to retrieve a cached value with mutable access.
    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Insert a key-value pair and return the previous value.
    fn cache_set(&mut self, k: K, v: V) -> Option<V>;

    /// Fallible variant of [`Self::cache_set`]. Returns `Err` if the store cannot accept the entry
    /// (e.g. the TTL duration overflows `Instant` bounds). The default implementation is
    /// infallible and delegates to [`Self::cache_set`].
    fn cache_try_set(&mut self, k: K, v: V) -> Result<Option<V>, Box<dyn std::error::Error>> {
        Ok(self.cache_set(k, v))
    }

    /// Get or insert a key-value pair, returning a mutable reference to the value.
    ///
    /// This is the mutable counterpart of [`cache_get_or_set_with`](Cached::cache_get_or_set_with).
    /// Stores implement this method; the shared-reference variant delegates to it.
    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V;

    /// Get or insert a key-value pair, propagating errors from the factory and
    /// returning a mutable reference to the value.
    ///
    /// This is the mutable counterpart of
    /// [`cache_try_get_or_set_with`](Cached::cache_try_get_or_set_with).
    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E>;

    /// Get or insert a key-value pair, returning a shared reference to the value.
    ///
    /// Returns `&V`. Use [`cache_get_or_set_with_mut`](Cached::cache_get_or_set_with_mut)
    /// when you need a mutable reference to the cached value. This is a provided default
    /// (it delegates to `cache_get_or_set_with_mut`); external stores implement `_mut`, not this.
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &V {
        &*self.cache_get_or_set_with_mut(key, f)
    }

    /// Get or insert a key-value pair, propagating errors from the factory and
    /// returning a shared reference to the value.
    ///
    /// Returns `Result<&V, E>`. Use
    /// [`cache_try_get_or_set_with_mut`](Cached::cache_try_get_or_set_with_mut)
    /// when you need a mutable reference to the cached value. This is a provided default
    /// (it delegates to `cache_try_get_or_set_with_mut`); external stores implement `_mut`, not this.
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&V, E> {
        self.cache_try_get_or_set_with_mut(key, f).map(|v| &*v)
    }

    /// Remove a cached value, returning it if it was both present and still live.
    ///
    /// Removing any present entry fires the store's `on_evict` callback (if set) and,
    /// for stores that track evictions, increments the `evictions` metric consistent
    /// with automatic eviction. For stores with expiry, an entry that is present but
    /// already expired is still removed (and still fires `on_evict` / counts as an
    /// eviction when the store tracks evictions), but `None` is returned because the
    /// value is no longer valid.
    ///
    /// Use [`cache_remove_entry`](Cached::cache_remove_entry) when you need to
    /// distinguish "key absent" from "key present but expired", or when you need
    /// the stored key back (relevant when `K`'s `Eq` ignores some fields).
    ///
    /// ```rust
    /// # use cached::{Cached, UnboundCache};
    /// # let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
    /// # cache.set("k1".to_string(), "v1".to_string());
    /// # cache.set("k2".to_string(), "v2".to_string());
    /// let r1 = cache.remove("k1");
    /// let r2 = cache.remove(&"k2".to_string());
    /// # assert_eq!(r1, Some("v1".to_string()));
    /// # assert_eq!(r2, Some("v2".to_string()));
    /// ```
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Remove a cached entry, returning the stored key and value whenever an entry
    /// was physically deleted — including entries that were present but already expired.
    ///
    /// This is the key difference from [`cache_remove`](Cached::cache_remove):
    /// - `cache_remove` returns `None` for both "key absent" and "key present but expired"
    /// - `cache_remove_entry` returns `Some((stored_key, value))` whenever anything was
    ///   physically deleted, and `None` only when the key was not in the store at all
    ///
    /// This lets callers distinguish between the two cases, and also returns the *stored*
    /// key rather than the lookup key — relevant when `K`'s `Eq`/`Hash` ignores some
    /// fields and the stored and lookup instances may differ.
    ///
    /// Removing any present entry fires the store's `on_evict` callback (if set) and,
    /// for stores that track evictions, increments the `evictions` metric.
    ///
    /// ```rust
    /// use cached::{Cached, UnboundCache};
    ///
    /// let mut cache: UnboundCache<String, u32> = UnboundCache::builder().build().unwrap();
    /// cache.set("key".to_string(), 42);
    ///
    /// // cache_remove_entry returns Some even for the key that was just inserted.
    /// // (keeping cache_remove_entry name here as a representative cache_*-prefixed example)
    /// let entry = cache.cache_remove_entry("key");
    /// assert_eq!(entry, Some(("key".to_string(), 42)));
    ///
    /// // Returns None only when the key was never present.
    /// assert_eq!(cache.cache_remove_entry("missing"), None);
    /// ```
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Remove all cached entries but preserve capacity allocation and metrics.
    /// To also reset metrics, call [`cache_reset_metrics`](Cached::cache_reset_metrics) afterward,
    /// or use [`cache_reset`](Cached::cache_reset) to do both at once.
    fn cache_clear(&mut self);

    /// Reset all entries and metrics (hits, misses, evictions) to zero.
    /// Store configuration — capacity, TTL, and `on_evict` callbacks — is preserved.
    /// To reset entries without resetting metrics, use [`cache_clear`](Cached::cache_clear).
    fn cache_reset(&mut self);

    /// Return the number of entries currently in the cache.
    ///
    /// For stores with TTL-based expiry, this count may include entries that have expired
    /// but not yet been evicted (lazy eviction).
    fn cache_size(&self) -> usize;

    // ── Optional overrides ────────────────────────────────────────────────

    /// Reset hit/miss counters.
    fn cache_reset_metrics(&mut self) {}

    /// Return the number of times a cached value was successfully retrieved.
    fn cache_hits(&self) -> Option<u64> {
        None
    }

    /// Return the number of times a cached value was not found.
    fn cache_misses(&self) -> Option<u64> {
        None
    }

    /// Return the cache capacity, if bounded.
    fn cache_capacity(&self) -> Option<usize> {
        None
    }

    // ── Ergonomic aliases (new preferred API) ────────────────────────────

    /// Retrieve a cached value. Delegates to [`cache_get`](Cached::cache_get).
    ///
    /// # Mutability
    ///
    /// Like [`cache_get`](Cached::cache_get), this takes `&mut self` (unlike
    /// [`HashMap::get`](std::collections::HashMap::get)) because some stores update recency
    /// or refresh TTL on read. For a `&self` read where the store supports it, use
    /// [`CachedPeek::cache_peek`] (non-mutating, no recency/TTL/metrics updates) or
    /// [`CachedRead::cache_get_read`] (shared-lock read preserving normal read semantics).
    fn get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_get(k)
    }

    /// Retrieve a cached value with mutable access. Delegates to [`cache_get_mut`](Cached::cache_get_mut).
    fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_get_mut(k)
    }

    /// Insert a key-value pair and return the previous value. Delegates to [`cache_set`](Cached::cache_set).
    fn set(&mut self, k: K, v: V) -> Option<V> {
        self.cache_set(k, v)
    }

    /// Fallible insert. Delegates to [`cache_try_set`](Cached::cache_try_set).
    fn try_set(&mut self, k: K, v: V) -> Result<Option<V>, Box<dyn std::error::Error>> {
        self.cache_try_set(k, v)
    }

    /// Get or insert a key-value pair. Delegates to [`cache_get_or_set_with`](Cached::cache_get_or_set_with).
    fn get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &V {
        self.cache_get_or_set_with(key, f)
    }

    /// Get or insert a key-value pair, returning a mutable reference. Delegates to
    /// [`cache_get_or_set_with_mut`](Cached::cache_get_or_set_with_mut).
    fn get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.cache_get_or_set_with_mut(key, f)
    }

    /// Get or insert a key-value pair with error handling. Delegates to [`cache_try_get_or_set_with`](Cached::cache_try_get_or_set_with).
    fn try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(&mut self, k: K, f: F) -> Result<&V, E> {
        self.cache_try_get_or_set_with(k, f)
    }

    /// Get or insert a key-value pair with error handling, returning a mutable reference.
    /// Delegates to [`cache_try_get_or_set_with_mut`](Cached::cache_try_get_or_set_with_mut).
    fn try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        k: K,
        f: F,
    ) -> Result<&mut V, E> {
        self.cache_try_get_or_set_with_mut(k, f)
    }

    /// Remove a cached value. Delegates to [`cache_remove`](Cached::cache_remove).
    fn remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove(k)
    }

    /// Remove a cached entry, returning the stored key and value. Delegates to
    /// [`cache_remove_entry`](Cached::cache_remove_entry).
    fn remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove_entry(k)
    }

    /// Delete a cached entry without returning it. Returns `true` if an entry was
    /// physically deleted (including expired entries), `false` if the key was absent.
    ///
    /// Unlike [`cache_remove`](Cached::cache_remove), this returns `true` even when the
    /// deleted entry was already expired. Delegates to
    /// [`cache_remove_entry`](Cached::cache_remove_entry).
    ///
    /// ```rust
    /// use cached::{Cached, UnboundCache};
    ///
    /// let mut cache: UnboundCache<String, u32> = UnboundCache::builder().build().unwrap();
    /// cache.set("key".to_string(), 42);
    /// assert!(cache.delete("key"));    // present -- returns true
    /// assert!(!cache.delete("key"));   // already gone -- returns false
    /// ```
    fn cache_delete<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove_entry(k).is_some()
    }

    /// Delete a cached entry without returning it. Returns `true` if an entry was
    /// physically deleted (including expired entries). Delegates to
    /// [`cache_delete`](Cached::cache_delete).
    fn delete<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_delete(k)
    }

    /// Return `true` if the cache contains a value for the given key.
    ///
    /// Requires `&mut self` because some stores update recency, expiration
    /// timestamps, or metrics during reads. For a non-mutating presence check
    /// use [`CachedPeek::cache_peek`] if the store implements it.
    fn contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.get(k).is_some()
    }

    /// Remove all entries, keeping allocated memory for reuse. Delegates to [`cache_clear`](Cached::cache_clear).
    fn clear(&mut self) {
        self.cache_clear()
    }

    /// Return the number of entries currently in the cache. Delegates to [`cache_size`](Cached::cache_size).
    fn len(&self) -> usize {
        self.cache_size()
    }

    /// Return `true` if the cache contains no entries.
    fn is_empty(&self) -> bool {
        self.cache_size() == 0
    }

    /// Return the number of cache hits, if tracked.
    fn hits(&self) -> Option<u64> {
        self.cache_hits()
    }

    /// Return the number of cache misses, if tracked.
    fn misses(&self) -> Option<u64> {
        self.cache_misses()
    }

    /// Return a snapshot of cache metrics.
    fn metrics(&self) -> CacheMetrics {
        CacheMetrics {
            hits: self.cache_hits(),
            misses: self.cache_misses(),
            evictions: self.cache_evictions(),
            entry_count: self.cache_size(),
            capacity: self.cache_capacity(),
        }
    }

    /// Return the number of times a value was evicted from the cache.
    fn cache_evictions(&self) -> Option<u64> {
        None
    }
}

/// Iteration over cache contents for stores that can expose borrowed entries.
///
/// Timed stores may omit expired entries from these iterators without eagerly removing them.
pub trait CachedIter<K, V> {
    /// Return an iterator over the key-value pairs in the cache.
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        Self: Sized,
        K: 'a,
        V: 'a;

    /// Return an iterator over the keys in the cache.
    fn keys<'a>(&'a self) -> impl Iterator<Item = &'a K> + 'a
    where
        Self: Sized,
        K: 'a,
        V: 'a,
    {
        self.iter().map(|(k, _)| k)
    }

    /// Return an iterator over the values in the cache.
    fn values<'a>(&'a self) -> impl Iterator<Item = &'a V> + 'a
    where
        Self: Sized,
        K: 'a,
        V: 'a,
    {
        self.iter().map(|(_, v)| v)
    }
}

/// A snapshot of cache hit/miss and size statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheMetrics {
    /// Number of successful cache lookups, if tracked.
    pub hits: Option<u64>,
    /// Number of failed cache lookups, if tracked.
    pub misses: Option<u64>,
    /// Number of entries evicted from the cache, if tracked.
    pub evictions: Option<u64>,
    /// Current number of entries in the cache.
    pub entry_count: usize,
    /// Maximum capacity, if bounded.
    pub capacity: Option<usize>,
}

impl CacheMetrics {
    /// Return the cache hit ratio as a value in `[0.0, 1.0]`, or `None` if no lookups have occurred.
    pub fn hit_ratio(&self) -> Option<f64> {
        let hits = self.hits?;
        let misses = self.misses?;
        let total = hits + misses;
        if total == 0 {
            None
        } else {
            Some(hits as f64 / total as f64)
        }
    }
}

/// Non-mutating cache lookup for stores that can expose a value by shared reference.
///
/// Peeking does not update recency, refresh TTLs, increment hit/miss metrics, or evict expired
/// values. It is useful for diagnostics and for implementing read APIs in stores whose normal
/// lookup semantics do not require recency or refresh mutation.
pub trait CachedPeek<K, V> {
    /// Attempt to retrieve a cached value without mutating the cache.
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;
}

/// Shared-reference cache lookup for stores that can preserve normal read semantics without an
/// exclusive mutable borrow.
///
/// This trait is intentionally narrower than [`CachedPeek`]. Stores with LRU recency updates,
/// refresh-on-hit TTL behavior, or other read-side mutation should not implement it. Macro-defined
/// functions use this through `#[cached(unsync_reads = true)]` to take a shared read lock for the
/// initial cache-hit path while still taking an exclusive write lock for insertion.
pub trait CachedRead<K, V>: CachedPeek<K, V> {
    /// Attempt to retrieve a cached value through a shared reference.
    fn cache_get_read<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_peek(k)
    }
}

/// Extra cache operations for types that implement `Clone`.
///
/// [`cache_get_with_expiry_status`](CloneCached::cache_get_with_expiry_status)
/// returns `(value, expired)` — note an expired entry is still *returned*
/// (`(Some(v), true)`) so callers can fall back to the stale value if a refresh
/// fails:
///
/// ```rust
/// # #[cfg(feature = "time_stores")]
/// # {
/// use cached::{Cached, CloneCached, TtlCache};
/// use cached::time::Duration;
///
/// let mut c = TtlCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
/// c.set("k".to_string(), 1);
/// assert_eq!(c.get_with_expiry_status(&"k".to_string()), (Some(1), false)); // live
/// assert_eq!(c.get_with_expiry_status(&"x".to_string()), (None, false));    // absent
/// // (a present-but-expired entry would yield `(Some(v), true)`)
/// # }
/// ```
pub trait CloneCached<K, V> {
    /// Look up a cached value and report whether the found entry is expired.
    ///
    /// Returns `(value, expired)` where:
    /// - `(None, false)` — key not present
    /// - `(Some(v), false)` — key present and live
    /// - `(Some(v), true)` — key present but expired (the stale value is returned so callers
    ///   can fall back to it if the refresh fails)
    fn cache_get_with_expiry_status<Q>(&mut self, key: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized;

    /// Ergonomic alias for [`cache_get_with_expiry_status`](Self::cache_get_with_expiry_status).
    fn get_with_expiry_status<Q>(&mut self, key: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_get_with_expiry_status(key)
    }

    /// Non-renewing peek that also reports whether the found entry is expired.
    ///
    /// This is a required method. Implementations must satisfy the contract: same
    /// `(value, expired)` return shape as
    /// [`cache_get_with_expiry_status`](Self::cache_get_with_expiry_status), but the read
    /// must not produce any observable side effects: no LRU promotion, no hit/miss counter
    /// increment, no TTL renewal.
    ///
    /// Returns `(Some(v), true)` for a present-but-expired entry, `(None, false)` for an
    /// absent key, `(Some(v), false)` for a live entry.
    ///
    /// This is used on the `force_refresh` bypass path of `#[cached(result_fallback = true)]`
    /// to capture the stale fallback value without touching recency or metrics.
    fn cache_peek_with_expiry_status<Q>(&self, key: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone;
}

/// Concurrent analogue of [`CloneCached`] for the internally-synchronized sharded stores.
///
/// Like [`CloneCached`], [`cache_get_with_expiry_status`](ConcurrentCloneCached::cache_get_with_expiry_status)
/// returns a present-but-expired entry **without removing it**, so callers (e.g.
/// [`result_fallback`](macro@crate::macros::concurrent_cached)) can fall back to the stale
/// value if a refresh fails. Takes `&self` instead of `&mut self` because sharded stores are
/// internally synchronized and never need exclusive ownership from the caller.
///
/// Implemented by the four expiry-capable sharded stores:
/// [`ShardedTtlCache`], [`ShardedLruTtlCache`], [`ShardedExpiringCache`], and
/// [`ShardedExpiringLruCache`].
/// Non-expiry stores ([`ShardedUnboundCache`], [`ShardedLruCache`]) do not implement this trait,
/// mirroring how [`CloneCached`] is absent on [`UnboundCache`] and [`LruCache`].
///
/// **Why `&K` instead of `&Q` (`Borrow<Q>`)**: same reason as [`ConcurrentCached`] — the
/// concurrent cache trait family includes external stores that must serialize the key, so
/// the trait takes `&K` directly rather than a generic `Borrow<Q>` that carries no
/// serialization guarantee.
///
/// **Race window**: between the expiry check and the subsequent re-cache on `Err`, another
/// thread may have stored a fresh `Ok`. The re-cache will overwrite that fresh value, but
/// the outcome is only a redundant function call on the next expiry — not data corruption.
///
/// # Examples
///
/// ```rust
/// # #[cfg(feature = "time_stores")]
/// # {
/// use cached::{ConcurrentCached, ConcurrentCloneCached, ShardedTtlCache};
/// use cached::time::Duration;
///
/// let c = ShardedTtlCache::builder().ttl(Duration::from_secs(60)).build().unwrap();
/// c.set("k".to_string(), 1_i32).expect("infallible ShardedTtlCache set");
/// assert_eq!(c.cache_get_with_expiry_status(&"k".to_string()), (Some(1_i32), false)); // live
/// assert_eq!(c.cache_get_with_expiry_status(&"x".to_string()), (None, false));        // absent
/// // a present-but-expired entry yields (Some(v), true)
/// # }
/// ```
pub trait ConcurrentCloneCached<K, V> {
    /// Look up a cached value and report whether the found entry is expired.
    ///
    /// Returns `(value, expired)` where:
    /// - `(None, false)` — key not present
    /// - `(Some(v), false)` — key present and live (hits counter incremented)
    /// - `(Some(v), true)` — key present but expired; the stale value is returned so callers
    ///   can fall back to it if a refresh fails. The entry is **not** removed from the cache
    ///   and eviction counters are **not** incremented (misses counter incremented).
    ///
    /// Unlike [`ConcurrentCached::cache_get`], this never removes an expired entry — it
    /// intentionally leaves it in place so it can be returned as a stale fallback. Expired
    /// entries are swept by a subsequent `cache_get`, an explicit `cache_remove`, or `evict()`.
    fn cache_get_with_expiry_status(&self, key: &K) -> (Option<V>, bool);

    /// Look up a cached value and report whether the found entry is expired without any read
    /// side effects.
    ///
    /// This is a required method. Implementations must satisfy the contract: same
    /// `(value, expired)` return shape as
    /// [`cache_get_with_expiry_status`](Self::cache_get_with_expiry_status), but the read
    /// must not increment hits or misses counters, must not update LRU recency, and must
    /// not renew the TTL.
    ///
    /// Returns `(Some(v), true)` for a present-but-expired entry, `(None, false)` for an
    /// absent key, `(Some(v), false)` for a live entry.
    ///
    /// This is used on the `force_refresh` bypass path of `#[concurrent_cached(result_fallback = true)]`
    /// to capture the stale fallback value without touching counters or recency.
    fn cache_peek_with_expiry_status(&self, key: &K) -> (Option<V>, bool);
}

/// TTL management for single-owner time-bounded cache stores.
///
/// Implemented by the `&mut self` single-owner in-memory stores [`TtlCache`],
/// [`LruTtlCache`], and [`TtlSortedCache`].
///
/// Internally-synchronized concurrent stores (the sharded TTL stores, `RedisCache`,
/// `AsyncRedisCache`, and `RedbCache`) do **not** implement this trait. They are held
/// behind an `Arc`/`static` and cannot offer `&mut self`. Manage their TTL through the
/// `&self` methods on [`ConcurrentCached`]/[`ConcurrentCachedAsync`]
/// (`ttl`/`set_ttl`/`unset_ttl`/`set_refresh_on_hit`) instead.
///
/// This trait requires the `time_stores` feature.
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub trait CacheTtl {
    /// Return the TTL applied to newly inserted entries.
    fn ttl(&self) -> Option<Duration>;

    /// Set the TTL for newly inserted entries, returning the previous value.
    ///
    /// The TTL is stored unchecked: a zero `ttl` is accepted but makes every
    /// subsequently inserted entry expire immediately (reads return it as absent).
    /// Use [`try_set_ttl`](Self::try_set_ttl) to reject a zero `ttl`, or
    /// [`unset_ttl`](Self::unset_ttl) to disable expiry entirely.
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration>;

    /// Validated variant of [`set_ttl`](Self::set_ttl): returns [`SetTtlError::ZeroTtl`]
    /// when `ttl` is zero (which would otherwise silently make every inserted entry
    /// expire on insertion) instead of storing it. Use [`unset_ttl`](Self::unset_ttl)
    /// to disable expiry.
    fn try_set_ttl(&mut self, ttl: Duration) -> Result<Option<Duration>, crate::SetTtlError> {
        if ttl.is_zero() {
            return Err(crate::SetTtlError::ZeroTtl);
        }
        Ok(self.set_ttl(ttl))
    }

    /// Remove the TTL so entries are retained indefinitely.
    ///
    /// No-op for stores that cannot retain values indefinitely.
    fn unset_ttl(&mut self) -> Option<Duration>;

    /// Return `true` if cache hits refresh the TTL of the accessed entry.
    fn refresh_on_hit(&self) -> bool {
        false
    }

    /// Set whether cache hits should refresh the TTL. Returns the previous value.
    fn set_refresh_on_hit(&mut self, _refresh: bool) -> bool {
        false
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait CachedAsync<K, V> {
    /// Get the value for `k`, or compute and insert it by awaiting `f` on a miss.
    ///
    /// The async counterpart of [`Cached::get_or_set_with`]. It is `async_`-prefixed
    /// so that importing both [`Cached`] and [`CachedAsync`] (common, since the
    /// in-memory stores implement both) does not make `get_or_set_with` ambiguous
    /// at the call site.
    ///
    /// Returns `&V`. Use
    /// [`async_get_or_set_with_mut`](CachedAsync::async_get_or_set_with_mut) for a
    /// mutable reference.
    ///
    /// This default returns a `Send` future, so it carries `Self: Send, K: Send`
    /// (the future captures `&mut self` and `k` across the await). A store that is
    /// genuinely `!Send` cannot use this default and should implement
    /// [`async_get_or_set_with_mut`](CachedAsync::async_get_or_set_with_mut)
    /// directly; the `&V` wrapper is only a convenience over it.
    fn async_get_or_set_with<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a V> + Send + 'a
    where
        Self: Send,
        K: Send + 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move { &*self.async_get_or_set_with_mut(k, f).await }
    }

    /// The mutable counterpart of
    /// [`async_get_or_set_with`](CachedAsync::async_get_or_set_with): returns
    /// `&mut V`. Stores implement this method; the shared-reference variant
    /// delegates to it.
    fn async_get_or_set_with_mut<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a;

    /// Like [`async_get_or_set_with`](CachedAsync::async_get_or_set_with), but
    /// `f` is fallible: on a miss the value is cached only if `f` resolves to
    /// `Ok`, and an `Err` is returned without caching.
    ///
    /// Returns `Result<&V, E>`. Use
    /// [`async_try_get_or_set_with_mut`](CachedAsync::async_try_get_or_set_with_mut)
    /// for a mutable reference.
    ///
    /// Like [`async_get_or_set_with`](CachedAsync::async_get_or_set_with), this
    /// default returns a `Send` future and so carries `Self: Send, K: Send`; a
    /// `!Send` store should implement the `_mut` variant directly.
    fn async_try_get_or_set_with<'a, F, Fut, E>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<&'a V, E>> + Send + 'a
    where
        Self: Send,
        K: Send + 'a,
        V: Send + 'a,
        E: 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = Result<V, E>> + Send + 'a,
    {
        async move { self.async_try_get_or_set_with_mut(k, f).await.map(|v| &*v) }
    }

    /// The mutable counterpart of
    /// [`async_try_get_or_set_with`](CachedAsync::async_try_get_or_set_with):
    /// returns `Result<&mut V, E>`.
    fn async_try_get_or_set_with_mut<'a, F, Fut, E>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<&'a mut V, E>> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        E: 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = Result<V, E>> + Send + 'a;

    /// Retrieve a cached value asynchronously.
    ///
    /// Defaults to calling the synchronous [`Cached::cache_get`] implementation. Stores can
    /// override to provide a truly async path.
    fn get_async<'a, Q>(&'a mut self, k: &'a Q) -> impl Future<Output = Option<&'a V>> + Send + 'a
    where
        Self: Cached<K, V> + Send,
        K: std::borrow::Borrow<Q> + 'a,
        Q: std::hash::Hash + Eq + ?Sized + Sync,
        V: 'a,
    {
        async move { self.get(k) }
    }

    /// Insert a key-value pair asynchronously.
    ///
    /// Defaults to calling the synchronous [`Cached::cache_set`] implementation.
    fn set_async(&mut self, k: K, v: V) -> impl Future<Output = Option<V>> + Send
    where
        Self: Cached<K, V> + Send,
        K: Send,
        V: Send,
    {
        async move { self.set(k, v) }
    }

    /// Remove a cached value asynchronously.
    ///
    /// Defaults to calling the synchronous [`Cached::cache_remove`] implementation.
    fn remove_async<'a, Q>(&'a mut self, k: &'a Q) -> impl Future<Output = Option<V>> + Send + 'a
    where
        Self: Cached<K, V> + Send,
        K: std::borrow::Borrow<Q> + 'a,
        Q: std::hash::Hash + Eq + ?Sized + Sync,
        V: 'a,
    {
        async move { self.remove(k) }
    }

    /// Remove all entries asynchronously.
    ///
    /// Defaults to calling the synchronous [`Cached::cache_clear`] implementation.
    fn clear_async(&mut self) -> impl Future<Output = ()> + Send
    where
        Self: Cached<K, V> + Send,
    {
        async move { self.clear() }
    }
}

/// Cache operations on a store that manages its own synchronization (a shared,
/// `&self` API with owned return values and a fallible `Error`). Implemented by
/// `RedisCache`/`RedbCache`; implement it directly for a custom concurrent or
/// IO-backed store (this is the ~10-line pattern the 1.0 migration guide
/// recommends in place of the removed `InMemoryAdapter`):
///
/// ```rust
/// use cached::ConcurrentCached;
/// use std::collections::HashMap;
/// use std::sync::Mutex;
///
/// struct MyStore(Mutex<HashMap<String, u32>>);
///
/// impl ConcurrentCached<String, u32> for MyStore {
///     type Error = std::convert::Infallible;
///     fn cache_get(&self, k: &String) -> Result<Option<u32>, Self::Error> {
///         Ok(self.0.lock().unwrap().get(k).copied())
///     }
///     fn cache_set(&self, k: String, v: u32) -> Result<Option<u32>, Self::Error> {
///         Ok(self.0.lock().unwrap().insert(k, v))
///     }
///     fn cache_remove(&self, k: &String) -> Result<Option<u32>, Self::Error> {
///         Ok(self.0.lock().unwrap().remove(k))
///     }
///     fn cache_remove_entry(&self, k: &String) -> Result<Option<(String, u32)>, Self::Error> {
///         Ok(self.0.lock().unwrap().remove_entry(k))
///     }
///     fn set_refresh_on_hit(&self, _refresh: bool) -> bool { false }
/// }
///
/// let store = MyStore(Mutex::new(HashMap::new()));
/// assert_eq!(store.get(&"k".to_string()).expect("MyStore is infallible"), None);
/// assert_eq!(store.set("k".to_string(), 7).expect("MyStore is infallible"), None);
/// assert_eq!(store.get(&"k".to_string()).expect("MyStore is infallible"), Some(7));
/// assert_eq!(store.remove(&"k".to_string()).expect("MyStore is infallible"), Some(7));
/// ```
/// **Async counterpart**:
///
/// The asynchronous [`ConcurrentCachedAsync`] trait names its core operations with an `async_`
/// prefix (`async_cache_get`, `async_cache_set`, …) so they never collide with these synchronous
/// operations when both traits are imported.
///
/// **Why key-lookup methods take `&K` instead of `&Q` (`Borrow<Q>`)**:
///
/// [`Cached`] uses `Borrow<Q>` for all key-lookup methods (e.g. look up a `String` key with a
/// `&str`). `ConcurrentCached` cannot follow the same pattern because its implementors include
/// external stores (`RedbCache`, `RedisCache`) that must *serialize* the key in order to perform
/// a lookup. A generic `&Q` where only `K: Borrow<Q>` carries no serialization guarantee, and
/// adding a `Q: Serialize` bound to the trait would bleed a serde dependency into every
/// `ConcurrentCached` implementation. All key-lookup methods therefore take `&K` directly.
pub trait ConcurrentCached<K, V> {
    type Error;

    /// Attempt to retrieve a cached value
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Insert a key, value pair and return the previous value at the key, if any,
    /// without checking expiry. For TTL-based stores the returned value may have
    /// elapsed its TTL; for per-value expiry stores (implementing [`Expires`]) the
    /// returned value may report `is_expired() == true`. Check expiry on the returned
    /// value if you need to distinguish a live previous entry from an expired one.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error>;

    /// Remove a cached value, returning it if it was both present and still live.
    ///
    /// For stores with expiry, an entry that is present but already expired is still
    /// removed (and fires `on_evict` / counts an eviction), but `None` is returned.
    /// Use [`cache_remove_entry`](ConcurrentCached::cache_remove_entry) when you need
    /// to distinguish "key absent" from "key present but expired".
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Remove a cached entry, returning the stored key and value whenever an entry
    /// was physically deleted — including entries that were present but already expired.
    ///
    /// This is the key difference from [`cache_remove`](ConcurrentCached::cache_remove):
    /// - `cache_remove` returns `None` for both "key absent" and "key present but expired"
    /// - `cache_remove_entry` returns `Some((stored_key, value))` whenever anything was
    ///   physically deleted, and `None` only when the key was not in the store at all
    ///
    /// This expired-but-present guarantee holds for the in-memory stores (sharded and
    /// non-sharded) and for `RedbCache`, which enforces TTL client-side and can still read
    /// the stored value of an expired entry. Stores that enforce TTL server-side —
    /// `RedisCache` / `AsyncRedisCache` — cannot read the value of a TTL-expired key and so
    /// return `None` for it, exactly like `cache_remove`; see
    /// ["Note on Redis and external stores"](#note-on-redis-and-external-stores) below.
    ///
    /// Removing any present entry fires the store's `on_evict` callback (if set) and,
    /// for stores that track evictions, increments the `evictions` metric.
    ///
    /// # Note on `K: Clone`
    ///
    /// Implementations that reconstruct the stored key from the lookup key — such as
    /// `RedbCache` and `RedisCache` — require `K: Clone` to produce the stored-key half
    /// of the returned tuple.  Sharded in-memory stores return the physically stored key
    /// and do not impose this bound.
    ///
    /// # Note on Redis and external stores
    ///
    /// Stores that enforce TTL server-side (e.g. `RedisCache`, `AsyncRedisCache`) cannot
    /// retrieve the value of a TTL-expired key — `GET` returns nil once the TTL elapses,
    /// even before the background expiry sweep removes the key.  For such stores,
    /// `cache_remove_entry` behaves identically to `cache_remove` and returns `None` for
    /// server-side-expired entries.  Use [`cache_delete`](ConcurrentCached::cache_delete)
    /// (which issues `DEL` directly) to reliably confirm whether any physical entry was removed.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{ConcurrentCached, ShardedUnboundCache};
    ///
    /// let cache: ShardedUnboundCache<String, u32> = ShardedUnboundCache::builder().build().unwrap();
    /// cache.set("key".to_string(), 42).expect("ShardedUnboundCache is infallible");
    ///
    /// // remove_entry always returns Some when the key was present.
    /// let entry = cache.remove_entry(&"key".to_string()).expect("ShardedUnboundCache is infallible");
    /// assert_eq!(entry, Some(("key".to_string(), 42)));
    ///
    /// // Returns None only when the key was never present.
    /// assert_eq!(cache.remove_entry(&"missing".to_string()).expect("ShardedUnboundCache is infallible"), None);
    /// ```
    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error>;

    /// Delete a cached value without returning or decoding the stored value.
    ///
    /// Returns `true` if an entry (live or expired) was physically removed from the
    /// store, `false` if the key was not present. This differs from the 1.x
    /// behaviour where `cache_delete` returned `false` for expired entries — use
    /// [`cache_remove`](ConcurrentCached::cache_remove) if you need to distinguish
    /// a live removal from an expired one.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_delete(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_remove_entry(k).map(|removed| removed.is_some())
    }

    /// Retrieve a cached value. Delegates to [`cache_get`](ConcurrentCached::cache_get).
    #[inline]
    fn get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        self.cache_get(k)
    }

    /// Insert a key-value pair and return the previous value. Delegates to [`cache_set`](ConcurrentCached::cache_set).
    #[inline]
    fn set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        self.cache_set(k, v)
    }

    /// Remove a cached value and return it. Delegates to [`cache_remove`](ConcurrentCached::cache_remove).
    #[inline]
    fn remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        self.cache_remove(k)
    }

    /// Remove a cached entry and return the stored key and value. Delegates to [`cache_remove_entry`](ConcurrentCached::cache_remove_entry).
    #[inline]
    fn remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        self.cache_remove_entry(k)
    }

    /// Delete a cached value without returning it. Delegates to [`cache_delete`](ConcurrentCached::cache_delete).
    #[inline]
    fn delete(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_delete(k)
    }

    /// Report the number of entries currently held by the store, if the store can
    /// determine it cheaply.
    ///
    /// Returns `Ok(Some(n))` for stores that track their own size (all in-memory sharded
    /// stores), and `Ok(None)` for stores that cannot answer without an expensive or
    /// semantically-ambiguous query — `RedisCache` / `AsyncRedisCache` (the key count is a
    /// server-side `DBSIZE`/`SCAN` over a shared keyspace, not just this cache's entries) and
    /// `RedbCache` (an `O(n)` scan of the backing table). Those stores return `Ok(None)` rather
    /// than pay that cost implicitly. For `RedisCache` / `AsyncRedisCache` you can query the server
    /// directly if you need a count; `RedbCache` holds an exclusive lock on its file and exposes no
    /// live database handle (`RedbCache::disk_path()` gives the file location for offline inspection,
    /// but the database cannot be opened by a second instance while the cache is live).
    ///
    /// This is the concurrent analogue of [`Cached::cache_size`], widened to
    /// `Result<Option<usize>, _>` because concurrent stores may be fallible and may not know
    /// their size. The default returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if determining the size fails.
    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(None)
    }

    /// Ergonomic alias for [`cache_size`](Self::cache_size).
    fn len(&self) -> Result<Option<usize>, Self::Error> {
        self.cache_size()
    }

    /// Return `Ok(Some(true))` if the cache is known to be empty, `Ok(None)` if the size is unknown.
    fn is_empty(&self) -> Result<Option<bool>, Self::Error> {
        Ok(self.cache_size()?.map(|n| n == 0))
    }

    /// Remove all cached entries while preserving capacity allocation and metrics.
    ///
    /// This is a required method. The concurrent analogue of [`Cached::cache_clear`].
    /// The internally-synchronized sharded in-memory stores clear every shard; `RedbCache`
    /// clears its (local, single-file) redb table; and `RedisCache` / `AsyncRedisCache`
    /// use a namespace-scoped `SCAN` + batched `DEL` (O(n) in matching keys and not atomic;
    /// see the store docs). To also reset metrics, call
    /// [`cache_reset_metrics`](ConcurrentCached::cache_reset_metrics), or use
    /// [`cache_reset`](ConcurrentCached::cache_reset) to do both at once.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn cache_clear(&self) -> Result<(), Self::Error>;

    /// Reset all entries and metrics (hits, misses, evictions) to zero.
    ///
    /// This is a required method. The concurrent analogue of [`Cached::cache_reset`]. Store
    /// configuration (capacity, TTL, `on_evict` callbacks) is preserved. The sharded in-memory
    /// stores clear every shard and zero their metrics; `RedbCache` and `RedisCache` /
    /// `AsyncRedisCache` clear their entries (they track no in-memory metrics, so resetting
    /// is exactly [`cache_clear`](ConcurrentCached::cache_clear)).
    /// To reset entries without resetting metrics, use
    /// [`cache_clear`](ConcurrentCached::cache_clear).
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn cache_reset(&self) -> Result<(), Self::Error>;

    /// Reset hit/miss/eviction counters to zero without removing entries.
    ///
    /// The concurrent analogue of [`Cached::cache_reset_metrics`]. The default is a **no-op**;
    /// the sharded in-memory stores override it to zero their per-shard metrics.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    fn cache_reset_metrics(&self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized (sharded stores use an
    /// `AtomicBool`; `RedisCache` / `RedbCache` use interior mutability), so this is callable
    /// through a shared reference such as an `Arc` or a `LazyLock` static.
    fn set_refresh_on_hit(&self, refresh: bool) -> bool;

    /// Return the ttl of cached values (time to eviction).
    fn ttl(&self) -> Option<Duration> {
        None
    }

    /// Set the ttl of cached values, returning the previous value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized, so this is callable
    /// through a shared reference. The default is a no-op returning `None`.
    fn set_ttl(&self, _ttl: Duration) -> Option<Duration> {
        None
    }

    /// Remove the ttl for cached values, returning the previous value.
    ///
    /// For cache implementations that don't support retaining values indefinitely, this method is
    /// a no-op. Takes `&self`: concurrent stores are internally synchronized, so this is
    /// callable through a shared reference.
    fn unset_ttl(&self) -> Option<Duration> {
        None
    }
}

/// **Direct-call syntax warning**:
///
/// `ConcurrentCachedAsync` names its core operations with an `async_` prefix
/// (`async_cache_get`, `async_cache_set`, `async_cache_remove`, `async_cache_remove_entry`,
/// `async_cache_delete`) so they never collide with the synchronous [`ConcurrentCached`]
/// operations (`cache_get`, `cache_set`, …) even when both traits are imported. This means
/// method-call syntax on a store that implements both traits is unambiguous:
/// ```rust,ignore
/// store.async_cache_get(&key).await
/// ```
///
/// **Short aliases not provided**: Unlike [`ConcurrentCached`], this trait intentionally does
/// **not** expose `get`/`set`/`remove`/`delete` short aliases. The `async_`-prefixed names are
/// the full and only spelling of these operations.
///
/// **Custom `!Sync` implementors**: the default method bodies (`async_cache_delete`,
/// `async_cache_clear`, `async_cache_reset`, `async_cache_reset_metrics`) require `Self: Sync`
/// so the returned future is `Send`. A store that is not `Sync` must provide its own override
/// for these methods (even an identical no-op) rather than relying on the defaults.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait ConcurrentCachedAsync<K, V> {
    type Error;
    #[doc(alias = "async_get")]
    #[doc(alias = "cache_get")]
    fn async_cache_get(&self, k: &K)
    -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    #[doc(alias = "async_set")]
    #[doc(alias = "cache_set")]
    fn async_cache_set(
        &self,
        k: K,
        v: V,
    ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached value, returning it if it was both present and still live.
    #[doc(alias = "async_remove")]
    #[doc(alias = "cache_remove")]
    fn async_cache_remove(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached entry, returning the stored key and value whenever an entry
    /// was physically deleted — including entries that were present but already expired.
    #[doc(alias = "async_remove_entry")]
    #[doc(alias = "cache_remove_entry")]
    fn async_cache_remove_entry(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<(K, V)>, Self::Error>> + Send;

    /// Delete a cached value without returning or decoding the stored value.
    /// Returns `true` if an entry (live or expired) was physically removed from the
    /// store, `false` if the key was not present.
    #[doc(alias = "async_delete")]
    #[doc(alias = "cache_delete")]
    fn async_cache_delete(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync,
    {
        async move { self.async_cache_remove_entry(k).await.map(|r| r.is_some()) }
    }

    /// Remove all cached entries while preserving capacity allocation and metrics.
    ///
    /// This is a required method. The async counterpart of [`ConcurrentCached::cache_clear`].
    /// The internally-synchronized sharded in-memory stores and `RedbCache` clear their entries;
    /// `AsyncRedisCache` uses a namespace-scoped `SCAN` + batched `DEL` (O(n) in matching keys
    /// and not atomic; see the store docs).
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[doc(alias = "cache_clear")]
    fn async_cache_clear(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync;

    /// Reset all entries and metrics (hits, misses, evictions) to zero.
    ///
    /// This is a required method. The async counterpart of [`ConcurrentCached::cache_reset`].
    /// Store configuration (capacity, TTL, `on_evict` callbacks) is preserved. The sharded
    /// in-memory stores clear every shard and zero their metrics; `RedbCache` and
    /// `AsyncRedisCache` clear their entries (they track no in-memory metrics, so resetting
    /// is exactly [`async_cache_clear`](ConcurrentCachedAsync::async_cache_clear)).
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[doc(alias = "cache_reset")]
    fn async_cache_reset(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync;

    /// Reset hit/miss/eviction counters to zero without removing entries.
    ///
    /// The async counterpart of [`ConcurrentCached::cache_reset_metrics`]. The default is a
    /// **no-op** that resolves to `Ok(())`; the sharded in-memory stores override it to zero
    /// their per-shard metrics.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails.
    #[doc(alias = "cache_reset_metrics")]
    fn async_cache_reset_metrics(&self) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sync,
    {
        async move { Ok(()) }
    }

    /// Report the number of entries currently held by the store, if the store can
    /// determine it cheaply.
    ///
    /// The concurrent-async analogue of [`Cached::cache_size`]. Returns `Ok(None)` by default;
    /// external stores (`RedbCache`, `RedisCache`, `AsyncRedisCache`) keep that default because
    /// they cannot report their entry count without an expensive or ambiguous backend query.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if determining the size fails.
    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(None)
    }

    /// Ergonomic alias for [`cache_size`](Self::cache_size).
    fn len(&self) -> Result<Option<usize>, Self::Error> {
        self.cache_size()
    }

    /// Return `Ok(Some(true))` if the cache is known to be empty, `Ok(None)` if the size is unknown.
    fn is_empty(&self) -> Result<Option<bool>, Self::Error> {
        Ok(self.cache_size()?.map(|n| n == 0))
    }

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized (sharded stores use an
    /// `AtomicBool`; `RedisCache` / `RedbCache` use interior mutability), so this is callable
    /// through a shared reference such as an `Arc` or a `LazyLock` static.
    fn set_refresh_on_hit(&self, refresh: bool) -> bool;

    /// Return the ttl of cached values (time to eviction).
    fn ttl(&self) -> Option<Duration> {
        None
    }

    /// Set the ttl of cached values, returning the previous value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized, so this is callable
    /// through a shared reference. The default is a no-op returning `None`.
    fn set_ttl(&self, _ttl: Duration) -> Option<Duration> {
        None
    }

    /// Remove the ttl for cached values, returning the previous value.
    ///
    /// For cache implementations that don't support retaining values indefinitely, this method is
    /// a no-op. Takes `&self`: concurrent stores are internally synchronized, so this is
    /// callable through a shared reference.
    fn unset_ttl(&self) -> Option<Duration> {
        None
    }
}

/// Borrowed-set extension for serialize-based concurrent stores.
///
/// Stores that persist values by serialization (`RedisCache`, `RedbCache`) can accept the
/// key and value by reference, avoiding the clone that
/// [`ConcurrentCached::cache_set`] requires to take ownership.
/// This trait is additive: it does not replace `cache_set`, and the sharded in-memory stores
/// (which store the value directly) do not implement it.
///
/// The `#[concurrent_cached]` macro automatically routes its cache-set through this trait when
/// the concrete store implements it (no opt-in needed), falling back to the owned `cache_set`
/// otherwise.
///
/// Implementing this on a custom `#[concurrent_cached]` store (one supplied via `ty` / `create`)
/// is worthwhile when the store serializes its values: the macro will then set entries from the
/// borrowed `&V` with no clone. Without it, the macro takes the owned fallback, which requires
/// `V: Clone` and clones the value on every set.
pub trait SerializeCached<K, V>: ConcurrentCached<K, V> {
    /// Insert a key/value pair, taking both by reference, and return the previous value if any.
    ///
    /// Semantically equivalent to [`ConcurrentCached::cache_set`] but serializes from the
    /// borrowed `k`/`v` without cloning.
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if the operation fails.
    fn cache_set_ref(&self, k: &K, v: &V) -> Result<Option<V>, Self::Error>;
}

/// Async borrowed-set extension for serialize-based concurrent stores.
///
/// The async counterpart of [`SerializeCached`], implemented by `AsyncRedisCache` and `RedbCache`.
///
/// Note that for the async set path the macro holds the `&V` across the set future, so the
/// borrowed (clone-eliding) route is taken only when the store is also `Sync` and `V: Sync`.
/// A custom `Send + !Sync` async store that implements this trait still falls back to the owned
/// `async_cache_set` clone path (requiring `V: Clone`); a `Send + !Sync + !Clone` value cannot
/// take either route and fails to compile. That failure surfaces as a trait-resolution error at the
/// generated `#[concurrent_cached]` set site (referring to internal dispatch helpers), not at your
/// `impl`; add a `V: Sync` or `V: Clone` bound to resolve it.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait SerializeCachedAsync<K, V>: ConcurrentCachedAsync<K, V> {
    /// Insert a key/value pair, taking both by reference, and return the previous value if any.
    ///
    /// The async counterpart of [`SerializeCached::cache_set_ref`].
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if the operation fails.
    fn async_cache_set_ref(
        &self,
        k: &K,
        v: &V,
    ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;
}

/// Autoref-specialization shim used by the generated `#[concurrent_cached]` set site
/// to prefer the borrowed setter (`SerializeCached::cache_set_ref`, no value clone) when
/// the concrete store implements `SerializeCached`, falling back to owned `cache_set`
/// (cloning the value) otherwise. Internal implementation detail of the proc-macro; no
/// stability guarantee.
#[doc(hidden)]
pub mod __set_dispatch {
    use super::{ConcurrentCached, SerializeCached};
    use core::marker::PhantomData;

    pub struct SetDispatch<'s, S, K, V> {
        store: &'s S,
        _pd: PhantomData<(K, V)>,
    }

    impl<'s, S, K, V> SetDispatch<'s, S, K, V> {
        #[inline]
        pub fn new(store: &'s S) -> Self {
            SetDispatch {
                store,
                _pd: PhantomData,
            }
        }
    }

    // PREFERRED arm: inherent method, only exists when S: SerializeCached. No V: Clone.
    impl<S, K, V> SetDispatch<'_, S, K, V>
    where
        S: SerializeCached<K, V>,
    {
        #[inline]
        pub fn cache_set_dispatch(
            &self,
            key: K,
            value: &V,
        ) -> Result<Option<V>, <S as ConcurrentCached<K, V>>::Error> {
            SerializeCached::cache_set_ref(self.store, &key, value)
        }
    }

    // FALLBACK arm: trait method, reached only when the inherent one is pruned. Requires V: Clone.
    pub trait SetDispatchFallback<K, V> {
        type Error;
        fn cache_set_dispatch(&self, key: K, value: &V) -> Result<Option<V>, Self::Error>;
    }

    impl<S, K, V> SetDispatchFallback<K, V> for SetDispatch<'_, S, K, V>
    where
        S: ConcurrentCached<K, V>,
        V: Clone,
    {
        type Error = <S as ConcurrentCached<K, V>>::Error;
        #[inline]
        fn cache_set_dispatch(&self, key: K, value: &V) -> Result<Option<V>, Self::Error> {
            ConcurrentCached::cache_set(self.store, key, value.clone())
        }
    }
}

/// Async counterpart of [`__set_dispatch`]: prefers the borrowed async setter
/// (`SerializeCachedAsync::async_cache_set_ref`, no value clone) when the concrete store
/// implements `SerializeCachedAsync`, falling back to owned `async_cache_set` (cloning the
/// value) otherwise. Internal implementation detail of the proc-macro; no stability guarantee.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
#[doc(hidden)]
pub mod __set_dispatch_async {
    use super::{ConcurrentCachedAsync, SerializeCachedAsync};
    use core::future::Future;
    use core::marker::PhantomData;

    pub struct SetDispatchAsync<'s, S, K, V> {
        store: &'s S,
        _pd: PhantomData<(K, V)>,
    }

    impl<'s, S, K, V> SetDispatchAsync<'s, S, K, V> {
        #[inline]
        pub fn new(store: &'s S) -> Self {
            SetDispatchAsync {
                store,
                _pd: PhantomData,
            }
        }
    }

    // PREFERRED async arm: inherent, gated on SerializeCachedAsync. The key is moved into the
    // method by value, so we wrap in `async move` to own it across the await; `value: &V` is
    // borrowed from the caller and lives across the caller's immediate await. (Direct-forward
    // of the future would let it borrow the local `key`, which escapes; the async-move form
    // moves `key` into the future instead.)
    //
    // Because the returned future captures `&V` across its `.await`, it is `Send` only when
    // `V: Sync` -- a stronger bound than the store's own `async_cache_set_ref` needs (`V: Send`,
    // since the store serializes `&V` before its first await and does not hold it across one).
    // A `Send + !Sync` value therefore does not match this arm: if it is `Clone` it takes the
    // owned fallback below (one clone, the pre-shim behavior); if it is also `!Clone` neither arm
    // applies and `#[concurrent_cached]` fails to compile. Both are rare for serialize-backed
    // async stores and never worse than the previous always-clone path.
    impl<S, K, V> SetDispatchAsync<'_, S, K, V>
    where
        S: SerializeCachedAsync<K, V> + Sync,
        K: Send,
        V: Sync,
    {
        #[inline]
        pub fn cache_set_dispatch(
            &self,
            key: K,
            value: &V,
        ) -> impl Future<Output = Result<Option<V>, <S as ConcurrentCachedAsync<K, V>>::Error>> + Send
        {
            let store = self.store;
            async move { SerializeCachedAsync::async_cache_set_ref(store, &key, value).await }
        }
    }

    // FALLBACK async arm: trait. Clone the value EAGERLY before building the future so no
    // &V borrow is held across .await (matches the macro's current behavior).
    pub trait SetDispatchAsyncFallback<K, V> {
        type Error;
        fn cache_set_dispatch(
            &self,
            key: K,
            value: &V,
        ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;
    }

    impl<S, K, V> SetDispatchAsyncFallback<K, V> for SetDispatchAsync<'_, S, K, V>
    where
        S: ConcurrentCachedAsync<K, V> + Sync,
        K: Send,
        V: Clone + Send,
    {
        type Error = <S as ConcurrentCachedAsync<K, V>>::Error;
        #[inline]
        fn cache_set_dispatch(
            &self,
            key: K,
            value: &V,
        ) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send {
            let v = value.clone();
            let store = self.store;
            async move { ConcurrentCachedAsync::async_cache_set(store, key, v).await }
        }
    }
}

// `Cached` stores are single-owner (`&mut self`); to share one across threads,
// bring your own lock or use a macro (`#[cached]`/`#[once]` generate the lock).
// `ConcurrentCached`/`ConcurrentCachedAsync` is the contract for stores that
// manage their own synchronization (`RedisCache`, `RedbCache`).
