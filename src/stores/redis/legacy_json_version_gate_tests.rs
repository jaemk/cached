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
    match deserialize_cached_redis_value::<String>(&bytes, Default::default()) {
        Ok(_) => {
            panic!("JSON with an unexpected `version` value must not be accepted as a legacy entry")
        }
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
    match deserialize_cached_redis_value::<u64>(&bytes, Default::default()) {
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
    let result = deserialize_cached_redis_value::<String>(&bytes, Default::default());
    assert!(
        result.is_ok(),
        "JSON with version=1 must be accepted as a legacy entry"
    );
    assert_eq!(result.unwrap().value, "ok");
}
