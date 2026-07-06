use crate::time::Duration;
use crate::{ConcurrentCacheBase, ConcurrentCacheTtl, ConcurrentCached};
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
mod ttl_millis_tests {
    // Pure-function coverage for the Redis millisecond TTL helpers: reject zero,
    // preserve sub-second precision, clamp to `i64::MAX` ms. These need no
    // Redis server and guard both the `PSETEX` (cache_set) and `PEXPIRE`
    // (refresh) paths, which share `ttl_millis`/`ttl_millis_i64`.
    use super::{ttl_millis, ttl_millis_i64};
    use crate::time::Duration;

    #[test]
    fn zero_is_rejected() {
        assert!(ttl_millis(Duration::ZERO).is_err());
        assert!(ttl_millis_i64(Duration::ZERO).is_err());
    }

    #[test]
    fn whole_seconds_become_milliseconds() {
        assert_eq!(ttl_millis(Duration::from_secs(1)).unwrap(), 1_000);
        assert_eq!(ttl_millis(Duration::from_secs(60)).unwrap(), 60_000);
        assert_eq!(ttl_millis_i64(Duration::from_secs(60)).unwrap(), 60_000);
    }

    #[test]
    fn subsecond_precision_is_preserved() {
        // Unlike the old SETEX path, sub-second TTLs are NOT rounded up to 1s.
        assert_eq!(ttl_millis(Duration::from_millis(1)).unwrap(), 1);
        assert_eq!(ttl_millis(Duration::from_millis(250)).unwrap(), 250);
        assert_eq!(ttl_millis(Duration::from_millis(999)).unwrap(), 999);
    }

    #[test]
    fn nonzero_submillisecond_clamps_to_one() {
        // A non-zero Duration under 1ms truncates to 0ms, which PSETEX/PEXPIRE
        // reject (or treat as immediate-delete). The low-end clamp lifts it to 1ms.
        let one_ns = Duration::from_nanos(1);
        assert!(!one_ns.is_zero());
        // Truncation to milliseconds is still 0...
        assert_eq!(one_ns.as_millis(), 0);
        // ...but ttl_millis clamps a non-zero sub-ms Duration up to 1ms.
        assert_eq!(ttl_millis(one_ns).unwrap(), 1);
        assert_eq!(ttl_millis_i64(one_ns).unwrap(), 1);
        // Other sub-millisecond, non-zero durations also clamp to 1.
        assert_eq!(ttl_millis(Duration::from_nanos(999_999)).unwrap(), 1);
        assert_eq!(ttl_millis(Duration::from_micros(500)).unwrap(), 1);
    }

    #[test]
    fn zero_never_reaches_the_clamp() {
        // The is_zero guard rejects an actually-zero Duration before the
        // low-end `.max(1)` clamp can lift it to 1ms.
        assert!(ttl_millis(Duration::ZERO).is_err());
        assert!(ttl_millis_i64(Duration::ZERO).is_err());
    }

    #[test]
    fn subsecond_mixed_passes_through() {
        assert_eq!(ttl_millis(Duration::from_millis(1_500)).unwrap(), 1_500);
        assert_eq!(ttl_millis(Duration::new(5, 1_000_000)).unwrap(), 5_001);
    }

    #[test]
    fn very_large_clamps_to_i64_max() {
        // A Duration that, in milliseconds, overflows i64 is clamped.
        let huge = Duration::from_secs(u64::MAX);
        assert_eq!(ttl_millis(huge).unwrap(), i64::MAX as u64);
        assert_eq!(ttl_millis_i64(huge).unwrap(), i64::MAX);
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
        let b = RedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_secs(7);
        assert_eq!(b.ttl, Some(Duration::from_secs(7)));

        let b = RedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_millis(250);
        assert_eq!(b.ttl, Some(Duration::from_millis(250)));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        // ttl(secs=10) then ttl_secs(5) -> 5s
        let b = RedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl(Duration::from_secs(10))
            .ttl_secs(5);
        assert_eq!(b.ttl, Some(Duration::from_secs(5)));

        // ttl_secs then ttl_millis -> the millis value
        let b = RedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_secs(10)
            .ttl_millis(500);
        assert_eq!(b.ttl, Some(Duration::from_millis(500)));

        // ttl_millis then ttl -> the ttl value
        let b = RedisCacheBuilder::<String, String>::new()
            .prefix("p")
            .ttl_millis(500)
            .ttl(Duration::from_secs(3));
        assert_eq!(b.ttl, Some(Duration::from_secs(3)));
    }
}

#[cfg(test)]
mod builder_empty_scope_tests {
    // No Redis server needed -- verifies the empty-scope guard in `build()`.
    use super::{RedisCacheBuildError, RedisCacheBuilder};
    use crate::time::Duration;

    #[test]
    fn empty_namespace_and_prefix_is_rejected() {
        let result = RedisCacheBuilder::<String, String>::new()
            .prefix("")
            .ttl(Duration::from_secs(1))
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
        let result = RedisCacheBuilder::<String, String>::new()
            .prefix("")
            .ttl(Duration::from_secs(1))
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
        let result = RedisCacheBuilder::<String, String>::new()
            .prefix("my-prefix")
            .ttl(Duration::from_secs(1))
            .namespace("")
            .build();
        // The only failure here would be a missing connection string, not EmptyScope.
        assert!(
            !matches!(result, Err(RedisCacheBuildError::EmptyScope)),
            "EmptyScope must not fire when prefix is non-empty"
        );
    }
}

#[cfg(test)]
mod credential_leak_tests {
    // No Redis server needed -- verifies that a bad connection URL containing a
    // password does not surface the password in the build error's Display/Debug.
    // Guards against the S3 credential-leak finding: the `#[from] redis::RedisError`
    // path used to propagate raw connection errors before we started sanitizing them.
    use super::{RedisCacheBuildError, RedisCacheBuilder};
    use crate::time::Duration;

    /// Building with a URL that contains an embedded password but has an invalid
    /// scheme (so `Client::open` fails immediately) must not include the password
    /// in the resulting error's `Display` or `Debug`.
    #[test]
    fn bad_url_with_password_does_not_leak_password_in_build_error() {
        // `not-redis` is not a valid redis scheme, so `Client::open` / `into_connection_info`
        // will fail before any network connection is attempted. The URL contains a
        // plaintext password that must not appear in the error message.
        let secret = "super_secret_password_xyz";
        let bad_url = format!("not-redis://:{secret}@nonexistent-host:9999");

        let result = RedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(&bad_url)
            .build();

        let err = result.expect_err("build must fail with a bad URL");
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
        // The full raw URL (host included) must not appear in Display/Debug either.
        assert!(
            !display.contains(&bad_url) && !debug.contains(&bad_url),
            "neither Display nor Debug may echo the raw URL; got display={display}, debug={debug}"
        );
        // The error must be a Connection variant (the sanitized one).
        assert!(
            matches!(err, RedisCacheBuildError::Connection { .. }),
            "expected Connection error, got: {err:?}"
        );
    }

    /// `resolve_connection_string()` returns a redacting [`ConnectionString`]: its
    /// `Debug`/`Display` render the placeholder (never the password) while
    /// `reveal()` hands back the exact raw URL. This is the surface an external
    /// caller sees, so it must not expose credentials by default.
    #[test]
    fn resolve_connection_string_returns_redacting_wrapper() {
        let secret = "resolve_secret_abc";
        let raw = format!("redis://:{secret}@127.0.0.1:6379/0");

        let builder = RedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(&raw);

        let cs = builder
            .resolve_connection_string()
            .expect("connection string was set");

        let display = cs.to_string();
        let debug = format!("{cs:?}");
        assert_eq!(display, "[REDACTED connection string]");
        assert_eq!(debug, "[REDACTED connection string]");
        assert!(
            !display.contains(secret) && !debug.contains(secret),
            "wrapper must not expose the password in Display/Debug"
        );
        // reveal() must return the exact raw URL, credentials included.
        assert_eq!(cs.reveal(), raw);
        assert!(cs.reveal().contains(secret));
    }

    /// The full derived `Debug` of a `Connection` error built from a
    /// password-bearing bad URL (sync build path) must contain neither the
    /// password nor the raw URL. Guards the removal of the blanket
    /// `#[from] redis::RedisError` on the `Connection` variant, whose raw redis
    /// error's `Debug` could otherwise echo the connection string.
    #[test]
    fn sync_connection_error_debug_does_not_leak_url_or_password() {
        let secret = "sync_debug_secret_123";
        let bad_url = format!("not-redis://:{secret}@nonexistent-host:9999");

        let result = RedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(&bad_url)
            .build();

        let err = result.expect_err("build must fail with a bad URL");
        assert!(
            matches!(err, RedisCacheBuildError::Connection { .. }),
            "expected Connection error, got: {err:?}"
        );
        // Exercise the full enum Debug (which recurses into the inner redis error).
        let debug = format!("{err:?}");
        assert!(
            !debug.contains(secret),
            "enum Debug must not expose the password; got: {debug}"
        );
        assert!(
            !debug.contains(&bad_url),
            "enum Debug must not echo the raw URL; got: {debug}"
        );
    }

    /// The boxed `source` inside `Connection { source }` must not expose the planted
    /// password through its own `Debug` or `Display`. Verifies that the box wraps a
    /// sanitized synthetic error, not the raw redis error that would echo the URL.
    #[test]
    fn connection_boxed_source_debug_does_not_leak_password() {
        let secret = "boxed_source_secret_xyz999";
        let bad_url = format!("not-redis://:{secret}@nonexistent-host:9999");

        let result = RedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(&bad_url)
            .build();

        let err = result.expect_err("build must fail with a bad URL");
        if let RedisCacheBuildError::Connection { ref source } = err {
            let src_debug = format!("{source:?}");
            let src_display = source.to_string();
            assert!(
                !src_debug.contains(secret),
                "boxed source Debug must not expose the password; got: {src_debug}"
            );
            assert!(
                !src_display.contains(secret),
                "boxed source Display must not expose the password; got: {src_display}"
            );
            // Walk the full cause chain.
            let mut cause = source.source();
            while let Some(c) = cause {
                let c_str = format!("{c:?}{c}");
                assert!(
                    !c_str.contains(secret),
                    "cause chain must not expose the password; got: {c_str}"
                );
                cause = c.source();
            }
        } else {
            panic!("expected Connection error, got: {err:?}");
        }
    }

    /// A valid-scheme connection string with embedded credentials pointing at a
    /// refused host fails during eager pool construction. The resulting `Pool`
    /// error and its full cause chain must not leak the password (REDIS-8).
    #[test]
    fn pool_build_error_does_not_leak_password() {
        let secret = "pool_build_secret_abc123";
        // Valid redis scheme (so `Client::open` succeeds), credentials in the
        // URL, pointing at a refused port so the eager pool connect fails fast.
        let bad_url = format!("redis://:{secret}@127.0.0.1:1");

        let result = RedisCacheBuilder::<String, String>::new()
            .prefix("test")
            .ttl(Duration::from_secs(1))
            .connection_string(&bad_url)
            .connection_pool_min_idle(1)
            .connection_pool_connection_timeout(Duration::from_millis(50))
            .build();

        let err = result.expect_err("build must fail against a refused host");
        assert!(
            matches!(err, RedisCacheBuildError::Pool { .. }),
            "expected Pool error, got: {err:?}"
        );
        let debug = format!("{err:?}");
        assert!(
            !debug.contains(secret),
            "Pool Debug leaked the password: {debug}"
        );
        if let RedisCacheBuildError::Pool { ref source } = err {
            let src = format!("{source:?}{source}");
            assert!(
                !src.contains(secret),
                "boxed Pool source leaked the password: {src}"
            );
            let mut cause = source.source();
            while let Some(c) = cause {
                let c_str = format!("{c:?}{c}");
                assert!(
                    !c_str.contains(secret),
                    "cause chain leaked the password: {c_str}"
                );
                cause = c.source();
            }
        }
    }
}

#[cfg(test)]
mod legacy_json_version_gate_tests {
    // No Redis server needed -- verifies that the S4 backward-read gate requires
    // the `version` key to equal the known version constant, not merely exist.
    use super::{RedisCacheError, deserialize_cached_redis_value};

    /// A JSON object whose `version` key exists but holds an unexpected value must
    /// NOT be accepted as a legacy entry — it falls through to `CacheDeserialization`.
    #[test]
    fn json_with_wrong_version_value_is_rejected() {
        // `version: 99` is not the known REDIS_VALUE_VERSION (Some(1)).
        // Before the fix, `json.get("version").is_some()` would have accepted this.
        let bytes = br#"{"value": "hello", "version": 99}"#.to_vec();
        match deserialize_cached_redis_value::<String>(&bytes) {
            Ok(_) => panic!(
                "JSON with an unexpected `version` value must not be accepted as a legacy entry"
            ),
            Err(RedisCacheError::CacheDeserialization { cached_value, .. }) => {
                assert_eq!(
                    cached_value, bytes,
                    "raw bytes must be preserved in the error"
                );
            }
            Err(other) => panic!("expected CacheDeserialization, got: {other:?}"),
        }
    }

    /// A JSON object with `version: null` (the JSON encoding of `None`) is also
    /// not the expected version (`Some(1)`) and must be rejected.
    #[test]
    fn json_with_null_version_is_rejected() {
        let bytes = br#"{"value": 42, "version": null}"#.to_vec();
        match deserialize_cached_redis_value::<u64>(&bytes) {
            Ok(_) => panic!("JSON with version=null must not be accepted as a legacy entry"),
            Err(RedisCacheError::CacheDeserialization { .. }) => {}
            Err(other) => panic!("expected CacheDeserialization, got: {other:?}"),
        }
    }

    /// A JSON object with `version: 1` (the known version, matches `Some(1)`) IS
    /// accepted as a valid legacy entry. This confirms the gate is a value-check,
    /// not just a presence-check, and that the correct version still passes through.
    #[test]
    fn json_with_correct_version_one_is_accepted() {
        // `{"value": "ok", "version": 1}` is the pre-3.0 JSON format with the known version.
        let bytes = br#"{"value": "ok", "version": 1}"#.to_vec();
        let result = deserialize_cached_redis_value::<String>(&bytes);
        assert!(
            result.is_ok(),
            "JSON with version=1 must be accepted as a legacy entry"
        );
        assert_eq!(result.unwrap().value, "ok");
    }
}

/// A Redis connection URL stored in memory with credentials redacted in `Debug`/`Display`.
///
/// Both [`Debug`](std::fmt::Debug) and [`Display`](std::fmt::Display) render the placeholder
/// `[REDACTED connection string]`, so the value is safe to log or include in error messages.
/// The raw URL (including any password) is available via [`reveal`](Self::reveal) and must not
/// be logged or exposed in error messages.
#[derive(Clone)]
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
#[derive(Error, Debug)]
pub enum RedisCacheBuildError {
    /// Redis client or connection failed to open.
    ///
    /// The `source` is always a *sanitized* synthetic error — no raw connection
    /// URL or credential ever reaches this field. See the build-path comment in
    /// `create_pool` / `create_multiplexed_connection` for details.
    #[error("redis connection error")]
    Connection {
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("redis pool error")]
    Pool {
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error(transparent)]
    Build(#[from] super::BuildError),
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
    /// The connection URL explicitly pins the RESP2 protocol (`?protocol=resp2`)
    /// while `client_side_caching` is enabled. Client-side caching requires
    /// RESP3; the combination is rejected at build time to prevent the
    /// invalidation listener from silently no-oping and serving stale data.
    #[error(
        "client_side_caching requires RESP3 but the connection URL explicitly pins \
        protocol=resp2; remove the protocol parameter or set it to resp3"
    )]
    Resp2DowngradeWithClientSideCaching,
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

    /// Set the prefix for cache keys (required).
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
    /// function to recompute and overwrite. The previous-value returned by
    /// `cache_set` is also silently discarded when it cannot be decoded.
    ///
    /// When `true`, any deserialization failure returns
    /// `Err(RedisCacheError::CacheDeserialization { .. })` immediately, matching
    /// the behavior of versions prior to 3.0.
    #[must_use]
    pub fn strict_deserialization(mut self, strict: bool) -> Self {
        self.strict_deserialization = strict;
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
                    error: e,
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
    /// - `EmptyScope`: both the namespace (after trimming trailing colons) and
    ///   the prefix are empty. `cache_clear` would otherwise issue `SCAN MATCH *`
    ///   and delete every key in the Redis database.
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
        let prefix = self.prefix.as_deref().unwrap_or_default();
        if self.namespace.trim_end_matches(':').is_empty() && prefix.is_empty() {
            return Err(RedisCacheBuildError::EmptyScope);
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
    strict_deserialization: bool,
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
#[derive(Error, Debug)]
pub enum RedisCacheError {
    #[error("redis error")]
    Redis {
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("redis pool error")]
    Pool {
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    /// **Security note:** `cached_value` may contain sensitive application data
    /// (it is the raw bytes retrieved from Redis). Do not log this variant or
    /// expose `cached_value` in error messages or observability pipelines.
    #[error("Error deserializing cached value")]
    CacheDeserialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
        cached_value: Vec<u8>,
    },
    #[error("Error serializing cached value")]
    CacheSerialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
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
    pub(crate) fn serialization(e: rmp_serde::encode::Error) -> Self {
        Self::CacheSerialization {
            source: Box::new(e),
        }
    }
    pub(crate) fn deserialization(e: rmp_serde::decode::Error, cached_value: Vec<u8>) -> Self {
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
const REDIS_VALUE_VERSION: Option<u64> = Some(1);

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
/// serialize from a `&V` without cloning. Produces the same MessagePack bytes
/// as `CachedRedisValue::new(value)` (same field names and order).
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

/// Deserialize a stored [`CachedRedisValue`] from its raw Redis bytes, reading
/// both the current MessagePack format and the pre-3.0 JSON format.
///
/// Single source of truth for every value-deserialize site (sync and async) so
/// the backward-read behavior cannot drift between them.
///
/// Logic:
/// 1. Try MessagePack (`rmp_serde`) — the format written since 3.0.
/// 2. On failure, attempt the legacy pre-3.0 JSON encoding: parse the bytes as a
///    generic JSON value and, only if it carries a `version` key (the shape this
///    store always wrote), deserialize it into a [`CachedRedisValue`]. This
///    transparently reads entries written by cached 2.x.
/// 3. If neither path succeeds, return
///    [`RedisCacheError::CacheDeserialization`] preserving the *original*
///    MessagePack error as `source` and the raw bytes in `cached_value`.
fn deserialize_cached_redis_value<V: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<CachedRedisValue<V>, RedisCacheError> {
    match rmp_serde::from_slice::<CachedRedisValue<V>>(bytes) {
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
    /// With [`refresh_on_hit`](crate::ConcurrentCacheTtl::refresh_on_hit) enabled, however, a
    /// `cache_get` hit re-applies the current TTL to the key it touched (via `PEXPIRE`), so a
    /// changed TTL does reach an existing key on its next hit.
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
            Some(bytes) => match deserialize_cached_redis_value(&bytes) {
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
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
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
            pipe.pset_ex::<String, Vec<u8>>(key_str, serialized, ttl_millis(ttl)?)
                .ignore();
        }

        let res: (Option<Vec<u8>>,) = pipe.query(&mut *conn).map_err(RedisCacheError::redis)?;
        // REDIS-10: if the displaced previous value fails to decode, the new write
        // succeeded — return Ok(None) (garbage old value) rather than surfacing an error.
        Ok(res.0.and_then(|bytes| {
            deserialize_cached_redis_value::<V>(&bytes)
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
            Some(bytes) => match deserialize_cached_redis_value(&bytes) {
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
    /// **Note:** the `prefix` is what scopes a clear to a single logical cache. A
    /// cache built with an empty prefix but a non-empty namespace will match every
    /// key under that namespace on `cache_clear` (pattern `<namespace>:*`), which
    /// includes entries written by every other cache that shares the same namespace.
    /// Set a unique prefix per logical cache to avoid this.
    fn cache_clear(&self) -> Result<(), RedisCacheError> {
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
        let pattern = self.clear_match_pattern();
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
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
        let mut conn = self.pool.get().map_err(RedisCacheError::pool_err)?;
        let key_str = self.generate_key(key);

        let ttl = *self.ttl.lock();

        let val = CachedRedisValueRef::new(val);
        let serialized = rmp_serde::to_vec(&val).map_err(RedisCacheError::serialization)?;

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
mod async_redis {
    use crate::time::Duration;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{
        CachedRedisValue, CachedRedisValueRef, ConnectionString, DEFAULT_NAMESPACE,
        DeserializeOwned, Display, ENV_KEY, PhantomData, RedisCacheBuildError, RedisCacheError,
        Serialize,
    };
    use crate::{ConcurrentCacheBase, ConcurrentCacheTtl, ConcurrentCachedAsync};
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

        /// Set the prefix for cache keys (required).
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
        /// returns `Ok(None)`. When `true`, any deserialization failure returns
        /// `Err(RedisCacheError::CacheDeserialization { .. })`.
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
                        error: e,
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
        /// - `EmptyScope`: both the namespace (after trimming trailing colons) and
        ///   the prefix are empty. `async_cache_clear` would otherwise issue
        ///   `SCAN MATCH *` and delete every key in the Redis database.
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
            let prefix = self.prefix.as_deref().unwrap_or_default();
            if self.namespace.trim_end_matches(':').is_empty() && prefix.is_empty() {
                return Err(RedisCacheBuildError::EmptyScope);
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
    /// Values have a TTL applied and enforced by Redis.
    /// Uses a `redis::aio::MultiplexedConnection` by default, or a
    /// `redis::aio::ConnectionManager` when the cache was built with
    /// [`AsyncRedisCacheBuilder::connection_manager(true)`](AsyncRedisCacheBuilder::connection_manager)
    /// (requires the `redis_connection_manager` feature). Enabling that feature
    /// only makes the option available; it does not change the default.
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
        /// `{namespace}:{prefix}:` scope with a trailing `*`, with glob
        /// metacharacters in the segments escaped (see
        /// [`clear_match_pattern`](super::clear_match_pattern)). Used by
        /// `async_cache_clear`.
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
        /// With [`refresh_on_hit`](crate::ConcurrentCacheTtl::refresh_on_hit) enabled, however, a
        /// `cache_get` hit re-applies the current TTL to the key it touched (via `PEXPIRE`), so a
        /// changed TTL does reach an existing key on its next hit.
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

        // No Redis server needed -- verifies the empty-scope guard in async `build()`.
        #[tokio::test]
        async fn async_empty_namespace_and_prefix_is_rejected() {
            let result = AsyncRedisCacheBuilder::<String, String>::new()
                .prefix("")
                .ttl(Duration::from_secs(1))
                .namespace("")
                .build()
                .await;
            assert!(
                matches!(result, Err(RedisCacheBuildError::EmptyScope)),
                "expected EmptyScope"
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
            let c: AsyncRedisCache<u32, u32> = AsyncRedisCache::builder(format!("{}:async-redis-cache-test", now_millis()))
                .ttl(Duration::from_secs(2))
                .build()
                .await
                .unwrap();

            assert!(c.async_cache_get(&1).await.unwrap().is_none());

            assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
            assert!(c.async_cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(2, 500_000));
            assert!(c.async_cache_get(&1).await.unwrap().is_none());

            let old = ConcurrentCacheTtl::set_ttl(&c, Duration::from_secs(1)).unwrap();
            assert_eq!(2, old.as_secs());
            assert!(c.async_cache_set(1, 100).await.unwrap().is_none());
            assert!(c.async_cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(1, 600_000));
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
            let key = format!("cached-redis-store:{}:1", prefix);
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
            let key = format!("cached-redis-store:{}:1", prefix);
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
            let key = format!("cached-redis-store:{}:1", prefix);
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
            let key = format!("cached-redis-store:{}:1", prefix);
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
            let key = format!("cached-redis-store:{}:1", prefix);
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
            let key = format!("cached-redis-store:{}:1", prefix);
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
            let key = format!("cached-redis-store:{}:1", prefix);

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
            let key = format!("cached-redis-store:{}:7", prefix);

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
            let key = format!("cached-redis-store:{}:1", prefix);
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
            let key = format!("cached-redis-store:{}:1", prefix);
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
        fn multiplexed_variant_exists(
            c: redis::aio::MultiplexedConnection,
        ) -> AsyncRedisConnection {
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
}

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
mod error_source_tests {
    use std::error::Error;

    use super::{RedisCacheBuildError, RedisCacheError};

    /// `RedisCacheBuildError::MissingConnectionString` must expose its inner
    /// `VarError` via `Error::source()`.
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

    /// `MissingConnectionString`'s Display must read cleanly: env key and the
    /// VarError's human message, with no `VarError { .. }` / `NotPresent`
    /// debug noise.
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
        // The inner error's *Display* message is present.
        assert!(
            rendered.contains(&std::env::VarError::NotPresent.to_string()),
            "Display must include the VarError's human message; got: {rendered}"
        );
        // No Debug-form noise.
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
    /// `rmp_serde::decode::Error` via `Error::source()`.
    #[test]
    fn cache_deserialization_has_source() {
        // Construct a decode error by trying to decode garbage bytes.
        let bad_bytes: Vec<u8> = vec![0xc1]; // 0xc1 is an unused msgpack byte
        let inner: rmp_serde::decode::Error = rmp_serde::from_slice::<u32>(&bad_bytes).unwrap_err();
        let inner_display = inner.to_string();
        let err = RedisCacheError::deserialization(inner, bad_bytes.clone());
        let source = err
            .source()
            .expect("CacheDeserialization must expose its inner decode::Error as source()");
        assert!(
            source.downcast_ref::<rmp_serde::decode::Error>().is_some(),
            "source() must downcast to rmp_serde::decode::Error"
        );
        let rendered = err.to_string();
        assert!(
            !rendered.is_empty(),
            "Display must produce a non-empty string; got: {rendered}"
        );
        // The source error's message is reachable via source().
        assert_eq!(
            source.to_string(),
            inner_display,
            "source() display must match the original decode error"
        );
        // The cached_value field is accessible on the variant.
        if let RedisCacheError::CacheDeserialization { cached_value, .. } = &err {
            assert_eq!(cached_value, &bad_bytes);
        } else {
            panic!("expected CacheDeserialization");
        }
    }

    /// `RedisCacheError::CacheSerialization` must expose its inner
    /// `rmp_serde::encode::Error` via `Error::source()`.
    #[test]
    fn cache_serialization_has_source() {
        // Construct an encode error via a type that fails to serialize.
        #[derive(Debug)]
        struct Unserializable;
        impl serde::Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional failure"))
            }
        }
        let inner: rmp_serde::encode::Error = rmp_serde::to_vec(&Unserializable).unwrap_err();
        let inner_display = inner.to_string();
        let err = RedisCacheError::serialization(inner);
        let source = err
            .source()
            .expect("CacheSerialization must expose its inner encode::Error as source()");
        assert!(
            source.downcast_ref::<rmp_serde::encode::Error>().is_some(),
            "source() must downcast to rmp_serde::encode::Error"
        );
        // The inner serde error message is reachable.
        assert_eq!(
            source.to_string(),
            inner_display,
            "source() display must match the original encode error"
        );
    }

    /// MessagePack round-trip: a value serialized with rmp_serde can be
    /// deserialized back to the same value without going through Redis.
    /// This verifies the codec chosen for the redis store works end-to-end.
    #[test]
    fn msgpack_round_trip_via_cached_redis_value() {
        use super::CachedRedisValue;

        let original: u64 = 42;
        let wrapped = CachedRedisValue::new(original);
        let bytes = rmp_serde::to_vec(&wrapped).expect("serialize must succeed");

        // Bytes must be non-empty and not UTF-8 text (they are binary msgpack).
        assert!(!bytes.is_empty());
        // The msgpack encoding of a struct is not the same as JSON text.
        assert!(
            std::str::from_utf8(&bytes).is_err() || !bytes.starts_with(b"{"),
            "msgpack output should not look like JSON"
        );

        let recovered: CachedRedisValue<u64> =
            rmp_serde::from_slice(&bytes).expect("deserialize must succeed");
        assert_eq!(recovered.value, original);
        assert_eq!(recovered.version, Some(1));
    }

    /// MessagePack round-trip for a string value.
    #[test]
    fn msgpack_round_trip_string_value() {
        use super::CachedRedisValue;

        let original = "hello, msgpack!".to_string();
        let wrapped = CachedRedisValue::new(original.clone());
        let bytes = rmp_serde::to_vec(&wrapped).expect("serialize must succeed");
        let recovered: CachedRedisValue<String> =
            rmp_serde::from_slice(&bytes).expect("deserialize must succeed");
        assert_eq!(recovered.value, original);
    }

    /// The shared backward-read helper round-trips the current MessagePack format.
    #[test]
    fn deserialize_helper_reads_msgpack() {
        use super::{CachedRedisValue, deserialize_cached_redis_value};

        let bytes = rmp_serde::to_vec(&CachedRedisValue::new(7u64)).expect("serialize");
        let recovered: CachedRedisValue<u64> =
            deserialize_cached_redis_value(&bytes).expect("msgpack must deserialize");
        assert_eq!(recovered.value, 7u64);
        assert_eq!(recovered.version, Some(1));
    }

    /// The helper transparently reads the legacy pre-3.0 JSON format: a
    /// `CachedRedisValue` serialized with `serde_json` (the cached 2.x on-disk
    /// shape, carrying a `version` key) must deserialize via the helper.
    #[test]
    fn deserialize_helper_reads_legacy_json() {
        use super::{CachedRedisValue, deserialize_cached_redis_value};

        // Old format: JSON object with `value` and `version` keys.
        let json = serde_json::to_vec(&CachedRedisValue::new("legacy".to_string()))
            .expect("json serialize");
        // Sanity: this is JSON text, not msgpack, so the msgpack path must fail
        // first and the helper must fall through to the JSON path.
        assert!(json.starts_with(b"{"));
        assert!(rmp_serde::from_slice::<CachedRedisValue<String>>(&json).is_err());

        let recovered: CachedRedisValue<String> =
            deserialize_cached_redis_value(&json).expect("legacy JSON must deserialize");
        assert_eq!(recovered.value, "legacy");
        assert_eq!(recovered.version, Some(1));
    }

    /// A legacy JSON object that lacks the `version` key is NOT treated as the
    /// old format: the helper must surface a `CacheDeserialization` error
    /// (preserving the original msgpack error) rather than silently coercing.
    #[test]
    fn deserialize_helper_rejects_json_without_version() {
        use super::{RedisCacheError, deserialize_cached_redis_value};

        // `{"value": 1}` parses as JSON but has no `version` key.
        let bytes = br#"{"value": 1}"#.to_vec();
        match deserialize_cached_redis_value::<u64>(&bytes) {
            Ok(_) => panic!("JSON without a version key must not be accepted"),
            Err(RedisCacheError::CacheDeserialization { cached_value, .. }) => {
                assert_eq!(cached_value, bytes, "raw bytes must be preserved");
            }
            Err(other) => panic!("expected CacheDeserialization, got: {other:?}"),
        }
    }

    /// Corrupt bytes (neither valid msgpack nor legacy JSON) yield a
    /// `CacheDeserialization` error that preserves the original raw bytes.
    #[test]
    fn deserialize_helper_corrupt_bytes_preserve_value() {
        use super::{RedisCacheError, deserialize_cached_redis_value};

        // 0xc1 is an unused/reserved msgpack byte and is not valid JSON either.
        let bytes: Vec<u8> = vec![0xc1, 0x00, 0xff];
        match deserialize_cached_redis_value::<u64>(&bytes) {
            Ok(_) => panic!("corrupt bytes must not deserialize"),
            Err(RedisCacheError::CacheDeserialization { cached_value, .. }) => {
                assert_eq!(
                    cached_value, bytes,
                    "the original corrupt bytes must be preserved in the error"
                );
            }
            Err(other) => panic!("expected CacheDeserialization, got: {other:?}"),
        }
    }

    /// Non-contract but documented-to-function: the opaque boxed `source` of a
    /// `RedisCacheError::Redis` still downcasts back to `redis::RedisError`. The
    /// semver note says consumers should match on the variant rather than
    /// downcast, but this must keep working for callers that do.
    #[test]
    fn redis_error_source_downcasts_to_redis_error() {
        let re = redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "boom"));
        let err = RedisCacheError::redis(re);
        let source = err.source().expect("Redis variant must expose a source");
        assert!(
            source.downcast_ref::<redis::RedisError>().is_some(),
            "source() must still downcast to redis::RedisError"
        );
    }

    /// The sanitized boxed `source` of a `RedisCacheBuildError::Connection` also
    /// downcasts to `redis::RedisError` (the build path wraps a synthetic redis
    /// error). Downcasting is non-contract but functional.
    #[test]
    fn build_connection_source_downcasts_to_redis_error() {
        let err = RedisCacheBuildError::connection(redis::RedisError::from((
            redis::ErrorKind::InvalidClientConfig,
            "sanitized synthetic error",
        )));
        let source = err
            .source()
            .expect("Connection variant must expose a source");
        assert!(
            source.downcast_ref::<redis::RedisError>().is_some(),
            "Connection source() must downcast to redis::RedisError"
        );
    }

    /// `RedisCacheError::is_deserialization` classifies only the decode variant,
    /// not `Redis` / `CacheSerialization`. Stable classifier under the opaque
    /// source reshape.
    #[test]
    fn is_deserialization_classifier_distinguishes_variants() {
        let bad: Vec<u8> = vec![0xc1];
        let deser =
            RedisCacheError::deserialization(rmp_serde::from_slice::<u32>(&bad).unwrap_err(), bad);
        assert!(
            deser.is_deserialization(),
            "decode error must classify true"
        );

        let redis_err = RedisCacheError::redis(redis::RedisError::from((
            redis::ErrorKind::InvalidClientConfig,
            "x",
        )));
        assert!(
            !redis_err.is_deserialization(),
            "redis error must classify false"
        );

        #[derive(Debug)]
        struct Unserializable;
        impl serde::Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional failure"))
            }
        }
        let ser = RedisCacheError::serialization(rmp_serde::to_vec(&Unserializable).unwrap_err());
        assert!(
            !ser.is_deserialization(),
            "serialization error must classify false"
        );
    }

    /// `RedisCache` is `Clone` - compile-time bound check.
    #[allow(dead_code)]
    fn assert_clone<T: Clone>() {}
    #[allow(dead_code)]
    fn check_redis_cache_is_clone() {
        assert_clone::<super::RedisCache<String, String>>();
    }
    /// `AsyncRedisCache` is `Clone` - compile-time bound check.
    #[cfg(any(
        feature = "redis_smol",
        feature = "redis_smol_native_tls",
        feature = "redis_smol_rustls",
        feature = "redis_tokio",
        feature = "redis_tokio_native_tls",
        feature = "redis_tokio_rustls",
    ))]
    #[allow(dead_code)]
    fn check_async_redis_cache_is_clone() {
        assert_clone::<super::AsyncRedisCache<String, String>>();
    }
}

#[cfg(test)]
mod connection_string_tests {
    // No Redis server needed -- verifies the redaction behavior of
    // `ConnectionString` (returned by `connection_string()`) and that `reveal()`
    // exposes the raw URL.
    use super::ConnectionString;

    /// `Display` of `ConnectionString` returns the redacted placeholder, not the raw URL.
    #[test]
    fn display_is_redacted() {
        let cs = ConnectionString("redis://:secret@127.0.0.1:6379".to_string());
        let displayed = cs.to_string();
        assert_eq!(
            displayed, "[REDACTED connection string]",
            "Display must return the redacted placeholder, got: {displayed}"
        );
        assert!(
            !displayed.contains("secret"),
            "Display must not expose the password; got: {displayed}"
        );
    }

    /// `Debug` of `ConnectionString` also returns the redacted placeholder.
    #[test]
    fn debug_is_redacted() {
        let cs = ConnectionString("redis://:secret@127.0.0.1:6379".to_string());
        let debugged = format!("{cs:?}");
        assert_eq!(
            debugged, "[REDACTED connection string]",
            "Debug must return the redacted placeholder, got: {debugged}"
        );
        assert!(
            !debugged.contains("secret"),
            "Debug must not expose the password; got: {debugged}"
        );
    }

    /// `reveal()` returns the raw URL, including credentials.
    #[test]
    fn reveal_returns_raw() {
        let raw = "redis://:secret@127.0.0.1:6379";
        let cs = ConnectionString(raw.to_string());
        assert_eq!(cs.reveal(), raw);
        assert!(cs.reveal().contains("secret"));
    }

    /// Both `Debug` and `Display` redact while `reveal()` still exposes the raw value.
    #[test]
    fn debug_and_display_redact_but_reveal_does_not() {
        let cs = ConnectionString("redis://:s3cr3t@localhost:6379/0".to_string());
        assert_eq!(cs.to_string(), "[REDACTED connection string]");
        assert_eq!(format!("{cs:?}"), "[REDACTED connection string]");
        assert!(!cs.to_string().contains("s3cr3t"));
        assert!(!format!("{cs:?}").contains("s3cr3t"));
        // The raw value is still recoverable via reveal().
        assert!(cs.reveal().contains("s3cr3t"));
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
        let c: RedisCache<u32, u32> = RedisCache::builder(format!("{}:redis-cache-test", now_millis()))
            .ttl(Duration::from_secs(2))
            .namespace("in-tests:")
            .build()
            .unwrap();

        assert!(c.cache_get(&1).unwrap().is_none());

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(2, 500_000));
        assert!(c.cache_get(&1).unwrap().is_none());

        let old = ConcurrentCacheTtl::set_ttl(&c, Duration::from_secs(1)).unwrap();
        assert_eq!(2, old.as_secs());
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(1, 600_000));
        assert!(c.cache_get(&1).unwrap().is_none());

        ConcurrentCacheTtl::set_ttl(&c, Duration::from_secs(10)).unwrap();
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_set(2, 100).unwrap().is_none());
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
    }

    #[test]
    fn remove() {
        let c: RedisCache<u32, u32> = RedisCache::builder(format!("{}:redis-cache-test-remove", now_millis()))
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_set(2, 200).unwrap().is_none());
        assert!(c.cache_set(3, 300).unwrap().is_none());

        assert_eq!(100, c.cache_remove(&1).unwrap().unwrap());
    }

    /// D2: default mode -- a corrupt cache entry is deleted and `cache_get`
    /// returns `Ok(None)` (self-heal) instead of propagating the decode error.
    #[test]
    fn cache_get_self_heals_corrupted_entry_by_default() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:selfheal-default", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        // Write garbage bytes so deserialization will fail.
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        // In default mode (strict_deserialization=false), corrupt entry returns Ok(None)
        let result = c.cache_get(&1).unwrap();
        assert!(
            result.is_none(),
            "expected Ok(None) after self-heal, got: {result:?}"
        );

        // The key must have been deleted (self-heal).
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(!exists, "corrupt key must be deleted after self-heal");
    }

    /// D2: strict mode -- a corrupt cache entry returns `Err(CacheDeserialization)`
    /// and the key is NOT deleted.
    #[test]
    fn cache_get_strict_mode_returns_error_for_corrupted_entry() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:selfheal-strict", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .strict_deserialization(true)
            .build()
            .unwrap();

        // In strict mode, corrupt entry must return a deserialization error.
        let err = c
            .cache_get(&1)
            .expect_err("strict mode must return Err for corrupt entry");
        assert!(
            err.is_deserialization(),
            "expected CacheDeserialization, got: {err:?}"
        );

        // The key must NOT have been deleted in strict mode.
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(exists, "corrupt key must NOT be deleted in strict mode");

        // cleanup
        let _: () = redis::cmd("DEL").arg(&key).query(&mut conn).unwrap();
    }

    /// REDIS-10: on the SET path, a pre-existing corrupt value returns Ok(None)
    /// not an error, in both default and strict modes.
    #[test]
    fn cache_set_displaced_corrupt_previous_returns_ok_none() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:redis10-test", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        // Plant a corrupt value so the GET in the SET pipeline will see garbage.
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        // cache_set must succeed and return Ok(None) even though the displaced value
        // was corrupt.
        let result = c.cache_set(1, 42);
        assert!(
            result.is_ok(),
            "cache_set must not error on corrupt displaced value; got: {result:?}"
        );
        assert!(
            result.unwrap().is_none(),
            "displaced corrupt value must yield Ok(None)"
        );
    }

    /// D2: after a self-healed miss the corrupt key is deleted; recomputing via
    /// `cache_set` and reading again must produce a HIT. Covers the full
    /// self-heal -> recompute -> hit cycle, not just the initial `Ok(None)`.
    #[test]
    fn cache_get_self_heal_then_recompute_produces_hit() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:selfheal-recompute", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        // Corrupt entry self-heals to a miss and is deleted.
        assert_eq!(c.cache_get(&1).unwrap(), None, "self-heal returns a miss");
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(!exists, "self-heal must delete the corrupt key");

        // Recompute over the healed miss (previous value is None), then a HIT.
        assert_eq!(
            c.cache_set(1, 77).unwrap(),
            None,
            "recompute writes over the healed miss"
        );
        assert_eq!(
            c.cache_get(&1).unwrap(),
            Some(77),
            "the read after recompute is a HIT"
        );

        let _: () = redis::cmd("DEL").arg(&key).query(&mut conn).unwrap();
    }

    /// `cache_set_ref` returns `Ok(())` regardless of any pre-existing (even
    /// corrupt) value under the key: it does not read the previous value back, so
    /// an undecodable displaced value can never surface as an error. The new value
    /// is written and readable.
    #[test]
    fn cache_set_ref_over_corrupt_previous_returns_ok_unit() {
        use crate::SerializeCached;
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:redis10-setref", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        let val = 42u32;
        let result = SerializeCached::cache_set_ref(&c, &1, &val);
        assert!(
            result.is_ok(),
            "cache_set_ref must not error over a corrupt previous value; got: {result:?}"
        );
        // The new value was written and is now readable.
        assert_eq!(c.cache_get(&1).unwrap(), Some(42));

        let _: () = redis::cmd("DEL").arg(&key).query(&mut conn).unwrap();
    }

    /// BUG-2 (sync, default mode): `cache_remove` on a key with corrupt stored bytes
    /// returns `Ok(None)` and the key is deleted. The corrupt bytes must NOT surface
    /// as `Err(CacheDeserialization)` in default (non-strict) mode.
    #[test]
    fn cache_remove_corrupt_default_mode_returns_ok_none() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:remove-corrupt-default", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        // Plant garbage bytes so deserialization will fail.
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        // Default mode: corrupt entry must be silently discarded as Ok(None).
        let result = c.cache_remove(&1);
        assert!(
            result.is_ok(),
            "cache_remove must not error in default mode on corrupt entry; got: {result:?}"
        );
        assert!(
            result.unwrap().is_none(),
            "cache_remove must return Ok(None) for corrupt entry in default mode"
        );

        // The key must be gone after remove regardless of decode failure.
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(
            !exists,
            "cache_remove must delete the key even when bytes are corrupt"
        );
    }

    /// BUG-2 (sync, strict mode): `cache_remove` on a key with corrupt stored bytes
    /// returns `Err(CacheDeserialization)` in strict mode AND the key is still deleted
    /// (the pipeline already ran GET+DEL atomically).
    #[test]
    fn cache_remove_corrupt_strict_mode_returns_error_and_key_is_gone() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:remove-corrupt-strict", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .strict_deserialization(true)
            .build()
            .unwrap();

        // Strict mode: must return a deserialization error.
        let err = c
            .cache_remove(&1)
            .expect_err("strict cache_remove must return Err for corrupt entry");
        assert!(
            err.is_deserialization(),
            "expected CacheDeserialization error, got: {err:?}"
        );

        // The key must still be gone -- the GET+DEL pipeline already ran.
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(
            !exists,
            "key must be deleted even when strict cache_remove errors"
        );
    }

    /// BUG-2 (sync, happy path): `cache_remove` on a VALID stored value returns
    /// `Ok(Some(value))` and the key is actually gone afterward (a follow-up get
    /// returns None). Locks the non-regressed remove-and-return contract next to
    /// the corrupt-path tests. The pre-existing `remove` test asserts the returned
    /// value but never verifies the key was deleted.
    #[test]
    fn cache_remove_valid_value_returns_some_and_deletes_key() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:remove-valid", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert_eq!(
            c.cache_remove(&1).unwrap(),
            Some(100),
            "cache_remove must return the stored value"
        );
        // The key is physically gone...
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(!exists, "cache_remove must delete the key");
        // ...and a follow-up get is a miss.
        assert_eq!(
            c.cache_get(&1).unwrap(),
            None,
            "get after remove must be a miss"
        );
    }

    /// BUG-2 (sync, missing key): `cache_remove` on a key that was never stored
    /// returns `Ok(None)` (the `res.0 == None` branch), not an error.
    #[test]
    fn cache_remove_missing_key_returns_ok_none() {
        let prefix = format!("{}:remove-missing", now_millis());
        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        let result = c.cache_remove(&12345);
        assert!(
            result.is_ok(),
            "cache_remove on a missing key must not error; got: {result:?}"
        );
        assert_eq!(
            result.unwrap(),
            None,
            "cache_remove on a missing key must return Ok(None)"
        );
    }

    /// BUG-2 (sync `cache_remove_entry` delegation, valid value): the entry variant
    /// must return `Ok(Some((key, value)))` and delete the key. Pins that
    /// `cache_remove_entry` forwards the key alongside the removed value.
    #[test]
    fn cache_remove_entry_valid_returns_key_and_value() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:remove-entry-valid", now_millis());
        let key = format!("cached-redis-store:{}:7", prefix);

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        assert!(c.cache_set(7, 700).unwrap().is_none());
        assert_eq!(
            c.cache_remove_entry(&7).unwrap(),
            Some((7, 700)),
            "cache_remove_entry must return the key and the stored value"
        );
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(!exists, "cache_remove_entry must delete the key");
    }

    /// BUG-2 (sync `cache_remove_entry` delegation, default mode): corrupt bytes
    /// must be discarded as `Ok(None)` (the entry delegates to `cache_remove`, so it
    /// inherits the non-strict self-heal) and the key is deleted. Direct coverage —
    /// the delegation was previously only inferred.
    #[test]
    fn cache_remove_entry_corrupt_default_mode_returns_ok_none() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:remove-entry-corrupt-default", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        let result = c.cache_remove_entry(&1);
        assert!(
            result.is_ok(),
            "cache_remove_entry must not error in default mode on corrupt entry; got: {result:?}"
        );
        assert_eq!(
            result.unwrap(),
            None,
            "cache_remove_entry must return Ok(None) for a corrupt entry in default mode"
        );
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(
            !exists,
            "cache_remove_entry must delete the key even when bytes are corrupt"
        );
    }

    /// BUG-2 (sync `cache_remove_entry` delegation, strict mode): corrupt bytes must
    /// surface as the exact `RedisCacheError::CacheDeserialization { .. }` variant
    /// (not merely any Err) and the key is still deleted. Direct coverage of the
    /// strict delegation, and the only place the exact variant is matched by shape.
    #[test]
    fn cache_remove_entry_corrupt_strict_mode_returns_error_and_key_is_gone() {
        let mut conn = redis::Client::open("redis://127.0.0.1:6399")
            .unwrap()
            .get_connection()
            .unwrap();
        let prefix = format!("{}:remove-entry-corrupt-strict", now_millis());
        let key = format!("cached-redis-store:{}:1", prefix);
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(b"\xff\xfe\xfd".as_ref())
            .query(&mut conn)
            .unwrap();

        let c: RedisCache<u32, u32> = RedisCache::builder(prefix)
            .ttl(Duration::from_secs(3600))
            .strict_deserialization(true)
            .build()
            .unwrap();

        let err = c
            .cache_remove_entry(&1)
            .expect_err("strict cache_remove_entry must return Err for corrupt entry");
        assert!(
            matches!(err, RedisCacheError::CacheDeserialization { .. }),
            "expected the exact CacheDeserialization variant, got: {err:?}"
        );
        let exists: bool = redis::cmd("EXISTS").arg(&key).query(&mut conn).unwrap();
        assert!(
            !exists,
            "key must be deleted even when strict cache_remove_entry errors"
        );
    }
}
