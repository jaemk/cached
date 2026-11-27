#[cfg(feature = "compression_zstd")]
use crate::stores::Compression;
use crate::stores::io_value_compression::IoValueCompression;
use crate::time::Duration;
use crate::{
    ConcurrentCacheBase, ConcurrentCacheRefreshOnHit, ConcurrentCacheTtl, ConcurrentCached,
};
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Display;
use std::marker::PhantomData;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Conditional self-heal delete (C6). Redis has no native compare-and-delete, so
/// the GET-then-DEL self-heal is closed with a Lua script that deletes the key
/// only if its current value still equals the corrupt bytes we read. This makes
/// the delete a no-op when a concurrent `SET`/`PSETEX` replaced the entry with a
/// valid value between our read and the self-heal, so that fresh write is never
/// lost. `redis::Script` caches the SHA and uses `EVALSHA` with an automatic
/// `EVAL` fallback on `NOSCRIPT`.
static SELF_HEAL_CONDITIONAL_DEL: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        "if redis.call('GET', KEYS[1]) == ARGV[1] then \
         return redis.call('DEL', KEYS[1]) else return 0 end",
    )
});

pub struct RedisCacheBuilder<K, V> {
    ttl: Option<Duration>,
    refresh: bool,
    namespace: String,
    prefix: Option<String>,
    connection_string: Option<String>,
    pool_max_size: Option<u32>,
    pool_min_idle: Option<u32>,
    pool_max_lifetime: Option<Duration>,
    pool_idle_timeout: Option<Duration>,
    pool_connection_timeout: Option<Duration>,
    strict_deserialization: bool,
    compression: IoValueCompression,
    // fn-pointer phantom — see the rationale on `RedisCache::_phantom`.
    _phantom: PhantomData<fn() -> (K, V)>,
}

const ENV_KEY: &str = "CACHED_REDIS_CONNECTION_STRING";
const DEFAULT_NAMESPACE: &str = "cached-redis-store:";

fn ttl_millis(ttl: Duration) -> Result<u64, RedisCacheError> {
    if ttl.is_zero() {
        return Err(RedisCacheError::redis(redis::RedisError::from((
            redis::ErrorKind::InvalidClientConfig,
            "invalid ttl: must be greater than zero",
            format!("got {ttl:?}"),
        ))));
    }
    // Convert to milliseconds with saturating arithmetic so pathologically large
    // durations do not overflow. Clamp to `i64::MAX` milliseconds so the same
    // bounded value fits both `PSETEX` (u64) and `PEXPIRE` (i64) without a second
    // clamp at the call site. This only bounds the value to the command argument's
    // integer type; it is not a guarantee the command is accepted. Redis validates
    // the expiry itself (e.g. it rejects a `PEXPIRE` whose `mstime() + ms` overflows),
    // so a TTL near this clamp can still be refused by the server. Clamp the low end to 1ms: a
    // non-zero sub-millisecond Duration truncates to 0ms, which `PSETEX`/`PEXPIRE`
    // reject (or treat as immediate-delete). The `is_zero` guard above already
    // rejects an actually-zero Duration, so the `.max(1)` only lifts a truncated
    // (but non-zero) sub-millisecond value up to the minimum valid TTL.
    let millis = ttl.as_millis();
    Ok(millis.min(i64::MAX as u128).max(1) as u64)
}

fn ttl_millis_i64(ttl: Duration) -> Result<i64, RedisCacheError> {
    // `ttl_millis` is already clamped to `i64::MAX`, so this cast is lossless.
    Ok(ttl_millis(ttl)? as i64)
}

/// Separator between the namespace, prefix and key fields of a generated Redis key.
const KEY_FIELD_SEPARATOR: char = ':';

/// Escape character introduced by [`escape_key_field`].
const KEY_FIELD_ESCAPE: char = '%';

/// Percent-escape the two characters that would otherwise make a key field
/// ambiguous: the field separator `:` (`%3A`) and the escape character `%`
/// itself (`%25`).
///
/// A field containing neither is returned borrowed and unchanged, so a key built
/// from ordinary namespace/prefix/key fields is byte-identical to a plain colon
/// join and stays readable in `redis-cli`. Percent-escaping is used rather than
/// backslash-escaping because `%` is not a glob metacharacter, so the escaped
/// field needs no further quoting to be embedded in the `SCAN MATCH` pattern
/// built by [`clear_match_pattern`].
fn escape_key_field(field: &str) -> std::borrow::Cow<'_, str> {
    if !field.contains([KEY_FIELD_SEPARATOR, KEY_FIELD_ESCAPE]) {
        return std::borrow::Cow::Borrowed(field);
    }
    let mut out = String::with_capacity(field.len() + 4);
    for c in field.chars() {
        match c {
            KEY_FIELD_SEPARATOR => out.push_str("%3A"),
            KEY_FIELD_ESCAPE => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Canonical form of a namespace: trailing separators are trimmed, so `"ns"`,
/// `"ns:"` and `"ns::"` all name the same namespace. This is what lets the
/// default namespace be written with its separator included
/// (`cached-redis-store:`) while still producing `cached-redis-store:<prefix>:<key>`.
fn canonical_namespace(namespace: &str) -> &str {
    namespace.trim_end_matches(KEY_FIELD_SEPARATOR)
}

/// Frame three already-escaped fields into the final key layout:
/// `{namespace}:{prefix}:{key}`. The arity is fixed: every field contributes a
/// field and a separator, so a key always carries exactly two separators, and an
/// empty namespace or prefix shows up as an empty field (`:{prefix}:{key}`,
/// `{namespace}::{key}`).
///
/// Shared by [`generate_redis_key`] and [`clear_match_pattern`] so the key
/// layout has exactly one definition and `cache_clear` cannot drift out of sync
/// with the keys the store writes.
fn join_key_fields(namespace: &str, prefix: &str, key: &str) -> String {
    let mut out = String::with_capacity(namespace.len() + prefix.len() + key.len() + 2);
    out.push_str(namespace);
    out.push(KEY_FIELD_SEPARATOR);
    out.push_str(prefix);
    out.push(KEY_FIELD_SEPARATOR);
    out.push_str(key);
    out
}

/// Build the Redis key: `{namespace}:{prefix}:{key}`, with every field
/// percent-escaped by [`escape_key_field`] and the fixed arity of
/// [`join_key_fields`].
///
/// Distinct `(canonical namespace, prefix, key)` triples always produce distinct
/// keys. Escaping removes `:` from the namespace and prefix fields, so the first
/// two `:` in the result are always the separators written here: splitting there
/// recovers the three escaped fields verbatim, and percent-unescaping recovers
/// the fields themselves, so the whole triple is reconstructible from the key.
/// Namespaces differing only in trailing separators are deliberately identified
/// by [`canonical_namespace`] and are the same namespace, not a collision.
///
/// The fixed arity is what keeps the *scopes* disjoint as well as the keys: an
/// empty namespace writes `:{prefix}:{key}`, which no cache with a non-empty
/// namespace can produce, so [`clear_match_pattern`] for a namespace-less cache
/// cannot reach a cache whose namespace happens to equal that prefix.
///
/// **Data-impacting in 3.0**: field escaping and the fixed arity are new, so a
/// cache with an empty namespace, or whose namespace, prefix or keys contain `:`
/// or `%`, writes at a different key than it did before. The value envelope's
/// `version` field does not cover the key layout: old keys are simply not found
/// after upgrading and the values are recomputed and rewritten at the new key.
/// (The 1.0 change from raw concatenation to this colon join was data-impacting
/// in the same way; see migration §8.) Single source of truth shared by the sync
/// and async stores so the two formats cannot drift.
fn generate_redis_key(namespace: &str, prefix: &str, key: &str) -> String {
    join_key_fields(
        &escape_key_field(canonical_namespace(namespace)),
        &escape_key_field(prefix),
        &escape_key_field(key),
    )
}

/// `SCAN MATCH` glob covering every key a cache with this `namespace`/`prefix`
/// writes: the [`generate_redis_key`] scope with a trailing `*`.
///
/// The fields are percent-escaped exactly as [`generate_redis_key`] escapes them
/// (so the pattern's literal part is the byte-for-byte scope of this cache's
/// keys), and only then glob-escaped: glob metacharacters (`*`, `?`, `[`, `]`)
/// and `\` are backslash-escaped so they match literally. Otherwise a prefix
/// like `cache[v2]` would scan (and `cache_clear` would delete) keys outside
/// this cache's scope. The two escapes do not interfere: percent-escaping only
/// ever emits `%` and hex digits (never a glob metacharacter), and glob-escaping
/// only ever inserts `\` (never a `:` or `%`).
///
/// Because escaping strips `:` out of the namespace and prefix fields, the
/// pattern also stops matching a *neighbouring* cache whose fields split the
/// same characters differently: `namespace = "a:b"` now scans `a%3Ab:p:*` and
/// leaves `namespace = "a", prefix = "b:p"` (`a:b%3Ap:*`) untouched. The fixed
/// arity closes the same overlap for an empty namespace: the scope is `:p:*`,
/// not `p:*`, so it cannot reach a cache whose namespace is `p`.
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
    join_key_fields(
        &escape_glob(&escape_key_field(canonical_namespace(namespace))),
        &escape_glob(&escape_key_field(prefix)),
        "*",
    )
}

#[cfg(test)]
mod clear_pattern_tests;

#[cfg(test)]
mod generate_key_tests;

#[cfg(test)]
mod ttl_millis_tests;

#[cfg(test)]
mod builder_ttl_setter_tests;

#[cfg(test)]
mod builder_empty_prefix_tests;

#[cfg(test)]
mod var_error_sanitize_tests;

#[cfg(test)]
mod credential_leak_tests;

#[cfg(test)]
mod error_explicit_source_tests;

#[cfg(test)]
mod legacy_json_version_gate_tests;

/// A Redis connection URL stored in memory with credentials redacted in `Debug`/`Display`.
///
/// Both [`Debug`](std::fmt::Debug) and [`Display`](std::fmt::Display) render the placeholder
/// `[REDACTED connection string]`, so the value is safe to log or include in error messages.
/// The raw URL (including any password) is available via [`reveal`](Self::reveal) and must not
/// be logged or exposed in error messages.
///
/// Equality and hashing compare the raw (unredacted) URL, so two values compare
/// equal exactly when their underlying connection strings match.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ConnectionString(String);

impl ConnectionString {
    /// Return the raw connection URL, including any embedded credentials.
    ///
    /// **Warning:** the returned string may contain credentials
    /// (e.g. `redis://:password@host`). Do not log or expose it in error messages.
    /// The redacting [`Debug`](std::fmt::Debug)/[`Display`](std::fmt::Display) impls exist
    /// precisely to keep this value out of logs; only call `reveal` when the full
    /// credentials are genuinely required.
    #[must_use]
    pub fn reveal(&self) -> &str {
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

/// Error returned when building a [`RedisCache`]/[`AsyncRedisCache`].
///
/// Configuration problems (a missing `prefix`, or an explicitly-set zero `ttl`)
/// surface as the transparent [`Build`](Self::Build) variant wrapping a
/// [`BuildError`](super::BuildError). The TTL is optional: an unset TTL is not an
/// error (entries are stored without expiry).
///
/// ```ignore
/// // `RedisCacheBuilder::new()` omits the required prefix, so `build` reports it.
/// match RedisCacheBuilder::<String, u32>::new().build() {
///     Err(RedisCacheBuildError::Build(BuildError::MissingRequired(field))) => { /* e.g. "prefix" */ }
///     Err(RedisCacheBuildError::Build(BuildError::InvalidValue { field, reason })) => { /* e.g. "ttl" */ }
///     _ => {}
/// }
/// ```
///
/// ## Semver note on error sources
///
/// The concrete type behind every `source` field is intentionally opaque: it is
/// `Box<dyn std::error::Error + Send + Sync + 'static>`. The wrapped type is an
/// implementation detail and is **not** part of the semver contract. Match on
/// the variant (e.g. `Connection { .. }`, `Pool { .. }`) rather than
/// downcast-inspecting the source.
#[non_exhaustive]
#[derive(Error)]
pub enum RedisCacheBuildError {
    /// Redis client or connection failed to open.
    ///
    /// The `source` is always a *sanitized* synthetic error — no raw connection
    /// URL or credential ever reaches this field. See the build-path comment in
    /// `create_pool` / `create_multiplexed_connection` for details.
    #[error("redis connection error: {source}")]
    Connection {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("redis pool error: {source}")]
    Pool {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error(transparent)]
    Build(#[from] super::BuildError),
    /// No connection string was set and the env var is absent or invalid.
    ///
    /// The `error` is always *sanitized* before it is stored here: a
    /// [`VarError::NotUnicode`](std::env::VarError::NotUnicode) would otherwise
    /// carry the raw env-var value — the connection string itself, credentials
    /// included — and print it through `Display`/`Debug`. See
    /// `sanitize_var_error` in this module (private).
    #[error("Connection string not specified or invalid in env var {env_key:?}: {error}")]
    MissingConnectionString {
        env_key: String,
        #[source]
        error: std::env::VarError,
    },
    /// The connection URL explicitly pins the RESP2 protocol (`?protocol=resp2`)
    /// while `client_side_caching` is enabled. Client-side caching requires
    /// RESP3; the combination is rejected at build time to prevent the
    /// invalidation listener from silently no-oping and serving stale data.
    #[cfg(feature = "redis_async_cache")]
    #[cfg_attr(docsrs, doc(cfg(feature = "redis_async_cache")))]
    #[error(
        "client_side_caching requires RESP3 but the connection URL explicitly pins \
        protocol=resp2; remove the protocol parameter or set it to resp3"
    )]
    Resp2DowngradeWithClientSideCaching,
}

// Manual `Debug`, matching `RedisCacheError` / `RedbCacheError` / `RedbCacheBuildError`.
// Every payload here is sanitized at construction (see the variant docs), so a derived
// impl would not leak today; the manual impl keeps the whole error family on the same
// pattern and gives future variants a single place where redaction is the norm.
impl std::fmt::Debug for RedisCacheBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection { source } => f
                .debug_struct("Connection")
                .field("source", source)
                .finish(),
            Self::Pool { source } => f.debug_struct("Pool").field("source", source).finish(),
            Self::Build(e) => f.debug_tuple("Build").field(e).finish(),
            Self::MissingConnectionString { env_key, error } => f
                .debug_struct("MissingConnectionString")
                .field("env_key", env_key)
                .field("error", error)
                .finish(),
            #[cfg(feature = "redis_async_cache")]
            Self::Resp2DowngradeWithClientSideCaching => {
                f.write_str("Resp2DowngradeWithClientSideCaching")
            }
        }
    }
}

/// Sanitize a [`std::env::VarError`] before storing it in
/// [`RedisCacheBuildError::MissingConnectionString`].
///
/// `VarError::NotUnicode` carries the raw env-var value — for
/// `CACHED_REDIS_CONNECTION_STRING` that is the connection string itself,
/// credentials included — and `VarError`'s `Display`/`Debug` both print it.
/// Replace the payload with a fixed redaction marker; the variant is kept so
/// callers can still distinguish "unset" from "invalid unicode".
fn sanitize_var_error(e: std::env::VarError) -> std::env::VarError {
    match e {
        std::env::VarError::NotPresent => std::env::VarError::NotPresent,
        std::env::VarError::NotUnicode(_) => {
            std::env::VarError::NotUnicode("[REDACTED connection string]".into())
        }
    }
}

impl RedisCacheBuildError {
    /// Wrap a sanitized connection error.
    ///
    /// The caller is responsible for ensuring `e` does NOT carry the raw
    /// connection URL or any credential. All build-path callers construct a
    /// synthetic `redis::RedisError` with a redacted message before calling this.
    pub(crate) fn connection(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Connection {
            source: Box::new(e),
        }
    }
}

impl<K, V> Default for RedisCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> RedisCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `RedisCacheBuilder`.
    ///
    /// The key `prefix` is required; set it with [`prefix`](Self::prefix) before
    /// calling [`build`](Self::build) (or use [`RedisCache::builder`] to supply it
    /// positionally). The TTL is optional; when left unset, entries are stored
    /// without expiry. Set it with [`ttl`](Self::ttl) (or
    /// [`ttl_secs`](Self::ttl_secs) / [`ttl_millis`](Self::ttl_millis)).
    #[must_use]
    pub fn new() -> RedisCacheBuilder<K, V> {
        Self {
            ttl: None,
            refresh: false,
            namespace: DEFAULT_NAMESPACE.to_string(),
            prefix: None,
            connection_string: None,
            pool_max_size: None,
            pool_min_idle: None,
            pool_max_lifetime: None,
            pool_idle_timeout: None,
            pool_connection_timeout: None,
            strict_deserialization: false,
            compression: IoValueCompression::default(),
            _phantom: PhantomData,
        }
    }

    /// Specify the cache TTL as a `Duration` (optional).
    ///
    /// TTL is stored with millisecond precision via `PSETEX`/`PEXPIRE`. When no
    /// TTL is set, entries are stored without expiry (a plain `SET`) and persist
    /// until explicitly removed. An explicitly-set TTL must be greater than zero
    /// (a zero TTL is rejected by [`build`](Self::build) with `InvalidValue`; use
    /// no TTL at all to disable expiry).
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
    /// namespace. An empty namespace still contributes its (empty) field, so a
    /// namespace-less cache writes `:{prefix}:{key}`.
    ///
    /// **Note:** colons in the namespace are percent-escaped (`:` becomes `%3A`,
    /// and `%` itself becomes `%25`), so a namespace containing an interior colon
    /// (e.g. `"a:b"`) cannot collide with a shorter namespace combined with a
    /// prefix (e.g. namespace `"a"`, prefix `"b"`). Escaping applies to the key
    /// layout only; the namespace is stored and reported unescaped.
    #[must_use]
    pub fn namespace<S: AsRef<str>>(mut self, namespace: S) -> Self {
        self.namespace = namespace.as_ref().to_string();
        self
    }

    /// Set the prefix for cache keys (required, non-empty).
    /// Used to generate keys formatted as: `{namespace}:{prefix}:{key}`.
    ///
    /// **Note:** colons in the prefix are percent-escaped, so a prefix containing
    /// a colon cannot collide with a differently-split namespace/prefix pair over
    /// the same characters.
    ///
    /// **Note:** the prefix is what distinguishes logical caches sharing a
    /// namespace, so give each of them a distinct prefix. An empty prefix is
    /// rejected by [`build`](Self::build) with `BuildError::InvalidValue`: two
    /// caches under the same namespace that both leave it empty would share one
    /// keyspace and one `cache_clear` scope (`<namespace>::*`), so each would
    /// read, overwrite and delete the other's entries.
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

    /// Bound how long the pool waits to establish a connection before failing.
    ///
    /// Maps to `r2d2::Builder::connection_timeout` (default 30s). A shorter value
    /// makes [`build`](Self::build) fail faster when the redis server is
    /// unreachable, rather than blocking for the full default.
    #[must_use]
    pub fn connection_pool_connection_timeout(mut self, connection_timeout: Duration) -> Self {
        self.pool_connection_timeout = Some(connection_timeout);
        self
    }

    /// Enable strict deserialization mode.
    ///
    /// When `false` (the default), a corrupt or otherwise undecodable cached
    /// value on the `cache_get` path is self-healed: the offending entry is
    /// deleted and the call returns `Ok(None)` (a miss), allowing the cached
    /// function to recompute and overwrite.
    ///
    /// When `true`, a deserialization failure on the read path returns
    /// `Err(RedisCacheError::CacheDeserialization { .. })` immediately, matching
    /// the behavior of versions prior to 3.0.
    ///
    /// In **both** modes, the previous value displaced by `cache_set` is silently
    /// discarded when it cannot be decoded (the write itself succeeds and
    /// `Ok(None)` is returned): the setting governs reads, not the best-effort
    /// previous-value decode on writes.
    #[must_use]
    pub fn strict_deserialization(mut self, strict: bool) -> Self {
        self.strict_deserialization = strict;
        self
    }

    /// Enable compression for new writes. With the `compression_zstd` feature,
    /// reads accept compressed entries even when this setting is omitted.
    #[cfg(feature = "compression_zstd")]
    #[cfg_attr(docsrs, doc(cfg(feature = "compression_zstd")))]
    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression.set_algorithm(compression);
        self
    }

    /// Minimum serialized size in bytes to attempt compression (default: 256).
    /// The store keeps compressed bytes only when they are smaller.
    #[cfg(feature = "compression_zstd")]
    #[cfg_attr(docsrs, doc(cfg(feature = "compression_zstd")))]
    pub fn min_compress_size(mut self, size: usize) -> Self {
        self.compression.set_min_compress_size(size);
        self
    }

    /// Maximum decompressed frame size in bytes (default: 64 MiB).
    /// Larger values are written raw, and this limit does not apply to raw reads.
    #[cfg(feature = "compression_zstd")]
    #[cfg_attr(docsrs, doc(cfg(feature = "compression_zstd")))]
    pub fn max_decompressed_size(mut self, size: usize) -> Self {
        self.compression.set_max_decompressed_size(size);
        self
    }

    /// Return the current connection string or load from the env var: `CACHED_REDIS_CONNECTION_STRING`.
    ///
    /// The value is wrapped in a redacting [`ConnectionString`]: its
    /// `Debug`/`Display` render `[REDACTED connection string]`, so it is safe to
    /// log or include in error messages. Call [`ConnectionString::reveal`] to
    /// obtain the raw URL (including any embedded credentials) when needed.
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
                    error: sanitize_var_error(e),
                }
            }),
        }
    }

    fn create_pool(&self) -> Result<r2d2::Pool<redis::Client>, RedisCacheBuildError> {
        let s = self.resolve_connection_string()?;
        // Open the client, catching any error and replacing it with a sanitized
        // `Connection` error. A malformed URL such as `redis://:password@host`
        // would otherwise surface the raw connection string (including the
        // password) in the error's `Display`/`Debug`. `Connection` is constructed
        // explicitly here (no blanket `From` impl) so no raw error can reach it.
        let client: redis::Client = redis::Client::open(s.reveal()).map_err(|_| {
            RedisCacheBuildError::connection(redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "failed to open redis client (connection string redacted)",
            )))
        })?;
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
        let pool_builder = if let Some(connection_timeout) = self.pool_connection_timeout {
            pool_builder.connection_timeout(connection_timeout)
        } else {
            pool_builder
        };

        // `Pool::build` eagerly opens the initial connections, so its `r2d2::Error`
        // wraps the underlying redis connect error, which can carry the connection
        // URL (and therefore credentials). Discard the raw error and substitute a
        // redacted synthetic one, matching the `Connection` path above (REDIS-8).
        let pool = pool_builder.build(client).map_err(|_| RedisCacheBuildError::Pool {
            source: Box::new(redis::RedisError::from((
                redis::ErrorKind::Io,
                "failed to establish initial redis pool connection (connection string redacted)",
            ))),
        })?;
        Ok(pool)
    }

    /// The last step in building a `RedisCache` is to call `build()`
    ///
    /// # Errors
    ///
    /// - `Build(BuildError::MissingRequired("prefix"))`: no key prefix was set.
    /// - `Build(BuildError::InvalidValue { field: "ttl", .. })`: an explicitly-set TTL is zero.
    /// - `Build(BuildError::InvalidValue { field: "prefix", .. })`: the prefix is
    ///   empty. The prefix is what distinguishes logical caches sharing a
    ///   namespace; two that both leave it empty would share one keyspace and one
    ///   `cache_clear` scope (`SCAN MATCH <namespace>::*`).
    /// - `MissingConnectionString`: no connection string was set and the
    ///   `CACHED_REDIS_CONNECTION_STRING` env var is absent or invalid.
    /// - `Connection` / `Pool`: the Redis client or connection pool could not
    ///   be created.
    ///
    /// The TTL is optional: when no TTL is set, entries are stored without expiry
    /// (a plain `SET`, no `PSETEX`/`PEXPIRE`) and persist until explicitly removed.
    pub fn build(self) -> Result<RedisCache<K, V>, RedisCacheBuildError> {
        // Validate required fields before any IO/connection attempt so the
        // missing-required error is returned without needing a server.
        if self.prefix.is_none() {
            return Err(super::BuildError::MissingRequired("prefix").into());
        }
        // TTL is optional. When unset, store entries with no expiry (represented
        // internally by a zero `Duration`, the "expiry disabled" sentinel). An
        // explicitly-set TTL still must be greater than zero.
        let ttl = match self.ttl {
            Some(ttl) => {
                super::validate_ttl(ttl)?;
                ttl
            }
            None => Duration::ZERO,
        };
        // The prefix is what distinguishes logical caches sharing a namespace:
        // two that both leave it empty share one keyspace and one `cache_clear`
        // scope (`<namespace>::*`), so each reads, overwrites and deletes the
        // other's entries. Reject it outright.
        if self.prefix.as_deref().is_some_and(str::is_empty) {
            return Err(super::BuildError::InvalidValue {
                field: "prefix",
                reason: "prefix must be non-empty: it is what scopes cache_clear to this \
                         cache; with an empty prefix cache_clear would delete every key \
                         under the namespace",
            }
            .into());
        }
        let connection_string = self.resolve_connection_string()?;
        let pool = self.create_pool()?;
        Ok(RedisCache {
            ttl: Mutex::new(ttl),
            refresh: AtomicBool::new(self.refresh),
            connection_string,
            pool,
            namespace: self.namespace,
            prefix: self.prefix.unwrap_or_default(),
            strict_deserialization: self.strict_deserialization,
            compression: self.compression,
            _phantom: PhantomData,
        })
    }
}

/// Cache store backed by redis
///
/// The TTL is optional and enforced by redis itself: entries built with a TTL
/// expire server-side; entries built without one persist until explicitly
/// removed. Uses an r2d2 connection pool under the hood.
///
/// # Format stability
///
/// Values are encoded with MessagePack in its compact (non-named) form: the
/// envelope is written as a **positional array** of `value` then `version`, with
/// the field names absent from the wire. **Field order is part of the frozen
/// layout**: reordering, inserting or removing a field of the stored envelope
/// changes how existing bytes decode and must bump the embedded version, exactly
/// as a type change would.
///
/// That layout is stable for the 3.x series: entries written by any 3.x release
/// remain readable by every later 3.x release. A schema change bumps the embedded
/// version, keeps a backward-read path within the same major version, and is
/// otherwise reserved for a major release.
/// [`AsyncRedisCache`](crate::stores::AsyncRedisCache) uses the same encoding.
///
/// The **key** layout (`{namespace}:{prefix}:{key}`, fixed arity with
/// percent-escaped fields, see `generate_redis_key`) is *not* covered by the
/// envelope's `version` field: the version lives inside the value, so it can only
/// be read once the key has been found. A key-layout change is therefore
/// invisible to a reader: entries written under the old layout are simply never
/// looked up. 3.0 escapes `:` and `%` in the key fields and always writes both
/// separators, so a cache with an empty namespace, or whose namespace, prefix or
/// keys contain either character, misses its pre-3.0 entries after upgrading and
/// recomputes them at the new key. The stale entries are not deleted by the
/// upgrade; they expire with their TTL, or persist until removed by hand if the
/// cache has none.
pub struct RedisCache<K, V> {
    pub(super) ttl: Mutex<Duration>,
    pub(super) refresh: AtomicBool,
    pub(super) namespace: String,
    pub(super) prefix: String,
    connection_string: ConnectionString,
    pool: r2d2::Pool<redis::Client>,
    strict_deserialization: bool,
    compression: IoValueCompression,
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
            strict_deserialization: self.strict_deserialization,
            compression: self.compression,
            _phantom: PhantomData,
        }
    }
}

impl<K, V> RedisCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `RedisCacheBuilder` with the required key `prefix`.
    ///
    /// The `prefix` namespaces every key this cache reads and writes; it can be
    /// overridden later via [`prefix`](RedisCacheBuilder::prefix). A TTL is
    /// optional (see [`ttl`](RedisCacheBuilder::ttl)); when unset, entries are
    /// stored without expiry.
    ///
    /// To construct a builder without supplying the prefix up front, use
    /// [`RedisCacheBuilder::new`] directly; `build` then returns
    /// `Err(`[`BuildError::MissingRequired`](super::BuildError::MissingRequired)`("prefix"))`
    /// if the prefix is never set.
    #[must_use]
    pub fn builder(prefix: impl Into<String>) -> RedisCacheBuilder<K, V> {
        RedisCacheBuilder::new().prefix(prefix.into())
    }

    fn generate_key(&self, key: &K) -> String {
        // Format `{namespace}:{prefix}:{key}`, fixed arity with percent-escaped
        // fields, see `generate_redis_key`. Changed from raw concatenation in 1.0
        // (migration §8) and from a variable-arity unescaped join in 3.0: keys
        // written by an older release under an empty namespace, or with `:` or
        // `%` in a field, are not hit after upgrading.
        generate_redis_key(&self.namespace, &self.prefix, &key.to_string())
    }

    /// `SCAN MATCH` glob covering every key this cache writes: the same
    /// `{namespace}:{prefix}:` scope as [`generate_key`](Self::generate_key) with a
    /// trailing `*`, with the fields percent-escaped exactly as the keys are and
    /// glob metacharacters escaped (see [`clear_match_pattern`]). Used by
    /// `cache_clear` to delete only this cache's entries.
    fn clear_match_pattern(&self) -> String {
        clear_match_pattern(&self.namespace, &self.prefix)
    }

    /// Return the redis connection string as a [`ConnectionString`].
    ///
    /// `ConnectionString`'s `Debug`/`Display` render `[REDACTED connection string]`,
    /// so the returned value is safe to log or include in error messages.
    /// Call [`ConnectionString::reveal`] to retrieve the raw URL when the full
    /// credentials are required.
    #[must_use]
    pub fn connection_string(&self) -> ConnectionString {
        self.connection_string.clone()
    }
}

/// Error returned by Redis cache operations.
///
/// ## Semver note on error sources
///
/// The concrete type behind every `source` field is intentionally opaque:
/// `Box<dyn std::error::Error + Send + Sync + 'static>`. The wrapped type is an
/// implementation detail and is **not** part of the semver contract. Consumers
/// should match on the variant (e.g. `Redis { .. }`, `CacheDeserialization { .. }`)
/// rather than downcast-inspecting the source. Use [`is_deserialization`](Self::is_deserialization)
/// as a stable classifier when you only need to distinguish decode failures.
#[non_exhaustive]
#[derive(Error)]
pub enum RedisCacheError {
    #[error("redis error: {source}")]
    Redis {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("redis pool error: {source}")]
    Pool {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    /// **Security note:** `cached_value` may contain sensitive application data
    /// (it is the raw bytes retrieved from Redis). Do not log this variant or
    /// expose `cached_value` in error messages or observability pipelines.
    #[error("error deserializing cached value: {source}")]
    CacheDeserialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
        cached_value: Vec<u8>,
    },
    #[error("error serializing cached value: {source}")]
    CacheSerialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

// Manual `Debug` that redacts the raw `cached_value` bytes (they may carry
// sensitive application data) as `<N bytes redacted>` instead of printing them,
// so a `{:?}` of the error never leaks the payload.
impl std::fmt::Debug for RedisCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Redis { source } => f.debug_struct("Redis").field("source", source).finish(),
            Self::Pool { source } => f.debug_struct("Pool").field("source", source).finish(),
            Self::CacheDeserialization {
                source,
                cached_value,
            } => f
                .debug_struct("CacheDeserialization")
                .field("source", source)
                .field(
                    "cached_value",
                    &format_args!("<{} bytes redacted>", cached_value.len()),
                )
                .finish(),
            Self::CacheSerialization { source } => f
                .debug_struct("CacheSerialization")
                .field("source", source)
                .finish(),
        }
    }
}

impl RedisCacheError {
    pub(crate) fn redis(e: redis::RedisError) -> Self {
        Self::Redis {
            source: Box::new(e),
        }
    }
    pub(crate) fn pool_err(e: r2d2::Error) -> Self {
        Self::Pool {
            source: Box::new(e),
        }
    }
    pub(crate) fn serialization(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::CacheSerialization {
            source: Box::new(e),
        }
    }
    pub(crate) fn deserialization(
        e: impl std::error::Error + Send + Sync + 'static,
        cached_value: Vec<u8>,
    ) -> Self {
        Self::CacheDeserialization {
            source: Box::new(e),
            cached_value,
        }
    }
    /// Returns `true` when this error is a deserialization failure.
    ///
    /// Stable classifier for callers that need to distinguish corrupt-value errors
    /// from network/pool errors without downcast-inspecting the opaque `source`.
    #[must_use]
    pub fn is_deserialization(&self) -> bool {
        matches!(self, Self::CacheDeserialization { .. })
    }
}

/// On-disk schema version stamped into every value written by this store.
/// Shared by [`CachedRedisValue::new`] and [`CachedRedisValueRef::new`] so the
/// two constructors cannot drift. The field type is `Option<u64>`.
///
/// Stability: the on-wire encoding is MessagePack in its compact (non-named)
/// form: every write goes through `rmp_serde::to_vec`, never `to_vec_named`, so
/// [`CachedRedisValue`] is written as a **positional array** of `value` then
/// `version` and the field names never reach the wire. **The field order is part
/// of the frozen layout**: reordering the fields silently reinterprets every
/// stored entry, so it is a format change and must bump this version like any
/// other. The layout is stable for the 3.x series: entries written by any 3.x
/// release remain readable by every later 3.x release. Any schema change bumps
/// this version, keeps a backward-read path within the same major version, and is
/// otherwise reserved for a major release.
const REDIS_VALUE_VERSION: Option<u64> = Some(1);

/// Stored value envelope. Serialized positionally (see [`REDIS_VALUE_VERSION`]):
/// the wire form is a 2-element MessagePack array of `value` then `version`.
/// Field order is frozen for 3.x.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedRedisValue<V> {
    value: V,
    version: Option<u64>,
}
impl<V> CachedRedisValue<V> {
    fn new(value: V) -> Self {
        Self {
            value,
            version: REDIS_VALUE_VERSION,
        }
    }
}

/// Borrowed counterpart of [`CachedRedisValue`] used by `cache_set_ref` to
/// serialize from a `&V` without cloning. Produces the same MessagePack bytes as
/// `CachedRedisValue::new(value)`: the encoding is positional, so the two structs
/// must keep the same fields in the same order.
#[derive(serde::Serialize)]
struct CachedRedisValueRef<'a, V> {
    value: &'a V,
    version: Option<u64>,
}
impl<'a, V> CachedRedisValueRef<'a, V> {
    fn new(value: &'a V) -> Self {
        Self {
            value,
            version: REDIS_VALUE_VERSION,
        }
    }
}

fn serialize_cached_redis_value(
    value: &impl serde::Serialize,
    compression: IoValueCompression,
) -> Result<Vec<u8>, RedisCacheError> {
    let bytes = rmp_serde::to_vec(value).map_err(RedisCacheError::serialization)?;
    compression
        .encode(bytes)
        .map_err(RedisCacheError::serialization)
}

/// Deserialize a stored [`CachedRedisValue`] from its raw Redis bytes, reading
/// both the current MessagePack format and the pre-3.0 JSON format.
///
/// Single source of truth for every value-deserialize site (sync and async) so
/// the backward-read behavior cannot drift between them.
///
/// A zstd frame is unpacked before MessagePack decoding when compression support
/// is compiled in. A frame error preserves the original bytes for the store's error
/// policy. If MessagePack fails, attempt the legacy pre-3.0 JSON encoding: parse the bytes as a
/// generic JSON value and require the known version before deserializing it into
/// a [`CachedRedisValue`]. If both formats fail, preserve the MessagePack error
/// as the source of [`RedisCacheError::CacheDeserialization`].
fn deserialize_cached_redis_value<V: serde::de::DeserializeOwned>(
    bytes: &[u8],
    compression: IoValueCompression,
) -> Result<CachedRedisValue<V>, RedisCacheError> {
    let decoded = compression
        .decode(bytes)
        .map_err(|e| RedisCacheError::deserialization(e, bytes.to_vec()))?;
    match rmp_serde::from_slice::<CachedRedisValue<V>>(&decoded) {
        Ok(v) => Ok(v),
        Err(msgpack_err) => {
            // Fall back to the pre-3.0 JSON format. Only treat the bytes as the
            // legacy format if they parse as JSON AND carry a `version` key whose
            // value matches the known version constant — otherwise this is genuinely
            // corrupt data and we should surface the original MessagePack error.
            // Checking the exact version value (not merely `is_some`) prevents a
            // JSON object with an unexpected `version` (e.g. a future incompatible
            // schema) from being silently accepted as a legacy entry.
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes)
                && json.get("version") == Some(&serde_json::json!(REDIS_VALUE_VERSION))
                && let Ok(v) = serde_json::from_value::<CachedRedisValue<V>>(json)
            {
                return Ok(v);
            }
            Err(RedisCacheError::deserialization(
                msgpack_err,
                bytes.to_vec(),
            ))
        }
    }
}

impl<K, V> ConcurrentCacheBase for RedisCache<K, V> {
    type Error = RedisCacheError;
}

impl<K, V> ConcurrentCacheTtl for RedisCache<K, V> {
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
    /// Subsequent `cache_set` writes use a plain `SET` (no expiry), so the keys persist
    /// until explicitly removed. Use [`try_set_ttl`](crate::ConcurrentCacheTtl::try_set_ttl) if you
    /// want a zero TTL rejected instead.
    fn set_ttl(&self, ttl: Duration) -> Option<Duration> {
        let mut guard = self.ttl.lock();
        let old = *guard;
        *guard = ttl;
        if old.is_zero() { None } else { Some(old) }
    }

    /// Disable expiry: subsequent `cache_set` writes store keys without a TTL (plain `SET`).
    /// Returns the previous TTL, or `None` if expiry was already disabled.
    fn unset_ttl(&self) -> Option<Duration> {
        let mut guard = self.ttl.lock();
        let old = *guard;
        *guard = Duration::ZERO;
        if old.is_zero() { None } else { Some(old) }
    }
}

impl<K, V> ConcurrentCacheRefreshOnHit for RedisCache<K, V> {
    fn refresh_on_hit(&self) -> bool {
        self.refresh.load(Ordering::Relaxed)
    }

    fn set_refresh_on_hit(&self, refresh: bool) -> bool {
        self.refresh.swap(refresh, Ordering::Relaxed)
    }
}

impl<K, V> ConcurrentCached<K, V> for RedisCache<K, V>
where
    K: Display + Clone,
    V: Serialize + DeserializeOwned,
{
    fn cache_get(&self, key: &K) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
        let mut pipe = redis::pipe();
        let key_str = self.generate_key(key);

        pipe.get(&key_str);
        if self.refresh.load(Ordering::Relaxed) {
            let ttl = *self.ttl.lock();
            // A zero (disabled) TTL means entries are stored without expiry; skip the
            // refresh `PEXPIRE` so the key stays persistent (no TTL to renew).
            if !ttl.is_zero() {
                pipe.pexpire(&key_str, ttl_millis_i64(ttl)?).ignore();
            }
        }
        // ugh: https://github.com/mitsuhiko/redis-rs/pull/388#issuecomment-910919137
        let res: (Option<Vec<u8>>,) = pipe.query(&mut *conn).map_err(RedisCacheError::redis)?;
        match res.0 {
            None => Ok(None),
            Some(bytes) => match deserialize_cached_redis_value(&bytes, self.compression) {
                Ok(v) => Ok(Some(v.value)),
                Err(e) if !self.strict_deserialization => {
                    // Self-heal: the stored bytes are corrupt or incompatible with V.
                    // Delete the entry so the caller can recompute on the next call.
                    // Use a conditional Lua delete (C6) that only removes the key
                    // if its current value still equals the corrupt `bytes` we
                    // read; a concurrent valid `SET`/`PSETEX` in between is left
                    // untouched instead of being clobbered by an unconditional DEL.
                    let _: i64 = SELF_HEAL_CONDITIONAL_DEL
                        .key(&key_str)
                        .arg(&bytes)
                        .invoke(&mut *conn)
                        .map_err(RedisCacheError::redis)?;
                    let _ = e;
                    Ok(None)
                }
                Err(e) => Err(e),
            },
        }
    }

    fn cache_set(&self, key: K, val: V) -> Result<Option<V>, RedisCacheError> {
        let val = CachedRedisValue::new(val);
        let serialized = serialize_cached_redis_value(&val, self.compression)?;
        let mut pipe = redis::pipe();
        let key_str = self.generate_key(&key);

        let ttl = *self.ttl.lock();

        pipe.get(&key_str);
        if ttl.is_zero() {
            // Disabled TTL: write the key without expiry (plain `SET`).
            pipe.set::<String, Vec<u8>>(key_str, serialized).ignore();
        } else {
            pipe.pset_ex::<String, Vec<u8>>(key_str, serialized, ttl_millis(ttl)?)
                .ignore();
        }

        let res: (Option<Vec<u8>>,) = {
            let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
            pipe.query(&mut *conn).map_err(RedisCacheError::redis)?
        };
        // REDIS-10: if the displaced previous value fails to decode, the new write
        // succeeded — return Ok(None) (garbage old value) rather than surfacing an error.
        Ok(res.0.and_then(|bytes| {
            deserialize_cached_redis_value::<V>(&bytes, self.compression)
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
    /// on the [`strict_deserialization`](RedisCacheBuilder::strict_deserialization) setting:
    ///
    /// - **Default (non-strict):** the corrupt entry is removed and the method returns
    ///   `Ok(None)` (the undecodable previous value is discarded).
    /// - **Strict (`strict_deserialization(true)`):** the corrupt entry is still removed
    ///   and the method returns `Err(RedisCacheError::CacheDeserialization { .. })`.
    fn cache_remove(&self, key: &K) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
        let mut pipe = redis::pipe();
        let key_str = self.generate_key(key);

        pipe.get(&key_str);
        pipe.del::<String>(key_str).ignore();
        let res: (Option<Vec<u8>>,) = pipe.query(&mut *conn).map_err(RedisCacheError::redis)?;
        match res.0 {
            None => Ok(None),
            Some(bytes) => match deserialize_cached_redis_value(&bytes, self.compression) {
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
    /// though the key may still be physically present in Redis. Use [`cache_delete`](ConcurrentCached::cache_delete)
    /// (which uses `DEL` directly) to reliably detect whether any physical entry was removed.
    fn cache_remove_entry(&self, key: &K) -> Result<Option<(K, V)>, Self::Error> {
        self.cache_remove(key)
            .map(|opt| opt.map(|v| (key.clone(), v)))
    }

    fn cache_delete(&self, key: &K) -> Result<bool, RedisCacheError> {
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
        let key_str = self.generate_key(key);
        let removed: usize = redis::cmd("DEL")
            .arg(key_str)
            .query(&mut *conn)
            .map_err(RedisCacheError::redis)?;
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
    /// **Note:** the `prefix` is what scopes a clear to a single logical cache.
    /// Two caches sharing a namespace *and* a prefix share one keyspace and one
    /// clear scope, so each clears the other's entries. Set a unique prefix per
    /// logical cache to avoid this. An empty prefix is rejected at build time for
    /// the same reason.
    fn cache_clear(&self) -> Result<(), RedisCacheError> {
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
        let pattern = self.clear_match_pattern();
        let mut cursor: u64 = 0;
        loop {
            // Redis keys are binary-safe, and this scope can legitimately contain
            // keys that are not valid UTF-8 (a non-`String` key type, another
            // client writing under the same prefix, ...). Decoding as
            // `Vec<String>` made the whole clear fail on the first such key and
            // never self-heal, because the offending key was never deleted.
            // `DEL` takes the raw bytes unchanged.
            let (next, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query(&mut *conn)
                .map_err(RedisCacheError::redis)?;
            if !keys.is_empty() {
                redis::cmd("DEL")
                    .arg(keys)
                    .query::<()>(&mut *conn)
                    .map_err(RedisCacheError::redis)?;
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

    /// Returns `true` if the cache contains a non-expired entry for `key`.
    ///
    /// This delegates to [`cache_get`](ConcurrentCached::cache_get): the value is
    /// fetched and deserialized to determine presence. There is no separate Redis
    /// EXISTS round-trip in this implementation.
    ///
    /// **Note:** because it is a full `cache_get`, this is not a read-only probe
    /// when [`refresh_on_hit`](crate::ConcurrentCacheRefreshOnHit::refresh_on_hit)
    /// is enabled: a `true` result renews the entry's TTL (a `PEXPIRE` write) just
    /// as a real read would, and an undecodable entry is self-healed (deleted) and
    /// reported absent. `RedbCache::cache_contains` behaves the same way.
    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        self.cache_get(k).map(|v| v.is_some())
    }
}

impl<K, V> crate::SerializeCached<K, V> for RedisCache<K, V>
where
    K: Display + Clone,
    V: Serialize + DeserializeOwned,
{
    /// Serializes from the borrowed `val` (no clone) and `SET`s it. Equivalent to
    /// [`ConcurrentCached::cache_set`] but avoids taking ownership of `val` and does
    /// not read back the previous value, so the write is a single round-trip
    /// (no GET). Call [`ConcurrentCached::cache_get`] first if you need the prior value.
    fn cache_set_ref(&self, key: &K, val: &V) -> Result<(), RedisCacheError> {
        let val = CachedRedisValueRef::new(val);
        let serialized = serialize_cached_redis_value(&val, self.compression)?;
        let key_str = self.generate_key(key);
        let ttl = *self.ttl.lock();
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;

        if ttl.is_zero() {
            // Disabled TTL: write the key without expiry (plain `SET`).
            let _: () = redis::cmd("SET")
                .arg(&key_str)
                .arg(serialized)
                .query(&mut *conn)
                .map_err(RedisCacheError::redis)?;
        } else {
            let _: () = redis::cmd("PSETEX")
                .arg(&key_str)
                .arg(ttl_millis(ttl)?)
                .arg(serialized)
                .query(&mut *conn)
                .map_err(RedisCacheError::redis)?;
        }
        Ok(())
    }
}

// Canonical `AsyncRedisCache` availability gate (kept in sync with src/lib.rs and
// src/stores/mod.rs): a redis async runtime feature must be enabled. The six runtime features
// each imply `redis_store` + `async`; the capability-only features are excluded because they
// carry no runtime.
#[cfg(any(
    feature = "redis_smol",
    feature = "redis_smol_native_tls",
    feature = "redis_smol_rustls",
    feature = "redis_tokio",
    feature = "redis_tokio_native_tls",
    feature = "redis_tokio_rustls",
))]
mod async_redis;

// Canonical `AsyncRedisCache` availability gate (kept in sync with src/lib.rs and
// src/stores/mod.rs): a redis async runtime feature must be enabled.
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
pub use async_redis::{AsyncRedisCache, AsyncRedisCacheBuilder};

#[cfg(test)]
mod error_source_tests;

#[cfg(test)]
mod connection_string_tests;

#[cfg(test)]
/// Cache store tests
mod tests;
