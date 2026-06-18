use crate::ConcurrentCached;
use crate::time::Duration;
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Display;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct RedisCacheBuilder<K, V> {
    ttl: Duration,
    refresh: bool,
    namespace: String,
    prefix: String,
    connection_string: Option<String>,
    pool_max_size: Option<u32>,
    pool_min_idle: Option<u32>,
    pool_max_lifetime: Option<Duration>,
    pool_idle_timeout: Option<Duration>,
    // fn-pointer phantom — see the rationale on `RedisCache::_phantom`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

const ENV_KEY: &str = "CACHED_REDIS_CONNECTION_STRING";
const DEFAULT_NAMESPACE: &str = "cached-redis-store:";

fn ttl_seconds(ttl: Duration) -> Result<u64, RedisCacheError> {
    if ttl.is_zero() {
        return Err(redis::RedisError::from((
            redis::ErrorKind::InvalidClientConfig,
            "invalid ttl: must be greater than zero",
            format!("got {ttl:?}"),
        ))
        .into());
    }
    // Redis only supports whole-second granularity. Round up so keys never
    // expire earlier than the caller requested (any non-zero duration yields >= 1).
    // Saturate rather than overflow on pathologically large durations, then clamp
    // to Redis' supported TTL range (`i64::MAX` seconds). Clamping here means the
    // same bounded value is used by both `SETEX` (cache_set) and `EXPIRE`
    // (refresh); an out-of-range value would otherwise be rejected by Redis.
    let secs = ttl
        .as_secs()
        .saturating_add(u64::from(ttl.subsec_nanos() > 0));
    Ok(secs.min(i64::MAX as u64))
}

fn ttl_seconds_i64(ttl: Duration) -> Result<i64, RedisCacheError> {
    // `ttl_seconds` is already clamped to `i64::MAX`, so this cast is lossless.
    Ok(ttl_seconds(ttl)? as i64)
}

/// Build the Redis key: `{namespace}:{prefix}:{key}`, colon-joined with empty
/// segments skipped and the namespace's trailing `:` trimmed.
///
/// **Data-impacting in 1.0** (see migration §8): pre-1.0 used raw
/// concatenation. Single source of truth shared by the sync and async stores
/// so the two formats cannot drift.
fn generate_redis_key(namespace: &str, prefix: &str, key: &str) -> String {
    let namespace = namespace.trim_end_matches(':');
    let cap = namespace.len()
        + if !namespace.is_empty() { 1 } else { 0 }
        + prefix.len()
        + if !prefix.is_empty() { 1 } else { 0 }
        + key.len();
    let mut out = String::with_capacity(cap);
    if !namespace.is_empty() {
        out.push_str(namespace);
        out.push(':');
    }
    if !prefix.is_empty() {
        out.push_str(prefix);
        out.push(':');
    }
    out.push_str(key);
    out
}

/// `SCAN MATCH` glob covering every key a cache with this `namespace`/`prefix`
/// writes: the [`generate_redis_key`] scope with a trailing `*`. Glob
/// metacharacters (`*`, `?`, `[`, `]`) and `\` in the namespace/prefix are
/// escaped so they match literally — otherwise a prefix like `cache[v2]` would
/// scan (and `cache_clear` would delete) keys outside this cache's scope.
/// Single source of truth shared by the sync and async stores.
fn clear_match_pattern(namespace: &str, prefix: &str) -> String {
    fn escape_glob(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if matches!(c, '*' | '?' | '[' | ']' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }
    generate_redis_key(&escape_glob(namespace), &escape_glob(prefix), "*")
}

#[cfg(test)]
mod clear_pattern_tests {
    // No Redis server needed — pins the `SCAN MATCH` pattern used by `cache_clear`.
    use super::clear_match_pattern;

    #[test]
    fn plain_segments_get_scope_and_trailing_star() {
        assert_eq!(clear_match_pattern("ns", "p"), "ns:p:*");
        assert_eq!(clear_match_pattern("", "p"), "p:*");
        assert_eq!(clear_match_pattern("ns", ""), "ns:*");
    }

    #[test]
    fn glob_metacharacters_in_segments_are_escaped() {
        // Unescaped, `cache[v2]` would glob-match keys outside this cache's
        // scope (e.g. `cachev:...`) and `cache_clear` would delete them.
        assert_eq!(clear_match_pattern("ns", "cache[v2]"), "ns:cache\\[v2\\]:*");
        assert_eq!(clear_match_pattern("n*s", "p?x"), "n\\*s:p\\?x:*");
        assert_eq!(clear_match_pattern("back\\slash", "p"), "back\\\\slash:p:*");
    }
}

#[cfg(test)]
mod generate_key_tests {
    // No Redis server needed — pins the data-impacting 1.0 key format (§8).
    use super::{DEFAULT_NAMESPACE, generate_redis_key};

    #[test]
    fn default_namespace_trailing_colon_trimmed_and_rejoined() {
        // The canonical §8 example.
        assert_eq!(
            generate_redis_key(DEFAULT_NAMESPACE, "my_prefix", "my_key"),
            "cached-redis-store:my_prefix:my_key"
        );
        // DEFAULT_NAMESPACE itself ends in ':' — only trailing colons are trimmed.
        assert!(DEFAULT_NAMESPACE.ends_with(':'));
    }

    #[test]
    fn empty_segments_are_skipped() {
        assert_eq!(generate_redis_key("", "p", "k"), "p:k");
        assert_eq!(generate_redis_key("ns", "", "k"), "ns:k");
        assert_eq!(generate_redis_key("", "", "k"), "k");
        assert_eq!(generate_redis_key(":", "", "k"), "k"); // ":" trims to empty
    }

    #[test]
    fn full_form_and_multiple_trailing_colons() {
        assert_eq!(generate_redis_key("ns", "p", "k"), "ns:p:k");
        // `trim_end_matches(':')` strips *all* trailing colons.
        assert_eq!(generate_redis_key("ns:::", "p", "k"), "ns:p:k");
        // Interior colons in segments are preserved.
        assert_eq!(generate_redis_key("a:b", "p", "k"), "a:b:p:k");
    }

    #[test]
    fn interior_colon_collision() {
        // Documents the known limitation: a namespace containing an interior colon
        // produces the same key as a shorter namespace with the remainder as a prefix.
        // See `namespace()` / `prefix()` doc notes on the builders.
        let with_interior = generate_redis_key("ns:evil", "", "k");
        let split_across = generate_redis_key("ns", "evil", "k");
        assert_eq!(
            with_interior, split_across,
            "interior colons can cause key collisions"
        );
    }
}

#[cfg(test)]
mod ttl_seconds_tests {
    // Pure-function coverage for the subtle Redis TTL normalization (reject
    // zero, round any sub-second remainder up, clamp to `i64::MAX`). These need
    // no Redis server and guard both the `SETEX` and `EXPIRE` paths, which
    // share `ttl_seconds`/`ttl_seconds_i64`.
    use super::{ttl_seconds, ttl_seconds_i64};
    use crate::time::Duration;

    #[test]
    fn zero_is_rejected() {
        assert!(ttl_seconds(Duration::ZERO).is_err());
        assert!(ttl_seconds_i64(Duration::ZERO).is_err());
    }

    #[test]
    fn whole_seconds_pass_through() {
        assert_eq!(ttl_seconds(Duration::from_secs(1)).unwrap(), 1);
        assert_eq!(ttl_seconds(Duration::from_secs(60)).unwrap(), 60);
        assert_eq!(ttl_seconds_i64(Duration::from_secs(60)).unwrap(), 60);
    }

    #[test]
    fn subsecond_rounds_up_to_one() {
        assert_eq!(ttl_seconds(Duration::from_nanos(1)).unwrap(), 1);
        assert_eq!(ttl_seconds(Duration::from_millis(1)).unwrap(), 1);
        assert_eq!(ttl_seconds(Duration::from_millis(999)).unwrap(), 1);
    }

    #[test]
    fn fractional_rounds_up() {
        assert_eq!(ttl_seconds(Duration::from_millis(1_500)).unwrap(), 2);
        assert_eq!(ttl_seconds(Duration::new(5, 1)).unwrap(), 6);
    }

    #[test]
    fn very_large_clamps_to_i64_max() {
        let huge = Duration::from_secs(u64::MAX);
        assert_eq!(ttl_seconds(huge).unwrap(), i64::MAX as u64);
        assert_eq!(ttl_seconds_i64(huge).unwrap(), i64::MAX);
    }
}

#[cfg(test)]
mod builder_ttl_setter_tests {
    // No Redis server needed -- these only inspect the builder's ttl field set by
    // the convenience setters, without calling `build()`.
    use super::RedisCacheBuilder;
    use crate::time::Duration;

    #[test]
    fn ttl_secs_and_ttl_millis_set_duration() {
        let b = RedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1)).ttl_secs(7);
        assert_eq!(b.ttl, Duration::from_secs(7));

        let b =
            RedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1)).ttl_millis(250);
        assert_eq!(b.ttl, Duration::from_millis(250));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        // ttl(secs=10) then ttl_secs(5) -> 5s
        let b = RedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1))
            .ttl(Duration::from_secs(10))
            .ttl_secs(5);
        assert_eq!(b.ttl, Duration::from_secs(5));

        // ttl_secs then ttl_millis -> the millis value
        let b = RedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1))
            .ttl_secs(10)
            .ttl_millis(500);
        assert_eq!(b.ttl, Duration::from_millis(500));

        // ttl_millis then ttl -> the ttl value
        let b = RedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1))
            .ttl_millis(500)
            .ttl(Duration::from_secs(3));
        assert_eq!(b.ttl, Duration::from_secs(3));
    }
}

#[cfg(test)]
mod builder_empty_scope_tests {
    // No Redis server needed -- verifies the empty-scope guard in `build()`.
    use super::{RedisCacheBuildError, RedisCacheBuilder};
    use crate::time::Duration;

    #[test]
    fn empty_namespace_and_prefix_is_rejected() {
        let result = RedisCacheBuilder::<String, String>::new("", Duration::from_secs(1))
            .namespace("")
            .build();
        assert!(
            matches!(result, Err(RedisCacheBuildError::EmptyScope)),
            "expected EmptyScope"
        );
    }

    #[test]
    fn namespace_all_colons_and_empty_prefix_is_rejected() {
        // ":::" trims to "" so the effective namespace is also empty.
        let result = RedisCacheBuilder::<String, String>::new("", Duration::from_secs(1))
            .namespace(":::")
            .build();
        assert!(
            matches!(result, Err(RedisCacheBuildError::EmptyScope)),
            "expected EmptyScope"
        );
    }

    #[test]
    fn non_empty_prefix_builds_ok() {
        // Guard must not fire when the prefix is set -- no real Redis needed
        // because the build error would come before the connection attempt.
        let result = RedisCacheBuilder::<String, String>::new("my-prefix", Duration::from_secs(1))
            .namespace("")
            .build();
        // The only failure here would be a missing connection string, not EmptyScope.
        assert!(
            !matches!(result, Err(RedisCacheBuildError::EmptyScope)),
            "EmptyScope must not fire when prefix is non-empty"
        );
    }
}

/// A Redis connection URL stored in memory with credentials redacted in `Debug`/`Display`.
///
/// The inner string (accessible via `.as_str()`) is the full URL including any password
/// and should not be logged or exposed in error messages.
#[derive(Clone)]
struct ConnectionString(String);

impl ConnectionString {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ConnectionString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED connection string]")
    }
}

impl std::fmt::Display for ConnectionString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED connection string]")
    }
}

use thiserror::Error;

#[non_exhaustive]
#[derive(Error, Debug)]
pub enum RedisCacheBuildError {
    #[error("redis connection error")]
    Connection(#[from] redis::RedisError),
    #[error("redis pool error")]
    Pool(#[from] r2d2::Error),
    #[error(transparent)]
    InvalidTtl(#[from] super::BuildError),
    #[error("Connection string not specified or invalid in env var {env_key:?}: {error}")]
    MissingConnectionString {
        env_key: String,
        #[source]
        error: std::env::VarError,
    },
    #[error(
        "empty scope: namespace (after trimming trailing colons) and prefix are both empty; \
        cache_clear would run SCAN MATCH * and delete every key in the Redis DB. \
        Set a non-empty namespace or prefix."
    )]
    EmptyScope,
}

impl<K, V> RedisCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `RedisCacheBuilder`
    pub fn new<S: AsRef<str>>(prefix: S, ttl: Duration) -> RedisCacheBuilder<K, V> {
        Self {
            ttl,
            refresh: false,
            namespace: DEFAULT_NAMESPACE.to_string(),
            prefix: prefix.as_ref().to_string(),
            connection_string: None,
            pool_max_size: None,
            pool_min_idle: None,
            pool_max_lifetime: None,
            pool_idle_timeout: None,
            _phantom: PhantomData,
        }
    }

    /// Specify the cache TTL as a `Duration`.
    /// Redis enforces whole-second granularity; sub-second non-zero TTLs round up to 1 second.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Specify the cache TTL in whole seconds. Equivalent to
    /// `ttl(Duration::from_secs(secs))`.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl_secs(self, secs: u64) -> Self {
        self.ttl(Duration::from_secs(secs))
    }

    /// Specify the cache TTL in milliseconds. Equivalent to
    /// `ttl(Duration::from_millis(millis))`.
    /// Redis enforces whole-second granularity; sub-second non-zero TTLs round up to 1 second.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl_millis(self, millis: u64) -> Self {
        self.ttl(Duration::from_millis(millis))
    }

    /// Specify whether cache hits refresh the TTL
    #[must_use]
    pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set the namespace for cache keys. Defaults to `cached-redis-store:`.
    /// Used to generate keys formatted as: `{namespace}:{prefix}:{key}`.
    /// Empty namespace values are omitted from the generated key.
    ///
    /// **Note:** colons in the namespace are not escaped. A namespace containing
    /// an interior colon (e.g. `"a:b"`) can produce keys that collide with a
    /// shorter namespace combined with a prefix (e.g. namespace `"a"`, prefix `"b"`).
    #[must_use]
    pub fn namespace<S: AsRef<str>>(mut self, namespace: S) -> Self {
        self.namespace = namespace.as_ref().to_string();
        self
    }

    /// Set the prefix for cache keys.
    /// Used to generate keys formatted as: `{namespace}:{prefix}:{key}`.
    /// Empty prefix values are omitted from the generated key.
    ///
    /// **Note:** colons in the prefix are not escaped and can cause key collisions
    /// with differently-split namespace/prefix combinations sharing the same segments.
    ///
    /// **Note:** the prefix is what scopes `cache_clear` to this logical cache.
    /// With an empty prefix, `cache_clear` matches `<namespace>:*` and will delete
    /// entries belonging to every cache that shares the same namespace. Set a unique
    /// prefix per logical cache to ensure `cache_clear` is scoped correctly.
    #[must_use]
    pub fn prefix<S: AsRef<str>>(mut self, prefix: S) -> Self {
        self.prefix = prefix.as_ref().to_string();
        self
    }

    /// Set the connection string for redis
    #[must_use]
    pub fn connection_string(mut self, cs: &str) -> Self {
        self.connection_string = Some(cs.to_string());
        self
    }

    /// Set the max size of the underlying redis connection pool
    #[must_use]
    pub fn connection_pool_max_size(mut self, max_size: u32) -> Self {
        self.pool_max_size = Some(max_size);
        self
    }

    /// Set the minimum number of idle redis connections that should be maintained by the
    /// underlying redis connection pool
    #[must_use]
    pub fn connection_pool_min_idle(mut self, min_idle: u32) -> Self {
        self.pool_min_idle = Some(min_idle);
        self
    }

    /// Set the max lifetime of connections used by the underlying redis connection pool
    #[must_use]
    pub fn connection_pool_max_lifetime(mut self, max_lifetime: Duration) -> Self {
        self.pool_max_lifetime = Some(max_lifetime);
        self
    }

    /// Set the max lifetime of idle connections maintained by the underlying redis connection pool
    #[must_use]
    pub fn connection_pool_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.pool_idle_timeout = Some(idle_timeout);
        self
    }

    /// Return the current connection string or load from the env var: `CACHED_REDIS_CONNECTION_STRING`
    ///
    /// # Errors
    ///
    /// Will return `RedisCacheBuildError::MissingConnectionString` if connection string is not set
    pub fn resolve_connection_string(&self) -> Result<String, RedisCacheBuildError> {
        match self.connection_string {
            Some(ref s) => Ok(s.to_string()),
            None => {
                std::env::var(ENV_KEY).map_err(|e| RedisCacheBuildError::MissingConnectionString {
                    env_key: ENV_KEY.to_string(),
                    error: e,
                })
            }
        }
    }

    fn create_pool(&self) -> Result<r2d2::Pool<redis::Client>, RedisCacheBuildError> {
        let s = self.resolve_connection_string()?;
        let client: redis::Client = redis::Client::open(s)?;
        // some pool-builder defaults are set when the builder is initialized
        // so we can't overwrite any values with Nones...
        let pool_builder = r2d2::Pool::builder();
        let pool_builder = if let Some(max_size) = self.pool_max_size {
            pool_builder.max_size(max_size)
        } else {
            pool_builder
        };
        let pool_builder = if let Some(min_idle) = self.pool_min_idle {
            pool_builder.min_idle(Some(min_idle))
        } else {
            pool_builder
        };
        let pool_builder = if let Some(max_lifetime) = self.pool_max_lifetime {
            pool_builder.max_lifetime(Some(max_lifetime))
        } else {
            pool_builder
        };
        let pool_builder = if let Some(idle_timeout) = self.pool_idle_timeout {
            pool_builder.idle_timeout(Some(idle_timeout))
        } else {
            pool_builder
        };

        let pool: r2d2::Pool<redis::Client> = pool_builder.build(client)?;
        Ok(pool)
    }

    /// The last step in building a `RedisCache` is to call `build()`
    ///
    /// # Errors
    ///
    /// - `InvalidTtl`: the configured TTL is zero.
    /// - `EmptyScope`: both the namespace (after trimming trailing colons) and
    ///   the prefix are empty. `cache_clear` would otherwise issue `SCAN MATCH *`
    ///   and delete every key in the Redis database.
    /// - `MissingConnectionString`: no connection string was set and the
    ///   `CACHED_REDIS_CONNECTION_STRING` env var is absent or invalid.
    /// - `Connection` / `Pool`: the Redis client or connection pool could not
    ///   be created.
    pub fn build(self) -> Result<RedisCache<K, V>, RedisCacheBuildError> {
        super::validate_ttl(self.ttl)?;
        if self.namespace.trim_end_matches(':').is_empty() && self.prefix.is_empty() {
            return Err(RedisCacheBuildError::EmptyScope);
        }
        Ok(RedisCache {
            ttl: Mutex::new(self.ttl),
            refresh: AtomicBool::new(self.refresh),
            connection_string: ConnectionString(self.resolve_connection_string()?),
            pool: self.create_pool()?,
            namespace: self.namespace,
            prefix: self.prefix,
            _phantom: PhantomData,
        })
    }
}

/// Cache store backed by redis
///
/// Values have a ttl applied and enforced by redis.
/// Uses an r2d2 connection pool under the hood.
pub struct RedisCache<K, V> {
    pub(super) ttl: Mutex<Duration>,
    pub(super) refresh: AtomicBool,
    pub(super) namespace: String,
    pub(super) prefix: String,
    connection_string: ConnectionString,
    pool: r2d2::Pool<redis::Client>,
    // `RedisCache` owns no live `K`/`V` — values are serialized to Redis and
    // `K`/`V` appear only in method signatures. Use a fn-pointer phantom so the
    // type is unconditionally `Send + Sync` regardless of whether `K`/`V` are
    // (e.g. a `V` containing a `Cell` is `Send` but `!Sync`). `#[concurrent_cached]`
    // emits a `LazyLock<RedisCache<_, _>>` static directly (no inner lock — the
    // `&self`-API of `ConcurrentCached` is self-synchronizing), so the cache
    // type must itself be `Sync` for the static to be `Sync`.
    // Variance is unchanged: covariant in `K` and `V`, same as `PhantomData<(K, V)>`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

impl<K, V> std::fmt::Debug for RedisCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCache")
            .field("namespace", &self.namespace)
            .field("prefix", &self.prefix)
            .field("ttl", &*self.ttl.lock())
            .field("refresh", &self.refresh.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<K, V> Clone for RedisCache<K, V> {
    /// Shallow clone - both handles share the same r2d2 connection pool
    /// (`r2d2::Pool` is `Arc`-backed). The `ttl` is snapshot into a fresh
    /// `Mutex` so the two handles can independently update their TTL view.
    fn clone(&self) -> Self {
        Self {
            ttl: Mutex::new(*self.ttl.lock()),
            refresh: AtomicBool::new(self.refresh.load(Ordering::Relaxed)),
            namespace: self.namespace.clone(),
            prefix: self.prefix.clone(),
            connection_string: self.connection_string.clone(),
            pool: self.pool.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<K, V> RedisCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `RedisCacheBuilder`.
    pub fn builder<S: AsRef<str>>(prefix: S, ttl: Duration) -> RedisCacheBuilder<K, V> {
        RedisCacheBuilder::new(prefix, ttl)
    }

    fn generate_key(&self, key: &K) -> String {
        // Format `{namespace}:{prefix}:{key}` — see `generate_redis_key`.
        // Changed from raw concatenation in 1.0 (migration §8): existing
        // pre-1.0 keys will not be hit after upgrading.
        generate_redis_key(&self.namespace, &self.prefix, &key.to_string())
    }

    /// `SCAN MATCH` glob covering every key this cache writes: the same
    /// `{namespace}:{prefix}:` scope as [`generate_key`](Self::generate_key) with a
    /// trailing `*`, with glob metacharacters in the segments escaped (see
    /// [`clear_match_pattern`]). Used by `cache_clear` to delete only this
    /// cache's entries.
    fn clear_match_pattern(&self) -> String {
        clear_match_pattern(&self.namespace, &self.prefix)
    }

    /// Return the redis connection string used.
    ///
    /// **Note:** the returned string may contain credentials (e.g. `redis://:password@host`).
    /// Do not log or expose it in error messages.
    #[must_use]
    pub fn connection_string(&self) -> String {
        self.connection_string.as_str().to_string()
    }
}

#[non_exhaustive]
#[derive(Error, Debug)]
pub enum RedisCacheError {
    #[error("redis error")]
    Redis(#[from] redis::RedisError),
    #[error("redis pool error")]
    Pool(#[from] r2d2::Error),
    #[error("Error deserializing cached value {cached_value:?}: {error}")]
    CacheDeserialization {
        cached_value: String,
        #[source]
        error: serde_json::Error,
    },
    #[error("Error serializing cached value: {error}")]
    CacheSerialization {
        #[source]
        error: serde_json::Error,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedRedisValue<V> {
    value: V,
    version: Option<u64>,
}
impl<V> CachedRedisValue<V> {
    fn new(value: V) -> Self {
        Self {
            value,
            version: Some(1),
        }
    }
}

/// Borrowed counterpart of [`CachedRedisValue`] used by `cache_set_ref` to
/// serialize from a `&V` without cloning. Serializes to the same JSON as
/// `CachedRedisValue::new(value)` (same field names and order).
#[derive(serde::Serialize)]
struct CachedRedisValueRef<'a, V> {
    value: &'a V,
    version: Option<u64>,
}
impl<'a, V> CachedRedisValueRef<'a, V> {
    fn new(value: &'a V) -> Self {
        Self {
            value,
            version: Some(1),
        }
    }
}

impl<K, V> ConcurrentCached<K, V> for RedisCache<K, V>
where
    K: Display + Clone,
    V: Serialize + DeserializeOwned,
{
    type Error = RedisCacheError;

    fn cache_get(&self, key: &K) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let mut pipe = redis::pipe();
        let key = self.generate_key(key);

        pipe.get(&key);
        if self.refresh.load(Ordering::Relaxed) {
            let ttl = *self.ttl.lock();
            pipe.expire(key, ttl_seconds_i64(ttl)?).ignore();
        }
        // ugh: https://github.com/mitsuhiko/redis-rs/pull/388#issuecomment-910919137
        let res: (Option<String>,) = pipe.query(&mut *conn)?;
        match res.0 {
            None => Ok(None),
            Some(s) => {
                let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                    RedisCacheError::CacheDeserialization {
                        cached_value: s,
                        error: e,
                    }
                })?;
                Ok(Some(v.value))
            }
        }
    }

    fn cache_set(&self, key: K, val: V) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let mut pipe = redis::pipe();
        let key = self.generate_key(&key);

        let ttl_secs = ttl_seconds(*self.ttl.lock())?;

        let val = CachedRedisValue::new(val);
        pipe.get(&key);
        pipe.set_ex::<String, String>(
            key,
            serde_json::to_string(&val)
                .map_err(|e| RedisCacheError::CacheSerialization { error: e })?,
            ttl_secs,
        )
        .ignore();

        let res: (Option<String>,) = pipe.query(&mut *conn)?;
        match res.0 {
            None => Ok(None),
            Some(s) => {
                let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                    RedisCacheError::CacheDeserialization {
                        cached_value: s,
                        error: e,
                    }
                })?;
                Ok(Some(v.value))
            }
        }
    }

    fn cache_remove(&self, key: &K) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let mut pipe = redis::pipe();
        let key = self.generate_key(key);

        pipe.get(&key);
        pipe.del::<String>(key).ignore();
        let res: (Option<String>,) = pipe.query(&mut *conn)?;
        match res.0 {
            None => Ok(None),
            Some(s) => {
                let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                    RedisCacheError::CacheDeserialization {
                        cached_value: s,
                        error: e,
                    }
                })?;
                Ok(Some(v.value))
            }
        }
    }

    /// Remove an entry and return the stored key and value.
    ///
    /// **Note:** Unlike in-memory stores, Redis manages TTL expiry server-side. A `GET` on a
    /// TTL-expired key returns nil, so this method returns `None` for expired entries even
    /// though the key may still be physically present in Redis. Use [`cache_delete`](ConcurrentCached::cache_delete)
    /// (which uses `DEL` directly) to reliably detect whether any physical entry was removed.
    fn cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        self.cache_remove(key)
            .map(|opt| opt.map(|v| (key.clone(), v)))
    }

    fn cache_delete(&self, key: &K) -> Result<bool, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let key = self.generate_key(key);
        let removed: usize = redis::cmd("DEL").arg(key).query(&mut *conn)?;
        Ok(removed > 0)
    }

    /// Remove every entry written by this cache instance.
    ///
    /// Scoped to this cache's `{namespace}:{prefix}:*` keyspace via `SCAN` +
    /// batched `DEL`. It is **not** a server `FLUSHDB`: keys outside this
    /// namespace/prefix are untouched, and entries written by other caches
    /// sharing the Redis server are preserved.
    ///
    /// Cost is **O(n)** in the number of matching keys (a cursored `SCAN`), so it
    /// is heavier than the in-memory `cache_clear`. New keys inserted concurrently
    /// during the scan may or may not be removed (standard `SCAN` semantics).
    ///
    /// **Note:** the `prefix` is what scopes a clear to a single logical cache. A
    /// cache built with an empty prefix but a non-empty namespace will match every
    /// key under that namespace on `cache_clear` (pattern `<namespace>:*`), which
    /// includes entries written by every other cache that shares the same namespace.
    /// Set a unique prefix per logical cache to avoid this.
    fn cache_clear(&self) -> Result<(), RedisCacheError> {
        let mut conn = self.pool.get()?;
        let pattern = self.clear_match_pattern();
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query(&mut *conn)?;
            if !keys.is_empty() {
                redis::cmd("DEL").arg(keys).query::<()>(&mut *conn)?;
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(())
    }

    /// Delegates to [`cache_clear`](crate::ConcurrentCached::cache_clear): the redis
    /// store tracks no in-memory metrics, so resetting is exactly clearing the
    /// entries (matching `RedbCache`, which also overrides both).
    fn cache_reset(&self) -> Result<(), RedisCacheError> {
        self.cache_clear()
    }

    fn ttl(&self) -> Option<Duration> {
        Some(*self.ttl.lock())
    }

    /// Set the TTL for newly inserted cache entries. Existing Redis keys are not affected;
    /// they retain whatever TTL was applied when they were originally inserted.
    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        let mut guard = self.ttl.lock();
        let old = *guard;
        *guard = ttl;
        Some(old)
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.refresh.swap(refresh, Ordering::Relaxed)
    }

    /// Redis cache entries always require a TTL. This method is a no-op and always returns `None`.
    fn unset_ttl(&self) -> Option<Duration> {
        None
    }
}

impl<K, V> crate::SerializeCached<K, V> for RedisCache<K, V>
where
    K: Display + Clone,
    V: Serialize + DeserializeOwned,
{
    /// Serializes from the borrowed `val` (no clone) and `SET`s it, returning the
    /// previous value if any. Equivalent to [`ConcurrentCached::cache_set`] but
    /// avoids taking ownership of `val`.
    fn cache_set_ref(&self, key: &K, val: &V) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let mut pipe = redis::pipe();
        let key = self.generate_key(key);

        let ttl_secs = ttl_seconds(*self.ttl.lock())?;

        let val = CachedRedisValueRef::new(val);
        pipe.get(&key);
        pipe.set_ex::<String, String>(
            key,
            serde_json::to_string(&val)
                .map_err(|e| RedisCacheError::CacheSerialization { error: e })?,
            ttl_secs,
        )
        .ignore();

        let res: (Option<String>,) = pipe.query(&mut *conn)?;
        match res.0 {
            None => Ok(None),
            Some(s) => {
                let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                    RedisCacheError::CacheDeserialization {
                        cached_value: s,
                        error: e,
                    }
                })?;
                Ok(Some(v.value))
            }
        }
    }
}

#[cfg(all(
    feature = "async",
    any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
        feature = "redis_async_cache",
        feature = "redis_connection_manager"
    )
))]
mod async_redis {
    use crate::time::Duration;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{
        CachedRedisValue, CachedRedisValueRef, ConnectionString, DEFAULT_NAMESPACE,
        DeserializeOwned, Display, ENV_KEY, PhantomData, RedisCacheBuildError, RedisCacheError,
        Serialize,
    };
    use crate::ConcurrentCachedAsync;
    #[cfg(feature = "redis_async_cache")]
    use redis::IntoConnectionInfo;

    pub struct AsyncRedisCacheBuilder<K, V> {
        ttl: Duration,
        refresh: bool,
        namespace: String,
        prefix: String,
        connection_string: Option<String>,
        #[cfg(feature = "redis_async_cache")]
        client_side_caching: bool,
        // fn-pointer phantom — see the rationale on `RedisCache::_phantom`.
        _phantom: PhantomData<fn() -> (K, V)>,
    }

    impl<K, V> AsyncRedisCacheBuilder<K, V>
    where
        K: Display,
        V: Serialize + DeserializeOwned,
    {
        /// Initialize a `AsyncRedisCacheBuilder`
        pub fn new<S: AsRef<str>>(prefix: S, ttl: Duration) -> AsyncRedisCacheBuilder<K, V> {
            Self {
                ttl,
                refresh: false,
                namespace: DEFAULT_NAMESPACE.to_string(),
                prefix: prefix.as_ref().to_string(),
                connection_string: None,
                #[cfg(feature = "redis_async_cache")]
                client_side_caching: false,
                _phantom: PhantomData,
            }
        }

        /// Specify the cache TTL as a `Duration`.
        /// Redis enforces whole-second granularity; sub-second non-zero TTLs round up to 1 second.
        ///
        /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
        #[must_use]
        pub fn ttl(mut self, ttl: Duration) -> Self {
            self.ttl = ttl;
            self
        }

        /// Specify the cache TTL in whole seconds. Equivalent to
        /// `ttl(Duration::from_secs(secs))`.
        ///
        /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
        #[must_use]
        pub fn ttl_secs(self, secs: u64) -> Self {
            self.ttl(Duration::from_secs(secs))
        }

        /// Specify the cache TTL in milliseconds. Equivalent to
        /// `ttl(Duration::from_millis(millis))`.
        /// Redis enforces whole-second granularity; sub-second non-zero TTLs round up to 1 second.
        ///
        /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
        #[must_use]
        pub fn ttl_millis(self, millis: u64) -> Self {
            self.ttl(Duration::from_millis(millis))
        }

        /// Specify whether cache hits refresh the TTL
        #[must_use]
        pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
            self.refresh = refresh;
            self
        }

        /// Set the namespace for cache keys. Defaults to `cached-redis-store:`.
        /// Used to generate keys formatted as: `{namespace}:{prefix}:{key}`.
        /// Empty namespace values are omitted from the generated key.
        ///
        /// **Note:** colons in the namespace are not escaped. A namespace containing
        /// an interior colon (e.g. `"a:b"`) can produce keys that collide with a
        /// shorter namespace combined with a prefix (e.g. namespace `"a"`, prefix `"b"`).
        #[must_use]
        pub fn namespace<S: AsRef<str>>(mut self, namespace: S) -> Self {
            self.namespace = namespace.as_ref().to_string();
            self
        }

        /// Set the prefix for cache keys.
        /// Used to generate keys formatted as: `{namespace}:{prefix}:{key}`.
        /// Empty prefix values are omitted from the generated key.
        ///
        /// **Note:** colons in the prefix are not escaped and can cause key collisions
        /// with differently-split namespace/prefix combinations sharing the same segments.
        ///
        /// **Note:** the prefix is what scopes `async_cache_clear` to this logical
        /// cache. With an empty prefix, `async_cache_clear` matches `<namespace>:*`
        /// and will delete entries belonging to every cache that shares the same
        /// namespace. Set a unique prefix per logical cache to ensure
        /// `async_cache_clear` is scoped correctly.
        #[must_use]
        pub fn prefix<S: AsRef<str>>(mut self, prefix: S) -> Self {
            self.prefix = prefix.as_ref().to_string();
            self
        }

        /// Set the connection string for redis
        #[must_use]
        pub fn connection_string(mut self, cs: &str) -> Self {
            self.connection_string = Some(cs.to_string());
            self
        }

        /// Enable client-side caching using RESP3 protocol
        #[cfg(feature = "redis_async_cache")]
        #[must_use]
        pub fn client_side_caching(mut self, enable: bool) -> Self {
            self.client_side_caching = enable;
            self
        }

        /// Return the current connection string or load from the env var: `CACHED_REDIS_CONNECTION_STRING`
        ///
        /// # Errors
        ///
        /// Will return `RedisCacheBuildError::MissingConnectionString` if connection string is not set
        pub fn resolve_connection_string(&self) -> Result<String, RedisCacheBuildError> {
            match self.connection_string {
                Some(ref s) => Ok(s.to_string()),
                None => std::env::var(ENV_KEY).map_err(|e| {
                    RedisCacheBuildError::MissingConnectionString {
                        env_key: ENV_KEY.to_string(),
                        error: e,
                    }
                }),
            }
        }

        /// Create a multiplexed redis connection. This is a single connection that can
        /// be used asynchronously by multiple futures.
        #[cfg(not(feature = "redis_connection_manager"))]
        async fn create_multiplexed_connection(
            &self,
        ) -> Result<redis::aio::MultiplexedConnection, RedisCacheBuildError> {
            let s = self.resolve_connection_string()?;

            #[cfg(feature = "redis_async_cache")]
            if self.client_side_caching {
                let mut connection_info = s.into_connection_info()?;

                let mut config = redis::AsyncConnectionConfig::default();
                let redis_settings = connection_info
                    .redis_settings()
                    .clone()
                    .set_protocol(redis::ProtocolVersion::RESP3);
                connection_info = connection_info.set_redis_settings(redis_settings);
                config = config.set_cache_config(redis::caching::CacheConfig::default());
                let client = redis::Client::open(connection_info)?;
                let conn = client
                    .get_multiplexed_async_connection_with_config(&config)
                    .await?;
                return Ok(conn);
            }

            let client = redis::Client::open(s)?;
            let conn = client.get_multiplexed_async_connection().await?;
            Ok(conn)
        }

        /// Create a multiplexed connection wrapped in a manager. The manager provides access
        /// to a multiplexed connection and will automatically reconnect to the server when
        /// necessary.
        #[cfg(feature = "redis_connection_manager")]
        async fn create_connection_manager(
            &self,
        ) -> Result<redis::aio::ConnectionManager, RedisCacheBuildError> {
            let s = self.resolve_connection_string()?;
            #[cfg(feature = "redis_async_cache")]
            if self.client_side_caching {
                let mut connection_info = s.into_connection_info()?;
                let redis_settings = connection_info
                    .redis_settings()
                    .clone()
                    .set_protocol(redis::ProtocolVersion::RESP3);
                connection_info = connection_info.set_redis_settings(redis_settings);
                let config = redis::aio::ConnectionManagerConfig::default()
                    .set_cache_config(redis::caching::CacheConfig::default());
                let client = redis::Client::open(connection_info)?;
                let conn = redis::aio::ConnectionManager::new_with_config(client, config).await?;
                return Ok(conn);
            }

            let client = redis::Client::open(s)?;
            let conn = redis::aio::ConnectionManager::new(client).await?;
            Ok(conn)
        }

        /// The last step in building an `AsyncRedisCache` is to call `build()`
        ///
        /// # Errors
        ///
        /// - `InvalidTtl`: the configured TTL is zero.
        /// - `EmptyScope`: both the namespace (after trimming trailing colons) and
        ///   the prefix are empty. `async_cache_clear` would otherwise issue
        ///   `SCAN MATCH *` and delete every key in the Redis database.
        /// - `MissingConnectionString`: no connection string was set and the
        ///   `CACHED_REDIS_CONNECTION_STRING` env var is absent or invalid.
        /// - `Connection`: the Redis client or multiplexed connection could not
        ///   be created.
        pub async fn build(self) -> Result<AsyncRedisCache<K, V>, RedisCacheBuildError> {
            super::super::validate_ttl(self.ttl)?;
            if self.namespace.trim_end_matches(':').is_empty() && self.prefix.is_empty() {
                return Err(RedisCacheBuildError::EmptyScope);
            }
            Ok(AsyncRedisCache {
                ttl: Mutex::new(self.ttl),
                refresh: AtomicBool::new(self.refresh),
                connection_string: ConnectionString(self.resolve_connection_string()?),
                #[cfg(not(feature = "redis_connection_manager"))]
                connection: self.create_multiplexed_connection().await?,
                #[cfg(feature = "redis_connection_manager")]
                connection: self.create_connection_manager().await?,
                namespace: self.namespace,
                prefix: self.prefix,
                _phantom: PhantomData,
            })
        }
    }

    /// Cache store backed by redis
    ///
    /// Values have a ttl applied and enforced by redis.
    /// Uses a `redis::aio::MultiplexedConnection` or `redis::aio::ConnectionManager`
    /// under the hood depending if feature `redis_connection_manager` is used or not.
    pub struct AsyncRedisCache<K, V> {
        pub(super) ttl: Mutex<Duration>,
        pub(super) refresh: AtomicBool,
        pub(super) namespace: String,
        pub(super) prefix: String,
        connection_string: ConnectionString,
        #[cfg(not(feature = "redis_connection_manager"))]
        connection: redis::aio::MultiplexedConnection,
        #[cfg(feature = "redis_connection_manager")]
        connection: redis::aio::ConnectionManager,
        // `AsyncRedisCache` owns no live `K`/`V` — see the rationale on
        // `RedisCache::_phantom`. Same fn-pointer phantom so a `Send`-but-`!Sync`
        // `V` (e.g. one containing a `Cell`) is usable, and the macro-emitted
        // `OnceCell<AsyncRedisCache<_, _>>` static stays `Sync` for the runtime
        // (the async path uses tokio's `OnceCell` rather than `LazyLock`).
        _phantom: PhantomData<fn() -> (K, V)>,
    }

    impl<K, V> std::fmt::Debug for AsyncRedisCache<K, V> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AsyncRedisCache")
                .field("namespace", &self.namespace)
                .field("prefix", &self.prefix)
                .field("ttl", &*self.ttl.lock())
                .field("refresh", &self.refresh.load(Ordering::Relaxed))
                .finish_non_exhaustive()
        }
    }

    impl<K, V> Clone for AsyncRedisCache<K, V> {
        /// Shallow clone - the underlying multiplexed connection or connection
        /// manager is `Clone` (internally `Arc`-backed). The `ttl` is snapshot
        /// into a fresh `Mutex` so the two handles can independently update
        /// their TTL view.
        fn clone(&self) -> Self {
            Self {
                ttl: Mutex::new(*self.ttl.lock()),
                refresh: AtomicBool::new(self.refresh.load(Ordering::Relaxed)),
                namespace: self.namespace.clone(),
                prefix: self.prefix.clone(),
                connection_string: self.connection_string.clone(),
                connection: self.connection.clone(),
                _phantom: PhantomData,
            }
        }
    }

    impl<K, V> AsyncRedisCache<K, V>
    where
        // `V: Sync` is intentionally absent: `V` is sent across the async
        // boundary by value (insert/get-set return owned values; references
        // never escape the cache), so `Send` is sufficient.
        K: Display + Send + Sync,
        V: Serialize + DeserializeOwned + Send,
    {
        /// Initialize an `AsyncRedisCacheBuilder`.
        pub fn builder<S: AsRef<str>>(prefix: S, ttl: Duration) -> AsyncRedisCacheBuilder<K, V> {
            AsyncRedisCacheBuilder::new(prefix, ttl)
        }

        fn generate_key(&self, key: &K) -> String {
            // Same format as the sync store — see `super::generate_redis_key`.
            super::generate_redis_key(&self.namespace, &self.prefix, &key.to_string())
        }

        /// `SCAN MATCH` glob covering every key this cache writes — the same
        /// `{namespace}:{prefix}:` scope with a trailing `*`, with glob
        /// metacharacters in the segments escaped (see
        /// [`clear_match_pattern`](super::clear_match_pattern)). Used by
        /// `async_cache_clear`.
        fn clear_match_pattern(&self) -> String {
            super::clear_match_pattern(&self.namespace, &self.prefix)
        }

        /// Return the redis connection string used.
        ///
        /// **Note:** the returned string may contain credentials (e.g. `redis://:password@host`).
        /// Do not log or expose it in error messages.
        #[must_use]
        pub fn connection_string(&self) -> String {
            self.connection_string.as_str().to_string()
        }
    }

    impl<K, V> ConcurrentCachedAsync<K, V> for AsyncRedisCache<K, V>
    where
        // `V: Sync` not needed — values cross the async boundary by value, never
        // by shared reference. Matches the async `RedbCache` impl.
        K: Display + Clone + Send + Sync,
        V: Serialize + DeserializeOwned + Send,
    {
        type Error = RedisCacheError;

        /// Get a cached value
        async fn async_cache_get(&self, key: &K) -> Result<Option<V>, Self::Error> {
            let mut conn = self.connection.clone();
            let mut pipe = redis::pipe();
            let key = self.generate_key(key);

            pipe.get(&key);
            if self.refresh.load(Ordering::Relaxed) {
                let ttl = *self.ttl.lock();
                pipe.expire(key, super::ttl_seconds_i64(ttl)?).ignore();
            }
            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                        RedisCacheError::CacheDeserialization {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Set a cached value
        async fn async_cache_set(&self, key: K, val: V) -> Result<Option<V>, Self::Error> {
            let mut conn = self.connection.clone();
            let mut pipe = redis::pipe();
            let key = self.generate_key(&key);

            let ttl_secs = super::ttl_seconds(*self.ttl.lock())?;

            let val = CachedRedisValue::new(val);
            pipe.get(&key);
            pipe.set_ex::<String, String>(
                key,
                serde_json::to_string(&val)
                    .map_err(|e| RedisCacheError::CacheSerialization { error: e })?,
                ttl_secs,
            )
            .ignore();

            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                        RedisCacheError::CacheDeserialization {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Remove a cached value
        async fn async_cache_remove(&self, key: &K) -> Result<Option<V>, Self::Error> {
            let mut conn = self.connection.clone();
            let mut pipe = redis::pipe();
            let key = self.generate_key(key);

            pipe.get(&key);
            pipe.del::<String>(key).ignore();
            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                        RedisCacheError::CacheDeserialization {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Remove an entry and return the stored key and value.
        ///
        /// **Note:** Unlike in-memory stores, Redis manages TTL expiry server-side. A `GET` on a
        /// TTL-expired key returns nil, so this method returns `None` for expired entries even
        /// though the key may still be physically present in Redis. Use [`async_cache_delete`](ConcurrentCachedAsync::async_cache_delete)
        /// (which uses `DEL` directly) to reliably detect whether any physical entry was removed.
        async fn async_cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
            self.async_cache_remove(key)
                .await
                .map(|opt| opt.map(|v| (key.clone(), v)))
        }

        async fn async_cache_delete(&self, key: &K) -> Result<bool, Self::Error> {
            let mut conn = self.connection.clone();
            let key = self.generate_key(key);
            let removed: usize = redis::cmd("DEL").arg(key).query_async(&mut conn).await?;
            Ok(removed > 0)
        }

        /// Remove every entry written by this cache instance.
        ///
        /// Async counterpart of [`ConcurrentCached::cache_clear`](crate::ConcurrentCached::cache_clear)
        /// for `RedisCache`. Scoped to this cache's `{namespace}:{prefix}:*` keyspace
        /// via `SCAN` + batched `DEL`; it is **not** a server `FLUSHDB` and leaves keys
        /// outside this namespace/prefix untouched. Cost is **O(n)** in the number of
        /// matching keys (a cursored `SCAN`).
        ///
        /// **Note:** the `prefix` is what scopes a clear to a single logical cache. A
        /// cache built with an empty prefix but a non-empty namespace will match every
        /// key under that namespace on `async_cache_clear` (pattern `<namespace>:*`),
        /// which includes entries written by every other cache that shares the same
        /// namespace. Set a unique prefix per logical cache to avoid this.
        async fn async_cache_clear(&self) -> Result<(), Self::Error> {
            let mut conn = self.connection.clone();
            let pattern = self.clear_match_pattern();
            let mut cursor: u64 = 0;
            loop {
                let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(100)
                    .query_async(&mut conn)
                    .await?;
                if !keys.is_empty() {
                    redis::cmd("DEL")
                        .arg(keys)
                        .query_async::<()>(&mut conn)
                        .await?;
                }
                if next == 0 {
                    break;
                }
                cursor = next;
            }
            Ok(())
        }

        /// Delegates to
        /// [`async_cache_clear`](crate::ConcurrentCachedAsync::async_cache_clear): the
        /// redis store tracks no in-memory metrics, so resetting is exactly clearing
        /// the entries (matching `RedbCache`, which also overrides both).
        async fn async_cache_reset(&self) -> Result<(), Self::Error> {
            self.async_cache_clear().await
        }

        /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
        fn set_refresh_on_hit(&self, refresh: bool) -> bool {
            self.refresh.swap(refresh, Ordering::Relaxed)
        }

        /// Return the ttl of cached values (time to eviction).
        fn ttl(&self) -> Option<Duration> {
            Some(*self.ttl.lock())
        }

        /// Set the TTL for newly inserted cache entries. Existing Redis keys are not affected;
        /// they retain whatever TTL was applied when they were originally inserted.
        fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
            let mut guard = self.ttl.lock();
            let old = *guard;
            *guard = ttl;
            Some(old)
        }

        /// Redis cache entries always require a TTL. This method is a no-op and always returns `None`.
        fn unset_ttl(&self) -> Option<Duration> {
            None
        }
    }

    impl<K, V> crate::SerializeCachedAsync<K, V> for AsyncRedisCache<K, V>
    where
        K: Display + Clone + Send + Sync,
        V: Serialize + DeserializeOwned + Send,
    {
        /// Serializes from the borrowed `val` (no clone) and `SET`s it, returning
        /// the previous value if any. Async counterpart of
        /// [`SerializeCached::cache_set_ref`](crate::SerializeCached::cache_set_ref).
        ///
        /// Serialization happens eagerly (before the returned future is awaited) so
        /// the borrowed `&V` is never held across the `.await`, keeping the `V: Send`
        /// (not `Sync`) bound consistent with `async_cache_set`.
        fn async_cache_set_ref(
            &self,
            key: &K,
            val: &V,
        ) -> impl std::future::Future<Output = Result<Option<V>, Self::Error>> + Send {
            let mut conn = self.connection.clone();
            let key = self.generate_key(key);
            let ttl_secs = super::ttl_seconds(*self.ttl.lock());
            let serialized = serde_json::to_string(&CachedRedisValueRef::new(val))
                .map_err(|e| RedisCacheError::CacheSerialization { error: e });
            async move {
                let mut pipe = redis::pipe();
                let serialized = serialized?;
                let ttl_secs = ttl_secs?;
                pipe.get(&key);
                pipe.set_ex::<String, String>(key, serialized, ttl_secs)
                    .ignore();

                let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
                match res.0 {
                    None => Ok(None),
                    Some(s) => {
                        let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                            RedisCacheError::CacheDeserialization {
                                cached_value: s,
                                error: e,
                            }
                        })?;
                        Ok(Some(v.value))
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::time::Duration;
        use std::thread::sleep;

        fn now_millis() -> u128 {
            crate::time::SystemTime::now()
                .duration_since(crate::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        }

        // No Redis server needed -- verifies the empty-scope guard in async `build()`.
        #[tokio::test]
        async fn async_empty_namespace_and_prefix_is_rejected() {
            let result = AsyncRedisCacheBuilder::<String, String>::new("", Duration::from_secs(1))
                .namespace("")
                .build()
                .await;
            assert!(
                matches!(result, Err(RedisCacheBuildError::EmptyScope)),
                "expected EmptyScope"
            );
        }

        #[tokio::test]
        async fn test_async_redis_cache() {
            let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(
                format!("{}:async-redis-cache-test", now_millis()),
                Duration::from_secs(2),
            )
            .build()
            .await
            .unwrap();

            assert!(c.async_cache_get(&1).await.unwrap().is_none());

            assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
            assert!(c.async_cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(2, 500_000));
            assert!(c.async_cache_get(&1).await.unwrap().is_none());

            let old = ConcurrentCachedAsync::set_ttl(&c, Duration::from_secs(1)).unwrap();
            assert_eq!(2, old.as_secs());
            assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
            assert!(c.async_cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(1, 600_000));
            assert!(c.async_cache_get(&1).await.unwrap().is_none());

            ConcurrentCachedAsync::set_ttl(&c, Duration::from_secs(10)).unwrap();
            assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
            assert!(c.async_cache_set(2, 100).await.unwrap().is_none());
            assert_eq!(c.async_cache_get(&1).await.unwrap().unwrap(), 100);
            assert_eq!(c.async_cache_get(&1).await.unwrap().unwrap(), 100);
        }
    }

    #[cfg(test)]
    mod async_builder_ttl_setter_tests {
        // No Redis server needed -- these only inspect the builder's ttl field set by
        // the convenience setters, without calling `build()`.
        use super::AsyncRedisCacheBuilder;
        use crate::time::Duration;

        #[test]
        fn ttl_secs_and_ttl_millis_set_duration() {
            let b = AsyncRedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1))
                .ttl_secs(7);
            assert_eq!(b.ttl, Duration::from_secs(7));

            let b = AsyncRedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1))
                .ttl_millis(250);
            assert_eq!(b.ttl, Duration::from_millis(250));
        }

        #[test]
        fn ttl_setters_override_last_writer_wins() {
            // ttl_secs then ttl_millis -> the millis value
            let b = AsyncRedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1))
                .ttl_secs(10)
                .ttl_millis(500);
            assert_eq!(b.ttl, Duration::from_millis(500));

            // ttl_millis then ttl_secs -> the secs value
            let b = AsyncRedisCacheBuilder::<String, String>::new("p", Duration::from_secs(1))
                .ttl_millis(500)
                .ttl_secs(10);
            assert_eq!(b.ttl, Duration::from_secs(10));
        }
    }
}

#[cfg(all(
    feature = "async",
    any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
        feature = "redis_async_cache",
        feature = "redis_connection_manager"
    )
))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(
        feature = "async",
        any(
            feature = "redis_smol",
            feature = "redis_smol_native_tls",
            feature = "redis_smol_rustls",
            feature = "redis_tokio",
            feature = "redis_tokio_native_tls",
            feature = "redis_tokio_rustls",
            feature = "redis_async_cache",
            feature = "redis_connection_manager"
        )
    )))
)]
pub use async_redis::{AsyncRedisCache, AsyncRedisCacheBuilder};

#[cfg(test)]
mod error_source_tests {
    use std::error::Error;

    use super::{RedisCacheBuildError, RedisCacheError};

    /// `RedisCacheBuildError::MissingConnectionString` must expose its inner
    /// `VarError` via `Error::source()` (item 10).
    #[test]
    fn missing_connection_string_has_source() {
        let inner = std::env::VarError::NotPresent;
        let err = RedisCacheBuildError::MissingConnectionString {
            env_key: "TEST_KEY".to_string(),
            error: inner,
        };
        let source = err
            .source()
            .expect("MissingConnectionString must expose its inner VarError as source()");
        // Non-tautological: the source must be the actual inner VarError, whose
        // Display is the std message - not some other wrapped error.
        assert_eq!(
            source.to_string(),
            std::env::VarError::NotPresent.to_string(),
            "source() must be the inner VarError"
        );
        // The source must downcast to VarError, proving the #[source] wiring
        // points at the real inner field and not a re-stringified copy.
        assert!(
            source.downcast_ref::<std::env::VarError>().is_some(),
            "source() must downcast to std::env::VarError"
        );
    }

    /// Item 10: `MissingConnectionString`'s Display switched from `{error:?}`
    /// to `{error}`. The rendered message must read cleanly - the env key and
    /// the VarError's human message - with no `VarError { .. }` / `NotPresent`
    /// debug noise leaking into the user-facing string.
    #[test]
    fn missing_connection_string_display_is_clean() {
        let err = RedisCacheBuildError::MissingConnectionString {
            env_key: "CACHED_REDIS_CONNECTION_STRING".to_string(),
            error: std::env::VarError::NotPresent,
        };
        let rendered = err.to_string();

        // The env key is surfaced (it is formatted with {env_key:?}, so quoted).
        assert!(
            rendered.contains("CACHED_REDIS_CONNECTION_STRING"),
            "Display must name the env var; got: {rendered}"
        );
        // The inner error's *Display* message is present (the {error} switch).
        assert!(
            rendered.contains(&std::env::VarError::NotPresent.to_string()),
            "Display must include the VarError's human message; got: {rendered}"
        );
        // No Debug-form noise: the old `{error:?}` would have rendered the
        // variant name `NotPresent`. The Display form must not.
        assert!(
            !rendered.contains("NotPresent"),
            "Display must not leak the Debug variant name `NotPresent`; got: {rendered}"
        );
        assert!(
            !rendered.contains("VarError"),
            "Display must not leak the `VarError` type name; got: {rendered}"
        );
    }

    /// `RedisCacheError::CacheDeserialization` must expose its inner
    /// `serde_json::Error` via `Error::source()` (item 10).
    #[test]
    fn cache_deserialization_has_source() {
        let inner: serde_json::Error = serde_json::from_str::<u32>("not-json").unwrap_err();
        let inner_display = inner.to_string();
        let err = RedisCacheError::CacheDeserialization {
            cached_value: "not-json".to_string(),
            error: inner,
        };
        let source = err
            .source()
            .expect("CacheDeserialization must expose its inner serde_json::Error as source()");
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "source() must downcast to serde_json::Error"
        );
        // Item 10 Display switch to `{error}`: the rendered message embeds the
        // inner serde error's human Display text, and names the bad value.
        let rendered = err.to_string();
        assert!(
            rendered.contains(&inner_display),
            "Display must include the inner serde error message; got: {rendered}"
        );
        assert!(
            rendered.contains("not-json"),
            "Display must include the offending cached value; got: {rendered}"
        );
    }

    /// `RedisCacheError::CacheSerialization` must expose its inner
    /// `serde_json::Error` via `Error::source()` (item 10).
    #[test]
    fn cache_serialization_has_source() {
        // Construct a serde_json serialization error via a type that fails to serialize.
        #[derive(Debug)]
        struct Unserializable;
        impl serde::Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional failure"))
            }
        }
        let inner: serde_json::Error = serde_json::to_string(&Unserializable).unwrap_err();
        let inner_display = inner.to_string();
        let err = RedisCacheError::CacheSerialization { error: inner };
        let source = err
            .source()
            .expect("CacheSerialization must expose its inner serde_json::Error as source()");
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "source() must downcast to serde_json::Error"
        );
        // Item 10 Display switch to `{error}`: the inner serde error message is
        // embedded in the user-facing Display string.
        assert!(
            err.to_string().contains(&inner_display),
            "Display must include the inner serde error message; got: {}",
            err
        );
    }

    /// `RedisCache` is `Clone` (item 11) - compile-time bound check.
    #[allow(dead_code)]
    fn assert_clone<T: Clone>() {}
    #[allow(dead_code)]
    fn check_redis_cache_is_clone() {
        assert_clone::<super::RedisCache<String, String>>();
    }
    /// `AsyncRedisCache` is `Clone` (item 11) - compile-time bound check.
    #[cfg(all(
        feature = "async",
        any(
            feature = "redis_smol",
            feature = "redis_smol_native_tls",
            feature = "redis_smol_rustls",
            feature = "redis_tokio",
            feature = "redis_tokio_native_tls",
            feature = "redis_tokio_rustls",
            feature = "redis_async_cache",
            feature = "redis_connection_manager"
        )
    ))]
    #[allow(dead_code)]
    fn check_async_redis_cache_is_clone() {
        assert_clone::<super::AsyncRedisCache<String, String>>();
    }
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use crate::time::Duration;
    use std::thread::sleep;

    use super::*;

    fn now_millis() -> u128 {
        crate::time::SystemTime::now()
            .duration_since(crate::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    #[test]
    fn redis_cache() {
        let c: RedisCache<u32, u32> = RedisCache::builder(
            format!("{}:redis-cache-test", now_millis()),
            Duration::from_secs(2),
        )
        .namespace("in-tests:")
        .build()
        .unwrap();

        assert!(c.cache_get(&1).unwrap().is_none());

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(2, 500_000));
        assert!(c.cache_get(&1).unwrap().is_none());

        let old = ConcurrentCached::set_ttl(&c, Duration::from_secs(1)).unwrap();
        assert_eq!(2, old.as_secs());
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(1, 600_000));
        assert!(c.cache_get(&1).unwrap().is_none());

        ConcurrentCached::set_ttl(&c, Duration::from_secs(10)).unwrap();
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_set(2, 100).unwrap().is_none());
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
    }

    #[test]
    fn remove() {
        let c: RedisCache<u32, u32> = RedisCache::builder(
            format!("{}:redis-cache-test-remove", now_millis()),
            Duration::from_secs(3600),
        )
        .build()
        .unwrap();

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_set(2, 200).unwrap().is_none());
        assert!(c.cache_set(3, 300).unwrap().is_none());

        assert_eq!(100, c.cache_remove(&1).unwrap().unwrap());
    }
}
