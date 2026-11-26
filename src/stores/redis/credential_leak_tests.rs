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
