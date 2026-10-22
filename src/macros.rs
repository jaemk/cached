#![cfg_attr(docsrs, doc(cfg(feature = "proc_macro")))]

/*!
Procedural macros for defining functions that wrap a static-ref cache object.

Macro-generated cache statics use an `RwLock` by default, so named
cache statics are inspected with `.read()` or `.write()`. Use `sync_lock = "mutex"` to force
a single-mutex shape. `sync_writes = true` / `"default"` holds the cache write lock
while computing a missing value; `sync_writes = "by_key"` uses bucketed per-key locks so
duplicate keys synchronize without blocking unrelated keys. `unsync_reads = true` uses
`CachedRead` under a shared read lock for the initial hit path, and only works with stores
whose read semantics do not require recency or refresh mutation.

- [Examples](#examples)
- [Synchronization](#synchronization)
- [Attribute reference](#attribute-reference)
- [Requirements](#requirements)
- [Concurrent, Redis, and disk examples](#concurrent-redis-and-disk-examples)

# Examples

```rust,no_run
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;

/// Use an lru cache with size 100 and a `(String, String)` cache key
#[cached(max_size=100)]
fn keyed(a: String, b: String) -> usize {
    let size = a.len() + b.len();
    sleep(Duration::new(size as u64, 0));
    size
}
# pub fn main() { }
```

----

```rust,no_run
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;

/// Use a timed-lru cache with size 1, a TTL of 60s,
/// and a `(usize, usize)` cache key
# #[cfg(feature = "time_stores")]
#[cached(max_size=1, ttl_secs=60)]
fn keyed(a: usize, b: usize) -> usize {
    let total = a + b;
    sleep(Duration::new(total as u64, 0));
    total
}
pub fn main() {
    # #[cfg(feature = "time_stores")]
    keyed(1, 2);  // Not cached, will sleep (1+2)s

    # #[cfg(feature = "time_stores")]
    keyed(1, 2);  // Cached, no sleep

    sleep(Duration::new(60, 0));  // Sleep for the TTL

    # #[cfg(feature = "time_stores")]
    keyed(1, 2);  // 60s TTL has passed so the cached
                  // value has expired, will sleep (1+2)s

    # #[cfg(feature = "time_stores")]
    keyed(1, 2);  // Cached, no sleep

    # #[cfg(feature = "time_stores")]
    keyed(2, 1);  // New args, not cached, will sleep (2+1)s

    # #[cfg(feature = "time_stores")]
    keyed(1, 2);  // Was evicted because of lru size of 1,
                  // will sleep (1+2)s
}
```

----

```rust,no_run
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;

/// Use a timed cache with a TTL of 60s
/// that refreshes the entry TTL on cache hit,
/// and a `(String, String)` cache key
# #[cfg(feature = "time_stores")]
#[cached(ttl_secs=60, refresh=true)]
fn keyed(a: String, b: String) -> usize {
    let size = a.len() + b.len();
    sleep(Duration::new(size as u64, 0));
    size
}
# pub fn main() { }
```

----

```rust,no_run
use cached::macros::cached;

# fn do_something_fallible() -> std::result::Result<(), ()> {
#     Ok(())
# }

/// Cache a fallible function. Only `Ok` results are cached.
#[cached(max_size=1)]
fn keyed(a: String) -> Result<usize, ()> {
    do_something_fallible()?;
    Ok(a.len())
}
# pub fn main() { }
```

----

```rust,no_run
use cached::macros::cached;

/// Cache an optional function. Only `Some` results are cached.
#[cached(max_size=1)]
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

```rust,no_run
use cached::macros::cached;

/// Cache an optional function. Only `Some` results are cached.
/// When called concurrently, duplicate argument-calls will be
/// synchronized so as to only run once - the remaining concurrent
/// calls return a cached value.
#[cached(max_size=1, sync_writes = true)]
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

```rust,no_run
use cached::macros::cached;
use cached::Return;

/// Get a `cached::Return` value that indicates
/// whether the value returned came from the cache:
/// `cached::Return.was_cached()`.
/// Use an LRU cache and a `String` cache key.
#[cached(max_size=1, with_cached_flag = true)]
fn calculate(a: String) -> Return<String> {
    Return::new(a)
}
pub fn main() {
    let r = calculate("a".to_string());
    assert!(!r.was_cached());
    let r = calculate("a".to_string());
    assert!(r.was_cached());
    // Return<String> derefs to String
    assert_eq!(r.to_uppercase(), "A");
}
```

----

```rust,no_run
use cached::macros::cached;
use cached::Return;

# fn do_something_fallible() -> std::result::Result<(), ()> {
#     Ok(())
# }

/// Same as the previous, but returning a Result
#[cached(max_size=1, with_cached_flag = true)]
fn calculate(a: String) -> Result<Return<usize>, ()> {
    do_something_fallible()?;
    Ok(Return::new(a.len()))
}
pub fn main() {
    match calculate("a".to_string()) {
        Err(e) => eprintln!("error: {:?}", e),
        Ok(r) => {
            println!("value: {:?}, was cached: {}", *r, r.was_cached());
            // value: "a", was cached: true
        }
    }
}
```

----

```rust,no_run
use cached::macros::cached;
use cached::Return;

/// Same as the previous, but returning an Option
#[cached(max_size=1, with_cached_flag = true)]
fn calculate(a: String) -> Option<Return<usize>> {
    if a == "a" {
        Some(Return::new(a.len()))
    } else {
        None
    }
}
pub fn main() {
    if let Some(a) = calculate("a".to_string()) {
        println!("value: {:?}, was cached: {}", *a, a.was_cached());
        // value: "a", was cached: true
    }
}
```

----

```rust,no_run
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;
use cached::LruCache;

/// Use an explicit cache-type with a custom creation block and custom cache-key generating block
#[cached(
    ty = "LruCache<String, usize>",
    create = LruCache::builder().max_size(100).build().unwrap(),
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

```rust,no_run
use cached::macros::once;
use cached::time::Duration;

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

```rust
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;

/// Use a timed cache with a TTL of 60s.
/// Run a background thread to continuously refresh a specific key.
# #[cfg(feature = "time_stores")]
#[cached(ttl_secs = 60, key = "String", convert = r#"{ String::from(a) }"#)]
fn keyed(a: &str) -> usize {
    a.len()
}
pub fn main() {
    # #[cfg(feature = "time_stores")]
    let _handler = std::thread::spawn(|| {
        loop {
            sleep(Duration::from_secs(50));
            // this method is generated by the `cached` macro
            keyed_prime_cache("a");
        }
    });
    // handler.join().unwrap();
}
```

----

```rust
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::once;

/// Run a background thread to continuously refresh a singleton.
#[once]
fn keyed() -> String {
    // do some long http request
    "some data".to_string()
}
pub fn main() {
    let _handler = std::thread::spawn(|| {
        loop {
            sleep(Duration::from_secs(60));
            // this method is generated by the `cached` macro
            keyed_prime_cache();
        }
    });
    // handler.join().unwrap();
}
```

----

```rust
use std::thread::sleep;
use cached::time::Duration;
use cached::macros::cached;

/// Run a background thread to continuously refresh a specific cache key
#[cached(key = "String", convert = r#"{ String::from(a) }"#)]
fn keyed(a: &str) -> usize {
    a.len()
}
pub fn main() {
    let _handler = std::thread::spawn(|| {
        loop {
            sleep(Duration::from_secs(60));
            // keyed_prime_cache computes a fresh value for the given key and stores
            // it in the cache, replacing any existing entry.
            keyed_prime_cache("hello");
        }
    });
    // handler.join().unwrap();
}
```


*/

/*!
# Synchronization

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

# Attribute reference

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

# Requirements

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

# Concurrent, Redis, and disk examples

`sync_writes` and `result_fallback` cannot be combined:

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
*/

#[doc(inline)]
pub use cached_proc_macro::{cached, concurrent_cached, once};
#[doc(inline)]
pub use cached_proc_macro_types::Return;
