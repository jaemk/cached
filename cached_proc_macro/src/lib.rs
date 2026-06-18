mod cached;
mod concurrent_cached;
mod helpers;
mod once;

use proc_macro::TokenStream;

/// Define a memoized function using a cache store that implements `cached::Cached` (and
/// `cached::CachedAsync` for async functions)
///
/// # Attributes
/// - `name`: (optional, string) specify the name for the generated cache, defaults to the function name uppercase.
/// - `max_size`: (optional, usize) specify an LRU max size, implies the cache type is a `LruCache` or `LruTtlCache`.
/// - `ttl`: (optional, Duration string) specify a cache TTL as a Duration-expression string literal,
///   e.g. `ttl = "Duration::from_secs(60)"`. Implies the cache type is a `TtlCache` or `LruTtlCache`
///   (requires the `time_stores` feature). Mutually exclusive with `ttl_secs`, `ttl_millis`, and `expires`.
/// - `ttl_secs`: (optional, u64) specify a cache TTL as a whole number of seconds. Equivalent to
///   `ttl = "Duration::from_secs(N)"` but accepts a bare integer. Mutually exclusive with `ttl`,
///   `ttl_millis`, and `expires`.
/// - `ttl_millis`: (optional, u64) specify a cache TTL in milliseconds. A finer-grained alternative
///   to `ttl_secs` with the same store selection (so it likewise requires the `time_stores` feature);
///   mutually exclusive with `ttl`, `ttl_secs`, and `expires`. On `#[cached]`'s default store selection this is an
///   in-memory store, so sub-second TTLs are honored exactly; a custom `ty`/`create` store honors them
///   only if that store itself supports sub-second granularity.
/// - `refresh`: (optional, bool) specify whether to refresh the TTL on cache hits.
/// - `force_refresh`: (optional, expression block) a boolean expression over the function arguments,
///   written in curly braces like `convert` (it is evaluated, not a magic flag and not a required
///   bool parameter). When it evaluates to `true`, any cached value is bypassed and the function body
///   is re-run and re-cached. Typically the condition is computed from existing arguments, e.g.
///   `force_refresh = "{ id == 0 }"` to always recompute the sentinel id; there is no extra argument
///   and the default key is correct as-is.
///
///   If instead you use a dedicated flag argument (e.g. `refresh: bool`), you must exclude it from
///   the cache key with `key` / `convert`:
///   `#[cached(key = "u64", convert = "{ id }", force_refresh = "{ refresh }")] fn fetch(id: u64, refresh: bool)`.
///   This is not optional. With the default key the flag is part of the key, so the two call shapes hit
///   different entries: a `refresh = true` call bypasses the read, recomputes, and stores under the
///   `(id, true)` key, while ordinary `refresh = false` calls read the `(id, false)` key. The forced
///   recompute therefore lands in an entry that normal calls never read, so the refresh is silently
///   lost (later `refresh = false` calls keep returning the stale value), and the `(id, true)` entry is
///   written but never read. Excluding the flag collapses both shapes onto the one `id` entry, so a
///   forced recompute overwrites exactly what subsequent calls read. This is orthogonal to `refresh`
///   (which renews a TTL on a cache hit):
///   `force_refresh` decides whether to use a cached value at all, `refresh` decides whether a
///   used cached value renews its TTL. With `result_fallback = true`, a force-refreshed call that
///   re-runs and returns `Err` still serves the previously cached `Ok` value (the fallback consults
///   the cache even though the hit was bypassed).
/// - `in_impl`: (optional, bool) allow `#[cached]` on a method that takes `self` inside an `impl`
///   block. The cache static is emitted inside the generated method body (so it does not collide with
///   same-named methods on other types). The receiver is not part of the cache key - the cache is
///   shared across all instances of the type. Note: the `{fn}_prime_cache` companion is not generated
///   for `in_impl` methods - the cache static is function-local and cannot be shared with a separate
///   prime sibling, so priming is not supported there. The `{fn}_no_cache` sibling (the uncached
///   origin function) is still generated and inherits the method's visibility, so a `pub` cached
///   method exposes a `pub {fn}_no_cache` cache-bypass sibling on the same `impl`.
/// - `sync_writes`: (optional, bool or string) specify whether to synchronize the execution and writing of uncached values.
///   When not specified or set to `false`, uncached calls execute without write synchronization. When set to `true`
///   or `"default"`, all keys synchronize by locking the whole cache during uncached execution. When set to
///   `"by_key"`, a per-key lock synchronizes uncached execution of duplicate keys only.
/// - `sync_writes_buckets`: (optional, usize) number of per-key lock buckets used by
///   `sync_writes = "by_key"`; defaults to 64. Each bucket is one `Arc<RwLock<()>>`. Keys
///   hash into a bucket, so two different keys may share a bucket and serialize unnecessarily
///   (false sharing). Increase this if you observe contention under high concurrency - a value
///   around 2-4x your expected peak concurrency eliminates most false sharing. Must be > 0.
/// - `sync_lock`: (optional, string) choose the generated cache lock. Defaults to `"rwlock"`. Use `"mutex"`
///   to force a mutex. `unsync_reads = true` requires an RwLock.
/// - `unsync_reads`: (optional, bool) use `CachedRead::cache_get_read` under a shared read lock for the initial
///   cache lookup, while keeping writes synchronized. This only works for stores that implement `CachedRead`;
///   recency-updating or refresh-on-hit stores intentionally do not. For non-mutating diagnostic lookups,
///   use the separate `CachedPeek` trait directly on stores.
/// - `ty`: (optional, string type) The cache store type to use. Defaults to `UnboundCache`.
///   When `max_size` is specified, defaults to `LruCache`.
///   When `ttl` is specified, defaults to `TtlCache`.
///   When `max_size` and `ttl` are specified, defaults to `LruTtlCache`. When `ty` is
///   specified, `create` must also be specified.
/// - `create`: (optional, string expr) specify an expression used to create a new cache store, e.g. `create = r##"{ CacheType::new() }"##`.
/// - `key`: (optional, string type) specify what type to use for the cache key, e.g. `key = "u32"`.
///   When `key` is specified, `convert` must also be specified.
/// - `convert`: (optional, string expr) specify an expression used to convert function arguments to a cache
///   key, e.g. `convert = r##"{ format!("{}:{}", arg1, arg2) }"##`. When `convert` is specified,
///   `key` or `ty` must also be set.
/// - `cache_err`: (optional, bool) If your function returns a `Result`, also cache `Err` values (by default only `Ok` is cached).
///   **Note:** when `cache_err = true`, the underlying store holds `Result<T, E>` as its value type,
///   so a direct `.cache_get()` on the generated cache static returns `Option<Result<T, E>>` - the outer
///   `Option` is the cache hit/miss, the inner `Result` is the stored value.
/// - `cache_none`: (optional, bool) If your function returns an `Option`, also cache `None` values (by default only `Some` is cached).
///   **Note:** when `cache_none = true`, the underlying store holds `Option<T>` as its value type,
///   so a direct `.cache_get()` on the generated cache static returns `Option<Option<T>>` - the outer
///   `Option` is the cache hit/miss, the inner `Option` is the stored value.
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return`,
///   `Result<cached::Return<T>, E>`, or `Option<cached::Return<T>>`,
///   the `cached::Return.was_cached` flag will be updated when a cached value is returned.
///   The wrapper type **must** be `cached::Return` - either written fully
///   qualified, or imported from `cached` (`use cached::Return;`). A proc macro
///   only sees tokens, not resolved types: an unrelated type that merely happens
///   to be named `Return<T>` passes the attribute check but then fails to
///   compile in the generated body (it calls `::cached::Return::new` /
///   `.was_cached`). Use a different name for any non-`cached` `Return` type.
/// - `result_fallback`: (optional, bool) If your function returns a `Result` and it fails, the cache will instead refresh the recently expired `Ok` value.
///   In other words, refreshes are best-effort - returning `Ok` refreshes as usual but `Err` falls back to the last `Ok`.
///   This is useful, for example, for keeping the last successful result of a network operation even during network disconnects.
///   *Note*, this option requires the cache type to implement `CloneCached`. The compatible built-in options are:
///   `ttl`, `ttl_secs`, or `ttl_millis` (uses `TtlCache`), `max_size` + `ttl`/`ttl_secs`/`ttl_millis` (uses `LruTtlCache`), and
///   `expires` (uses `ExpiringCache`/`ExpiringLruCache`).
///   A custom `ty` that implements `CloneCached` is also accepted.
///   Requires a `Result<T, E>` return type. Mutually exclusive with `cache_err` and `sync_writes`.
///   Requires the cache key type to implement `Clone` (the fallback path re-caches the key). The
///   default key already satisfies this, so it only matters with a custom non-`Clone` `key`/`convert`.
/// - `expires`: (optional, bool) Auto-select an expiry-aware store whose entries expire based on
///   per-value logic rather than a single global TTL.
///   The return type must implement `Expires`; for `Result<T, E>` or `Option<T>` returns, the inner `T` must implement `Expires`.
///   Without `max_size`, uses `ExpiringCache` (unbounded).
///   With `max_size = N`, uses `ExpiringLruCache` (LRU-bounded to N entries).
///   Unlike `ttl`, expiry logic lives in each value - useful for caching OAuth tokens,
///   HTTP responses with `Cache-Control` headers, or any payload with its own expiration timestamp.
///   Compatible with `result_fallback`: on `Err`, returns the last-cached `Ok` value wrapped in `Ok(...)`,
///   even if that value's `is_expired()` returns `true`. Callers must check the value's expiry themselves
///   if they need to distinguish a fresh result from a stale fallback.
///   Mutually exclusive with `ttl`, `ty`, `create`, `with_cached_flag`, `unsync_reads`, and `refresh`.
///
/// ## Note
/// The `ty`, `create`, `key`, and `convert` attributes must be in a `String`
/// This is because darling, which is used for parsing the attributes, does not support directly parsing
/// attributes into `Type`s or `Block`s.
///
/// `Result`/`Option` detection is exact: the macro matches only the bare identifiers `Result`
/// and `Option` (including qualified forms like `std::result::Result<T, E>`). Type aliases are
/// never resolved, so an alias - even one named `MyResult` (`type MyResult<T> = Result<T, E>`) -
/// is treated as a plain return value and its `Err` / `None` will be cached. Return
/// `Result<T, E>` / `Option<T>` directly when you need the default Ok-only / Some-only behavior.
///
/// **Generic functions** require `key` + `convert` to pin the cache key to a concrete type. The
/// cache static is a single monomorphic store shared across all instantiations and cannot name the
/// function's type parameters, so a generic function with the default key (no `convert`) is a
/// compile error. Provide `key`/`convert` (and `ty`/`create` if the value type is also generic) -
/// see the generic-`where` tests - or wrap the generic function in a non-generic `#[cached]`
/// function for each concrete type.
#[proc_macro_attribute]
pub fn cached(args: TokenStream, input: TokenStream) -> TokenStream {
    cached::cached(args, input)
}

/// Define a memoized function using a cache store that implements `cached::Cached` (and
/// `cached::CachedAsync` for async functions). Function arguments are not used to identify
/// a cached value, only one value is cached unless a `ttl` expiry is specified.
///
/// # Attributes
/// - `name`: (optional, string) specify the name for the generated cache, defaults to the function name uppercase.
/// - `ttl`: (optional, Duration string) specify an expiry as a Duration-expression string literal,
///   e.g. `ttl = "Duration::from_secs(60)"`, after which the single cached value is recomputed on
///   the next call. `#[once]` always stores one value in an `Option` (timestamped when `ttl` is
///   set) - it is not a `TtlCache`/`LruTtlCache`. Mutually exclusive with `ttl_secs`, `ttl_millis`,
///   and `expires`.
/// - `ttl_secs`: (optional, u64) specify an expiry as a whole number of seconds. Equivalent to
///   `ttl = "Duration::from_secs(N)"` but accepts a bare integer. Mutually exclusive with `ttl`,
///   `ttl_millis`, and `expires`.
/// - `ttl_millis`: (optional, u64) the same expiry expressed in milliseconds; mutually exclusive
///   with `ttl`, `ttl_secs`, and `expires`.
/// - `force_refresh`: (optional, expression block) a boolean expression over the function arguments,
///   in curly braces like `convert` (it is evaluated, not a magic flag), e.g.
///   `force_refresh = "{ stale }"`. When it evaluates to `true`, the single cached value is bypassed
///   and the body re-runs and re-caches. Because `#[once]` has no per-call key (one value is shared by
///   all callers), there is no "exclude the flag from the key" caveat as on `#[cached]`: a forced
///   recompute simply overwrites the one shared value. Orthogonal to `ttl` expiry.
/// - `in_impl`: (optional, bool) allow `#[once]` on a method that takes `self` inside an `impl`
///   block. Note: `#[once]` stores a single value for all calls, so an `in_impl` `#[once]`
///   method shares one cached value across every instance of the type. Priming is unavailable here:
///   the `{fn}_prime_cache` companion is not generated for `in_impl` methods, because the cache static
///   is function-local and cannot be shared with a separate prime sibling. The `{fn}_no_cache` sibling
///   (the uncached origin function) is still generated and inherits the method's visibility, so a
///   `pub` cached method exposes a `pub {fn}_no_cache` cache-bypass sibling on the same `impl`.
/// - `sync_writes`: (optional, bool or string) specify whether to synchronize the execution of writing of uncached values.
///   When set to `true` or `"default"`, uncached execution is synchronized with the whole cache.
///   When omitted or set to `false`, uncached calls are not synchronized. `sync_writes = "by_key"`
///   is not supported by `#[once]` because a `#[once]` cache stores a single value for all arguments.
/// - `cache_err`: (optional, bool) If your function returns a `Result`, also cache `Err` values (by default only `Ok` is cached).
/// - `cache_none`: (optional, bool) If your function returns an `Option`, also cache `None` values (by default only `Some` is cached).
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return`,
///   `Result<cached::Return<T>, E>`, or `Option<cached::Return<T>>`,
///   the `cached::Return.was_cached` flag will be updated when a cached value is returned.
///   The wrapper type **must** be `cached::Return` - either written fully
///   qualified, or imported from `cached` (`use cached::Return;`). A proc macro
///   only sees tokens, not resolved types: an unrelated type that merely happens
///   to be named `Return<T>` passes the attribute check but then fails to
///   compile in the generated body (it calls `::cached::Return::new` /
///   `.was_cached`). Use a different name for any non-`cached` `Return` type.
/// - `expires`: (optional, bool) Delegate expiry to the cached value instead of a fixed TTL.
///   The return type must implement `Expires`; for `Result<T, E>` or `Option<T>` returns, the inner `T` must implement `Expires`.
///   When a lookup finds the cached value reports `is_expired() == true`, the cached value is
///   skipped and the function re-executes; on success the new value replaces the old one.
///   If the function returns `Err`/`None`, the expired entry is left in place and the error/none
///   is returned to the caller - subsequent calls will re-execute the function until it succeeds.
///   Mutually exclusive with `ttl` and `with_cached_flag`.
///
/// `Result`/`Option` detection is exact: the macro matches only the bare identifiers `Result`
/// and `Option` (including qualified forms like `std::result::Result<T, E>`). Type aliases are
/// never resolved, so an alias - even one named `MyResult` (`type MyResult<T> = Result<T, E>`) -
/// is treated as a plain return value and its `Err` / `None` will be cached. Return
/// `Result<T, E>` / `Option<T>` directly when you need the default Ok-only / Some-only behavior.
///
/// **Generic functions are supported** by `#[once]`: its static only holds the (concrete) value
/// type, never the function's type parameters, so no `key`/`convert` is required.
#[proc_macro_attribute]
pub fn once(args: TokenStream, input: TokenStream) -> TokenStream {
    once::once(args, input)
}

/// Define a memoized function using a cache store that implements `cached::ConcurrentCached` (and
/// `cached::ConcurrentCachedAsync` for async functions).
///
/// **The macro preserves the function's sync/async-ness - it does not make a function async.**
/// Applied to a synchronous `fn`, it generates a synchronous `fn` you call without `.await`
/// (it uses the `ConcurrentCached` trait). Applied to an `async fn`, it generates an `async fn`
/// you call with `.await` (it uses `ConcurrentCachedAsync`). The `&self`-contract sharded stores
/// are internally synchronized, so a sync `#[concurrent_cached]` function is still safe to share
/// and call from multiple threads concurrently.
///
/// By default (no `redis`, `disk`, `ty`, or `create` attributes) the macro selects a sharded in-memory
/// store based on the combination of `max_size`, `ttl`, and `expires`:
///
/// | Attributes | Store selected |
/// |---|---|
/// | (none) | `ShardedUnboundCache` - unbounded, no TTL |
/// | `max_size = N` | `ShardedLruCache` - LRU-bounded |
/// | `ttl = T` | `ShardedTtlCache` - TTL-expiring, unbounded (`time_stores` feature) |
/// | `max_size = N, ttl = T` | `ShardedLruTtlCache` - LRU + TTL (`time_stores` feature) |
/// | `expires = true` | `ShardedExpiringCache` - per-value expiry, unbounded |
/// | `expires = true, max_size = N` | `ShardedExpiringLruCache` - per-value expiry, LRU-bounded |
///
/// `ttl_millis = T` selects the same store as `ttl = T` (the `ShardedTtlCache`/`ShardedLruTtlCache`
/// rows), just with millisecond rather than second granularity.
///
/// On the default in-memory path, do **not** specify `map_error` - the sharded stores are
/// infallible (`Error = Infallible`) and supplying `map_error` is a compile error.
/// Reserve `map_error` for `redis`/`disk`/custom `ty`/`create` stores where the error type is fallible.
/// Functions may return a plain `T`, `Option<T>`, or `Result<T, E>`. Plain values are
/// cached as-is. `Option<T>` skips caching `None` by default; use `cache_none = true`
/// to also cache `None`. `Result<T, E>` caches only successful `Ok(T)` values and returns
/// `Err(E)` without storing it; use `cache_err = true` to also cache `Err` values.
/// `result_fallback = true` is supported: on an `Err` return, the last cached `Ok` value
/// for the same key is returned instead (requires `ttl`, `ttl_secs`, or `ttl_millis`).
///
/// Result detection is exact: the macro matches only the bare identifier `Result` (including
/// qualified forms like `std::result::Result<T, E>`). Type aliases are never resolved, so any
/// alias - even one whose name ends with `Result` (e.g. `type MyResult<T> = Result<T, E>`) -
/// is treated as a plain return value and its `Err` variant will be cached. Return `Result<T, E>`
/// directly when you need Ok-only caching behavior.
///
/// **Note:** `on_evict` callbacks are not available via `#[concurrent_cached]`. To use an
/// eviction callback, construct the store manually with its builder (e.g.
/// `ShardedLruCache::builder().max_size(N).on_evict(|k, v| { ... }).build()`) and supply it via
/// `ty`/`create` (see the `ty` and `create` attributes below).
///
/// **Note (async methods):** The `ConcurrentCachedAsync` operations are named with an `async_`
/// prefix (`.async_cache_get`, `.async_cache_set`, etc.) so they never collide with the
/// synchronous `ConcurrentCached` operations even when both traits are in scope. Call them
/// directly on an async sharded store: `STORE.async_cache_get(&key).await`.
///
/// **Clone requirement:** When no `key` or `convert` attribute is specified, function arguments
/// are cloned to form the cache key tuple, so all argument types must implement `Clone`.
/// Use `key` + `convert` to map to an explicit key type and avoid the clone if needed.
///
/// # Attributes
/// - `map_error`: (required for `redis`/`disk` and custom `ty`/`create` stores; **not allowed**
///   on the default in-memory sharded path - those stores are infallible and supplying `map_error`
///   there is a compile error) a closure used to map store errors into the error type returned
///   by your function.
/// - `name`: (optional, string) specify the name for the generated cache, defaults to the function name uppercase.
/// - `redis`: (optional, bool) default to a `RedisCache` or `AsyncRedisCache`
/// - `disk`: (optional, bool) selects `RedbCache` (the default disk engine), this must be set to true even if `type` and `create` are specified.
///   On an `async fn`, `redb`'s blocking I/O is run on `tokio`'s blocking pool via
///   `spawn_blocking` (so it does not stall the async runtime); this requires a Tokio
///   runtime context and surfaces a `RedbCacheError::BackgroundTaskFailed` if that task is
///   cancelled or panics.
/// - `max_size`: (optional, usize) total LRU capacity for the default in-memory store. Selects
///   `ShardedLruCache` (or `ShardedLruTtlCache` when combined with `ttl`). A compile error is
///   emitted when combined with `redis`, `disk`, or `create`.
///   **Note:** effective capacity may exceed `N` - shards enforce a 16-entry minimum floor, so
///   `max_size = 4` on an 8-shard build silently gives 128 effective slots. For a strict cap use
///   `shards = 1` or the builder's `per_shard_max_size`.
/// - `ttl`: (optional, Duration string) TTL as a Duration-expression string literal, e.g.
///   `ttl = "Duration::from_secs(60)"`. For the default in-memory path, selects `ShardedTtlCache`
///   or `ShardedLruTtlCache` (requires the `time_stores` feature). For `redis` and `disk` stores,
///   sets the key/entry TTL on those backends. Mutually exclusive with `ttl_secs`, `ttl_millis`,
///   and `expires`.
/// - `ttl_secs`: (optional, u64) TTL as a whole number of seconds. Equivalent to
///   `ttl = "Duration::from_secs(N)"` but accepts a bare integer. Selects the same stores as `ttl`.
///   Mutually exclusive with `ttl`, `ttl_millis`, and `expires`.
/// - `ttl_millis`: (optional, u64) the same TTL expressed in milliseconds; mutually exclusive with
///   `ttl`, `ttl_secs`, and `expires`. On the default in-memory path it selects the same sharded TTL stores as `ttl`
///   (so it likewise requires the `time_stores` feature). Honored on every backend (in-memory sharded,
///   redis, and disk). The in-memory sharded and disk (redb) stores honor true sub-second expiry; only
///   the redis backend applies TTL at
///   whole-second granularity (any non-zero fractional second rounds up to the next whole second, so
///   `ttl_millis = 500` becomes 1s and `ttl_millis = 1500` becomes 2s on redis), so a
///   `ttl_millis` that is not a whole number of seconds gives finer expiry everywhere except redis.
/// - `force_refresh`: (optional, expression block) a boolean expression over the function arguments,
///   in curly braces like `convert` (it is evaluated, not a magic flag), e.g.
///   `force_refresh = "{ id == 0 }"`. When it evaluates to `true`, any cached value is bypassed and the
///   function body is re-run and re-cached. If instead you use a dedicated flag argument (e.g.
///   `refresh: bool`), you must exclude it from the cache key with `key` / `convert`. This is not
///   optional. With the default key the flag is part of the key, so the two call shapes hit different
///   entries: a `refresh = true` call bypasses the read, recomputes, and stores under the `(id, true)`
///   key, while ordinary `refresh = false` calls read the `(id, false)` key. The forced recompute
///   therefore lands in an entry that normal calls never read, so the refresh is silently lost (later
///   `refresh = false` calls keep returning the stale value), and the `(id, true)` entry is written but
///   never read. Excluding the flag collapses both shapes onto the one `id` entry, so a forced recompute
///   overwrites exactly what subsequent calls read. Orthogonal to `refresh` (TTL renewal on a hit). With
///   `result_fallback = true`, a force-refreshed call that re-runs and returns `Err` still serves the
///   previously cached `Ok` value (the fallback consults the cache even though the hit was bypassed).
/// - `in_impl`: (optional, bool) allow `#[concurrent_cached]` on a method that takes `self` inside an
///   `impl` block. The cache static is emitted inside the generated method body. The receiver is not
///   part of the cache key - the cache is shared across all instances of the type. Note: the
///   `{fn}_prime_cache` companion is not generated for `in_impl` methods - the cache static is
///   function-local and cannot be shared with a separate prime sibling, so priming is not supported there.
///   The `{fn}_no_cache` sibling (the uncached origin function) is still generated and inherits the
///   method's visibility, so a `pub` cached method exposes a `pub {fn}_no_cache` cache-bypass sibling.
/// - `shards`: (optional, usize) number of shards for the default in-memory store. Rounded up to
///   the next power of two. If omitted, defaults to `available_parallelism() x 4`, clamped to
///   8-1024; an explicit value is only rounded up to a power of two and is not clamped.
///   A compile error is emitted when combined with `redis`, `disk`, or `create`.
/// - `refresh`: (optional, bool) refresh the TTL on cache hits (TTL stores only). On the default
///   in-memory path, setting `refresh = true` without `ttl` is a compile error (`refresh = false`
///   without `ttl` is accepted but has no effect). On `redis`/`disk` paths `refresh` is forwarded
///   to the backend store builder.
/// - `expires`: (optional, bool) select a per-value expiry store. The cached value type must
///   implement the `Expires` trait. Without `max_size`, selects `ShardedExpiringCache` (unbounded);
///   with `max_size = N`, selects `ShardedExpiringLruCache` (LRU-bounded). Mutually exclusive with
///   `ttl`, `redis`, `disk`, `ty`, `create`, and `refresh`. May be combined with
///   `with_cached_flag`; in that case the inner `T` of `Return<T>` (not `Return<T>` itself)
///   must implement `Expires`. When the function returns `Option<T>`, `None` is not cached
///   by default (implicit smart-option), and the inner `T` (not `Option<T>`)
///   must implement `Expires`. When a cached entry is found but `is_expired()` returns `true`,
///   the function re-executes and the result is treated as a fresh uncached value; the returned
///   `Return<T>` will have `was_cached = false` in this case.
/// - `ty`: (optional, string type) explicitly specify the cache store type to use.
/// - `cache_prefix_block`: (optional, string expr) specify an expression used to create the string used as a
///   prefix for all cache keys of this function, e.g. `cache_prefix_block = r##"{ "my_prefix" }"##`.
///   When not specified, the cache prefix will be constructed from the name of the function. This
///   could result in unexpected conflicts between concurrent_cached-functions of the same name, so it's
///   recommended that you specify a prefix you're sure will be unique.
/// - `create`: (optional, string expr) specify an expression used to create a new cache store, e.g. `create = r##"{ CacheType::new() }"##`.
/// - `key`: (optional, string type) specify what type to use for the cache key, e.g. `key = "u32"`.
///   When `key` is specified, `convert` must also be specified.
/// - `convert`: (optional, string expr) specify an expression used to convert function arguments to a cache
///   key, e.g. `convert = r##"{ format!("{}:{}", arg1, arg2) }"##`. When `convert` is specified,
///   `key` or `ty` must also be set.
/// - `cache_none`: (optional, bool) If your function returns an `Option<T>`, also cache `None` values.
///   By default `None` is returned without being stored; set `cache_none = true` to store `None` as well.
///   Only supported on the default in-memory sharded path; combining it with `redis`/`disk`/custom `ty` is a compile error.
///   **Note:** when `cache_none = true`, the underlying store holds `Option<T>` as its value type,
///   so a direct `.cache_get()` call returns `Option<Option<T>>` - the outer `Option` is the
///   cache hit/miss indicator; the inner `Option` is the cached value.
/// - `cache_err`: (optional, bool) If your function returns a `Result<T, E>`, also cache `Err` values.
///   By default only `Ok(T)` is cached; set `cache_err = true` to store `Err` values too.
///   Only supported on the default in-memory sharded path; combining it with `redis`/`disk`/custom `ty` is a compile error.
///   **Note:** when `cache_err = true`, the underlying store holds `Result<T, E>` as its value type,
///   so a direct `.cache_get()` call returns `Option<Result<T, E>>` - the outer `Option` is the
///   cache hit/miss indicator; the inner `Result` is the cached value.
/// - `result_fallback`: (optional, bool) If your function returns a `Result<T, E>`, on an `Err`
///   return the last cached `Ok` value for the same key is returned instead (wrapped back in
///   `Ok`). If there is no prior `Ok` for that key (e.g., the function has never succeeded or
///   the cache was cleared), the original `Err` is returned as-is. Refreshes are best-effort:
///   an `Ok` return refreshes the cache as usual; an `Err` return re-caches the stale value
///   with a fresh TTL window. **Note:** the stale value's TTL is refreshed on *every* `Err`
///   call - if the backend stays down indefinitely, the stale entry will never expire. `ttl`
///   bounds staleness under normal (transient) failure; it does not bound it under permanent
///   failure. This is useful for keeping the last successful result available during transient
///   failures, e.g. network disconnects.
///   **Requires `ttl`, `ttl_secs`, or `ttl_millis`** - only implemented on the expiry-capable sharded stores
///   (`ShardedTtlCache` and `ShardedLruTtlCache`). Setting `ttl`/`ttl_secs`/`ttl_millis` without `max_size` selects
///   `ShardedTtlCache`; with `max_size` selects `ShardedLruTtlCache`. Omitting all three is a compile error.
///   Mutually exclusive with `cache_err`, `with_cached_flag`, `expires = true`, `redis = true`,
///   `disk = true`, and custom `ty`/`create`.
///   Requires the cache key type to implement `Clone` (the fallback path re-caches the key). The
///   default key already satisfies this, so it only matters with a custom non-`Clone` `key`/`convert`.
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return`,
///   `Result<cached::Return<T>, E>`, or `Option<cached::Return<T>>`, the
///   `cached::Return.was_cached` flag will be updated when a cached value is returned.
///   The wrapper type **must** be `cached::Return` - either written fully
///   qualified, or imported from `cached` (`use cached::Return;`). A proc macro
///   only sees tokens, not resolved types: an unrelated type that merely happens
///   to be named `Return<T>` passes the attribute check but then fails to
///   compile in the generated body (it calls `::cached::Return::new` /
///   `.was_cached`). Use a different name for any non-`cached` `Return` type.
/// - `disk_dir`: (optional, string) in the case of `RedbCache` specify the directory in which the redb
///   database file is stored. Defaults to a system cache directory. Requires `disk = true`; mutually
///   exclusive with `create`. Using it on the in-memory or `redis` path is a compile error.
/// - `durable`: (optional, bool) in the case of `RedbCache` specify whether to synchronize the cache to disk
///   (fsync) on each cache change. Defaults to `true` (durable). Set `false` to trade durability for write
///   throughput. Requires `disk = true`; using it on the in-memory or `redis` path is a compile error.
///
/// ## Note
/// The `ty`, `create`, `key`, and `convert` attributes must be in a `String`
/// This is because darling, which is used for parsing the attributes, does not support directly parsing
/// attributes into `Type`s or `Block`s.
///
/// `sync_writes` is not supported by `#[concurrent_cached]`. Use `#[cached(sync_writes = ...)]` instead
/// if you need to serialize concurrent first-call execution.
///
/// **Generic functions** require `key` + `convert` (and a concrete store `ty`/`create`) to pin the
/// cache key/value to concrete types: the cache static is monomorphic and cannot name the
/// function's type parameters, so a generic function with the default key is a compile error. Wrap
/// it in a non-generic `#[concurrent_cached]` function per concrete type if you cannot supply a
/// concrete key.
#[proc_macro_attribute]
pub fn concurrent_cached(args: TokenStream, input: TokenStream) -> TokenStream {
    concurrent_cached::concurrent_cached(args, input)
}
