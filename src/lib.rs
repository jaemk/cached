/*!
[![Build Status](https://github.com/jaemk/cached/actions/workflows/build.yml/badge.svg)](https://github.com/jaemk/cached/actions/workflows/build.yml)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Memoize Rust functions with one attribute. In-memory, sharded, Redis, and disk caches.

```rust
use cached::macros::cached;
# #[derive(Clone)] struct User;
# #[derive(Debug)] struct ApiError;
# mod api { pub fn fetch_user(_id: u64) -> Result<super::User, super::ApiError> { Ok(super::User) } }

/// Cache up to 1,000 users for 60 seconds each.
/// `Err` results are not cached, so a failed request is retried on the next call.
#[cached(max_size = 1_000, ttl_secs = 60)]
fn get_user(id: u64) -> Result<User, ApiError> {
    api::fetch_user(id) // slow network call
}

# fn main() -> Result<(), ApiError> {
let user = get_user(1)?; // calls the API
let user = get_user(1)?; // returned from the cache
# Ok(())
# }
```

The same attribute works on `async fn` (with the `async` feature), and `#[concurrent_cached]`
swaps in a sharded store, Redis, or an on-disk cache without changing the function.

```toml
[dependencies]
cached = "4"
```

## Contents

- [Memoizing functions](#memoizing-functions)
  - [The `#[cached]` macro](#the-cached-macro)
  - [The `#[once]` macro](#the-once-macro)
  - [The `#[concurrent_cached]` macro](#the-concurrent_cached-macro)
  - [Common attributes](#common-attributes)
  - [`Result` and `Option` returns](#result-and-option-returns)
  - [Async functions](#async-functions)
- [Redis and disk caches](#redis-and-disk-caches)
- [Using cache stores directly](#using-cache-stores-directly)
- [Choosing a store](#choosing-a-store)
- [Per-value expiry](#per-value-expiry)
- [Feature flags](#feature-flags)
- [Requirements](#requirements)
- [Further reading](#further-reading)
- [Upgrading](#upgrading)

## Memoizing functions

The macros cache a function's return value keyed by its arguments. On each call the cache is
checked first, and the function body only runs on a miss. Each macro also generates
`{fn}_no_cache` (calls the body directly) and `{fn}_prime_cache` (recomputes and stores a fresh
value).

### The `#[cached]` macro

The general-purpose macro. Unbounded by default; add `max_size` for LRU eviction and `ttl_secs`
for expiry.

```rust
use cached::macros::cached;

#[cached]
fn fib(n: u64) -> u64 {
    if n == 0 || n == 1 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

assert_eq!(fib(80), 23_416_728_348_467_685);
```

### The `#[once]` macro

Caches a single value regardless of arguments, for global config or tokens that are refreshed
periodically.

```rust
use cached::macros::once;
# #[derive(Clone)] struct Config;
# fn load_config() -> Config { Config }

#[once(ttl_secs = 300)]
fn config() -> Config {
    load_config() // re-run at most once every 5 minutes
}
```

### The `#[concurrent_cached]` macro

Uses a sharded store with per-shard locks instead of one lock around the whole cache, so
concurrent callers on different keys do not contend. It also backs the [Redis and disk
caches](#redis-and-disk-caches).

```rust
use cached::macros::concurrent_cached;
# #[derive(Clone)] struct Row;
# fn query(_id: u64) -> Row { Row }

#[concurrent_cached(max_size = 10_000)]
fn lookup(id: u64) -> Row {
    query(id)
}
```

### Common attributes

| Attribute | Effect |
|---|---|
| `max_size = N` | LRU eviction past `N` entries |
| `ttl_secs = N` / `ttl_millis = N` | Expire entries after a fixed duration |
| `refresh = true` | Reset an entry's TTL on every hit |
| `expires = true` | Each value decides its own expiry (see [Per-value expiry](#per-value-expiry)) |
| `key = "Type", convert = { expr }` | Build the cache key from the arguments yourself |
| `#[cached_skip]` on a parameter | Leave that argument out of the cache key |
| `sync_writes = "by_key"` | Concurrent first calls for the same key run the body once |
| `result_fallback = true` | On `Err`, return the last cached `Ok` value |
| `name = "NAME"` | Name of the generated cache static (defaults to the function name in caps) |
| `in_impl = true` | Cache a method inside an `impl` block |

See the [`macros`](https://docs.rs/cached/latest/cached/macros/index.html) module for the full
attribute reference, including synchronization options and argument and return-type
requirements.

### `Result` and `Option` returns

A function returning `Result<T, E>` caches only `Ok` values, and one returning `Option<T>` caches
only `Some`. Opt back in with `cache_err = true` or `cache_none = true`. Detection matches the
literal `Result` / `Option` type names, so a type alias is cached as a plain value.

### Async functions

Enable the `async` feature and annotate an `async fn`. The feature is runtime-agnostic and does
not depend on tokio.

```rust
# #[cfg(feature = "async")]
# mod example {
use cached::macros::cached;
# #[derive(Clone)] pub struct Body;
# async fn fetch(_url: &str) -> Body { Body }

#[cached(max_size = 100, ttl_secs = 30)]
async fn get(url: String) -> Body {
    fetch(&url).await
}
# }
```

## Redis and disk caches

`#[concurrent_cached]` can store results in Redis (`redis_*` features) or on disk with
[`redb`](https://docs.rs/redb) (`redb_store` feature). Values are serialized, so they must
implement `serde::Serialize` and `serde::DeserializeOwned`, and the function must return
`Result` so store errors have somewhere to go.

```rust,no_run
# #[cfg(all(feature = "redis_tokio", feature = "proc_macro"))]
# mod example {
use cached::macros::concurrent_cached;
# #[derive(Debug)] pub struct MyError(String);
# pub async fn fetch_report(_id: u64) -> Result<String, MyError> { Ok(String::new()) }

/// Reads the connection string from `CACHED_REDIS_CONNECTION_STRING`.
#[concurrent_cached(redis = true, ttl_secs = 300, map_error = |e| MyError(format!("{e:?}")))]
async fn report(id: u64) -> Result<String, MyError> {
    fetch_report(id).await
}
# }
```

```rust,no_run
# #[cfg(all(feature = "redb_store", feature = "proc_macro"))]
# mod example {
use cached::macros::concurrent_cached;
# #[derive(Debug)] pub struct MyError(String);
# pub fn render(_n: u64) -> String { String::new() }

/// Stored under the system cache directory unless `disk_dir` is set.
#[concurrent_cached(disk = true, map_error = |e| MyError(format!("{e:?}")))]
fn thumbnail(n: u64) -> Result<String, MyError> {
    Ok(render(n))
}
# }
```

See [`examples/redis-async-tokio.rs`](https://github.com/jaemk/cached/blob/master/examples/redis-async-tokio.rs)
and [`examples/disk.rs`](https://github.com/jaemk/cached/blob/master/examples/disk.rs) for
complete programs, including explicit store construction with `ty` and `create`.

### Compressing stored values

Enable `compression_zstd` alongside your Redis or redb feature, then pass
`Compression::Zstd { level: 3 }` to the store builder's `.compression(...)` method.
Building this feature requires a C toolchain. The builder attempts compression starting at
256 serialized bytes and keeps a frame only when it saves space. Set the write threshold with
`.min_compress_size(bytes)` and the frame limit with `.max_decompressed_size(bytes)` (default
64 MiB). Both sizes count the serialized envelope. Values above the limit are written raw.

```rust,no_run
# #[cfg(all(feature = "redis_store", feature = "compression_zstd"))]
# fn example() -> Result<(), Box<dyn std::error::Error>> {
use cached::{Compression, RedisCache};

let reports = RedisCache::<u64, String>::builder("reports")
    .compression(Compression::Zstd { level: 3 })
    .build()?;
# let _ = reports;
# Ok(())
# }
```

With the feature compiled in, stores read both raw values and zstd frames even when
compressed writes are disabled. On shared Redis caches, deploy the feature to every reader
before enabling compressed writes. A build without zstd treats frames as undecodable and
deletes them during default self-healing. To remove the feature, first disable compressed
writes across the cache and keep zstd support until existing frames expire or are removed.
This also applies when reopening a redb cache.

Give every reader a limit large enough for its writers. A zstd reader whose limit is too low
returns a deserialization error and preserves the entry. Async Redis performs codec work on
the calling thread, while async redb runs compression and decompression on its blocking pool. See the runnable
[`io_compression` example](https://github.com/jaemk/cached/blob/master/examples/io_compression.rs).

## Using cache stores directly

Every store the macros use is also a plain type. Import the store and the
[`prelude`](https://docs.rs/cached/latest/cached/prelude/index.html), which brings the cache
traits into scope:

```rust
use cached::LruCache;
use cached::prelude::*;

let mut cache: LruCache<String, u32> = LruCache::new(100);
cache.set("a".to_string(), 1);
assert_eq!(cache.get("a"), Some(&1));
assert_eq!(cache.len(), 1);
```

Single-owner stores like `LruCache` take `&mut self` and need your own lock to share across
threads. The `Sharded*` stores lock internally and can be shared directly:

```rust
use cached::ShardedLruCache;

let cache: ShardedLruCache<String, u32> = ShardedLruCache::new(1_000);
cache.set("a".to_string(), 1);
assert_eq!(cache.get("a"), Some(1));
```

Builders configure TTLs, eviction callbacks, and the rest:

```rust
# #[cfg(feature = "time_stores")]
# {
use cached::LruTtlCache;
use cached::time::Duration;

let cache: LruTtlCache<u64, String> = LruTtlCache::builder()
    .max_size(1_000)
    .ttl(Duration::from_secs(60))
    .on_evict(|key, _value| println!("evicted {key}"))
    .build()
    .unwrap();
# }
```

## Choosing a store

| Need | Single-owner | Concurrent |
|---|---|---|
| Unbounded | [`UnboundCache`](https://docs.rs/cached/latest/cached/struct.UnboundCache.html) | [`ShardedUnboundCache`](https://docs.rs/cached/latest/cached/struct.ShardedUnboundCache.html) |
| Size limit (LRU) | [`LruCache`](https://docs.rs/cached/latest/cached/struct.LruCache.html) | [`ShardedLruCache`](https://docs.rs/cached/latest/cached/struct.ShardedLruCache.html) |
| Fixed TTL | [`TtlCache`](https://docs.rs/cached/latest/cached/struct.TtlCache.html) | [`ShardedTtlCache`](https://docs.rs/cached/latest/cached/struct.ShardedTtlCache.html) |
| Size limit + fixed TTL | [`LruTtlCache`](https://docs.rs/cached/latest/cached/struct.LruTtlCache.html) | [`ShardedLruTtlCache`](https://docs.rs/cached/latest/cached/struct.ShardedLruTtlCache.html) |
| Per-value expiry | [`ExpiringCache`](https://docs.rs/cached/latest/cached/struct.ExpiringCache.html) | [`ShardedExpiringCache`](https://docs.rs/cached/latest/cached/struct.ShardedExpiringCache.html) |
| Size limit + per-value expiry | [`ExpiringLruCache`](https://docs.rs/cached/latest/cached/struct.ExpiringLruCache.html) | [`ShardedExpiringLruCache`](https://docs.rs/cached/latest/cached/struct.ShardedExpiringLruCache.html) |
| Shared across processes | | [`RedisCache`](https://docs.rs/cached/latest/cached/struct.RedisCache.html), [`AsyncRedisCache`](https://docs.rs/cached/latest/cached/struct.AsyncRedisCache.html) |
| Persisted to disk | | [`RedbCache`](https://docs.rs/cached/latest/cached/struct.RedbCache.html) |

[`TtlSortedCache`](https://docs.rs/cached/latest/cached/struct.TtlSortedCache.html) is a TTL
cache ordered by expiry with an optional size limit. Custom stores work with the macros by
implementing [`Cached`](https://docs.rs/cached/latest/cached/trait.Cached.html) (for
`#[cached]` / `#[once]`) or
[`ConcurrentCached`](https://docs.rs/cached/latest/cached/trait.ConcurrentCached.html) (for
`#[concurrent_cached]`).

The [`stores`](https://docs.rs/cached/latest/cached/stores/index.html) module documents each
store's eviction, locking, and `on_evict` behavior in detail.

## Per-value expiry

When each value knows its own lifetime (an OAuth token, an HTTP response with `Cache-Control`),
implement [`Expires`](https://docs.rs/cached/latest/cached/trait.Expires.html) and set
`expires = true`:

```rust
# #[cfg(feature = "proc_macro")]
# #[allow(dead_code)]
# mod example {
use cached::macros::cached;
use cached::Expires;
use cached::time::{Duration, Instant};

#[derive(Clone)]
struct Token {
    value: String,
    expires_at: Instant,
}

impl Expires for Token {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

#[cached(expires = true)]
fn token(scope: String) -> Token {
    Token { value: format!("token-{scope}"), expires_at: Instant::now() + Duration::from_secs(3600) }
}
# }
```

See [`examples/expires_per_key.rs`](https://github.com/jaemk/cached/blob/master/examples/expires_per_key.rs).

## Feature flags

| Feature | Default | Enables |
|---|---|---|
| `proc_macro` | yes | `#[cached]`, `#[once]`, `#[concurrent_cached]` |
| `time_stores` | yes | TTL stores and the `ttl_*` macro attributes (disable for `wasm32-unknown-unknown` without time support) |
| `ahash` | yes | `ahash` as the default hasher |
| `async` | | Async functions and async store traits (runtime-agnostic) |
| `async_core` | | Async traits only, without the `async-lock` dependency |
| `redis_store` | | Synchronous Redis store |
| `redis_tokio` / `redis_smol` | | Async Redis on tokio or smol |
| `redis_tokio_native_tls` / `redis_tokio_rustls` | | `redis_tokio` with TLS |
| `redis_smol_native_tls` / `redis_smol_rustls` | | `redis_smol` with TLS |
| `redis_connection_manager` | | Auto-reconnecting Redis connections (pair with a runtime feature) |
| `redis_async_cache` | | Redis client-side caching over RESP3 (pair with a runtime feature) |
| `redb_store` | | Disk store backed by `redb` |
| `compression_zstd` | | Opt-in zstd value compression for Redis and redb |

## Requirements

- Rust 1.95 or later.
- Arguments must be `Hash + Eq + Clone`, or supply `key` and `convert` to build a key that is.
  An argument marked `#[cached_skip]` is left out of the key and has no such requirement.
  Floats are the common case that needs `convert`, e.g. `convert = { format!("{x:.6}") }`.
- Return values must be `Clone`; they are cloned on every hit. Return `Arc<T>` when `T` is
  expensive to clone.
- Cached functions should not have side effects, since a hit skips the body.
- Generic functions need `key` and `convert` to produce a concrete key type.

## Further reading

- [`macros`](https://docs.rs/cached/latest/cached/macros/index.html): every attribute,
  synchronization options, and more examples
- [`stores`](https://docs.rs/cached/latest/cached/stores/index.html): store comparison, method
  naming, sharding, and behavioral guarantees
- [`claim`](https://docs.rs/cached/latest/cached/claim/index.html): single-flight guard for
  background refreshes
- [`examples/`](https://github.com/jaemk/cached/tree/master/examples): runnable programs,
  including [stale-while-revalidate](https://github.com/jaemk/cached/blob/master/examples/stale_while_revalidate.rs)
  and [refresh-before-expiry](https://github.com/jaemk/cached/blob/master/examples/refresh_before_expiry.rs)

## Upgrading

Migration guides for each release are in
[`docs/migrations`](https://github.com/jaemk/cached/tree/master/docs/migrations). The major
ones:

- [2.x to 3.0](https://github.com/jaemk/cached/blob/master/docs/migrations/2.0-to-3.0-human.md)
- [1.x to 2.0](https://github.com/jaemk/cached/blob/master/docs/migrations/1.1-to-2.0-human.md)
- [0.x to 1.0](https://github.com/jaemk/cached/blob/master/docs/migrations/0.x-to-1.0-human.md)
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
// Canonical `AsyncRedisCache` availability gate: a redis async *runtime* must be enabled. The
// six runtime features below each imply `redis_store` + `async` (see Cargo.toml), so the store
// and its async plumbing are present. The capability-only features (`redis_async_cache`,
// `redis_connection_manager`) are deliberately NOT in this list: they add a redis capability but
// no runtime, so on their own they cannot compile `AsyncRedisCache` (redis-rs needs a `*-comp`).
// This exact expression is repeated at the `async_redis` module gate (src/stores/redis/mod.rs) and
// the stores re-export (src/stores/mod.rs); keep the three in sync.
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
#[doc(inline)]
pub use stores::{AsyncRedisCache, AsyncRedisCacheBuilder};

// A capability-only feature (`redis_async_cache` / `redis_connection_manager`) enabled without a
// runtime feature pulls `redis/aio` with no `*-comp`, which makes the `redis` crate itself fail
// to build ("tokio-comp or smol-comp features required for aio feature"). That error originates
// in the dependency, which compiles before `cached`, so a `compile_error!` here could not
// preempt it. The requirement is documented on each capability feature in Cargo.toml: pair it
// with a `redis_tokio*` or `redis_smol*` runtime feature.
//
// `doc(inline)` documents the stores at the crate root (`cached/struct.LruCache.html`), where
// users import them and where the README's "Choosing a store" links point. Without it, `stores`
// being a public module makes rustdoc list these as plain re-exports with no root-level pages.
#[doc(inline)]
pub use stores::{
    BuildError, CacheEvict, CacheValue, ConcurrentCacheEvict, DefaultHashBuilder,
    DefaultShardHasher, Expires, ExpiringCache, ExpiringCacheBuilder, ExpiringLruCache,
    ExpiringLruCacheBuilder, IntoValues, LruCache, LruCacheBuilder, SetMaxSizeError, SetTtlError,
    ShardHasher, ShardedExpiringCache, ShardedExpiringCacheBuilder, ShardedExpiringLruCache,
    ShardedExpiringLruCacheBuilder, ShardedLruCache, ShardedLruCacheBuilder, ShardedUnboundCache,
    ShardedUnboundCacheBuilder, UnboundCache, UnboundCacheBuilder,
};
#[cfg(feature = "redis_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redis_store")))]
#[doc(inline)]
pub use stores::{
    ConnectionString, RedisCache, RedisCacheBuildError, RedisCacheBuilder, RedisCacheError,
};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
#[doc(inline)]
pub use stores::{HasEvict, NoEvict};
#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
#[doc(inline)]
pub use stores::{
    LruTtlCache, LruTtlCacheBuilder, ShardedLruTtlCache, ShardedLruTtlCacheBuilder,
    ShardedTtlCache, ShardedTtlCacheBuilder, TtlCache, TtlCacheBuilder, TtlSortedCache,
    TtlSortedCacheBuilder, TtlSortedSetBuilder,
};
#[cfg(feature = "redb_store")]
#[cfg_attr(docsrs, doc(cfg(feature = "redb_store")))]
#[doc(inline)]
pub use stores::{
    RedbCache, RedbCacheBuildError, RedbCacheBuilder, RedbCacheError, RedbCacheStats,
};

#[cfg(all(
    feature = "compression_zstd",
    any(feature = "redb_store", feature = "redis_store")
))]
#[doc(inline)]
pub use stores::Compression;

pub mod claim;
mod lru_list;
#[cfg(feature = "proc_macro")]
#[cfg_attr(docsrs, doc(cfg(feature = "proc_macro")))]
pub mod macros;
/// Alias of [`macros`] under the module path many proc-macro crates use,
/// so `use cached::proc_macro::cached;` resolves.
#[cfg(feature = "proc_macro")]
#[cfg_attr(docsrs, doc(cfg(feature = "proc_macro")))]
pub use macros as proc_macro;
pub mod stores;
/// Re-export of the [`web_time`](https://docs.rs/web_time) crate,
/// which provides time types compatible with both native and WebAssembly targets.
pub use web_time as time;

#[cfg(feature = "async")]
#[doc(hidden)]
pub mod async_sync {
    pub use async_lock::Mutex;
    pub use async_lock::OnceCell;
    pub use async_lock::RwLock;
}

#[doc(hidden)]
pub mod sync_sync {
    pub use parking_lot::Mutex;
    pub use parking_lot::RwLock;
}

/// Guard macro emitted by `#[cached]`/`#[once]`/`#[concurrent_cached]` for every async
/// function. When the `async` feature is enabled this expands to nothing. When `async` is
/// disabled it expands to a `compile_error!` pointing at the missing feature, providing a
/// clearer diagnostic than the obscure "cannot find `async_sync` in `cached`" error that
/// would otherwise appear.
///
/// This is an internal implementation detail; do not call it from user code.
#[cfg(feature = "async")]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_async_feature {
    () => {};
}

#[cfg(not(feature = "async"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_async_feature {
    () => {
        compile_error!(
            "#[cached]/#[once]/#[concurrent_cached] on an async function requires \
             the `async` feature of the `cached` crate"
        );
    };
}

/// Guard macro emitted by `#[cached]`/`#[concurrent_cached]` whenever the selected store is
/// one of the time-based TTL stores (`TtlCache`/`LruTtlCache`/`TtlSortedCache`), i.e. when a
/// `ttl`/`ttl_secs`/`ttl_millis` attribute is used. When the `time_stores` feature
/// is enabled this expands to nothing; when it is disabled it expands to a `compile_error!`
/// pointing at the missing feature, a clearer diagnostic than the raw "cannot find `TtlCache`
/// in `cached`" error that would otherwise appear.
///
/// This is an internal implementation detail; do not call it from user code.
#[cfg(feature = "time_stores")]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_time_stores_feature {
    () => {};
}

#[cfg(not(feature = "time_stores"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_time_stores_feature {
    () => {
        compile_error!(
            "#[cached]/#[concurrent_cached] with a `ttl`/`ttl_secs`/`ttl_millis` \
             attribute requires the `time_stores` feature of the `cached` crate"
        );
    };
}

/// Guard macro emitted by `#[concurrent_cached(disk = true)]`. When the `redb_store` feature is
/// enabled this expands to nothing; when it is disabled it expands to a `compile_error!` naming
/// `redb_store`, a clearer diagnostic than the raw "cannot find `RedbCache` in `cached`"
/// (E0433/E0425) pair that would otherwise appear from the generated code.
///
/// Invoked as `cached::__require_redb_store_feature!{}`.
///
/// This is an internal implementation detail; do not call it from user code.
#[cfg(feature = "redb_store")]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_redb_store_feature {
    () => {};
}

#[cfg(not(feature = "redb_store"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_redb_store_feature {
    () => {
        compile_error!(
            "#[concurrent_cached(disk = true)] requires the `redb_store` feature \
             of the `cached` crate"
        );
    };
}

/// Guard macro emitted by `#[concurrent_cached(redis = true)]`. Two call shapes:
///
/// - `cached::__require_redis_feature!{}` — the synchronous path, which builds a `RedisCache`
///   and is satisfied by the `redis_store` feature (every redis runtime feature implies it).
/// - `cached::__require_redis_feature!{async}` — the async path, which builds an
///   `AsyncRedisCache` and needs a redis *runtime* feature (`redis_tokio` / `redis_smol` or one
///   of their TLS variants). `redis_store` alone is not enough there.
///
/// Each shape expands to nothing when its requirement is met and to a `compile_error!` naming
/// the needed feature otherwise. The macro must be emitted *before* the
/// [`__require_async_feature`] guard on an async redis function, so that a build with no redis
/// feature reports the missing redis feature rather than the missing `async` feature: the redis
/// runtime features imply `async`, so enabling `async` alone would not fix the build.
///
/// This is an internal implementation detail; do not call it from user code.
#[cfg(all(
    feature = "redis_store",
    any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
    )
))]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_redis_feature {
    () => {};
    (async) => {};
}

#[cfg(all(
    feature = "redis_store",
    not(any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
    ))
))]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_redis_feature {
    () => {};
    (async) => {
        compile_error!(
            "#[concurrent_cached(redis = true)] on an async function requires a redis \
             runtime feature of the `cached` crate: `redis_tokio`, `redis_tokio_native_tls`, \
             `redis_tokio_rustls`, `redis_smol`, `redis_smol_native_tls`, or \
             `redis_smol_rustls` (each implies `redis_store` and `async`)"
        );
    };
}

#[cfg(not(feature = "redis_store"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __require_redis_feature {
    () => {
        compile_error!(
            "#[concurrent_cached(redis = true)] requires a redis feature of the `cached` \
             crate: `redis_store` for a synchronous function, or one of the redis runtime \
             features `redis_tokio`, `redis_tokio_native_tls`, `redis_tokio_rustls`, \
             `redis_smol`, `redis_smol_native_tls`, `redis_smol_rustls` for an async function"
        );
    };
    (async) => {
        compile_error!(
            "#[concurrent_cached(redis = true)] on an async function requires a redis \
             runtime feature of the `cached` crate: `redis_tokio`, `redis_tokio_native_tls`, \
             `redis_tokio_rustls`, `redis_smol`, `redis_smol_native_tls`, or \
             `redis_smol_rustls` (each implies `redis_store` and `async`)"
        );
    };
}

/// Internal support types used by macro-generated code.
///
/// Doc-hidden and **not** a stable public API: the contents may change in any release. It lives
/// in its own module rather than at the crate root so that its items do not participate in
/// name resolution against `cached::*` — a `#[doc(hidden)] pub` item at the root is still
/// offered by rustc as a "did you mean" suggestion for an unrelated mistyped import (e.g.
/// `use cached::TimedCache;`, a store name removed in 1.0, used to be answered with "did you
/// mean `KeyedCache`?").
///
/// `KeyedCache` therefore resolves here:
///
/// ```rust
/// use cached::__private::KeyedCache;
/// let _ = std::marker::PhantomData::<KeyedCache<(), ()>>;
/// ```
///
/// and **not** at the crate root:
///
/// ```compile_fail
/// use cached::KeyedCache;
/// let _ = std::marker::PhantomData::<KeyedCache<(), ()>>;
/// ```
///
/// nor through a glob of the crate root:
///
/// ```compile_fail
/// use cached::*;
/// let _ = std::marker::PhantomData::<KeyedCache<(), ()>>;
/// ```
#[doc(hidden)]
pub mod __private {
    /// Support type backing `sync_writes = "by_key"` named statics generated by `#[cached]`.
    ///
    /// Doc-hidden and generic over the cache lock `C` (e.g. `RwLock<Cache>`) and the per-key
    /// bucket lock `B` (e.g. `RwLock<()>`). It [`Deref`](std::ops::Deref)s to the inner cache
    /// lock, so a named `by_key` static is inspected with exactly the same `.read()`/`.write()`
    /// (or `.lock()`, sync or async) calls as any other generated static; the bucket vector and
    /// the per-static randomly-seeded hasher are private. The only stable surface is that
    /// `Deref` to `C`.
    ///
    /// Reachable only as `cached::__private::KeyedCache`. It was previously exported at the
    /// crate root, where rustc offered it as the nearest-match suggestion for unrelated
    /// mistyped imports such as `use cached::TimedCache;` (a name removed in 1.0).
    #[doc(hidden)]
    pub struct KeyedCache<C, B> {
        cache: C,
        buckets: std::boxed::Box<[std::sync::Arc<B>]>,
        hasher: std::collections::hash_map::RandomState,
    }

    impl<C, B> KeyedCache<C, B> {
        /// Construct from the cache lock and the pre-built bucket locks. Called by generated
        /// code.
        #[doc(hidden)]
        pub fn new(cache: C, buckets: std::vec::Vec<std::sync::Arc<B>>) -> Self {
            Self {
                cache,
                buckets: buckets.into_boxed_slice(),
                hasher: std::collections::hash_map::RandomState::new(),
            }
        }

        /// Select the per-key bucket lock for `key`. The hasher is seeded once per static from a
        /// process-random source
        /// ([`RandomState::new`](std::collections::hash_map::RandomState::new)), so an attacker
        /// who knows the key space cannot force every key into one bucket (a fixed-seed hasher
        /// could be collapsed that way). Called by generated code.
        #[doc(hidden)]
        pub fn bucket_for<Q: std::hash::Hash + ?Sized>(&self, key: &Q) -> &std::sync::Arc<B> {
            use std::hash::BuildHasher;
            let idx = (self.hasher.hash_one(key) as usize) % self.buckets.len();
            &self.buckets[idx]
        }
    }

    impl<C, B> std::ops::Deref for KeyedCache<C, B> {
        type Target = C;
        fn deref(&self) -> &C {
            &self.cache
        }
    }
}

/// Convenience re-exports of the commonly-needed cache traits.
///
/// Glob-import this module to bring the public cache traits into scope in one line —
/// `use cached::prelude::*;` — instead of importing each trait individually. This is
/// especially handy for the `ConcurrentCached` family, whose methods (`cache_get`,
/// `cache_set`, …) are trait methods and require the trait to be in scope to call.
///
/// Traits and the `CacheMetrics` snapshot struct are re-exported here; concrete store types
/// are intentionally omitted to avoid name clashes. Import those directly
/// (e.g. `use cached::ShardedUnboundCache;`).
///
/// This glob also brings in the four capacity/clear capability traits ([`CacheSetMaxSize`],
/// [`CacheClearWithOnEvict`], [`ConcurrentCacheSetMaxSize`], [`ConcurrentCacheClearWithOnEvict`]).
/// On a custom store type that defines its own same-named extension trait but has no inherent
/// method of that name, importing both can produce `E0034` ("multiple applicable items in
/// scope") at the call site. This never happens for this crate's own stores: every one of them
/// has the matching inherent method, and inherent methods win resolution before any trait is
/// even considered.
///
/// [`claim::ClaimRegistry`] and [`claim::Claim`] are re-exported here as well. They are not at the crate root, where their generic names would
/// be offered as nearest-match suggestions for unrelated mistyped imports (the reason
/// `KeyedCache` moved off the root); a glob of this module is opt-in, and a `Claim` the user
/// defines or imports directly shadows the glob rather than colliding with it.
///
/// Both names therefore resolve here (and at `cached::claim::`):
///
/// ```rust
/// use cached::prelude::ClaimRegistry;
/// let registry: ClaimRegistry<u32> = ClaimRegistry::new();
/// assert!(registry.is_empty());
/// ```
///
/// and **not** at the crate root:
///
/// ```compile_fail
/// use cached::ClaimRegistry;
/// let registry: ClaimRegistry<u32> = ClaimRegistry::new();
/// assert!(registry.is_empty());
/// ```
///
/// nor through a glob of the crate root:
///
/// ```compile_fail
/// use cached::*;
/// let registry: ClaimRegistry<u32> = ClaimRegistry::new();
/// assert!(registry.is_empty());
/// ```
pub mod prelude {
    pub use crate::claim::{Claim, ClaimRegistry};
    pub use crate::{
        CacheClearWithOnEvict, CacheEvict, CacheExpiry, CacheMetrics, CacheSetMaxSize, Cached,
        CachedExt, CachedIter, CachedPeek, CachedRead, CloneCached, ConcurrentCacheBase,
        ConcurrentCacheClearWithOnEvict, ConcurrentCacheEvict, ConcurrentCacheExpiry,
        ConcurrentCachePeek, ConcurrentCacheRefreshOnHit, ConcurrentCacheSetMaxSize,
        ConcurrentCacheTtl, ConcurrentCached, ConcurrentCachedExt, ConcurrentCloneCached, Expires,
        IntoValues, SerializeCached,
    };

    // Unconditional, like `ConcurrentCacheTtl` above: the traits themselves are
    // not feature-gated (only the built-in stores implementing them require
    // `time_stores`), so custom-store authors get them from the prelude in any
    // feature configuration.
    pub use crate::{CacheRefreshOnHit, CacheTtl};

    #[cfg(feature = "async_core")]
    #[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
    pub use crate::{
        CachedGetOrSetAsync, ConcurrentCachePeekAsync, ConcurrentCachedAsync,
        ConcurrentCachedAsyncExt, SerializeCachedAsync,
    };
}

mod traits;
pub use traits::*;

/// Autoref-specialization shim used by the generated `#[concurrent_cached]` set site
/// to prefer the borrowed setter (`SerializeCached::cache_set_ref`, no value clone) when
/// the concrete store implements `SerializeCached`, falling back to owned `cache_set`
/// (cloning the value) otherwise. Internal implementation detail of the proc-macro; no
/// stability guarantee.
#[doc(hidden)]
pub mod __set_dispatch {
    use super::{ConcurrentCacheBase, ConcurrentCached, SerializeCached};
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
        ) -> Result<(), <S as ConcurrentCacheBase>::Error> {
            SerializeCached::cache_set_ref(self.store, &key, value)
        }
    }

    // FALLBACK arm: trait method, reached only when the inherent one is pruned. Requires V: Clone.
    pub trait SetDispatchFallback<K, V> {
        type Error;
        fn cache_set_dispatch(&self, key: K, value: &V) -> Result<(), Self::Error>;
    }

    impl<S, K, V> SetDispatchFallback<K, V> for SetDispatch<'_, S, K, V>
    where
        S: ConcurrentCached<K, V>,
        V: Clone,
    {
        type Error = <S as ConcurrentCacheBase>::Error;
        #[inline]
        fn cache_set_dispatch(&self, key: K, value: &V) -> Result<(), Self::Error> {
            // The set site discards the previous value; drop it here so the
            // borrowed and owned arms share a `Result<(), _>` shape.
            ConcurrentCached::cache_set(self.store, key, value.clone()).map(|_| ())
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
    use super::{ConcurrentCacheBase, ConcurrentCachedAsync, SerializeCachedAsync};
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
        ) -> impl Future<Output = Result<(), <S as ConcurrentCacheBase>::Error>> + Send {
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
        ) -> impl Future<Output = Result<(), Self::Error>> + Send;
    }

    impl<S, K, V> SetDispatchAsyncFallback<K, V> for SetDispatchAsync<'_, S, K, V>
    where
        S: ConcurrentCachedAsync<K, V> + Sync,
        K: Send,
        V: Clone + Send,
    {
        type Error = <S as ConcurrentCacheBase>::Error;
        #[inline]
        fn cache_set_dispatch(
            &self,
            key: K,
            value: &V,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send {
            let v = value.clone();
            let store = self.store;
            // The set site discards the previous value; drop it here so both arms
            // share a `Result<(), _>` shape.
            async move {
                ConcurrentCachedAsync::async_cache_set(store, key, v)
                    .await
                    .map(|_| ())
            }
        }
    }
}

// `Cached` stores are single-owner (`&mut self`); to share one across threads,
// bring your own lock or use a macro (`#[cached]`/`#[once]` generate the lock).
// `ConcurrentCached`/`ConcurrentCachedAsync` is the contract for stores that
// manage their own synchronization (`RedisCache`, `RedbCache`).

#[cfg(test)]
mod expiry_read_doc_contract {
    //! Pins the claims the [`CacheExpiry`] and [`ConcurrentCacheExpiry`] docs make about return
    //! shapes an implementation may produce. The trait doctests cover the built-in stores; these
    //! cover the shapes only a hand-written implementation reaches, which is what the doc wording
    //! has to hold for.

    use super::{CacheExpiry, ConcurrentCacheExpiry, Expires};
    use crate::time::{Duration, Instant};
    use std::borrow::Borrow;

    const TTL: Duration = Duration::from_secs(60);
    const THRESHOLD: Duration = Duration::from_secs(10);

    /// Single-owner store holding one live entry under the key `"k"` with no deadline, i.e. the
    /// `(Some(v), None)` shape every `Expires` value type that does not override `expires_at`
    /// produces.
    struct NoDeadline;

    impl CacheExpiry<String, u32> for NoDeadline {
        fn cache_peek_expires_at<Q>(&self, key: &Q) -> (Option<u32>, Option<Instant>)
        where
            String: Borrow<Q>,
            Q: std::hash::Hash + Eq + ?Sized,
        {
            let stored = String::from("k");
            if Borrow::<Q>::borrow(&stored) == key {
                (Some(1), None)
            } else {
                (None, None)
            }
        }

        fn cache_expires_at<Q>(&self, key: &Q) -> (bool, Option<Instant>)
        where
            String: Borrow<Q>,
            Q: std::hash::Hash + Eq + ?Sized,
        {
            let stored = String::from("k");
            (Borrow::<Q>::borrow(&stored) == key, None)
        }
    }

    /// Sharded mirror of [`NoDeadline`], keyed by `&K` rather than a `Borrow<Q>` form.
    struct ShardedNoDeadline;

    impl ConcurrentCacheExpiry<String, u32> for ShardedNoDeadline {
        fn cache_peek_expires_at(&self, key: &String) -> (Option<u32>, Option<Instant>) {
            if key == "k" {
                (Some(1), None)
            } else {
                (None, None)
            }
        }

        fn cache_expires_at(&self, key: &String) -> (bool, Option<Instant>) {
            (key == "k", None)
        }
    }

    /// A value whose `expires_at` and `is_expired` deliberately disagree, which nothing in the
    /// [`Expires`] contract forbids.
    struct Divergent {
        deadline: Instant,
        expired: bool,
    }

    impl Expires for Divergent {
        fn is_expired(&self) -> bool {
            self.expired
        }

        fn expires_at(&self) -> Option<Instant> {
            Some(self.deadline)
        }
    }

    /// A threshold predicate applied to the `Option` (`!remaining.is_some_and(|d| d < THRESHOLD)`,
    /// which clippy normalizes to the `is_none_or` spelling below) holds for an entry with no
    /// deadline at all, so it asserts nothing about the instant. Binding the deadline is the only
    /// form that does, which is what both trait doctests do.
    #[test]
    fn a_missing_deadline_satisfies_the_option_chained_threshold_form() {
        let (value, expires_at) = NoDeadline.peek_expires_at("k");
        assert_eq!(value, Some(1), "the entry is present");
        assert!(expires_at.is_none(), "and carries no deadline");

        let remaining = expires_at.map(|t| t.saturating_duration_since(Instant::now()));
        assert!(
            remaining.is_none_or(|d| d >= THRESHOLD),
            "the option-chained form passes with no deadline to test"
        );

        let (value, expires_at) = ShardedNoDeadline.peek_expires_at(&"k".to_string());
        assert_eq!(value, Some(1));
        assert!(expires_at.is_none());
        let remaining = expires_at.map(|t| t.saturating_duration_since(Instant::now()));
        assert!(remaining.is_none_or(|d| d >= THRESHOLD));
    }

    /// [`CacheExpiry`] takes a `Borrow<Q>` key: a `String`-keyed store is peekable with a `&str`.
    /// [`ConcurrentCacheExpiry`] takes `&K`, so it needs the owned form.
    #[test]
    fn the_single_owner_read_accepts_a_borrowed_key_form() {
        assert_eq!(NoDeadline.peek_expires_at("k"), (Some(1), None));
        assert_eq!(
            NoDeadline.peek_expires_at(&"k".to_string()),
            (Some(1), None)
        );
        assert_eq!(NoDeadline.peek_expires_at("absent"), (None, None));

        assert_eq!(
            ShardedNoDeadline.peek_expires_at(&"k".to_string()),
            (Some(1), None)
        );
        assert_eq!(
            ShardedNoDeadline.peek_expires_at(&"absent".to_string()),
            (None, None)
        );
    }

    /// `Expires::expires_at` and `Expires::is_expired` can disagree in both directions, so a
    /// deadline in the future is not evidence that the entry is live on the `Expires`-based
    /// stores.
    #[test]
    fn expires_at_and_is_expired_can_disagree_in_both_directions() {
        let now = Instant::now();

        // Revoked early: a fixed future deadline on an entry that reports expired.
        let revoked = Divergent {
            deadline: now + TTL,
            expired: true,
        };
        assert!(
            revoked.expires_at().is_some_and(|t| t > now),
            "the deadline is in the future"
        );
        assert!(revoked.is_expired(), "yet the authority reports expired");

        // The other direction: a past deadline on an entry that reports live.
        let extended = Divergent {
            deadline: now - TTL,
            expired: false,
        };
        assert!(extended.expires_at().is_some_and(|t| t < now));
        assert!(!extended.is_expired());
    }

    /// The documented tie convention: `now >= t` is expired, matching the `now < t` liveness test
    /// the stores apply.
    #[test]
    fn a_deadline_equal_to_now_counts_as_expired() {
        let now = Instant::now();
        let live = |t: Instant| now < t;

        assert!(!live(now), "t == now is already past");
        assert!(!live(now - Duration::from_nanos(1)));
        assert!(live(now + Duration::from_nanos(1)));
    }
}

#[cfg(test)]
mod custom_shard_hasher_doc_contract {
    //! Pins the claims the crate-root "Custom shard hashers" blockquote makes about hand-written
    //! [`ShardHasher`] routers: a router that is not a `BuildHasher` keeps the six inherent
    //! lookups at every key type it implements, borrowed forms are an opt-in second impl, and
    //! the documented generic-helper signature is the working one. Prose cannot be compiled;
    //! this module is that prose expressed as code, so a claim that stops being true fails the
    //! build here rather than misleading a reader.

    use crate::{ShardHasher, ShardedLruCache};
    use std::hash::Hash;

    /// A hand-written router that is deliberately not a `BuildHasher`, implementing
    /// `ShardHasher` for the owned key type only.
    #[derive(Clone)]
    struct OwnedOnlyRouter;

    impl ShardHasher<u64> for OwnedOnlyRouter {
        fn shard_hash(&self, key: &u64) -> u64 {
            key.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        }
    }

    /// A router that opts into borrowed lookups with a second impl. The two impls agree on keys
    /// that compare equal (both hash the same bytes), which is the contract the docs place on a
    /// multi-impl router and which the compiler cannot check.
    #[derive(Clone)]
    struct BorrowedOptInRouter;

    fn bytes_hash(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    impl ShardHasher<String> for BorrowedOptInRouter {
        fn shard_hash(&self, key: &String) -> u64 {
            bytes_hash(key.as_bytes())
        }
    }

    impl ShardHasher<str> for BorrowedOptInRouter {
        fn shard_hash(&self, key: &str) -> u64 {
            bytes_hash(key.as_bytes())
        }
    }

    /// The generic-helper signature the `stores` module docs give as the working one, copied from the
    /// blockquote. `H: ShardHasher<K>` is the entire hasher bound: if the inherent `get` ever
    /// needs more, this definition stops compiling on its own, exactly as the docs describe.
    fn lookup<K, V, H>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V>
    where
        K: Hash + Eq + Clone,
        V: Clone,
        H: ShardHasher<K>,
    {
        c.get(k)
    }

    /// The borrowed-form helper signature the docs give: the looked-up form is named **in
    /// addition to** the store's key type, because the inherent `get`'s `impl` block is bounded
    /// on `H: ShardHasher<K>` and the borrowed call adds `ShardHasher<Q>` on top.
    /// `tests/sharded_generic_helper_bounds.rs` pins the same shape over the default hasher; this
    /// one pins it over a hand-written, non-`BuildHasher` router carrying both impls.
    fn lookup_borrowed<V, H>(c: &ShardedLruCache<String, V, H>, k: &str) -> Option<V>
    where
        V: Clone,
        H: ShardHasher<String> + ShardHasher<str>,
    {
        c.get(k)
    }

    fn owned_only_cache() -> ShardedLruCache<u64, u64, OwnedOnlyRouter> {
        ShardedLruCache::builder()
            .shards(4)
            .max_size(64)
            .hasher(OwnedOnlyRouter)
            .build()
            .expect("build must succeed")
    }

    /// A single `impl ShardHasher<K>` is enough for every owned-key inherent lookup.
    #[test]
    fn hand_written_router_keeps_the_six_inherent_owned_key_lookups() {
        let cache = owned_only_cache();
        cache.set(1, 10);
        cache.set(2, 20);
        cache.set(3, 30);
        cache.set(4, 40);

        assert_eq!(cache.get(&1), Some(10));
        assert_eq!(cache.peek(&1), Some(10));
        assert!(cache.contains(&1));
        assert_eq!(cache.remove(&2), Some(20));
        assert_eq!(cache.remove_entry(&3), Some((3, 30)));
        assert!(cache.delete(&4));
        assert!(!cache.contains(&4));
    }

    /// The documented helper bound set compiles and works over a non-`BuildHasher` router.
    #[test]
    fn documented_generic_helper_bound_set_works_over_a_hand_written_router() {
        let cache = owned_only_cache();
        cache.set(7, 70);

        assert_eq!(lookup(&cache, &7), Some(70));
        assert_eq!(lookup(&cache, &8), None);
    }

    /// The opt-in half: a second `impl ShardHasher<Q>` enables the borrowed lookups, and an
    /// owned insert is found through the borrowed form because the two impls agree.
    #[test]
    fn a_second_shard_hasher_impl_opts_the_router_into_borrowed_lookups() {
        let cache: ShardedLruCache<String, u32, BorrowedOptInRouter> = ShardedLruCache::builder()
            .shards(4)
            .max_size(64)
            .hasher(BorrowedOptInRouter)
            .build()
            .expect("build must succeed");

        cache.set("a".to_string(), 1);

        assert_eq!(cache.get("a"), Some(1));
        assert!(cache.contains("a"));
        // The documented borrowed-helper bound set (`ShardHasher<String> + ShardHasher<str>`)
        // reaches the same entry through a generic helper.
        assert_eq!(lookup_borrowed(&cache, "a"), Some(1));
        assert_eq!(lookup_borrowed(&cache, "b"), None);
        assert_eq!(cache.remove("a"), Some(1));
        assert_eq!(cache.get("a"), None);
    }
}
