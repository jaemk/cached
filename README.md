# cached

[![Build Status](https://github.com/jaemk/cached/actions/workflows/build.yml/badge.svg)](https://github.com/jaemk/cached/actions/workflows/build.yml)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Memoize Rust functions with one attribute. In-memory, sharded, Redis, and disk caches.

```rust
use cached::macros::cached;

/// Cache up to 1,000 users for 60 seconds each.
/// `Err` results are not cached, so a failed request is retried on the next call.
#[cached(max_size = 1_000, ttl_secs = 60)]
fn get_user(id: u64) -> Result<User, ApiError> {
    api::fetch_user(id) // slow network call
}

let user = get_user(1)?; // calls the API
let user = get_user(1)?; // returned from the cache
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
use cached::macros::cached;

#[cached(max_size = 100, ttl_secs = 30)]
async fn get(url: String) -> Body {
    fetch(&url).await
}
```

## Redis and disk caches

`#[concurrent_cached]` can store results in Redis (`redis_*` features) or on disk with
[`redb`](https://docs.rs/redb) (`redb_store` feature). Values are serialized, so they must
implement `serde::Serialize` and `serde::DeserializeOwned`, and the function must return
`Result` so store errors have somewhere to go.

```rust
use cached::macros::concurrent_cached;

/// Reads the connection string from `CACHED_REDIS_CONNECTION_STRING`.
#[concurrent_cached(redis = true, ttl_secs = 300, map_error = |e| MyError(format!("{e:?}")))]
async fn report(id: u64) -> Result<String, MyError> {
    fetch_report(id).await
}
```

```rust
use cached::macros::concurrent_cached;

/// Stored under the system cache directory unless `disk_dir` is set.
#[concurrent_cached(disk = true, map_error = |e| MyError(format!("{e:?}")))]
fn thumbnail(n: u64) -> Result<String, MyError> {
    Ok(render(n))
}
```

See [`examples/redis-async-tokio.rs`](https://github.com/jaemk/cached/blob/master/examples/redis-async-tokio.rs)
and [`examples/disk.rs`](https://github.com/jaemk/cached/blob/master/examples/disk.rs) for
complete programs, including explicit store construction with `ty` and `create`.

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
use cached::LruTtlCache;
use cached::time::Duration;

let cache: LruTtlCache<u64, String> = LruTtlCache::builder()
    .max_size(1_000)
    .ttl(Duration::from_secs(60))
    .on_evict(|key, _value| println!("evicted {key}"))
    .build()
    .unwrap();
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

## Requirements

- Rust 1.92 or later.
- Arguments must be `Hash + Eq + Clone`, or supply `key` and `convert` to build a key that is.
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

License: MIT
