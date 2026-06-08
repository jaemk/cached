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
    #[error("Connection string not specified or invalid in env var {env_key:?}: {error:?}")]
    MissingConnectionString {
        env_key: String,
        error: std::env::VarError,
    },
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
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
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
    /// Will return a `RedisCacheBuildError`, depending on the error
    pub fn build(self) -> Result<RedisCache<K, V>, RedisCacheBuildError> {
        super::validate_ttl(self.ttl)?;
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

impl<K, V> RedisCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    #[allow(clippy::new_ret_no_self)]
    /// Initialize a `RedisCacheBuilder`.
    pub fn new<S: AsRef<str>>(prefix: S, ttl: Duration) -> RedisCacheBuilder<K, V> {
        RedisCacheBuilder::new(prefix, ttl)
    }

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
    #[error("Error deserializing cached value {cached_value:?}: {error:?}")]
    CacheDeserialization {
        cached_value: String,
        error: serde_json::Error,
    },
    #[error("Error serializing cached value: {error:?}")]
    CacheSerialization { error: serde_json::Error },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedRedisValue<V> {
    pub(crate) value: V,
    pub(crate) version: Option<u64>,
}
impl<V> CachedRedisValue<V> {
    fn new(value: V) -> Self {
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

#[cfg(all(
    feature = "async",
    any(
        feature = "redis_smol",
        feature = "redis_tokio",
        feature = "redis_connection_manager"
    )
))]
mod async_redis {
    use crate::time::Duration;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{
        CachedRedisValue, ConnectionString, DEFAULT_NAMESPACE, DeserializeOwned, Display, ENV_KEY,
        PhantomData, RedisCacheBuildError, RedisCacheError, Serialize,
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
        /// Initialize a `RedisCacheBuilder`
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
        #[must_use]
        pub fn ttl(mut self, ttl: Duration) -> Self {
            self.ttl = ttl;
            self
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

        /// The last step in building a `RedisCache` is to call `build()`
        ///
        /// # Errors
        ///
        /// Will return a `RedisCacheBuildError`, depending on the error
        pub async fn build(self) -> Result<AsyncRedisCache<K, V>, RedisCacheBuildError> {
            super::super::validate_ttl(self.ttl)?;
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

    impl<K, V> AsyncRedisCache<K, V>
    where
        // `V: Sync` is intentionally absent: `V` is sent across the async
        // boundary by value (insert/get-set return owned values; references
        // never escape the cache), so `Send` is sufficient.
        K: Display + Send + Sync,
        V: Serialize + DeserializeOwned + Send,
    {
        #[allow(clippy::new_ret_no_self)]
        /// Initialize an `AsyncRedisCacheBuilder`
        pub fn new<S: AsRef<str>>(prefix: S, ttl: Duration) -> AsyncRedisCacheBuilder<K, V> {
            AsyncRedisCacheBuilder::new(prefix, ttl)
        }

        /// Initialize an `AsyncRedisCacheBuilder`.
        pub fn builder<S: AsRef<str>>(prefix: S, ttl: Duration) -> AsyncRedisCacheBuilder<K, V> {
            AsyncRedisCacheBuilder::new(prefix, ttl)
        }

        fn generate_key(&self, key: &K) -> String {
            // Same format as the sync store — see `super::generate_redis_key`.
            super::generate_redis_key(&self.namespace, &self.prefix, &key.to_string())
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

        #[tokio::test]
        async fn test_async_redis_cache() {
            let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::new(
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
}

#[cfg(all(
    feature = "async",
    any(
        feature = "redis_smol",
        feature = "redis_tokio",
        feature = "redis_connection_manager"
    )
))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(
        feature = "async",
        any(
            feature = "redis_smol",
            feature = "redis_tokio",
            feature = "redis_connection_manager"
        )
    )))
)]
pub use async_redis::{AsyncRedisCache, AsyncRedisCacheBuilder};

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
        let c: RedisCache<u32, u32> = RedisCache::new(
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
        let c: RedisCache<u32, u32> = RedisCache::new(
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
