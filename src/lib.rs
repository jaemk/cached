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

**Features**

- `default`: Include `proc_macro`, `ahash`, and `time_stores` features
- `proc_macro`: Include proc macros
- `ahash`: Enable the optional `ahash` hasher as default hashing algorithm.
- `async_core`: Include runtime-agnostic async traits used by async cache stores
- `async`: Include support for async functions and async cache stores using Tokio synchronization
- `async_tokio_rt_multi_thread`: Enable `tokio`'s optional `rt-multi-thread` feature.
- `redis_store`: Include Redis cache store
- `redis_smol`: Include async Redis support using `smol` and `smol` tls support, implies `redis_store` and `async`
- `redis_tokio`: Include async Redis support using `tokio` and `tokio` tls support, implies `redis_store` and `async`
- `redis_connection_manager`: Enable the optional `connection-manager` feature of `redis`. Any async redis caches created
  will use a connection manager instead of a `MultiplexedConnection`. Implies `async` (Tokio runtime) and `redis_store`,
  but does **not** enable TLS. Add `redis_tokio` alongside if TLS is required.
- `redis_async_cache`: Enable Redis client-side caching over RESP3 for async Redis caches.
  When enabled standalone, this feature defaults to the Tokio async Redis path.
- `redis_ahash`: Enable the optional `ahash` feature of `redis`
- `disk_store`: Include disk cache store
- `wasm`: Enable WASM support. Note that this feature is incompatible with `tokio`'s multi-thread
  runtime (`async_tokio_rt_multi_thread`) and all Redis features (`redis_store`, `redis_smol`, `redis_tokio`, `redis_ahash`)
- `time_stores`: Include time-based cache stores ([`TtlCache`](https://docs.rs/cached/latest/cached/struct.TtlCache.html), [`LruTtlCache`](https://docs.rs/cached/latest/cached/struct.LruTtlCache.html), [`TtlSortedCache`](https://docs.rs/cached/latest/cached/struct.TtlSortedCache.html), [`ShardedTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedTtlCache.html), and [`ShardedLruTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedLruTtlCache.html)).
  Also required when using `#[concurrent_cached(ttl = …)]` on the default in-memory path.
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
| TTL — expire results after N seconds | `#[cached(ttl = 60)] fn config() -> Config` |
| LRU + TTL | `#[cached(max_size = 500, ttl = 300)] fn search(q: String) -> Vec<Hit>` |
| Don't cache `None` returns (implicit for `Option<T>`) | `#[cached] fn find(id: u64) -> Option<User>` |
| Don't cache `Err` returns (implicit for `Result<T, E>`) | `#[cached] fn load(id: u64) -> Result<Data, E>` |
| Force-cache `None` returns | `#[cached(cache_none = true)] fn find(id: u64) -> Option<User>` |
| Force-cache `Err` returns | `#[cached(cache_err = true)] fn load(id: u64) -> Result<Data, E>` |
| Serve stale value when function returns `Err` | `#[cached(result_fallback = true, ttl = 60)] fn fetch(id: u64) -> Result<Data, E>` |
| Per-value / dynamic per-entry TTL (value carries its own expiry) | `#[cached(expires = true)] fn token(scope: String) -> Token` |
| Deduplicate concurrent first calls for same key | `#[cached(ttl = 30, sync_writes = "by_key")] fn expensive(id: u64) -> Payload` |
| Async | `#[cached(max_size = 100)] async fn remote(id: u64) -> Data` |
| **`#[once]`** | |
| Compute and cache a global value forever | `#[once] fn app_config() -> Config` |
| Refresh a global value periodically | `#[once(ttl = 300, sync_writes = true)] fn pubkey() -> Key` |
| Optional global — skip caching if `None` (implicit) | `#[once] fn feature_flag() -> Option<Flag>` |
| **`#[concurrent_cached]`** | |
| Thread-safe sharded memoize (no global lock per call) | `#[concurrent_cached] fn compute(x: u64) -> u64` |
| Sharded with LRU | `#[concurrent_cached(max_size = 1_000)] fn lookup(id: u64) -> Row` |
| Sharded with TTL | `#[concurrent_cached(ttl = 60)] fn fetch(url: String) -> Body` |
| Sharded LRU + TTL with custom shard count | `#[concurrent_cached(max_size = 1_000, ttl = 60, shards = 32)] fn query(id: u64) -> Row` |
| Per-value expiry, thread-safe | `#[concurrent_cached(expires = true)] fn session(id: u32) -> Token` |
| Per-value expiry with LRU bound | `#[concurrent_cached(expires = true, max_size = 1_000)] fn session(id: u32) -> Token` |
| Cache only successful results (implicit for `Result<T, E>`) | `#[concurrent_cached] fn load(id: u64) -> Result<Row, DbError>` |
| Don't cache `None` returns (implicit for `Option<T>`) | `#[concurrent_cached] fn find(id: u64) -> Option<Row>` |
| Serve stale value when function returns `Err` | `#[concurrent_cached(result_fallback = true, ttl = 60)] fn fetch(id: u64) -> Result<Data, E>` |
| Persist results to disk | `#[concurrent_cached(disk = true, map_error = \|e\| MyErr(e))] fn crunch(n: u64) -> Result<Data, MyErr>` |
| Redis-backed async cache | `#[concurrent_cached(ty = "AsyncRedisCache<u64, String>", create = r#"{ ... }"#, map_error = \|e\| MyErr(e))] async fn api(id: u64) -> Result<Resp, MyErr>` |

On `#[cached]` and `#[concurrent_cached]`, the preferred attribute is `max_size = N` (mirroring the `max_size` builder/constructor methods on the stores). The legacy `size = N` is still accepted as a deprecated alias, but emits a deprecation warning nudging you toward `max_size = N`. Either spelling works; setting both on one annotation is a compile error.

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
| [`ShardedCache`](https://docs.rs/cached/latest/cached/type.ShardedCache.html) | None (unbounded) | No | No | N/A | On explicit remove | Yes (`Arc`) | Yes |
| [`ShardedLruCache`](https://docs.rs/cached/latest/cached/type.ShardedLruCache.html) | LRU | Yes | No | N/A | Yes | Yes (`Arc`) | Yes |
| [`ShardedTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedTtlCache.html) | TTL (insert time) | No | Global | Optional | Yes | Yes (`Arc`) | Yes |
| [`ShardedLruTtlCache`](https://docs.rs/cached/latest/cached/type.ShardedLruTtlCache.html) | LRU + TTL | Yes | Global | Optional | Yes (†) | Yes (`Arc`) | Yes |
| [`ShardedExpiringCache`](https://docs.rs/cached/latest/cached/type.ShardedExpiringCache.html) | Value-defined | No | Per-value | N/A | Yes | Yes (`Arc`) | Yes |
| [`ShardedExpiringLruCache`](https://docs.rs/cached/latest/cached/type.ShardedExpiringLruCache.html) | LRU + value-defined | Yes | Per-value | N/A | Yes | Yes (`Arc`) | Yes |

> "On explicit remove" — `on_evict` fires only on `cache_remove`; there is no capacity eviction or TTL expiry trigger for these stores.
> † `ShardedLruTtlCacheBuilder::on_evict` requires `K: 'static + V: 'static`; see the builder docs for details.

`TtlCache`/`LruTtlCache`/`TtlSortedCache`/`ShardedTtlCache`/`ShardedLruTtlCache` require the `time_stores` feature.

`ShardedCache` and its variants are partitioned across power-of-two shards (default: `available_parallelism() × 4`, clamped to 8–1024; the 8–1024 clamp applies only to this computed default — an explicit `shards = N` is rounded up to a power of two but never clamped) each protected by a `parking_lot::RwLock`. Shard structs are padded to 128-byte alignment (covering Intel adjacent-line prefetch and Apple Silicon 128-byte L1 lines) to eliminate false sharing; on a 64-shard deployment this amounts to ~8 KB of padding overhead per cache array. The outer type is an `Arc` — cloning is a reference share, not a deep copy (use `deep_clone()` for an independent copy; note that `deep_clone()` is an inherent method on each concrete sharded type, not part of any trait). They implement `ConcurrentCached`/`ConcurrentCachedAsync` and are the default store selected by `#[concurrent_cached]`.
For sharded LRU variants, eviction is enforced independently per shard. `max_size = N` is divided across shards with ceiling division. Use the builder's `per_shard_max_size` method for an exact per-shard cap (builder-only; `#[concurrent_cached]` does not expose a `per_shard_max_size` attribute — use `shards` to control parallelism and `max_size` for total capacity). **Capacity Fragmentation Warning**: To protect against premature evictions due to hash collisions in extremely small caches (where a shard capacity could drop to 1-2 entries), when sharding is active (`shards > 1`) we enforce a minimum capacity of `16` entries **per shard** (e.g., minimum total capacity of `128` on a single-core machine with 8 shards, or `256` on a 4-core machine with 16 shards). If you require smaller, strict limits under low capacities, configure `shards = 1` or specify `per_shard_max_size` directly (builder-only; not available via `#[concurrent_cached]`).
Because LRU caches require updating access recency, `ShardedLruCache`, `ShardedLruTtlCache`, and `ShardedExpiringLruCache` must acquire an exclusive **write lock** on accessed shards during read hits, which can lead to contention under highly concurrent read-heavy workloads. Unbounded `ShardedCache`, time-only `ShardedTtlCache` (when `refresh_on_hit` is disabled — enabling it promotes read hits to exclusive write locks), and expiring `ShardedExpiringCache` require only a **shared read lock** on read hits, avoiding this contention. To mitigate contention on LRU variants, consider increasing the number of `shards` to distribute writes.

> **`*Base` types:** Each sharded store has a corresponding `*Base` generic (`ShardedCacheBase<K, V, H>`, `ShardedLruCacheBase<K, V, H>`, etc.) parameterized on a custom [`ShardHasher`]. The named aliases (`ShardedCache`, `ShardedLruCache`, …) use the default hasher and are what most users should reach for. Use the `*Base` types only when implementing a custom `ShardHasher` for non-standard shard routing.

**Behavioral guarantees**

- Non-sharded in-memory stores (`UnboundCache`, `LruCache`, `TtlCache`, etc.) are not internally
  synchronized. Macro-generated `#[cached]`/`#[once]` functions wrap them in locks; users
  managing these stores directly must add their own synchronization when sharing across threads.
  `Sharded*` stores are internally synchronized (per-shard `parking_lot::RwLock`) and implement
  `ConcurrentCached`/`ConcurrentCachedAsync` — no external lock is needed.
  Direct sharded-store method syntax is synchronous because these stores expose inherent
  `cache_get` / `cache_set` / `cache_remove` helpers. Use Universal Function Call Syntax (UFCS)
  for async trait calls (e.g., `cached::ConcurrentCachedAsync::cache_get(&*STORE, &key).await.expect("ShardedCache is infallible")`), where `&*STORE` dereferences a `LazyLock<Store>` or `OnceCell<Store>` static to obtain a `&Store` reference.
- `Cached::get` (and its legacy alias `cache_get`) requires mutable access because some
  stores update recency, expiration timestamps, or metrics during reads.
- Expired values can remain allocated until a mutating operation, `evict`, or
  store-specific cleanup removes them. Methods such as `len` may include expired values
  unless a store documents otherwise.
- `cache_remove` fires the `on_evict` callback (if set) and counts as an eviction for
  every successful removal, across all stores that track evictions. `ShardedCache` is the
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
  which provides `cache_get_with_expiry_status` for reading stale entries without evicting them.

**Per-Value Expiry via the `Expires` Trait**

While standard timed stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`) enforce a single, global Time-To-Live (TTL) duration applied to all entries in the cache, [`ExpiringLruCache`] and [`ExpiringCache`] let each individual value determine its own expiration. This is accomplished by storing values that implement the [`Expires`] trait.

This approach is highly useful when caching payloads like OAuth tokens, HTTP responses with varying `Cache-Control` headers, or database records that contain their own absolute expiration timestamps.

It is also the idiomatic way to give entries a **dynamic, per-entry TTL** — a lifetime computed at call time rather than the single uniform duration that `ttl = N` applies to every entry. Because the value carries its own expiry, each entry can be given a different lifetime derived from a function argument, runtime configuration, or a response header. (`expires = true` is mutually exclusive with `ttl`.) See the [`expires_per_key`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs) example for a runnable demonstration.

When using the `#[cached]` or `#[once]` proc macros, add `expires = true` to opt into per-value expiry automatically. For `#[cached]`, this selects `ExpiringCache` (unbounded) by default or `ExpiringLruCache` when `max_size` is also specified. For `#[once]`, this stores a single value whose expiry is polled on each call.

The macro form below derives each entry's TTL from a function argument — `key`/`convert` keep the TTL out of the cache key so it influences only the entry's lifetime, not which slot it occupies:

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
> call `evict()` periodically (bring [`CacheEvict`] into scope: `use cached::CacheEvict;`; note
> that `evict()` on sharded TTL and expiring stores requires `K: Clone`) or
> prefer `ExpiringLruCache` / `ShardedExpiringLruCache` with a `max_size` bound.

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
cache.cache_set("key1", Response {
    payload: "a".to_string(),
    expires_at: now + Duration::from_secs(1),
});
cache.cache_set("key2", Response {
    payload: "b".to_string(),
    expires_at: now + Duration::from_secs(3600),
});

// ExpiringLruCache — LRU-bounded, used with `#[cached(expires = true, max_size = N)]`
let mut lru = ExpiringLruCache::builder().max_size(10).build().unwrap();
lru.cache_set("key1", Response {
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
/// expires (according to `ttl` seconds).
/// When no (or expired) cache, concurrent calls
/// will synchronize (`sync_writes`) so the function
/// is only executed once.
# #[cfg(feature = "time_stores")]
#[once(ttl =10, sync_writes = true)]
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
    ttl = 1,
    sync_writes = "default",
    result_fallback = true
)]
fn doesnt_compile() -> Result<String, ()> {
    Ok("a".to_string())
}
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
            .refresh(true)
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
use cached::DiskCache;
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
/// to also cache `Err`). `result_fallback = true` is supported (requires `ttl`): on an `Err`
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
- Arguments and return values will be `cloned` in the process of insertion and retrieval. For Redis and
  disk stores, keys are additionally formatted into `String`s and values are de/serialized.
- Macro-defined functions should not be used to produce side-effectual results!
- Macro-defined functions cannot live directly under `impl` blocks since macros expand to a
  static initialization and one or more function definitions.
- Macro-defined functions cannot accept `Self` types as a parameter.


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
#[cfg(any(feature = "redis_smol", feature = "redis_tokio"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "redis_smol", feature = "redis_tokio"))))]
pub use stores::{AsyncRedisCache, AsyncRedisCacheBuilder};
pub use stores::{
    BuildError, CacheEvict, DefaultShardHasher, Expires, ExpiringCache, ExpiringCacheBuilder,
    ExpiringLruCache, ExpiringLruCacheBuilder, LruCache, LruCacheBuilder, ShardHasher,
    ShardedCache, ShardedCacheBase, ShardedCacheBuilder, ShardedExpiringCache,
    ShardedExpiringCacheBase, ShardedExpiringCacheBuilder, ShardedExpiringLruCache,
    ShardedExpiringLruCacheBase, ShardedExpiringLruCacheBuilder, ShardedLruCache,
    ShardedLruCacheBase, ShardedLruCacheBuilder, UnboundCache, UnboundCacheBuilder,
};
#[cfg(feature = "disk_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "disk_store")))]
pub use stores::{DiskCache, DiskCacheBuildError, DiskCacheBuilder, DiskCacheError};
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
#[doc(hidden)]
pub use web_time;

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

/// Internal marker referenced by `#[cached(size = N)]` / `#[concurrent_cached(size = N)]`
/// expansions to surface a deprecation warning steering users to the `max_size` spelling.
/// Not part of the public API.
#[doc(hidden)]
#[deprecated(
    since = "2.0.0",
    note = "the `size` macro attribute is deprecated; use `max_size` instead (same meaning)"
)]
pub const __DEPRECATED_SIZE_ATTR: () = ();

/// Cache operations
///
/// ```rust
/// use cached::{Cached, UnboundCache};
///
/// let mut cache: UnboundCache<String, String> = UnboundCache::builder().build().unwrap();
///
/// cache.set("key".to_string(), "owned value".to_string());
///
/// let borrowed_cache_value = cache.get("key");
/// assert_eq!(borrowed_cache_value, Some(&"owned value".to_string()))
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

    /// Get or insert a key-value pair.
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V;

    /// Get or insert a key-value pair, propagating errors from the factory.
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E>;

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
    /// cache.cache_set("key".to_string(), 42);
    ///
    /// // cache_remove_entry returns Some even for the key that was just inserted.
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
    fn get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.cache_get_or_set_with(key, f)
    }

    /// Get or insert a key-value pair with error handling. Delegates to [`cache_try_get_or_set_with`](Cached::cache_try_get_or_set_with).
    fn try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        k: K,
        f: F,
    ) -> Result<&mut V, E> {
        self.cache_try_get_or_set_with(k, f)
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
    /// cache.cache_set("key".to_string(), 42);
    /// assert!(cache.cache_delete("key"));    // present — returns true
    /// assert!(!cache.cache_delete("key"));   // already gone — returns false
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
            size: self.cache_size(),
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
    pub size: usize,
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
/// c.cache_set("k".to_string(), 1);
/// assert_eq!(c.cache_get_with_expiry_status(&"k".to_string()), (Some(1), false)); // live
/// assert_eq!(c.cache_get_with_expiry_status(&"x".to_string()), (None, false));    // absent
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
/// Non-expiry stores ([`ShardedCache`], [`ShardedLruCache`]) do not implement this trait,
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
/// c.cache_set("k".to_string(), 1_i32).expect("infallible ShardedTtlCache set");
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
}

/// TTL management for time-bounded cache stores.
///
/// Implemented by [`TtlCache`], [`LruTtlCache`], [`TtlSortedCache`],
/// [`ShardedTtlCache`], [`ShardedLruTtlCache`], [`RedisCache`], and [`DiskCache`].
///
/// This trait requires the `time_stores` feature. `DiskCache` implements it only when
/// both `disk_store` and `time_stores` features are enabled.
///
/// > **Note for `DiskCache` users**: if you disable the `default` feature set and
/// > enable only `disk_store`, the `time_stores` feature will not be present and
/// > `DiskCache` will not implement this trait — `set_ttl`, `ttl`, and related methods
/// > will be unavailable. Re-enable `time_stores` (or use the `default` feature set)
/// > to restore them.
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub trait CacheTtl {
    /// Return the TTL applied to newly inserted entries.
    fn ttl(&self) -> Option<Duration>;

    /// Set the TTL for newly inserted entries, returning the previous value.
    ///
    /// # Panics
    ///
    /// Implementations that accept a TTL panic if `ttl.is_zero()` — use
    /// [`unset_ttl`](Self::unset_ttl) to disable expiry instead.
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration>;

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
    fn async_get_or_set_with<'a, F, Fut>(
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
    fn async_try_get_or_set_with<'a, F, Fut, E>(
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
/// `RedisCache`/`DiskCache`; implement it directly for a custom concurrent or
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
/// assert_eq!(store.cache_get(&"k".to_string()).expect("MyStore is infallible"), None);
/// assert_eq!(store.cache_set("k".to_string(), 7).expect("MyStore is infallible"), None);
/// assert_eq!(store.cache_get(&"k".to_string()).expect("MyStore is infallible"), Some(7));
/// assert_eq!(store.cache_remove(&"k".to_string()).expect("MyStore is infallible"), Some(7));
/// ```
/// **Direct-call syntax warning**:
///
/// Both `ConcurrentCached` and `ConcurrentCachedAsync` define identical method names for their core
/// operations (`cache_get`, `cache_set`, `cache_remove`, `cache_delete`). Sharded in-memory stores
/// also expose synchronous inherent helpers with those names, so method-call syntax on a sharded
/// store resolves to the sync helper even if only `ConcurrentCachedAsync` is imported.
///
/// To resolve this, use Universal Function Call Syntax (UFCS) when calling these methods manually:
/// ```rust,ignore
/// ::cached::ConcurrentCached::cache_get(&cache, &key)
/// ```
///
/// **Why key-lookup methods take `&K` instead of `&Q` (`Borrow<Q>`)**:
///
/// [`Cached`] uses `Borrow<Q>` for all key-lookup methods (e.g. look up a `String` key with a
/// `&str`). `ConcurrentCached` cannot follow the same pattern because its implementors include
/// external stores (`DiskCache`, `RedisCache`) that must *serialize* the key in order to perform
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
    /// non-sharded). Stores that enforce TTL server-side — `RedisCache` / `AsyncRedisCache` —
    /// cannot read the value of a TTL-expired key and so return `None` for it, exactly like
    /// `cache_remove`; see ["Note on Redis and external stores"](#note-on-redis-and-external-stores)
    /// below.
    ///
    /// Removing any present entry fires the store's `on_evict` callback (if set) and,
    /// for stores that track evictions, increments the `evictions` metric.
    ///
    /// # Note on `K: Clone`
    ///
    /// Implementations that reconstruct the stored key from the lookup key — such as
    /// `DiskCache` and `RedisCache` — require `K: Clone` to produce the stored-key half
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
    /// use cached::{ConcurrentCached, ShardedCache};
    ///
    /// let cache: ShardedCache<String, u32> = ShardedCache::builder().build().unwrap();
    /// cache.cache_set("key".to_string(), 42).expect("ShardedCache is infallible");
    ///
    /// // cache_remove_entry always returns Some when the key was present.
    /// let entry = cache.cache_remove_entry(&"key".to_string()).expect("ShardedCache is infallible");
    /// assert_eq!(entry, Some(("key".to_string(), 42)));
    ///
    /// // Returns None only when the key was never present.
    /// assert_eq!(cache.cache_remove_entry(&"missing".to_string()).expect("ShardedCache is infallible"), None);
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
    /// `DiskCache` (an `O(n)` scan of the backing tree). Those stores return `Ok(None)` rather
    /// than pay that cost implicitly; query the backend directly if you need their size.
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

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized (sharded stores use an
    /// `AtomicBool`; `RedisCache` / `DiskCache` use interior mutability), so this is callable
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
/// Both `ConcurrentCached` and `ConcurrentCachedAsync` define identical method names for their core
/// operations (`cache_get`, `cache_set`, `cache_remove`, `cache_delete`). Sharded in-memory stores
/// also expose synchronous inherent helpers with those names, so method-call syntax on a sharded
/// store resolves to the sync helper even if only `ConcurrentCachedAsync` is imported.
///
/// To resolve this, use Universal Function Call Syntax (UFCS) when calling these methods manually:
/// ```rust,ignore
/// ::cached::ConcurrentCachedAsync::cache_get(&cache, &key).await
/// ```
///
/// **Short aliases not provided**: Unlike [`ConcurrentCached`], this trait intentionally does
/// **not** expose `get`/`set`/`remove`/`delete` short aliases. Adding them would worsen the
/// method-resolution ambiguity described above: a sharded store that implements both sync and
/// async concurrent traits would then have colliding inherent helpers and two sets of alias
/// defaults, making UFCS mandatory even for the aliases.
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait ConcurrentCachedAsync<K, V> {
    type Error;
    #[doc(alias = "get")]
    fn cache_get(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    #[doc(alias = "set")]
    fn cache_set(&self, k: K, v: V) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached value, returning it if it was both present and still live.
    #[doc(alias = "remove")]
    fn cache_remove(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached entry, returning the stored key and value whenever an entry
    /// was physically deleted — including entries that were present but already expired.
    #[doc(alias = "remove_entry")]
    fn cache_remove_entry(
        &self,
        k: &K,
    ) -> impl Future<Output = Result<Option<(K, V)>, Self::Error>> + Send;

    /// Delete a cached value without returning or decoding the stored value.
    /// Returns `true` if an entry (live or expired) was physically removed from the
    /// store, `false` if the key was not present.
    #[doc(alias = "delete")]
    fn cache_delete(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync,
    {
        async move { self.cache_remove_entry(k).await.map(|r| r.is_some()) }
    }

    /// Report the number of entries currently held by the store, if the store can
    /// determine it cheaply.
    ///
    /// The concurrent-async analogue of [`Cached::cache_size`]. Returns `Ok(None)` by default;
    /// external stores (`DiskCache`, `RedisCache`, `AsyncRedisCache`) keep that default because
    /// they cannot report their entry count without an expensive or ambiguous backend query.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if determining the size fails.
    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(None)
    }

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    ///
    /// Takes `&self`: concurrent stores are internally synchronized (sharded stores use an
    /// `AtomicBool`; `RedisCache` / `DiskCache` use interior mutability), so this is callable
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

// `Cached` stores are single-owner (`&mut self`); to share one across threads,
// bring your own lock or use a macro (`#[cached]`/`#[once]` generate the lock).
// `ConcurrentCached`/`ConcurrentCachedAsync` is the contract for stores that
// manage their own synchronization (`RedisCache`, `DiskCache`).
