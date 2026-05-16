# cached

[![Build Status](https://github.com/jaemk/cached/actions/workflows/build.yml/badge.svg)](https://github.com/jaemk/cached/actions/workflows/build.yml)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Caching structures and simplified function memoization

`cached` provides implementations of several caching structures as well as macros
for defining memoized functions.

Memoized functions defined using `#[cached]`/`#[once]`/`#[concurrent_cached]` macros are thread-safe with the backing
function-cache wrapped in a mutex/rwlock, or externally synchronized in the case of `#[concurrent_cached]`.
By default, the function-cache is **not** locked for the duration of the function's execution, so initial (on an empty cache)
concurrent calls of long-running functions with the same arguments will each execute fully and each overwrite
the memoized value as they complete. This mirrors the behavior of Python's `functools.lru_cache`. To synchronize the execution and caching
of un-cached arguments, specify `#[cached(sync_writes = true)]` / `#[once(sync_writes = true)]`; for
`#[cached]`, use `sync_writes = "by_key"` to synchronize duplicate keys through bucketed per-key locks
(not supported by `#[once]` or `#[concurrent_cached]`).

- See [`cached::stores` docs](https://docs.rs/cached/latest/cached/stores/index.html) cache stores available.
- See [`macros` docs](https://docs.rs/cached/latest/cached/macros/index.html) for more macro examples.

> **Upgrading from a pre-1.0 release?** 1.0 contains breaking changes (store
> renames, removed declarative macros, renamed macro/builder attributes, and a
> changed Redis key format). See the
> [1.0 migration guide](https://github.com/jaemk/cached/blob/master/docs/MIGRATION-1.0.md)
> for a step-by-step walkthrough, or the
> [agent-oriented guide](https://github.com/jaemk/cached/blob/master/docs/MIGRATION-1.0-AGENT.md)
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
- `time_stores`: Include time-based cache stores ([`TtlCache`](https://docs.rs/cached/latest/cached/struct.TtlCache.html), [`LruTtlCache`](https://docs.rs/cached/latest/cached/struct.LruTtlCache.html), and [`TtlSortedCache`](https://docs.rs/cached/latest/cached/struct.TtlSortedCache.html)).
  Disable this feature when targeting environments without system time support (e.g. `wasm32-unknown-unknown` without WASI or JS).

The procedural macros (`#[cached]`, `#[once]`, `#[concurrent_cached]`) offer a number of features, including async support.
See the [`macros`](https://docs.rs/cached/latest/cached/macros/index.html) module for more samples, and the
[`examples`](https://github.com/jaemk/cached/tree/master/examples) directory for runnable snippets.
Project automation targets are documented by `make help`, and `make check/help` verifies that the
help output stays in sync with supported Makefile targets.

Any custom cache that implements `cached::Cached`/`cached::CachedAsync` can be used with the `#[cached]`/`#[once]` macros in place of the built-ins.
Any custom cache that implements `cached::ConcurrentCached`/`cached::ConcurrentCachedAsync` can be used with the `#[concurrent_cached]` macro.

**Store comparison**

| Store | Eviction policy | Size limit | TTL | Refresh on hit | `on_evict` | Async |
|---|---|---|---|---|---|---|
| [`UnboundCache`](https://docs.rs/cached/latest/cached/struct.UnboundCache.html) | None (unbounded) | No | No | N/A | On explicit remove | Yes |
| [`LruCache`](https://docs.rs/cached/latest/cached/struct.LruCache.html) | LRU | Yes | No | N/A | Yes | Yes |
| [`TtlCache`](https://docs.rs/cached/latest/cached/struct.TtlCache.html) | TTL (insert time) | No | Global | Optional | Yes | Yes |
| [`LruTtlCache`](https://docs.rs/cached/latest/cached/struct.LruTtlCache.html) | LRU + TTL | Yes | Global | Optional | Yes | Yes |
| [`TtlSortedCache`](https://docs.rs/cached/latest/cached/struct.TtlSortedCache.html) | TTL (expiry-ordered) | Optional | Global | No | Yes | Yes |
| [`ExpiringLruCache`](https://docs.rs/cached/latest/cached/struct.ExpiringLruCache.html) | LRU + value-defined | Yes | Per-value | N/A | Yes | Yes |

`TtlCache`/`LruTtlCache`/`TtlSortedCache` require the `time_stores` feature.

**Behavioral guarantees**

- In-memory cache stores are not internally synchronized. Macro-defined functions wrap their
  backing stores in generated locks; users managing stores directly should add synchronization
  at the call site when sharing across threads.
- `Cached::get` (and its legacy alias `cache_get`) requires mutable access because some
  stores update recency, expiration timestamps, or metrics during reads.
- Expired values can remain allocated until a mutating operation, `evict`, or
  store-specific cleanup removes them. Methods such as `len` may include expired values
  unless a store documents otherwise.
- Bounded caches enforce capacity on insertion. Time-bounded caches enforce freshness on lookup.
- Redis and disk stores serialize values and return owned values; in-memory stores return
  references from direct store APIs and macro-generated functions clone cached return values.
- Macro-generated cache statics use `RwLock` by default. Named cache
  statics should be inspected with `.read()` or `.write()` unless `sync_lock = "mutex"` is set.
- `CachedPeek` provides non-mutating lookups that do not update recency, refresh TTLs, or record
  metrics. `CachedRead` is narrower and is only implemented where shared-lock lookups can preserve
  normal read-side semantics without recency or refresh mutation.

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
    create = "{ LruCache::with_size(100) }",
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
#[once(ttl =10, option = true, sync_writes = true)]
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
    result = true,
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
/// A `map_error` closure must be specified to convert any
/// redis cache errors into the same type of error returned
/// by your function. All `concurrent_cached` functions must return `Result`s.
#[concurrent_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    ty = "AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::new("cached_redis_prefix", Duration::from_secs(1))
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
/// A `map_error` closure must be specified to convert any
/// disk cache errors into the same type of error returned
/// by your function. All `concurrent_cached` functions must return `Result`s.
#[concurrent_cached(
    map_error = r##"|e| ExampleError::DiskError(format!("{:?}", e))"##,
    disk = true
)]
fn cached_sleep_secs(secs: u64) -> Result<String, ExampleError> {
    std::thread::sleep(cached::time::Duration::from_secs(secs));
    Ok(secs.to_string())
}
```


Functions defined via macros will have their results cached using the
function's arguments as a key, or a `convert` expression specified on the macro.

When a macro-defined function is called, the function's cache is first checked for an already
computed (and still valid) value before evaluating the function body.

Due to the requirements of storing arguments and return values in a global cache:

- Function return types:
  - For in-memory stores (`#[cached]` / `#[once]`), must be owned and implement `Clone`
  - For I/O-backed stores used by `#[concurrent_cached]` (Redis and disk), must be owned, implement
    `Clone` (the generated code clones the successful value), and additionally implement
    `serde::Serialize + serde::DeserializeOwned` (the store serializes it)
- Function arguments:
  - For in-memory stores (`#[cached]` / `#[once]`), must either be owned and implement `Hash + Eq + Clone`,
    or a `convert` expression must be specified on the macro to produce a key of a `Hash + Eq + Clone` type.
  - For I/O-backed stores used by `#[concurrent_cached]` (Redis and disk), must either be owned and
    implement `Display`, or a `convert` expression must be used to produce a key of a `Display` type.
- Arguments and return values will be `cloned` in the process of insertion and retrieval. For Redis and
  disk stores, keys are additionally formatted into `String`s and values are de/serialized.
- Macro-defined functions should not be used to produce side-effectual results!
- Macro-defined functions cannot live directly under `impl` blocks since macros expand to a
  static initialization and one or more function definitions.
- Macro-defined functions cannot accept `Self` types as a parameter.



License: MIT
