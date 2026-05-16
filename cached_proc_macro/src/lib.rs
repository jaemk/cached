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
/// - `size`: (optional, usize) specify an LRU max size, implies the cache type is a `LruCache` or `LruTtlCache`.
/// - `ttl`: (optional, u64) specify a cache TTL in seconds, implies the cache type is a `TtlCache` or `LruTtlCache`.
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
///   specified, defaults to `UnboundCache`. When `size` is specified, defaults to `LruCache`.
///   When `ttl` is specified, defaults to `TtlCache`.
///   When `size` and `ttl` are specified, defaults to `LruTtlCache`. When `ty` is
///   specified, `create` must also be specified.
/// - `create`: (optional, string expr) specify an expression used to create a new cache store, e.g. `create = r##"{ CacheType::new() }"##`.
/// - `key`: (optional, string type) specify what type to use for the cache key, e.g. `key = "u32"`.
///   When `key` is specified, `convert` must also be specified.
/// - `convert`: (optional, string expr) specify an expression used to convert function arguments to a cache
///   key, e.g. `convert = r##"{ format!("{}:{}", arg1, arg2) }"##`. When `convert` is specified,
///   `key` or `ty` must also be set.
/// - `result`: (optional, bool) If your function returns a `Result`, only cache `Ok` values returned by the function.
/// - `option`: (optional, bool) If your function returns an `Option`, only cache `Some` values returned by the function.
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return` or `Result<cached::Return, E>`,
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
///   *Note*, this option requires the cache type implements `CloneCached`.
///
/// ## Note
/// The `ty`, `create`, `key`, and `convert` attributes must be in a `String`
/// This is because darling, which is used for parsing the attributes, does not support directly parsing
/// attributes into `Type`s or `Block`s.
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
/// - `result`: (optional, bool) If your function returns a `Result`, only cache `Ok` values returned by the function.
/// - `option`: (optional, bool) If your function returns an `Option`, only cache `Some` values returned by the function.
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return` or `Result<cached::Return, E>`,
///   the `cached::Return.was_cached` flag will be updated when a cached value is returned.
///   The wrapper type **must** be `cached::Return` — either written fully
///   qualified, or imported from `cached` (`use cached::Return;`). A proc macro
///   only sees tokens, not resolved types: an unrelated type that merely happens
///   to be named `Return<T>` passes the attribute check but then fails to
///   compile in the generated body (it calls `::cached::Return::new` /
///   `.was_cached`). Use a different name for any non-`cached` `Return` type.
#[proc_macro_attribute]
pub fn once(args: TokenStream, input: TokenStream) -> TokenStream {
    once::once(args, input)
}

/// Define a memoized function using a cache store that implements `cached::ConcurrentCached` (and
/// `cached::ConcurrentCachedAsync` for async functions)
///
/// # Attributes
/// - `map_error`: (string, expr closure) specify a closure used to map any IO-store errors into
///   the error type returned by your function.
/// - `name`: (optional, string) specify the name for the generated cache, defaults to the function name uppercase.
/// - `redis`: (optional, bool) default to a `RedisCache` or `AsyncRedisCache`
/// - `disk`: (optional, bool) use a `DiskCache`, this must be set to true even if `type` and `create` are specified.
///   On an `async fn`, `sled`'s blocking I/O is run on `tokio`'s blocking pool via
///   `spawn_blocking` (so it does not stall the async runtime); this requires a Tokio
///   runtime context and surfaces a `DiskCacheError::BackgroundTaskFailed` if that task is
///   cancelled or panics.
/// - `ttl`: (optional, u64) specify a cache TTL in seconds, applied to the backing concurrent store
///   (e.g. the Redis key expiry, or the `DiskCache` entry TTL). `#[concurrent_cached]` uses a
///   Redis/disk/custom `ConcurrentCached` store, not a `TtlCache`/`LruTtlCache`.
/// - `refresh`: (optional, bool) specify whether to refresh the TTL on cache hits.
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
/// - `with_cached_flag`: (optional, bool) If your function returns a `cached::Return` or `Result<cached::Return, E>`,
///   the `cached::Return.was_cached` flag will be updated when a cached value is returned.
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
#[proc_macro_attribute]
pub fn concurrent_cached(args: TokenStream, input: TokenStream) -> TokenStream {
    concurrent_cached::concurrent_cached(args, input)
}
