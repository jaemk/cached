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
/// - `size`: **deprecated** alias for `max_size` — using it emits a deprecation warning. Prefer `max_size`.
/// - `ttl`: (optional, u64) specify a cache TTL in seconds, implies the cache type is a `TtlCache` or `LruTtlCache` (requires the `time_stores` feature).
/// - `refresh`: (optional, bool) specify whether to refresh the TTL on cache hits.
/// - `sync_writes`: (optional, bool or string) specify whether to synchronize the execution and writing of uncached values.
///   When not specified or set to `false`, uncached calls execute without write synchronization. When set to `true`
///   or `"default"`, all keys synchronize by locking the whole cache during uncached execution. When set to
///   `"by_key"`, a per-key lock synchronizes uncached execution of duplicate keys only.
/// - `sync_writes_buckets`: (optional, usize) number of per-key lock buckets used by
///   `sync_writes = "by_key"`; defaults to 64. Each bucket is one `Arc<RwLock<()>>`. Keys
///   hash into a bucket, so two different keys may share a bucket and serialize unnecessarily
///   (false sharing). Increase this if you observe contention under high concurrency — a value
///   around 2–4× your expected peak concurrency eliminates most false sharing. Must be > 0.
/// - `sync_lock`: (optional, string) choose the generated cache lock. Defaults to `"rwlock"`. Use `"mutex"`
///   to force a mutex. `unsync_reads = true` requires an RwLock.
/// - `unsync_reads`: (optional, bool) use `CachedRead::cache_get_read` under a shared read lock for the initial
///   cache lookup, while keeping writes synchronized. This only works for stores that implement `CachedRead`;
///   recency-updating or refresh-on-hit stores intentionally do not. For non-mutating diagnostic lookups,
///   use the separate `CachedPeek` trait directly on stores.
/// - `ty`: (optional, string type) The cache store type to use. Defaults to `UnboundCache`. When `unbound` is
///   specified, defaults to `UnboundCache`. When `max_size` is specified, defaults to `LruCache`.
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
///   so a direct `.cache_get()` on the generated cache static returns `Option<Result<T, E>>` — the outer
///   `Option` is the cache hit/miss, the inner `Result` is the stored value.
/// - `cache_none`: (optional, bool) If your function returns an `Option`, also cache `None` values (by default only `Some` is cached).
///   **Note:** when `cache_none = true`, the underlying store holds `Option<T>` as its value type,
///   so a direct `.cache_get()` on the generated cache static returns `Option<Option<T>>` — the outer
///   `Option` is the cache hit/miss, the inner `Option` is the stored value.
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return`,
///   `Result<cached::Return<T>, E>`, or `Option<cached::Return<T>>`,
///   the `cached::Return.was_cached` flag will be updated when a cached value is returned.
///   The wrapper type **must** be `cached::Return` — either written fully
///   qualified, or imported from `cached` (`use cached::Return;`). A proc macro
///   only sees tokens, not resolved types: an unrelated type that merely happens
///   to be named `Return<T>` passes the attribute check but then fails to
///   compile in the generated body (it calls `::cached::Return::new` /
///   `.was_cached`). Use a different name for any non-`cached` `Return` type.
/// - `result_fallback`: (optional, bool) If your function returns a `Result` and it fails, the cache will instead refresh the recently expired `Ok` value.
///   In other words, refreshes are best-effort - returning `Ok` refreshes as usual but `Err` falls back to the last `Ok`.
///   This is useful, for example, for keeping the last successful result of a network operation even during network disconnects.
///   *Note*, this option requires the cache type to implement `CloneCached`. The compatible built-in options are:
///   `ttl` (uses `TtlCache`), `max_size` + `ttl` (uses `LruTtlCache`), and `expires` (uses `ExpiringCache`/`ExpiringLruCache`).
///   A custom `ty` that implements `CloneCached` is also accepted.
///   Requires a `Result<T, E>` return type. Mutually exclusive with `cache_err` and `sync_writes`.
///   Requires the cache key type to implement `Clone` (the fallback path re-caches the key). The
///   default key already satisfies this, so it only matters with a custom non-`Clone` `key`/`convert`.
/// - `expires`: (optional, bool) Auto-select an expiry-aware store whose entries expire based on
///   per-value logic rather than a single global TTL.
///   The return type must implement `Expires`; for `Result<T, E>` or `Option<T>` returns, the inner `T` must implement `Expires`.
///   Without `max_size`, uses `ExpiringCache` (unbounded).
///   With `max_size = N`, uses `ExpiringLruCache` (LRU-bounded to N entries).
///   Unlike `ttl`, expiry logic lives in each value — useful for caching OAuth tokens,
///   HTTP responses with `Cache-Control` headers, or any payload with its own expiration timestamp.
///   Compatible with `result_fallback`: on `Err`, returns the last-cached `Ok` value wrapped in `Ok(...)`,
///   even if that value's `is_expired()` returns `true`. Callers must check the value's expiry themselves
///   if they need to distinguish a fresh result from a stale fallback.
///   Mutually exclusive with `ttl`, `ty`, `create`, `with_cached_flag`, `unsync_reads`, `refresh`, and `unbound`.
///
/// ## Note
/// The `ty`, `create`, `key`, and `convert` attributes must be in a `String`
/// This is because darling, which is used for parsing the attributes, does not support directly parsing
/// attributes into `Type`s or `Block`s.
///
/// `Result`/`Option` detection is exact: the macro matches only the bare identifiers `Result`
/// and `Option` (including qualified forms like `std::result::Result<T, E>`). Type aliases are
/// never resolved, so an alias — even one named `MyResult` (`type MyResult<T> = Result<T, E>`) —
/// is treated as a plain return value and its `Err` / `None` will be cached. Return
/// `Result<T, E>` / `Option<T>` directly when you need the default Ok-only / Some-only behavior.
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
/// - `ttl`: (optional, u64) specify an expiry in seconds, after which the single cached value is
///   recomputed on the next call. `#[once]` always stores one value in an `Option` (timestamped
///   when `ttl` is set) — it is not a `TtlCache`/`LruTtlCache`.
/// - `sync_writes`: (optional, bool or string) specify whether to synchronize the execution of writing of uncached values.
///   When set to `true` or `"default"`, uncached execution is synchronized with the whole cache.
///   When omitted or set to `false`, uncached calls are not synchronized. `sync_writes = "by_key"`
///   is not supported by `#[once]` because a `#[once]` cache stores a single value for all arguments.
/// - `cache_err`: (optional, bool) If your function returns a `Result`, also cache `Err` values (by default only `Ok` is cached).
/// - `cache_none`: (optional, bool) If your function returns an `Option`, also cache `None` values (by default only `Some` is cached).
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return`,
///   `Result<cached::Return<T>, E>`, or `Option<cached::Return<T>>`,
///   the `cached::Return.was_cached` flag will be updated when a cached value is returned.
///   The wrapper type **must** be `cached::Return` — either written fully
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
///   is returned to the caller — subsequent calls will re-execute the function until it succeeds.
///   Mutually exclusive with `ttl` and `with_cached_flag`.
///
/// `Result`/`Option` detection is exact: the macro matches only the bare identifiers `Result`
/// and `Option` (including qualified forms like `std::result::Result<T, E>`). Type aliases are
/// never resolved, so an alias — even one named `MyResult` (`type MyResult<T> = Result<T, E>`) —
/// is treated as a plain return value and its `Err` / `None` will be cached. Return
/// `Result<T, E>` / `Option<T>` directly when you need the default Ok-only / Some-only behavior.
#[proc_macro_attribute]
pub fn once(args: TokenStream, input: TokenStream) -> TokenStream {
    once::once(args, input)
}

/// Define a memoized function using a cache store that implements `cached::ConcurrentCached` (and
/// `cached::ConcurrentCachedAsync` for async functions).
///
/// **The macro preserves the function's sync/async-ness — it does not make a function async.**
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
/// | (none) | `ShardedCache` — unbounded, no TTL |
/// | `max_size = N` | `ShardedLruCache` — LRU-bounded |
/// | `ttl = T` | `ShardedTtlCache` — TTL-expiring, unbounded (`time_stores` feature) |
/// | `max_size = N, ttl = T` | `ShardedLruTtlCache` — LRU + TTL (`time_stores` feature) |
/// | `expires = true` | `ShardedExpiringCache` — per-value expiry, unbounded |
/// | `expires = true, max_size = N` | `ShardedExpiringLruCache` — per-value expiry, LRU-bounded |
///
/// On the default in-memory path, do **not** specify `map_error` — the sharded stores are
/// infallible (`Error = Infallible`) and supplying `map_error` is a compile error.
/// Reserve `map_error` for `redis`/`disk`/custom `ty`/`create` stores where the error type is fallible.
/// Functions may return a plain `T`, `Option<T>`, or `Result<T, E>`. Plain values are
/// cached as-is. `Option<T>` skips caching `None` by default; use `cache_none = true`
/// to also cache `None`. `Result<T, E>` caches only successful `Ok(T)` values and returns
/// `Err(E)` without storing it; use `cache_err = true` to also cache `Err` values.
/// `result_fallback = true` is supported: on an `Err` return, the last cached `Ok` value
/// for the same key is returned instead (requires `ttl`).
///
/// Result detection is exact: the macro matches only the bare identifier `Result` (including
/// qualified forms like `std::result::Result<T, E>`). Type aliases are never resolved, so any
/// alias — even one whose name ends with `Result` (e.g. `type MyResult<T> = Result<T, E>`) —
/// is treated as a plain return value and its `Err` variant will be cached. Return `Result<T, E>`
/// directly when you need Ok-only caching behavior.
///
/// **Note:** `on_evict` callbacks are not available via `#[concurrent_cached]`. To use an
/// eviction callback, construct the store manually with its builder (e.g.
/// `ShardedLruCache::builder().max_size(N).on_evict(|k, v| { ... }).build()`) and supply it via
/// `ty`/`create` (see the `ty` and `create` attributes below).
///
/// **Note (async + method disambiguation):** When calling `ConcurrentCachedAsync` methods
/// (`.cache_get`, `.cache_set`, etc.) directly on an async sharded store, both
/// `ConcurrentCached` and `ConcurrentCachedAsync` are in scope and the compiler may report
/// "multiple applicable items in scope". Use fully-qualified syntax to disambiguate:
/// `ConcurrentCachedAsync::cache_get(&*STORE, &key).await`.
///
/// **Clone requirement:** When no `key` or `convert` attribute is specified, function arguments
/// are cloned to form the cache key tuple, so all argument types must implement `Clone`.
/// Use `key` + `convert` to map to an explicit key type and avoid the clone if needed.
///
/// # Attributes
/// - `map_error`: (required for `redis`/`disk` and custom `ty`/`create` stores; **not allowed**
///   on the default in-memory sharded path — those stores are infallible and supplying `map_error`
///   there is a compile error) a closure used to map store errors into the error type returned
///   by your function.
/// - `name`: (optional, string) specify the name for the generated cache, defaults to the function name uppercase.
/// - `redis`: (optional, bool) default to a `RedisCache` or `AsyncRedisCache`
/// - `disk`: (optional, bool) use a `DiskCache`, this must be set to true even if `type` and `create` are specified.
///   On an `async fn`, `sled`'s blocking I/O is run on `tokio`'s blocking pool via
///   `spawn_blocking` (so it does not stall the async runtime); this requires a Tokio
///   runtime context and surfaces a `DiskCacheError::BackgroundTaskFailed` if that task is
///   cancelled or panics.
/// - `max_size`: (optional, usize) total LRU capacity for the default in-memory store. Selects
///   `ShardedLruCache` (or `ShardedLruTtlCache` when combined with `ttl`). A compile error is
///   emitted when combined with `redis`, `disk`, or `create`.
///   **Note:** effective capacity may exceed `N` — shards enforce a 16-entry minimum floor, so
///   `max_size = 4` on an 8-shard build silently gives 128 effective slots. For a strict cap use
///   `shards = 1` or the builder's `per_shard_max_size`.
/// - `size`: **deprecated** alias for `max_size` — using it emits a deprecation warning. Prefer `max_size`.
/// - `ttl`: (optional, u64) TTL in seconds. For the default in-memory path, selects
///   `ShardedTtlCache` or `ShardedLruTtlCache` (requires the `time_stores` feature). For `redis`
///   and `disk` stores, sets the key/entry TTL on those backends.
/// - `shards`: (optional, usize) number of shards for the default in-memory store. Rounded up to
///   the next power of two. If omitted, defaults to `available_parallelism() × 4`, clamped to
///   8–1024; an explicit value is only rounded up to a power of two and is not clamped.
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
///   so a direct `.cache_get()` call returns `Option<Option<T>>` — the outer `Option` is the
///   cache hit/miss indicator; the inner `Option` is the cached value.
/// - `cache_err`: (optional, bool) If your function returns a `Result<T, E>`, also cache `Err` values.
///   By default only `Ok(T)` is cached; set `cache_err = true` to store `Err` values too.
///   Only supported on the default in-memory sharded path; combining it with `redis`/`disk`/custom `ty` is a compile error.
///   **Note:** when `cache_err = true`, the underlying store holds `Result<T, E>` as its value type,
///   so a direct `.cache_get()` call returns `Option<Result<T, E>>` — the outer `Option` is the
///   cache hit/miss indicator; the inner `Result` is the cached value.
/// - `result_fallback`: (optional, bool) If your function returns a `Result<T, E>`, on an `Err`
///   return the last cached `Ok` value for the same key is returned instead (wrapped back in
///   `Ok`). If there is no prior `Ok` for that key (e.g., the function has never succeeded or
///   the cache was cleared), the original `Err` is returned as-is. Refreshes are best-effort:
///   an `Ok` return refreshes the cache as usual; an `Err` return re-caches the stale value
///   with a fresh TTL window. **Note:** the stale value's TTL is refreshed on *every* `Err`
///   call — if the backend stays down indefinitely, the stale entry will never expire. `ttl`
///   bounds staleness under normal (transient) failure; it does not bound it under permanent
///   failure. This is useful for keeping the last successful result available during transient
///   failures, e.g. network disconnects.
///   **Requires `ttl`** — only implemented on the expiry-capable sharded stores (`ShardedTtlCache`
///   and `ShardedLruTtlCache`). Setting `ttl` without `max_size` selects `ShardedTtlCache`; with
///   `max_size` selects `ShardedLruTtlCache`. Omitting `ttl` is a compile error.
///   Mutually exclusive with `cache_err`, `with_cached_flag`, `expires = true`, `redis = true`,
///   `disk = true`, and custom `ty`/`create`.
///   Requires the cache key type to implement `Clone` (the fallback path re-caches the key). The
///   default key already satisfies this, so it only matters with a custom non-`Clone` `key`/`convert`.
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return`,
///   `Result<cached::Return<T>, E>`, or `Option<cached::Return<T>>`, the
///   `cached::Return.was_cached` flag will be updated when a cached value is returned.
///   The wrapper type **must** be `cached::Return` — either written fully
///   qualified, or imported from `cached` (`use cached::Return;`). A proc macro
///   only sees tokens, not resolved types: an unrelated type that merely happens
///   to be named `Return<T>` passes the attribute check but then fails to
///   compile in the generated body (it calls `::cached::Return::new` /
///   `.was_cached`). Use a different name for any non-`cached` `Return` type.
/// - `sync_to_disk_on_cache_change`: (optional, bool) in the case of `DiskCache` specify whether to synchronize the cache to disk each
///   time the cache changes.
/// - connection_config: (optional, string expr) specify an expression which returns a `sled::Config`
///   to give more control over the connection to the disk cache, i.e. useful for controlling the rate at which the cache syncs to disk.
///   See the docs of `cached::stores::DiskCacheBuilder::connection_config` for more info.
///
/// ## Note
/// The `ty`, `create`, `key`, and `convert` attributes must be in a `String`
/// This is because darling, which is used for parsing the attributes, does not support directly parsing
/// attributes into `Type`s or `Block`s.
///
/// `sync_writes` is not supported by `#[concurrent_cached]`. Use `#[cached(sync_writes = …)]` instead
/// if you need to serialize concurrent first-call execution.
#[proc_macro_attribute]
pub fn concurrent_cached(args: TokenStream, input: TokenStream) -> TokenStream {
    concurrent_cached::concurrent_cached(args, input)
}
