//! Redis builder TTL validation after the TTL was made optional.
//!
//! Before the change an unset TTL was `MissingRequired("ttl")`. Now the TTL is
//! optional (unset => no expiry) and the ONLY remaining TTL error is an
//! *explicitly-set* zero TTL, which must be rejected as
//! `BuildError::InvalidValue { field: "ttl", .. }`. This validation runs before any
//! connection attempt, so the test needs no live redis.

#![cfg(feature = "redis_store")]

use cached::time::Duration;
use cached::{BuildError, RedisCacheBuildError, RedisCacheBuilder};

#[test]
fn sync_explicit_zero_ttl_is_invalid_value_server_free() {
    // Prefix IS set (so it is not the reported error) and TTL is explicitly zero.
    // Validation happens before any IO, so this errors without a live server.
    let result = RedisCacheBuilder::<u32, u32>::new()
        .prefix("zero-ttl-probe")
        .ttl(Duration::ZERO)
        .build();
    assert!(
        matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::InvalidValue {
                field: "ttl",
                ..
            }))
        ),
        "expected Build(InvalidValue {{ field: \"ttl\", .. }}) for an explicit zero TTL"
    );
}

#[test]
fn sync_zero_ttl_via_ttl_millis_is_invalid_value() {
    let result = RedisCacheBuilder::<u32, u32>::new()
        .prefix("zero-ttl-millis-probe")
        .ttl_millis(0)
        .build();
    assert!(
        matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::InvalidValue {
                field: "ttl",
                ..
            }))
        ),
        "expected Build(InvalidValue {{ field: \"ttl\", .. }}) for ttl_millis(0)"
    );
}

#[cfg(feature = "redis_tokio")]
#[tokio::test]
async fn async_explicit_zero_ttl_is_invalid_value_server_free() {
    use cached::AsyncRedisCacheBuilder;

    let result = AsyncRedisCacheBuilder::<u32, u32>::new()
        .prefix("zero-ttl-probe-async")
        .ttl(Duration::ZERO)
        .build()
        .await;
    assert!(
        matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::InvalidValue {
                field: "ttl",
                ..
            }))
        ),
        "expected Build(InvalidValue {{ field: \"ttl\", .. }}) for an explicit zero TTL (async)"
    );
}
