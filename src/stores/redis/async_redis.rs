use crate::time::Duration;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    CachedRedisValue, CachedRedisValueRef, ConnectionString, DEFAULT_NAMESPACE, DeserializeOwned,
    Display, ENV_KEY, PhantomData, RedisCacheBuildError, RedisCacheError, Serialize,
};
use crate::{
    ConcurrentCacheBase, ConcurrentCacheRefreshOnHit, ConcurrentCacheTtl, ConcurrentCachedAsync,
};
#[cfg(feature = "redis_async_cache")]
use redis::IntoConnectionInfo;

/// The async redis connection held by an [`AsyncRedisCache`].
///
/// The `Multiplexed` variant is always compiled; the `Manager`
/// (auto-reconnecting [`redis::aio::ConnectionManager`]) variant is compiled
/// in only when the `redis_connection_manager` feature is enabled.
///
/// Selecting the manager is a *per-cache runtime* choice made in
/// [`AsyncRedisCacheBuilder::build`] from the
/// [`connection_manager`](AsyncRedisCacheBuilder::connection_manager) flag,
/// not a build-wide behavior swap. That keeps the feature additive: enabling
/// `redis_connection_manager` anywhere in the dependency graph (Cargo unifies
/// features) only makes the option *available*; every existing cache stays
/// multiplexed (the 2.x default) unless its own builder opts in.
///
/// Both inner connection types implement [`redis::aio::ConnectionLike`] and
/// `Clone`, so this enum forwards to them and the command methods can keep
/// cloning `self.connection` uniformly.
#[derive(Clone)]
pub(crate) enum AsyncRedisConnection {
    Multiplexed(redis::aio::MultiplexedConnection),
    #[cfg(feature = "redis_connection_manager")]
    Manager(redis::aio::ConnectionManager),
}

impl redis::aio::ConnectionLike for AsyncRedisConnection {
    fn req_packed_command<'a>(
        &'a mut self,
        cmd: &'a redis::Cmd,
    ) -> redis::RedisFuture<'a, redis::Value> {
        match self {
            AsyncRedisConnection::Multiplexed(c) => c.req_packed_command(cmd),
            #[cfg(feature = "redis_connection_manager")]
            AsyncRedisConnection::Manager(c) => c.req_packed_command(cmd),
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        cmd: &'a redis::Pipeline,
        offset: usize,
        count: usize,
    ) -> redis::RedisFuture<'a, Vec<redis::Value>> {
        match self {
            AsyncRedisConnection::Multiplexed(c) => c.req_packed_commands(cmd, offset, count),
            #[cfg(feature = "redis_connection_manager")]
            AsyncRedisConnection::Manager(c) => c.req_packed_commands(cmd, offset, count),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            AsyncRedisConnection::Multiplexed(c) => c.get_db(),
            #[cfg(feature = "redis_connection_manager")]
            AsyncRedisConnection::Manager(c) => c.get_db(),
        }
    }
}

/// Builder for [`AsyncRedisCache`].
///
/// **Feature:** requires an async runtime feature: one of `redis_tokio`,
/// `redis_tokio_native_tls`, `redis_tokio_rustls`, `redis_smol`, `redis_smol_native_tls`, or
/// `redis_smol_rustls`. The capability features `redis_async_cache` /
/// `redis_connection_manager` are additive opt-ins layered on top of a runtime; they do not
/// provide `AsyncRedisCache` on their own.
///
/// **No connection-pool settings:** unlike [`RedisCacheBuilder`](crate::stores::RedisCacheBuilder),
/// there are no `connection_pool_*` methods here. The async cache does not use an r2d2
/// pool; it multiplexes all operations over a single pipelined connection (or the
/// auto-reconnecting connection manager when enabled via
/// `connection_manager(true)`), so pool sizing does not apply. Throughput is governed
/// by redis pipelining rather than a pool bound.
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
pub struct AsyncRedisCacheBuilder<K, V> {
    ttl: Option<Duration>,
    refresh: bool,
    namespace: String,
    prefix: Option<String>,
    connection_string: Option<String>,
    strict_deserialization: bool,
    #[cfg(feature = "redis_async_cache")]
    client_side_caching: bool,
    // Per-cache opt-in to the auto-reconnecting connection manager. Only
    // exists when the capability is compiled in; default `false` keeps the
    // 2.x multiplexed behavior even when the feature is enabled transitively.
    #[cfg(feature = "redis_connection_manager")]
    connection_manager: bool,
    // fn-pointer phantom — see the rationale on `RedisCache::_phantom`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

impl<K, V> Default for AsyncRedisCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> AsyncRedisCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize an `AsyncRedisCacheBuilder`.
    ///
    /// The key `prefix` is required; set it with [`prefix`](Self::prefix)
    /// before calling [`build`](Self::build) (or use
    /// [`AsyncRedisCache::builder`] to supply it positionally). The TTL is
    /// optional; when left unset, entries are stored without expiry. Set it
    /// with [`ttl`](Self::ttl) (or [`ttl_secs`](Self::ttl_secs) /
    /// [`ttl_millis`](Self::ttl_millis)).
    #[must_use]
    pub fn new() -> AsyncRedisCacheBuilder<K, V> {
        Self {
            ttl: None,
            refresh: false,
            namespace: DEFAULT_NAMESPACE.to_string(),
            prefix: None,
            connection_string: None,
            strict_deserialization: false,
            #[cfg(feature = "redis_async_cache")]
            client_side_caching: false,
            #[cfg(feature = "redis_connection_manager")]
            connection_manager: false,
            _phantom: PhantomData,
        }
    }

    /// Specify the cache TTL as a `Duration` (optional).
    ///
    /// TTL is stored with millisecond precision via `PSETEX`/`PEXPIRE`. When
    /// no TTL is set, entries are stored without expiry (a plain `SET`) and
    /// persist until explicitly removed. An explicitly-set TTL must be greater
    /// than zero (a zero TTL is rejected by [`build`](Self::build) with
    /// `InvalidValue`; use no TTL at all to disable expiry).
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
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
    /// TTL is stored with millisecond precision via `PSETEX`/`PEXPIRE`.
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
    /// Trailing colons are trimmed, so `"ns"` and `"ns:"` name the same
    /// namespace. An empty namespace still contributes its (empty) field, so
    /// a namespace-less cache writes `:{prefix}:{key}`.
    ///
    /// **Note:** colons in the namespace are percent-escaped (`:` becomes
    /// `%3A`, and `%` itself becomes `%25`), so a namespace containing an
    /// interior colon (e.g. `"a:b"`) cannot collide with a shorter namespace
    /// combined with a prefix (e.g. namespace `"a"`, prefix `"b"`). Escaping
    /// applies to the key layout only; the namespace is stored and reported
    /// unescaped.
    #[must_use]
    pub fn namespace<S: AsRef<str>>(mut self, namespace: S) -> Self {
        self.namespace = namespace.as_ref().to_string();
        self
    }

    /// Set the prefix for cache keys (required, non-empty).
    /// Used to generate keys formatted as: `{namespace}:{prefix}:{key}`.
    ///
    /// **Note:** colons in the prefix are percent-escaped, so a prefix
    /// containing a colon cannot collide with a differently-split
    /// namespace/prefix pair over the same characters.
    ///
    /// **Note:** the prefix is what distinguishes logical caches sharing a
    /// namespace, so give each of them a distinct prefix. An empty prefix is
    /// rejected by [`build`](Self::build) with `BuildError::InvalidValue`:
    /// two caches under the same namespace that both leave it empty would
    /// share one keyspace and one `async_cache_clear` scope
    /// (`<namespace>::*`), so each would read, overwrite and delete the
    /// other's entries.
    #[must_use]
    pub fn prefix<S: AsRef<str>>(mut self, prefix: S) -> Self {
        self.prefix = Some(prefix.as_ref().to_string());
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
    #[cfg_attr(docsrs, doc(cfg(feature = "redis_async_cache")))]
    #[must_use]
    pub fn client_side_caching(mut self, enable: bool) -> Self {
        self.client_side_caching = enable;
        self
    }

    /// Use the auto-reconnecting [`redis::aio::ConnectionManager`] instead of
    /// the default plain multiplexed connection for this cache.
    ///
    /// Default is `false` (a `redis::aio::MultiplexedConnection`, the 2.x
    /// behavior). Enabling the `redis_connection_manager` feature only makes
    /// this option *available*; it never changes a cache's behavior unless
    /// that cache's builder opts in here. This keeps the feature additive:
    /// because Cargo unifies features across the whole dependency graph, a
    /// distant transitive dependency turning the feature on must not silently
    /// switch your caches to the manager's auto-reconnect semantics.
    ///
    /// The manager composes with whichever async runtime feature you enable
    /// (`redis_tokio` or `redis_smol`); it does not force tokio.
    ///
    /// **Feature:** requires `redis_connection_manager`.
    #[cfg(feature = "redis_connection_manager")]
    #[cfg_attr(docsrs, doc(cfg(feature = "redis_connection_manager")))]
    #[must_use]
    pub fn connection_manager(mut self, yes: bool) -> Self {
        self.connection_manager = yes;
        self
    }

    /// Enable strict deserialization mode (default `false`).
    ///
    /// When `false` (the default), a corrupt or undecodable cached value on the
    /// `async_cache_get` path is self-healed: the entry is deleted and the call
    /// returns `Ok(None)`. When `true`, a deserialization failure on the read
    /// path returns `Err(RedisCacheError::CacheDeserialization { .. })`.
    ///
    /// In **both** modes, the previous value displaced by `async_cache_set` is
    /// silently discarded when it cannot be decoded (the write itself succeeds
    /// and `Ok(None)` is returned): the setting governs reads, not the
    /// best-effort previous-value decode on writes.
    #[must_use]
    pub fn strict_deserialization(mut self, strict: bool) -> Self {
        self.strict_deserialization = strict;
        self
    }

    /// Return the current connection string or load from the env var: `CACHED_REDIS_CONNECTION_STRING`.
    ///
    /// The value is wrapped in a redacting [`ConnectionString`](super::ConnectionString):
    /// its `Debug`/`Display` render `[REDACTED connection string]`, so it is
    /// safe to log or include in error messages. Call
    /// [`ConnectionString::reveal`](super::ConnectionString::reveal) to obtain
    /// the raw URL (including any embedded credentials) when needed.
    ///
    /// # Errors
    ///
    /// Will return `RedisCacheBuildError::MissingConnectionString` if connection string is not set
    pub fn resolve_connection_string(&self) -> Result<ConnectionString, RedisCacheBuildError> {
        match self.connection_string {
            Some(ref s) => Ok(ConnectionString(s.to_string())),
            None => std::env::var(ENV_KEY).map(ConnectionString).map_err(|e| {
                RedisCacheBuildError::MissingConnectionString {
                    env_key: ENV_KEY.to_string(),
                    // `NotUnicode` carries the raw connection string; redact it.
                    error: super::sanitize_var_error(e),
                }
            }),
        }
    }

    /// Returns `true` when the connection string URL explicitly pins `protocol=resp2`
    /// (or `protocol=2`) in the query string.
    ///
    /// The redis crate defaults to RESP2 when no `protocol=` param is present, so
    /// checking the parsed `ProtocolVersion` would incorrectly reject plain URLs.
    /// This helper inspects the raw query string instead and only rejects URLs that
    /// actively opt in to RESP2 — the scenarios where the user has explicitly
    /// signalled an intent incompatible with client-side caching.
    #[cfg(feature = "redis_async_cache")]
    fn url_pins_resp2(s: &str) -> bool {
        // Extract the query string fragment after `?` (if any) and scan for
        // `protocol=resp2` or `protocol=2` as a key=value pair.  A simple
        // string scan is sufficient because the redis crate's own `parse_protocol`
        // uses the same comparison logic.
        if let Some(query) = s.split('?').nth(1) {
            for pair in query.split('&') {
                if let Some(val) = pair.strip_prefix("protocol=") {
                    return val == "resp2" || val == "2";
                }
            }
        }
        false
    }

    /// Select and build the async connection for this cache.
    ///
    /// Runtime selection (not a compile-time feature fork): when the
    /// `redis_connection_manager` feature is compiled in AND this builder's
    /// [`connection_manager`](Self::connection_manager) flag is set, build a
    /// [`redis::aio::ConnectionManager`]; otherwise build a plain
    /// [`redis::aio::MultiplexedConnection`]. The default is multiplexed, so
    /// enabling the feature transitively never changes an existing cache.
    async fn create_connection(&self) -> Result<AsyncRedisConnection, RedisCacheBuildError> {
        #[cfg(feature = "redis_connection_manager")]
        if self.connection_manager {
            return Ok(AsyncRedisConnection::Manager(
                self.create_connection_manager().await?,
            ));
        }
        Ok(AsyncRedisConnection::Multiplexed(
            self.create_multiplexed_connection().await?,
        ))
    }

    /// Create a multiplexed redis connection. This is a single connection that can
    /// be used asynchronously by multiple futures.
    async fn create_multiplexed_connection(
        &self,
    ) -> Result<redis::aio::MultiplexedConnection, RedisCacheBuildError> {
        let s = self.resolve_connection_string()?;

        #[cfg(feature = "redis_async_cache")]
        if self.client_side_caching {
            // Reject URLs that explicitly pin RESP2 before parsing — client-side
            // caching requires RESP3 and silently downgrading would cause the
            // invalidation listener to no-op, serving stale data.
            if Self::url_pins_resp2(s.reveal()) {
                return Err(RedisCacheBuildError::Resp2DowngradeWithClientSideCaching);
            }

            // Sanitize any parse error so the raw URL (which may contain a
            // password) is not surfaced in the error's Display/Debug.
            let mut connection_info = s.reveal().into_connection_info().map_err(|_| {
                RedisCacheBuildError::connection(redis::RedisError::from((
                    redis::ErrorKind::InvalidClientConfig,
                    "failed to parse redis connection info (connection string redacted)",
                )))
            })?;

            let mut config = redis::AsyncConnectionConfig::default();
            let redis_settings = connection_info
                .redis_settings()
                .clone()
                .set_protocol(redis::ProtocolVersion::RESP3);
            connection_info = connection_info.set_redis_settings(redis_settings);
            config = config.set_cache_config(redis::caching::CacheConfig::default());
            let client = redis::Client::open(connection_info).map_err(|_| {
                RedisCacheBuildError::connection(redis::RedisError::from((
                    redis::ErrorKind::InvalidClientConfig,
                    "failed to open redis client (connection string redacted)",
                )))
            })?;
            // Sanitize the live-connection error: `Connection` has no blanket
            // `#[from]`, and a raw redis connect error's Debug can echo the URL.
            let conn = client
                .get_multiplexed_async_connection_with_config(&config)
                .await
                .map_err(|_| {
                    RedisCacheBuildError::connection(redis::RedisError::from((
                        redis::ErrorKind::Io,
                        "failed to establish redis connection (connection string redacted)",
                    )))
                })?;
            return Ok(conn);
        }

        // Non-client-side-caching path: sanitize the Client::open error too.
        let client = redis::Client::open(s.reveal()).map_err(|_| {
            RedisCacheBuildError::connection(redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "failed to open redis client (connection string redacted)",
            )))
        })?;
        // Sanitize the live-connection error (see the note above).
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|_| {
                RedisCacheBuildError::connection(redis::RedisError::from((
                    redis::ErrorKind::Io,
                    "failed to establish redis connection (connection string redacted)",
                )))
            })?;
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
            // Reject URLs that explicitly pin RESP2 before parsing — client-side
            // caching requires RESP3 and silently downgrading would cause the
            // invalidation listener to no-op, serving stale data.
            if Self::url_pins_resp2(s.reveal()) {
                return Err(RedisCacheBuildError::Resp2DowngradeWithClientSideCaching);
            }

            // Sanitize any parse error so the raw URL (which may contain a
            // password) is not surfaced in the error's Display/Debug.
            let mut connection_info = s.reveal().into_connection_info().map_err(|_| {
                RedisCacheBuildError::connection(redis::RedisError::from((
                    redis::ErrorKind::InvalidClientConfig,
                    "failed to parse redis connection info (connection string redacted)",
                )))
            })?;

            let redis_settings = connection_info
                .redis_settings()
                .clone()
                .set_protocol(redis::ProtocolVersion::RESP3);
            connection_info = connection_info.set_redis_settings(redis_settings);
            let config = redis::aio::ConnectionManagerConfig::default()
                .set_cache_config(redis::caching::CacheConfig::default());
            let client = redis::Client::open(connection_info).map_err(|_| {
                RedisCacheBuildError::connection(redis::RedisError::from((
                    redis::ErrorKind::InvalidClientConfig,
                    "failed to open redis client (connection string redacted)",
                )))
            })?;
            // Sanitize the live-connection error: `Connection` has no blanket
            // `#[from]`, and a raw redis connect error's Debug can echo the URL.
            let conn = redis::aio::ConnectionManager::new_with_config(client, config)
                .await
                .map_err(|_| {
                    RedisCacheBuildError::connection(redis::RedisError::from((
                        redis::ErrorKind::Io,
                        "failed to establish redis connection (connection string redacted)",
                    )))
                })?;
            return Ok(conn);
        }

        // Non-client-side-caching path: sanitize the Client::open error too.
        let client = redis::Client::open(s.reveal()).map_err(|_| {
            RedisCacheBuildError::connection(redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "failed to open redis client (connection string redacted)",
            )))
        })?;
        // Sanitize the live-connection error (see the note above).
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(|_| {
                RedisCacheBuildError::connection(redis::RedisError::from((
                    redis::ErrorKind::Io,
                    "failed to establish redis connection (connection string redacted)",
                )))
            })?;
        Ok(conn)
    }

    /// The last step in building an `AsyncRedisCache` is to call `build()`
    ///
    /// # Errors
    ///
    /// - `Build(BuildError::MissingRequired("prefix"))`: no key prefix was set.
    /// - `Build(BuildError::InvalidValue { field: "ttl", .. })`: an explicitly-set TTL is zero.
    /// - `Build(BuildError::InvalidValue { field: "prefix", .. })`: the prefix
    ///   is empty. The prefix is what distinguishes logical caches sharing a
    ///   namespace; two that both leave it empty would share one keyspace and
    ///   one `async_cache_clear` scope (`SCAN MATCH <namespace>::*`).
    /// - `MissingConnectionString`: no connection string was set and the
    ///   `CACHED_REDIS_CONNECTION_STRING` env var is absent or invalid.
    /// - `Connection`: the Redis client or the selected connection (multiplexed,
    ///   or the connection manager when `.connection_manager(true)` is set) could
    ///   not be created.
    ///
    /// The TTL is optional: when no TTL is set, entries are stored without
    /// expiry (a plain `SET`) and persist until explicitly removed.
    pub async fn build(self) -> Result<AsyncRedisCache<K, V>, RedisCacheBuildError> {
        // Validate required fields before any IO/connection attempt so the
        // missing-required error is returned without needing a server.
        if self.prefix.is_none() {
            return Err(super::super::BuildError::MissingRequired("prefix").into());
        }
        // TTL is optional. When unset, store entries with no expiry (zero
        // `Duration` sentinel). An explicitly-set TTL still must be > 0.
        let ttl = match self.ttl {
            Some(ttl) => {
                super::super::validate_ttl(ttl)?;
                ttl
            }
            None => Duration::ZERO,
        };
        // The prefix is what distinguishes logical caches sharing a namespace:
        // two that both leave it empty share one keyspace and one
        // `async_cache_clear` scope (`<namespace>::*`), so each reads,
        // overwrites and deletes the other's entries. Reject it outright.
        if self.prefix.as_deref().is_some_and(str::is_empty) {
            return Err(super::super::BuildError::InvalidValue {
                field: "prefix",
                reason: "prefix must be non-empty: it is what scopes cache_clear to this \
                         cache; with an empty prefix cache_clear would delete every key \
                         under the namespace",
            }
            .into());
        }
        let connection_string = self.resolve_connection_string()?;
        let connection = self.create_connection().await?;
        Ok(AsyncRedisCache {
            ttl: Mutex::new(ttl),
            refresh: AtomicBool::new(self.refresh),
            connection_string,
            connection,
            namespace: self.namespace,
            prefix: self.prefix.unwrap_or_default(),
            strict_deserialization: self.strict_deserialization,
            _phantom: PhantomData,
        })
    }
}

/// Async cache store backed by redis.
///
/// The TTL is optional and enforced by Redis itself: entries built with a
/// TTL expire server-side; entries built without one persist until
/// explicitly removed.
/// Uses a `redis::aio::MultiplexedConnection` by default, or a
/// `redis::aio::ConnectionManager` when the cache was built with
/// [`AsyncRedisCacheBuilder::connection_manager(true)`](AsyncRedisCacheBuilder::connection_manager)
/// (requires the `redis_connection_manager` feature). Enabling that feature
/// only makes the option available; it does not change the default.
///
/// Values use the same on-wire encoding, and keys the same escaped layout, as
/// [`RedisCache`](crate::stores::RedisCache); see its "Format stability"
/// section.
///
/// **Feature:** requires an async runtime feature: one of `redis_tokio`,
/// `redis_tokio_native_tls`, `redis_tokio_rustls`, `redis_smol`, `redis_smol_native_tls`, or
/// `redis_smol_rustls`. The capability features `redis_async_cache` /
/// `redis_connection_manager` are additive opt-ins layered on top of a runtime; they do not
/// provide `AsyncRedisCache` on their own.
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
pub struct AsyncRedisCache<K, V> {
    pub(super) ttl: Mutex<Duration>,
    pub(super) refresh: AtomicBool,
    pub(super) namespace: String,
    pub(super) prefix: String,
    connection_string: ConnectionString,
    // Always the enum: the manager is a per-cache runtime choice, not a
    // feature-driven type swap. Selected in `build()` from the builder's
    // `connection_manager` flag; defaults to `Multiplexed` (2.x behavior).
    connection: AsyncRedisConnection,
    strict_deserialization: bool,
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
            strict_deserialization: self.strict_deserialization,
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
    /// Initialize an `AsyncRedisCacheBuilder` with the required key `prefix`.
    ///
    /// The `prefix` namespaces every key this cache reads and writes; it can
    /// be overridden later via [`prefix`](AsyncRedisCacheBuilder::prefix). A
    /// TTL is optional (see [`ttl`](AsyncRedisCacheBuilder::ttl)); when unset,
    /// entries are stored without expiry.
    ///
    /// To construct a builder without supplying the prefix up front, use
    /// [`AsyncRedisCacheBuilder::new`] directly.
    #[must_use]
    pub fn builder(prefix: impl Into<String>) -> AsyncRedisCacheBuilder<K, V> {
        AsyncRedisCacheBuilder::new().prefix(prefix.into())
    }

    fn generate_key(&self, key: &K) -> String {
        // Same format as the sync store — see `super::generate_redis_key`.
        super::generate_redis_key(&self.namespace, &self.prefix, &key.to_string())
    }

    /// `SCAN MATCH` glob covering every key this cache writes — the same
    /// `{namespace}:{prefix}:` scope with a trailing `*`, with the fields
    /// percent-escaped exactly as the keys are and glob metacharacters
    /// escaped (see [`clear_match_pattern`](super::clear_match_pattern)).
    /// Used by `async_cache_clear`.
    fn clear_match_pattern(&self) -> String {
        super::clear_match_pattern(&self.namespace, &self.prefix)
    }

    /// Return the redis connection string as a [`ConnectionString`].
    ///
    /// `ConnectionString`'s `Debug`/`Display` render `[REDACTED connection string]`,
    /// so the returned value is safe to log or include in error messages.
    /// Call [`ConnectionString::reveal`](super::ConnectionString::reveal) to
    /// retrieve the raw URL when the full credentials are required.
    #[must_use]
    pub fn connection_string(&self) -> ConnectionString {
        self.connection_string.clone()
    }
}

impl<K, V> ConcurrentCacheBase for AsyncRedisCache<K, V> {
    type Error = RedisCacheError;
}

impl<K, V> ConcurrentCacheTtl for AsyncRedisCache<K, V> {
    /// Return the ttl of cached values (time to eviction), or `None` if expiry is disabled.
    fn ttl(&self) -> Option<Duration> {
        let ttl = *self.ttl.lock();
        if ttl.is_zero() { None } else { Some(ttl) }
    }

    /// Set the TTL for newly inserted cache entries, returning the previous TTL (or `None`
    /// if expiry was disabled). This call does not rewrite existing Redis keys; they retain
    /// whatever TTL was applied when they were originally inserted.
    ///
    /// With [`refresh_on_hit`](crate::ConcurrentCacheRefreshOnHit::refresh_on_hit) enabled,
    /// however, a `cache_get` hit re-applies the current TTL to the key it touched (via
    /// `PEXPIRE`), so a changed TTL does reach an existing key on its next hit.
    ///
    /// A zero `ttl` disables expiry — exactly equivalent to `unset_ttl`.
    /// Subsequent `async_cache_set` writes use a plain `SET` (no expiry), so the keys
    /// persist until explicitly removed. Use
    /// [`try_set_ttl`](crate::ConcurrentCacheTtl::try_set_ttl) if you want a zero TTL rejected.
    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        let mut guard = self.ttl.lock();
        let old = *guard;
        *guard = ttl;
        if old.is_zero() { None } else { Some(old) }
    }

    /// Disable expiry: subsequent `async_cache_set` writes store keys without a TTL
    /// (plain `SET`). Returns the previous TTL, or `None` if expiry was already disabled.
    fn unset_ttl(&self) -> Option<Duration> {
        let mut guard = self.ttl.lock();
        let old = *guard;
        *guard = Duration::ZERO;
        if old.is_zero() { None } else { Some(old) }
    }
}

impl<K, V> ConcurrentCacheRefreshOnHit for AsyncRedisCache<K, V> {
    fn refresh_on_hit(&self) -> bool {
        self.refresh.load(Ordering::Relaxed)
    }

    /// Set whether cache hits refresh the ttl of cached values, returning the previous flag value.
    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.refresh.swap(refresh, Ordering::Relaxed)
    }
}

impl<K, V> ConcurrentCachedAsync<K, V> for AsyncRedisCache<K, V>
where
    // `V: Sync` not needed — values cross the async boundary by value, never
    // by shared reference. Matches the async `RedbCache` impl.
    K: Display + Clone + Send + Sync,
    V: Serialize + DeserializeOwned + Send,
{
    /// Get a cached value
    async fn async_cache_get(&self, key: &K) -> Result<Option<V>, Self::Error> {
        let mut conn = self.connection.clone();
        let mut pipe = redis::pipe();
        let key_str = self.generate_key(key);

        pipe.get(&key_str);
        if self.refresh.load(Ordering::Relaxed) {
            let ttl = *self.ttl.lock();
            // A zero (disabled) TTL means entries are stored without expiry; skip the
            // refresh `PEXPIRE` so the key stays persistent (no TTL to renew).
            if !ttl.is_zero() {
                pipe.pexpire(&key_str, super::ttl_millis_i64(ttl)?).ignore();
            }
        }
        let res: (Option<Vec<u8>>,) = pipe
            .query_async(&mut conn)
            .await
            .map_err(RedisCacheError::redis)?;
        match res.0 {
            None => Ok(None),
            Some(bytes) => match super::deserialize_cached_redis_value(&bytes) {
                Ok(v) => Ok(Some(v.value)),
                Err(e) if !self.strict_deserialization => {
                    // Conditional self-heal delete (C6): only remove the key
                    // if its current value still equals the corrupt `bytes`
                    // we read, so a concurrent valid write is never clobbered.
                    let _: i64 = super::SELF_HEAL_CONDITIONAL_DEL
                        .key(&key_str)
                        .arg(&bytes)
                        .invoke_async(&mut conn)
                        .await
                        .map_err(RedisCacheError::redis)?;
                    let _ = e;
                    Ok(None)
                }
                Err(e) => Err(e),
            },
        }
    }

    /// Set a cached value
    async fn async_cache_set(&self, key: K, val: V) -> Result<Option<V>, Self::Error> {
        let mut conn = self.connection.clone();
        let mut pipe = redis::pipe();
        let key_str = self.generate_key(&key);

        let ttl = *self.ttl.lock();

        let val = CachedRedisValue::new(val);
        let serialized = rmp_serde::to_vec(&val).map_err(RedisCacheError::serialization)?;
        pipe.get(&key_str);
        if ttl.is_zero() {
            // Disabled TTL: write the key without expiry (plain `SET`).
            pipe.set::<String, Vec<u8>>(key_str, serialized).ignore();
        } else {
            pipe.pset_ex::<String, Vec<u8>>(key_str, serialized, super::ttl_millis(ttl)?)
                .ignore();
        }

        let res: (Option<Vec<u8>>,) = pipe
            .query_async(&mut conn)
            .await
            .map_err(RedisCacheError::redis)?;
        // REDIS-10: if the displaced previous value fails to decode, return Ok(None).
        Ok(res.0.and_then(|bytes| {
            super::deserialize_cached_redis_value::<V>(&bytes)
                .ok()
                .map(|v| v.value)
        }))
    }

    /// Remove a cached value.
    ///
    /// Returns the previous value stored under `key`, if any.
    ///
    /// The entry is always removed, regardless of whether the stored bytes can be
    /// deserialized. The behavior when the previous value fails to deserialize depends
    /// on the [`strict_deserialization`](AsyncRedisCacheBuilder::strict_deserialization) setting:
    ///
    /// - **Default (non-strict):** the corrupt entry is removed and the method returns
    ///   `Ok(None)` (the undecodable previous value is discarded).
    /// - **Strict (`strict_deserialization(true)`):** the corrupt entry is still removed
    ///   and the method returns `Err(RedisCacheError::CacheDeserialization { .. })`.
    async fn async_cache_remove(&self, key: &K) -> Result<Option<V>, Self::Error> {
        let mut conn = self.connection.clone();
        let mut pipe = redis::pipe();
        let key_str = self.generate_key(key);

        pipe.get(&key_str);
        pipe.del::<String>(key_str).ignore();
        let res: (Option<Vec<u8>>,) = pipe
            .query_async(&mut conn)
            .await
            .map_err(RedisCacheError::redis)?;
        match res.0 {
            None => Ok(None),
            Some(bytes) => match super::deserialize_cached_redis_value(&bytes) {
                Ok(v) => Ok(Some(v.value)),
                Err(_) if !self.strict_deserialization => Ok(None),
                Err(e) => Err(e),
            },
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
        let key_str = self.generate_key(key);
        let removed: usize = redis::cmd("DEL")
            .arg(key_str)
            .query_async(&mut conn)
            .await
            .map_err(RedisCacheError::redis)?;
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
    /// **Note:** the `prefix` is what scopes a clear to a single logical
    /// cache. Two caches sharing a namespace *and* a prefix share one
    /// keyspace and one clear scope, so each clears the other's entries. Set
    /// a unique prefix per logical cache to avoid this. An empty prefix is
    /// rejected at build time for the same reason.
    async fn async_cache_clear(&self) -> Result<(), Self::Error> {
        let mut conn = self.connection.clone();
        let pattern = self.clear_match_pattern();
        let mut cursor: u64 = 0;
        loop {
            // Binary-safe: see the sync `cache_clear`. Decoding as
            // `Vec<String>` aborted the clear on the first non-UTF-8 key in
            // scope and left this cache's own entries in place forever.
            let (next, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .map_err(RedisCacheError::redis)?;
            if !keys.is_empty() {
                redis::cmd("DEL")
                    .arg(keys)
                    .query_async::<()>(&mut conn)
                    .await
                    .map_err(RedisCacheError::redis)?;
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

    /// Returns `true` if the cache contains a non-expired entry for `key`.
    ///
    /// Delegates to [`async_cache_get`](ConcurrentCachedAsync::async_cache_get):
    /// the value is fetched and deserialized to determine presence. There is no
    /// separate Redis EXISTS round-trip in this implementation.
    ///
    /// **Note:** because it is a full `async_cache_get`, this is not a read-only
    /// probe when
    /// [`refresh_on_hit`](crate::ConcurrentCacheRefreshOnHit::refresh_on_hit) is
    /// enabled: a `true` result renews the entry's TTL (a `PEXPIRE` write) just
    /// as a real read would, and an undecodable entry is self-healed (deleted)
    /// and reported absent. `RedbCache` behaves the same way.
    async fn async_cache_contains(&self, k: &K) -> Result<bool, Self::Error>
    where
        Self: Sized + Sync,
        K: Sync,
    {
        self.async_cache_get(k).await.map(|v| v.is_some())
    }
}

impl<K, V> crate::SerializeCachedAsync<K, V> for AsyncRedisCache<K, V>
where
    K: Display + Clone + Send + Sync,
    V: Serialize + DeserializeOwned + Send,
{
    /// Serializes from the borrowed `val` (no clone) and `SET`s it. Async
    /// counterpart of
    /// [`SerializeCached::cache_set_ref`](crate::SerializeCached::cache_set_ref).
    /// Does not read back the previous value, so the write is a single
    /// round-trip (no GET).
    ///
    /// Serialization happens eagerly (before the returned future is awaited) so
    /// the borrowed `&V` is never held across the `.await`, keeping the `V: Send`
    /// (not `Sync`) bound consistent with `async_cache_set`.
    fn async_cache_set_ref(
        &self,
        key: &K,
        val: &V,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        let mut conn = self.connection.clone();
        let key = self.generate_key(key);
        let ttl = *self.ttl.lock();
        // Compute the milliseconds eagerly (only for a real, non-zero TTL) so any
        // error is surfaced before the future is awaited, matching the eager
        // serialization below.
        let ttl_ms = if ttl.is_zero() {
            Ok(None)
        } else {
            super::ttl_millis(ttl).map(Some)
        };
        let serialized = rmp_serde::to_vec(&CachedRedisValueRef::new(val))
            .map_err(RedisCacheError::serialization);
        async move {
            let serialized: Vec<u8> = serialized?;
            let ttl_ms = ttl_ms?;
            match ttl_ms {
                // Disabled TTL: write the key without expiry (plain `SET`).
                None => {
                    let _: () = redis::cmd("SET")
                        .arg(&key)
                        .arg(serialized)
                        .query_async(&mut conn)
                        .await
                        .map_err(RedisCacheError::redis)?;
                }
                Some(ttl_ms) => {
                    let _: () = redis::cmd("PSETEX")
                        .arg(&key)
                        .arg(ttl_ms)
                        .arg(serialized)
                        .query_async(&mut conn)
                        .await
                        .map_err(RedisCacheError::redis)?;
                }
            }
            Ok(())
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

    /// The raw Redis key a default-namespace cache with `prefix` writes for
    /// `key`. Built through the store's own key builder because the test
    /// prefixes carry a separator, which the key layout escapes.
    fn raw_key(prefix: &str, key: &str) -> String {
        super::super::generate_redis_key(super::super::DEFAULT_NAMESPACE, prefix, key)
    }

    // No Redis server needed -- verifies the empty-prefix guard in async `build()`.
    #[tokio::test]
    async fn async_empty_prefix_is_rejected() {
        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("")
            .ttl(Duration::from_secs(1))
            .build()
            .await;
        assert!(
            matches!(
                result,
                Err(RedisCacheBuildError::Build(
                    crate::stores::BuildError::InvalidValue {
                        field: "prefix",
                        ..
                    }
                ))
            ),
            "expected InvalidValue for empty prefix"
        );
    }

    /// S3: bad async URL with embedded password must not leak the password in
    /// the build error's Display or Debug. No Redis server needed.
    #[tokio::test]
    async fn async_bad_url_with_password_does_not_leak_password() {
        let secret = "async_super_secret_xyz";
        let bad_url = format!("not-redis://:{secret}@nonexistent-host:9999");

        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(&bad_url)
            .build()
            .await;

        let err = result.expect_err("build must fail with a bad async URL");
        let display = err.to_string();
        let debug = format!("{err:?}");

        assert!(
            !display.contains(secret),
            "Display must not expose the password; got: {display}"
        );
        assert!(
            !debug.contains(secret),
            "Debug must not expose the password; got: {debug}"
        );
        // Neither the full enum Display nor its Debug may echo the raw URL.
        assert!(
            !display.contains(&bad_url) && !debug.contains(&bad_url),
            "neither Display nor Debug may echo the raw URL; got display={display}, debug={debug}"
        );
        // A bad scheme fails at parse time and must surface as the sanitized
        // Connection variant (no server required).
        assert!(
            matches!(err, RedisCacheBuildError::Connection { .. }),
            "expected Connection error, got: {err:?}"
        );
    }

    /// S5: building an AsyncRedisCache with `client_side_caching` enabled and a
    /// URL that pins `protocol=resp2` must be rejected with
    /// `Resp2DowngradeWithClientSideCaching`. No Redis server needed.
    #[cfg(feature = "redis_async_cache")]
    #[tokio::test]
    async fn client_side_caching_rejects_resp2_url() {
        // A valid redis URL that explicitly pins RESP2 via the query parameter.
        let url_with_resp2 = "redis://127.0.0.1:6399?protocol=resp2";

        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(url_with_resp2)
            .client_side_caching(true)
            .build()
            .await;

        assert!(
            matches!(
                result,
                Err(RedisCacheBuildError::Resp2DowngradeWithClientSideCaching)
            ),
            "expected Resp2DowngradeWithClientSideCaching, got: {result:?}"
        );
    }

    /// S5: building an AsyncRedisCache with `client_side_caching` enabled and a
    /// URL that does NOT pin RESP2 must NOT be rejected for protocol reasons.
    /// (It may fail for other reasons like no server, but not for the RESP2 guard.)
    #[cfg(feature = "redis_async_cache")]
    #[tokio::test]
    async fn client_side_caching_accepts_resp3_url() {
        // A URL with `protocol=resp3` must pass the RESP2 guard.
        let url_with_resp3 = "redis://127.0.0.1:6399?protocol=resp3";

        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(url_with_resp3)
            .client_side_caching(true)
            .build()
            .await;

        // Must NOT be the RESP2 guard error. May fail for other reasons (no server).
        assert!(
            !matches!(
                result,
                Err(RedisCacheBuildError::Resp2DowngradeWithClientSideCaching)
            ),
            "resp3 URL must not trigger the RESP2 guard; got: {result:?}"
        );
    }

    /// S5: a URL without an explicit protocol query param also passes the RESP2 guard
    /// (the default is RESP2 inside the redis crate, but the guard only fires when
    /// the URL EXPLICITLY pins resp2).
    #[cfg(feature = "redis_async_cache")]
    #[tokio::test]
    async fn client_side_caching_accepts_url_without_protocol_param() {
        // No `protocol=` query param — must not trigger the guard.
        let url_plain = "redis://127.0.0.1:6399";

        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(url_plain)
            .client_side_caching(true)
            .build()
            .await;

        assert!(
            !matches!(
                result,
                Err(RedisCacheBuildError::Resp2DowngradeWithClientSideCaching)
            ),
            "plain URL must not trigger the RESP2 guard; got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_async_redis_cache() {
        let c: AsyncRedisCache<u32, u32> =
            AsyncRedisCache::builder(format!("{}:async-redis-cache-test", now_millis()))
                .ttl(Duration::from_secs(2))
                .build()
                .await
                .unwrap();

        assert!(c.async_cache_get(&1).await.unwrap().is_none());

        assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
        assert!(c.async_cache_get(&1).await.unwrap().is_some());

        // 500ms past the 2s ttl. `Duration::new`'s second argument is nanoseconds, so the
        // literal 500_000 this used to pass left a 500us margin between a local sleep and
        // redis's own expiry clock, which is not enough under load.
        sleep(Duration::from_millis(2_500));
        assert!(c.async_cache_get(&1).await.unwrap().is_none());

        let old = ConcurrentCacheTtl::set_ttl(&c, Duration::from_secs(1)).unwrap();
        assert_eq!(2, old.as_secs());
        assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
        assert!(c.async_cache_get(&1).await.unwrap().is_some());

        // 600ms past the 1s ttl; see the note above.
        sleep(Duration::from_millis(1_600));
        assert!(c.async_cache_get(&1).await.unwrap().is_none());

        ConcurrentCacheTtl::set_ttl(&c, Duration::from_secs(10)).unwrap();
        assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
        assert!(c.async_cache_set(2, 100).await.unwrap().is_none());
        assert_eq!(c.async_cache_get(&1).await.unwrap().unwrap(), 100);
        assert_eq!(c.async_cache_get(&1).await.unwrap().unwrap(), 100);
    }

    // Plant raw bytes at the given fully-qualified redis key via a sync
    // connection to the same server the async cache uses. Kept as a committed
    // helper (not an inline probe) so the async self-heal/REDIS-10 tests below
    // share one planting/inspection path.
    fn plant_raw(key: &str, bytes: &[u8]) {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(bytes)
            .query(&mut conn)
            .unwrap();
    }

    fn key_exists(key: &str) -> bool {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        redis::cmd("EXISTS").arg(key).query(&mut conn).unwrap()
    }

    fn delete_key(key: &str) {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let _: () = redis::cmd("DEL").arg(key).query(&mut conn).unwrap();
    }

    /// D2 (async): a corrupt entry self-heals — `async_cache_get` deletes it and
    /// returns `Ok(None)` — and a subsequent recompute produces a HIT. Async
    /// parity for the sync self-heal + recompute test.
    #[tokio::test]
    async fn async_cache_get_self_heals_and_recomputes_to_hit() {
        let prefix = format!("{}:async-selfheal", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        assert_eq!(
            c.async_cache_get(&1).await.unwrap(),
            None,
            "async self-heal returns a miss"
        );
        assert!(
            !key_exists(&key),
            "async self-heal must delete the corrupt key"
        );

        assert_eq!(c.async_cache_set(1, 88).await.unwrap(), None);
        assert_eq!(
            c.async_cache_get(&1).await.unwrap(),
            Some(88),
            "the read after recompute is a HIT"
        );

        delete_key(&key);
    }

    /// D2 (async): strict mode returns `Err(CacheDeserialization)` on a corrupt
    /// entry and leaves it in place (does not self-heal). Async parity for the
    /// sync strict-mode test.
    #[tokio::test]
    async fn async_cache_get_strict_mode_errors_and_keeps_corrupt_entry() {
        let prefix = format!("{}:async-strict", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .strict_deserialization(true)
            .build()
            .await
            .unwrap();

        let err = c
            .async_cache_get(&1)
            .await
            .expect_err("strict async must error on a corrupt entry");
        assert!(
            err.is_deserialization(),
            "expected CacheDeserialization, got: {err:?}"
        );
        assert!(
            key_exists(&key),
            "strict mode must NOT delete the corrupt key"
        );

        delete_key(&key);
    }

    /// REDIS-10 (async `async_cache_set`): a corrupt displaced previous value
    /// returns `Ok(None)`, not an error; the new value is written.
    #[tokio::test]
    async fn async_cache_set_displaced_corrupt_previous_returns_ok_none() {
        let prefix = format!("{}:async-redis10-set", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        let result = c.async_cache_set(1, 42).await;
        assert!(
            result.is_ok(),
            "async_cache_set must not error on corrupt displaced value; got: {result:?}"
        );
        assert!(
            result.unwrap().is_none(),
            "displaced corrupt value must yield Ok(None)"
        );
        assert_eq!(c.async_cache_get(&1).await.unwrap(), Some(42));

        delete_key(&key);
    }

    /// `async_cache_set_ref` returns `Ok(())` over any pre-existing (even
    /// corrupt) value: it does not read the previous value back, so an
    /// undecodable displaced value can never surface as an error. The new value
    /// is written from a borrow and readable.
    #[tokio::test]
    async fn async_cache_set_ref_over_corrupt_previous_returns_ok_unit() {
        use crate::SerializeCachedAsync;
        let prefix = format!("{}:async-redis10-setref", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        let val = 42u32;
        let result = c.async_cache_set_ref(&1, &val).await;
        assert!(
            result.is_ok(),
            "async_cache_set_ref must not error over a corrupt previous value; got: {result:?}"
        );
        assert_eq!(c.async_cache_get(&1).await.unwrap(), Some(42));

        delete_key(&key);
    }

    /// Security (async build path): the boxed `source` inside the sanitized
    /// `Connection { source }` must not expose the planted password through its
    /// own Debug/Display OR its full `source()` cause chain. The existing async
    /// leak test only checks the top-level enum Display/Debug; this walks the
    /// boxed source's whole chain, matching the sync
    /// `connection_boxed_source_debug_does_not_leak_password` coverage.
    #[tokio::test]
    async fn async_connection_boxed_source_chain_does_not_leak_password() {
        let secret = "async_boxed_chain_secret_qzx999";
        let bad_url = format!("not-redis://:{secret}@nonexistent-host:9999");

        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(&bad_url)
            .build()
            .await;

        let err = result.expect_err("build must fail with a bad async URL");
        let RedisCacheBuildError::Connection { source } = &err else {
            panic!("expected Connection error, got: {err:?}");
        };
        assert!(
            !format!("{source:?}").contains(secret),
            "boxed source Debug must not expose the password"
        );
        assert!(
            !source.to_string().contains(secret),
            "boxed source Display must not expose the password"
        );
        // Walk the full cause chain: neither the password nor the raw URL may
        // appear at any depth.
        let mut cause = source.source();
        while let Some(c) = cause {
            let rendered = format!("{c:?}{c}");
            assert!(
                !rendered.contains(secret),
                "cause chain must not expose the password; got: {rendered}"
            );
            assert!(
                !rendered.contains(&bad_url),
                "cause chain must not echo the raw URL; got: {rendered}"
            );
            cause = c.source();
        }
    }

    /// BUG-2 (async, default mode): `async_cache_remove` on a key with corrupt
    /// stored bytes returns `Ok(None)` and the key is deleted. The corrupt bytes
    /// must NOT surface as `Err(CacheDeserialization)` in default (non-strict) mode.
    #[tokio::test]
    async fn async_cache_remove_corrupt_default_mode_returns_ok_none() {
        let prefix = format!("{}:async-remove-corrupt-default", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        // Default mode: corrupt previous value must be silently discarded as Ok(None).
        let result = c.async_cache_remove(&1).await;
        assert!(
            result.is_ok(),
            "async_cache_remove must not error in default mode on corrupt entry; got: {result:?}"
        );
        assert!(
            result.unwrap().is_none(),
            "async_cache_remove must return Ok(None) for corrupt entry in default mode"
        );

        // The key must be gone after remove regardless of decode failure.
        assert!(
            !key_exists(&key),
            "async_cache_remove must delete the key even when bytes are corrupt"
        );
    }

    /// BUG-2 (async, strict mode): `async_cache_remove` on a key with corrupt
    /// stored bytes returns `Err(CacheDeserialization)` in strict mode AND the key
    /// is still deleted (the GET+DEL pipeline already ran atomically).
    #[tokio::test]
    async fn async_cache_remove_corrupt_strict_mode_returns_error_and_key_is_gone() {
        let prefix = format!("{}:async-remove-corrupt-strict", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .strict_deserialization(true)
            .build()
            .await
            .unwrap();

        // Strict mode: must return a deserialization error.
        let err = c
            .async_cache_remove(&1)
            .await
            .expect_err("strict async_cache_remove must return Err for corrupt entry");
        assert!(
            err.is_deserialization(),
            "expected CacheDeserialization error, got: {err:?}"
        );

        // The key must be gone -- the GET+DEL pipeline already ran.
        assert!(
            !key_exists(&key),
            "key must be deleted even when strict async_cache_remove errors"
        );
    }

    /// TEST-2 (connection-manager): building an `AsyncRedisCache` with
    /// `client_side_caching(true)`, `connection_manager(true)`, and a URL that
    /// pins `protocol=resp2` must be rejected with
    /// `Resp2DowngradeWithClientSideCaching`. This is the connection-manager
    /// sibling of `client_side_caching_rejects_resp2_url` (multiplexed path).
    /// No Redis server is required -- the guard fires before connection is attempted.
    #[cfg(all(feature = "redis_connection_manager", feature = "redis_async_cache"))]
    #[tokio::test]
    async fn connection_manager_client_side_caching_rejects_resp2_url() {
        let url_with_resp2 = "redis://127.0.0.1:6399?protocol=resp2";

        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(url_with_resp2)
            .client_side_caching(true)
            .connection_manager(true)
            .build()
            .await;

        assert!(
            matches!(
                result,
                Err(RedisCacheBuildError::Resp2DowngradeWithClientSideCaching)
            ),
            "expected Resp2DowngradeWithClientSideCaching on connection-manager path, got: {result:?}"
        );
    }

    /// TEST-2 (connection-manager, positive counterpart): with
    /// `connection_manager(true)` + `client_side_caching(true)` and a URL that
    /// pins `protocol=resp3`, the RESP2 guard must NOT fire on the
    /// connection-manager path. Without this the rejection test could pass
    /// vacuously (e.g. if the guard fired for every connection-manager build).
    /// It may still fail for other reasons (e.g. no server), just not the guard.
    #[cfg(all(feature = "redis_connection_manager", feature = "redis_async_cache"))]
    #[tokio::test]
    async fn connection_manager_client_side_caching_accepts_resp3_url() {
        let url_with_resp3 = "redis://127.0.0.1:6399?protocol=resp3";

        let result = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(url_with_resp3)
            .client_side_caching(true)
            .connection_manager(true)
            .build()
            .await;

        assert!(
            !matches!(
                result,
                Err(RedisCacheBuildError::Resp2DowngradeWithClientSideCaching)
            ),
            "resp3 URL must not trigger the RESP2 guard on the connection-manager path; got: {result:?}"
        );
    }

    /// BUG-2 (async, happy path): `async_cache_remove` on a VALID stored value
    /// returns `Ok(Some(value))` and the key is gone afterward (a follow-up get
    /// is a miss). Async parity for the sync remove-and-return contract.
    #[tokio::test]
    async fn async_cache_remove_valid_value_returns_some_and_deletes_key() {
        let prefix = format!("{}:async-remove-valid", now_millis());
        let key = raw_key(&prefix, "1");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
        assert_eq!(
            c.async_cache_remove(&1).await.unwrap(),
            Some(100),
            "async_cache_remove must return the stored value"
        );
        assert!(!key_exists(&key), "async_cache_remove must delete the key");
        assert_eq!(
            c.async_cache_get(&1).await.unwrap(),
            None,
            "get after remove must be a miss"
        );
    }

    /// BUG-2 (async, missing key): `async_cache_remove` on a key that was never
    /// stored returns `Ok(None)` (the `res.0 == None` branch), not an error.
    #[tokio::test]
    async fn async_cache_remove_missing_key_returns_ok_none() {
        let prefix = format!("{}:async-remove-missing", now_millis());
        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        let result = c.async_cache_remove(&12345).await;
        assert!(
            result.is_ok(),
            "async_cache_remove on a missing key must not error; got: {result:?}"
        );
        assert_eq!(
            result.unwrap(),
            None,
            "async_cache_remove on a missing key must return Ok(None)"
        );
    }

    /// BUG-2 (async `async_cache_remove_entry` delegation, valid value): returns
    /// `Ok(Some((key, value)))` and deletes the key. Pins that the entry variant
    /// forwards the key alongside the removed value.
    #[tokio::test]
    async fn async_cache_remove_entry_valid_returns_key_and_value() {
        let prefix = format!("{}:async-remove-entry-valid", now_millis());
        let key = raw_key(&prefix, "7");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        assert!(c.async_cache_set(7, 700).await.unwrap().is_none());
        assert_eq!(
            c.async_cache_remove_entry(&7).await.unwrap(),
            Some((7, 700)),
            "async_cache_remove_entry must return the key and the stored value"
        );
        assert!(
            !key_exists(&key),
            "async_cache_remove_entry must delete the key"
        );
    }

    /// BUG-2 (async `async_cache_remove_entry` delegation, default mode): corrupt
    /// bytes are discarded as `Ok(None)` (the entry delegates to
    /// `async_cache_remove`) and the key is deleted. Direct coverage of the
    /// delegation, previously only inferred.
    #[tokio::test]
    async fn async_cache_remove_entry_corrupt_default_mode_returns_ok_none() {
        let prefix = format!("{}:async-remove-entry-corrupt-default", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .build()
            .await
            .unwrap();

        let result = c.async_cache_remove_entry(&1).await;
        assert!(
            result.is_ok(),
            "async_cache_remove_entry must not error in default mode on corrupt entry; got: {result:?}"
        );
        assert_eq!(
            result.unwrap(),
            None,
            "async_cache_remove_entry must return Ok(None) for a corrupt entry in default mode"
        );
        assert!(
            !key_exists(&key),
            "async_cache_remove_entry must delete the key even when bytes are corrupt"
        );
    }

    /// BUG-2 (async `async_cache_remove_entry` delegation, strict mode): corrupt
    /// bytes surface as the exact `RedisCacheError::CacheDeserialization { .. }`
    /// variant (not merely any Err) and the key is still deleted. Direct coverage
    /// of the strict delegation, matching the variant by shape.
    #[tokio::test]
    async fn async_cache_remove_entry_corrupt_strict_mode_returns_error_and_key_is_gone() {
        let prefix = format!("{}:async-remove-entry-corrupt-strict", now_millis());
        let key = raw_key(&prefix, "1");
        plant_raw(&key, b"\xff\xfe\xfd");

        let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .connection_string("redis://127.0.0.1:6399")
            .strict_deserialization(true)
            .build()
            .await
            .unwrap();

        let err = c
            .async_cache_remove_entry(&1)
            .await
            .expect_err("strict async_cache_remove_entry must return Err for corrupt entry");
        assert!(
            matches!(err, RedisCacheError::CacheDeserialization { .. }),
            "expected the exact CacheDeserialization variant, got: {err:?}"
        );
        assert!(
            !key_exists(&key),
            "key must be deleted even when strict async_cache_remove_entry errors"
        );
    }

    // B5: url_pins_resp2 must be case-sensitive, matching redis-rs 1.x which
    // compares the `protocol=` query value with exact lowercase literals ("resp2",
    // "2").  A mixed-case value like "RESP2" must NOT be treated as pinning RESP2
    // because redis-rs would reject or ignore it rather than downgrading.
    #[cfg(feature = "redis_async_cache")]
    #[test]
    fn url_pins_resp2_is_case_sensitive() {
        // Exact lowercase matches: must return true.
        assert!(
            AsyncRedisCacheBuilder::<String, String>::url_pins_resp2(
                "redis://127.0.0.1:6379?protocol=resp2"
            ),
            "protocol=resp2 must be detected"
        );
        assert!(
            AsyncRedisCacheBuilder::<String, String>::url_pins_resp2(
                "redis://127.0.0.1:6379?protocol=2"
            ),
            "protocol=2 must be detected"
        );
        // Mixed-case: redis-rs does not recognize these, so we must not either.
        assert!(
            !AsyncRedisCacheBuilder::<String, String>::url_pins_resp2(
                "redis://127.0.0.1:6379?protocol=RESP2"
            ),
            "protocol=RESP2 (uppercase) must NOT be treated as pinning RESP2"
        );
        assert!(
            !AsyncRedisCacheBuilder::<String, String>::url_pins_resp2(
                "redis://127.0.0.1:6379?protocol=Resp2"
            ),
            "protocol=Resp2 (mixed case) must NOT be treated as pinning RESP2"
        );
        // RESP3 and no protocol: must return false.
        assert!(
            !AsyncRedisCacheBuilder::<String, String>::url_pins_resp2(
                "redis://127.0.0.1:6379?protocol=resp3"
            ),
            "protocol=resp3 must not be detected as RESP2"
        );
        assert!(
            !AsyncRedisCacheBuilder::<String, String>::url_pins_resp2("redis://127.0.0.1:6379"),
            "no protocol param must not be detected as RESP2"
        );
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
        let b = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_secs(7);
        assert_eq!(b.ttl, Some(Duration::from_secs(7)));

        let b = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_millis(250);
        assert_eq!(b.ttl, Some(Duration::from_millis(250)));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        // ttl_secs then ttl_millis -> the millis value
        let b = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_secs(10)
            .ttl_millis(500);
        assert_eq!(b.ttl, Some(Duration::from_millis(500)));

        // ttl_millis then ttl_secs -> the secs value
        let b = AsyncRedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_millis(500)
            .ttl_secs(10);
        assert_eq!(b.ttl, Some(Duration::from_secs(10)));
    }

    // Additivity guard (no Redis server needed). With `redis_connection_manager`
    // compiled in, merely enabling the feature must NOT switch a cache to the
    // manager: the builder's `connection_manager` flag defaults to `false`
    // (multiplexed, the 2.x behavior), and only `.connection_manager(true)`
    // flips it. `build()` reads exactly this flag to pick the `Multiplexed` vs
    // `Manager` variant, so the default-false here is what keeps the feature
    // additive across a unified dependency graph.
    #[cfg(feature = "redis_connection_manager")]
    #[test]
    fn connection_manager_defaults_false_and_flips() {
        let b = AsyncRedisCacheBuilder::<String, String>::new().prefix("p");
        assert!(
            !b.connection_manager,
            "default must be multiplexed (connection_manager == false) so enabling \
             the feature is additive and never swaps behavior on its own"
        );

        let b = b.connection_manager(true);
        assert!(
            b.connection_manager,
            ".connection_manager(true) must opt the cache into the manager"
        );

        let b = b.connection_manager(false);
        assert!(
            !b.connection_manager,
            ".connection_manager(false) must return to the multiplexed default"
        );
    }
}

#[cfg(test)]
mod async_connection_enum_tests {
    // No Redis server needed -- these assert the connection enum's variant
    // selection is a pure function of the builder flag, without opening a
    // socket. They lock in Part A's runtime (not feature) selection.
    use super::AsyncRedisConnection;

    /// The multiplexed variant is always present regardless of features; this
    /// is a compile-time proof that `AsyncRedisConnection::Multiplexed` exists
    /// and that the enum is the connection type the default build path wraps.
    #[allow(dead_code)]
    fn multiplexed_variant_exists(c: redis::aio::MultiplexedConnection) -> AsyncRedisConnection {
        AsyncRedisConnection::Multiplexed(c)
    }

    /// The manager variant is compiled in only with the feature; this proves
    /// the enum carries it (and that `.connection_manager(true)` has a variant
    /// to select) when `redis_connection_manager` is enabled.
    #[cfg(feature = "redis_connection_manager")]
    #[allow(dead_code)]
    fn manager_variant_exists(c: redis::aio::ConnectionManager) -> AsyncRedisConnection {
        AsyncRedisConnection::Manager(c)
    }

    /// `AsyncRedisConnection` must be `Clone` (the command methods clone it per
    /// call) and `ConnectionLike` (so it can be used as a redis connection).
    #[allow(dead_code)]
    fn assert_bounds<T: Clone + redis::aio::ConnectionLike>() {}
    #[allow(dead_code)]
    fn check_connection_enum_bounds() {
        assert_bounds::<AsyncRedisConnection>();
    }
}
