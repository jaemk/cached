//! The `Debug` impls for `RedisCacheError` / `RedbCacheError` redact the raw
//! `cached_value` bytes of the `CacheDeserialization` variant as
//! `<N bytes redacted>`, so a `{:?}` of the error never leaks the payload (the
//! bytes may carry sensitive application data).

// A recognizable byte pattern that must never appear verbatim in Debug output.
const SECRET: &[u8] = b"SUPER_SECRET_PAYLOAD_0xDEADBEEF";

#[cfg(feature = "redb_store")]
#[test]
fn redb_cache_deserialization_debug_redacts_cached_value() {
    use cached::RedbCacheError;

    // Build the error via the crate's Debug/Display surface: reproduce a
    // CacheDeserialization by decoding invalid bytes through the same path the
    // store uses. The variant is public, so construct it through a decode failure.
    // 0xc1 is the MessagePack "never used" marker, so decoding it always fails;
    // the source error's content is unrelated to the redacted cached_value.
    let decode_err = rmp_serde::from_slice::<u32>(&[0xc1]).unwrap_err();
    let err = RedbCacheError::CacheDeserialization {
        source: Box::new(decode_err),
        cached_value: SECRET.to_vec(),
    };

    let dbg = format!("{err:?}");
    assert!(
        !dbg.contains("SUPER_SECRET_PAYLOAD"),
        "Debug must not contain the raw cached_value bytes: {dbg}"
    );
    assert!(
        dbg.contains(&format!("<{} bytes redacted>", SECRET.len())),
        "Debug must show the redaction marker: {dbg}"
    );
}

#[cfg(feature = "redis_store")]
#[test]
fn redis_cache_deserialization_debug_redacts_cached_value() {
    use cached::RedisCacheError;

    // 0xc1 is the MessagePack "never used" marker, so decoding it always fails;
    // the source error's content is unrelated to the redacted cached_value.
    let decode_err = rmp_serde::from_slice::<u32>(&[0xc1]).unwrap_err();
    let err = RedisCacheError::CacheDeserialization {
        source: Box::new(decode_err),
        cached_value: SECRET.to_vec(),
    };

    let dbg = format!("{err:?}");
    assert!(
        !dbg.contains("SUPER_SECRET_PAYLOAD"),
        "Debug must not contain the raw cached_value bytes: {dbg}"
    );
    assert!(
        dbg.contains(&format!("<{} bytes redacted>", SECRET.len())),
        "Debug must show the redaction marker: {dbg}"
    );
}
