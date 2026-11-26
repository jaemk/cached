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

/// `RedisCacheError::CacheDeserialization` must expose its inner decode
/// error via `Error::source()`. The downcast below pins current behavior
/// only — the concrete source type is non-contract (see the semver note on
/// the error type) and may change with the codec in a major release.
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

/// `RedisCacheError::CacheSerialization` must expose its inner encode
/// error via `Error::source()`. The downcast below pins current behavior
/// only — the concrete source type is non-contract (see the semver note on
/// the error type) and may change with the codec in a major release.
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

/// The stored envelope is MessagePack in its compact form: a positional
/// 2-element array of `value` then `version`. The field names are not on the
/// wire, so the field ORDER is what a reader depends on. Reordering the
/// fields of `CachedRedisValue`, or switching a write site to
/// `rmp_serde::to_vec_named`, changes these bytes and breaks every entry
/// written by an earlier 3.x release.
#[test]
fn cached_redis_value_is_a_positional_array_of_value_then_version() {
    use super::{CachedRedisValue, CachedRedisValueRef};

    let bytes = rmp_serde::to_vec(&CachedRedisValue::new("hello".to_string())).expect("serialize");
    // 0x92: 2-element fixarray (a named encoding would start 0x82, fixmap).
    // 0xa5 "hello": 5-byte fixstr. 0x01: version 1 (`Some(1)` is written as
    // the bare payload).
    assert_eq!(
        bytes,
        vec![0x92, 0xa5, b'h', b'e', b'l', b'l', b'o', 0x01],
        "the frozen 3.x envelope is a positional array of `value` then `version`"
    );

    // The same bytes decode as a positional tuple in that order.
    let (value, version): (String, Option<u64>) =
        rmp_serde::from_slice(&bytes).expect("positional decode");
    assert_eq!(value, "hello");
    assert_eq!(version, super::REDIS_VALUE_VERSION);

    // The borrowed envelope used by `cache_set_ref` must be byte-identical,
    // which requires it to keep the same fields in the same order.
    let borrowed = "hello".to_string();
    assert_eq!(
        rmp_serde::to_vec(&CachedRedisValueRef::new(&borrowed)).expect("serialize borrowed"),
        bytes,
        "`cache_set_ref` must write the same positional layout as `cache_set`"
    );
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
    let json =
        serde_json::to_vec(&CachedRedisValue::new("legacy".to_string())).expect("json serialize");
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
