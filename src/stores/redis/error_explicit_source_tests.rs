// No Redis server needed -- verifies the explicit `#[source]` on
// `RedisCacheError::Redis`, `RedisCacheError::Pool`, and
// `RedisCacheBuildError::Pool` wires `std::error::Error::source()` correctly.
use super::{RedisCacheBuildError, RedisCacheError};
use std::error::Error;

/// `RedisCacheBuildError::Pool` must expose its inner error through `.source()`.
/// Constructs the variant directly (no live Redis needed) and verifies that
/// `source()` returns `Some(...)` pointing at the boxed error, and that the
/// returned reference downcasts to `std::io::Error`.
#[test]
fn redis_cache_build_error_pool_source_is_accessible() {
    let inner = std::io::Error::other("synthetic pool error");
    let err = RedisCacheBuildError::Pool {
        source: Box::new(inner),
    };
    let src = err.source().expect("Pool source must be Some");
    assert!(
        src.downcast_ref::<std::io::Error>().is_some(),
        "source should downcast to std::io::Error; got: {src:?}"
    );
}

/// `RedisCacheError::Redis` must expose its inner error through `.source()`.
#[test]
fn redis_cache_error_redis_source_is_accessible() {
    let inner = std::io::Error::other("synthetic redis error");
    let err = RedisCacheError::Redis {
        source: Box::new(inner),
    };
    let src = err.source().expect("Redis source must be Some");
    assert!(
        src.downcast_ref::<std::io::Error>().is_some(),
        "source should downcast to std::io::Error; got: {src:?}"
    );
}

/// `RedisCacheError::Pool` must expose its inner error through `.source()`.
#[test]
fn redis_cache_error_pool_source_is_accessible() {
    let inner = std::io::Error::other("synthetic redis pool error");
    let err = RedisCacheError::Pool {
        source: Box::new(inner),
    };
    let src = err.source().expect("Pool source must be Some");
    assert!(
        src.downcast_ref::<std::io::Error>().is_some(),
        "source should downcast to std::io::Error; got: {src:?}"
    );
}
