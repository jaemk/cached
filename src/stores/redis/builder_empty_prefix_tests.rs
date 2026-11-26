// No Redis server needed -- verifies the empty-prefix guard in `build()`.
use super::super::BuildError;
use super::{RedisCacheBuildError, RedisCacheBuilder};
use crate::time::Duration;

#[test]
fn empty_prefix_with_default_namespace_is_rejected() {
    // The dangerous shape: the default namespace is shared by every cache, so
    // an empty prefix would let cache_clear delete all of their entries.
    let result = RedisCacheBuilder::<String, String>::new()
        .prefix("")
        .ttl(Duration::from_secs(1))
        .build();
    assert!(
        matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::InvalidValue {
                field: "prefix",
                ..
            }))
        ),
        "expected InvalidValue for empty prefix"
    );
}

#[test]
fn empty_prefix_with_custom_namespace_is_rejected() {
    let result = RedisCacheBuilder::<String, String>::new()
        .prefix("")
        .ttl(Duration::from_secs(1))
        .namespace("my-ns")
        .build();
    assert!(
        matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::InvalidValue {
                field: "prefix",
                ..
            }))
        ),
        "expected InvalidValue for empty prefix"
    );
}

#[test]
fn non_empty_prefix_passes_the_guard() {
    // Guard must not fire when the prefix is set -- no real Redis needed
    // because a build error past the guard would come from the connection
    // path, not prefix validation.
    let result = RedisCacheBuilder::<String, String>::new()
        .prefix("my-prefix")
        .ttl(Duration::from_secs(1))
        .namespace("")
        .build();
    assert!(
        !matches!(
            result,
            Err(RedisCacheBuildError::Build(BuildError::InvalidValue {
                field: "prefix",
                ..
            }))
        ),
        "prefix guard must not fire when prefix is non-empty"
    );
}
