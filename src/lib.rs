/*!
[![Build Status](https://github.com/jaemk/cached/actions/workflows/build.yml/badge.svg)](https://github.com/jaemk/cached/actions/workflows/build.yml)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)
[![CodSpeed Badge](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/jaemk/cached?utm_source=badge)

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
| [`ExpiringCache`](https://docs.rs/cached/latest/cached/struct.ExpiringCache.html) | Value-defined | No | Per-value | N/A | Yes | Yes |

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

**Per-Value Expiry via the `Expires` Trait**

While standard timed stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`) enforce a single, global Time-To-Live (TTL) duration applied to all entries in the cache, [`ExpiringLruCache`] and [`ExpiringCache`] let each individual value determine its own expiration. This is accomplished by storing values that implement the [`Expires`] trait.

This approach is highly useful when caching payloads like OAuth tokens, HTTP responses with varying `Cache-Control` headers, or database records that contain their own absolute expiration timestamps.

When using the `#[cached]` or `#[once]` proc macros, add `expires = true` to opt into per-value expiry automatically. For `#[cached]`, this selects `ExpiringCache` (unbounded) by default or `ExpiringLruCache` when `size` is also specified. For `#[once]`, this stores a single value whose expiry is polled on each call.

> **Memory note:** `ExpiringCache` is unbounded and only removes expired entries when the same
> key is accessed again. `CachedIter::iter()` filters expired entries from the iterator but does
> not remove them from the map. For high-cardinality workloads, call `evict()` periodically or
> prefer `ExpiringLruCache` with a `size` bound.

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
let mut cache = ExpiringCache::new();
cache.cache_set("key1", Response {
    payload: "a".to_string(),
    expires_at: now + Duration::from_secs(1),
});
cache.cache_set("key2", Response {
    payload: "b".to_string(),
    expires_at: now + Duration::from_secs(3600),
});

// ExpiringLruCache — LRU-bounded, used with `#[cached(expires = true, size = N)]`
let mut lru = ExpiringLruCache::with_size(10);
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


*/

#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(clippy::manual_async_fn)]

use crate::time::Duration;
#[cfg(feature = "proc_macro")]
#[cfg_attr(docsrs, doc(cfg(feature = "proc_macro")))]
pub use macros::{cached, concurrent_cached, once, Return};
#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
use std::future::Future;
#[cfg(any(feature = "redis_smol", feature = "redis_tokio"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "redis_smol", feature = "redis_tokio"))))]
pub use stores::{AsyncRedisCache, AsyncRedisCacheBuilder};
pub use stores::{
    BuildError, CacheEvict, Expires, ExpiringCache, ExpiringCacheBuilder, ExpiringLruCache,
    ExpiringLruCacheBuilder, LruCache, LruCacheBuilder, UnboundCache, UnboundCacheBuilder,
};
#[cfg(feature = "disk_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "disk_store")))]
pub use stores::{DiskCache, DiskCacheBuildError, DiskCacheBuilder, DiskCacheError};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use stores::{
    HasEvict, LruTtlCache, LruTtlCacheBuilder, NoEvict, TtlCache, TtlCacheBuilder, TtlSortedCache,
    TtlSortedCacheBuilder, TtlSortedCacheError,
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

/// Cache operations
///
/// ```rust
/// use cached::{Cached, UnboundCache};
///
/// let mut cache: UnboundCache<String, String> = UnboundCache::new();
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
    /// # let mut cache: UnboundCache<String, String> = UnboundCache::new();
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

    /// Remove a cached value.
    ///
    /// ```rust
    /// # use cached::{Cached, UnboundCache};
    /// # let mut cache: UnboundCache<String, String> = UnboundCache::new();
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
/// let mut c = TtlCache::with_ttl(Duration::from_secs(60));
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

/// TTL management for time-bounded cache stores.
///
/// Implemented by [`TtlCache`], [`LruTtlCache`], [`TtlSortedCache`],
/// [`RedisCache`], and [`DiskCache`].
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
///     fn set_refresh_on_hit(&mut self, _refresh: bool) -> bool { false }
/// }
///
/// let store = MyStore(Mutex::new(HashMap::new()));
/// assert_eq!(store.cache_get(&"k".to_string()).unwrap(), None);
/// assert_eq!(store.cache_set("k".to_string(), 7).unwrap(), None);
/// assert_eq!(store.cache_get(&"k".to_string()).unwrap(), Some(7));
/// assert_eq!(store.cache_remove(&"k".to_string()).unwrap(), Some(7));
/// ```
pub trait ConcurrentCached<K, V> {
    type Error;

    /// Attempt to retrieve a cached value
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Insert a key, value pair and return the previous value at the key, if any,
    /// without checking TTL expiry.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error>;

    /// Remove a cached value
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error>;

    /// Delete a cached value without returning or decoding the stored value.
    ///
    /// This is useful when callers do not need the previous value, or when an
    /// IO-backed store may contain corrupted serialized data that should be
    /// removed directly.
    ///
    /// # Errors
    ///
    /// Should return `Self::Error` if the operation fails
    fn cache_delete(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_remove(k).map(|removed| removed.is_some())
    }

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    fn set_refresh_on_hit(&mut self, refresh: bool) -> bool;

    /// Return the ttl of cached values (time to eviction).
    fn ttl(&self) -> Option<Duration> {
        None
    }

    /// Set the ttl of cached values, returning the previous value.
    fn set_ttl(&mut self, _ttl: Duration) -> Option<Duration> {
        None
    }

    /// Remove the ttl for cached values, returning the previous value.
    ///
    /// For cache implementations that don't support retaining values indefinitely, this method is
    /// a no-op.
    fn unset_ttl(&mut self) -> Option<Duration> {
        None
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
pub trait ConcurrentCachedAsync<K, V> {
    type Error;
    fn cache_get(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    fn cache_set(&self, k: K, v: V) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Remove a cached value
    fn cache_remove(&self, k: &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send;

    /// Delete a cached value without returning or decoding the stored value.
    fn cache_delete(&self, k: &K) -> impl Future<Output = Result<bool, Self::Error>> + Send
    where
        Self: Sync,
        K: Sync,
    {
        async move { self.cache_remove(k).await.map(|removed| removed.is_some()) }
    }

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    fn set_refresh_on_hit(&mut self, refresh: bool) -> bool;

    /// Return the ttl of cached values (time to eviction).
    fn ttl(&self) -> Option<Duration> {
        None
    }

    /// Set the ttl of cached values, returning the previous value.
    fn set_ttl(&mut self, _ttl: Duration) -> Option<Duration> {
        None
    }

    /// Remove the ttl for cached values, returning the previous value.
    ///
    /// For cache implementations that don't support retaining values indefinitely, this method is
    /// a no-op.
    fn unset_ttl(&mut self) -> Option<Duration> {
        None
    }
}

// `Cached` stores are single-owner (`&mut self`); to share one across threads,
// bring your own lock or use a macro (`#[cached]`/`#[once]` generate the lock).
// `ConcurrentCached`/`ConcurrentCachedAsync` is the contract for stores that
// manage their own synchronization (`RedisCache`, `DiskCache`).
