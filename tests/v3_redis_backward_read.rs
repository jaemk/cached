/*!
Integration tests for Redis store backward-compatibility and millisecond TTL precision.

All tests require a live Redis server. They gate on the `redis_store` feature and skip
cleanly when `CACHED_REDIS_CONNECTION_STRING` is not set (return early without panicking).

Covered:
- M7b: backward read of a pre-3.0 JSON-encoded entry (written directly via raw redis,
  read back transparently via `RedisCache::cache_get`).
- M7c: msgpack round-trip with a structured (non-string) value type through `RedisCache`.
- M7a: sub-second TTL precision -- `PTTL` confirms the key was written with millisecond
  granularity rather than being rounded up to a whole second.
*/

#![cfg(feature = "redis_store")]

use std::time::Duration;

use cached::{ConcurrentCached, RedisCache};

const ENV_KEY: &str = "CACHED_REDIS_CONNECTION_STRING";

/// Return the connection string from the env var, or skip (return from the caller) if absent.
macro_rules! conn_or_skip {
    () => {
        match std::env::var(ENV_KEY) {
            Ok(s) => s,
            Err(_) => return,
        }
    };
}

// ─────────────────────────────── M7b: backward read ──────────────────────────
//
// Write a key in the OLD 2.x JSON format (`{"value": <V>, "version": 1}`) directly
// via the raw redis client at the exact key a `RedisCache` with the matching
// namespace/prefix would read, then assert `cache_get` returns the value
// transparently.

#[test]
fn redis_backward_read_legacy_json_entry() {
    let _conn_url = conn_or_skip!();

    let prefix = "v3_backward_read_legacy";
    let namespace = "";

    let cache = RedisCache::<String, String>::builder(prefix)
        .namespace(namespace)
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build RedisCache");

    cache.cache_clear().expect("clear");

    // The key scheme for namespace="" prefix="v3_backward_read_legacy" key="hello" is
    // "v3_backward_read_legacy:hello" (namespace skipped when empty).
    let raw_key = format!("{prefix}:hello");
    let legacy_value = r#"{"value":"world","version":1}"#;

    // Write the legacy JSON directly into Redis with a generous TTL.
    let conn_str = cache.connection_string();
    let mut raw = redis::Client::open(conn_str.reveal())
        .expect("raw client")
        .get_connection()
        .expect("raw connection");

    redis::cmd("SET")
        .arg(&raw_key)
        .arg(legacy_value)
        .arg("EX")
        .arg(60i64)
        .query::<()>(&mut raw)
        .expect("SET legacy entry");

    // RedisCache must transparently deserialize the legacy JSON entry.
    let got = cache
        .cache_get(&"hello".to_string())
        .expect("cache_get legacy");
    assert_eq!(
        got,
        Some("world".to_string()),
        "cache_get must transparently read a pre-3.0 JSON-encoded entry"
    );

    // Now write a fresh entry through the store (msgpack) and read it back.
    cache
        .cache_set("fresh_key".to_string(), "fresh_val".to_string())
        .expect("cache_set msgpack");
    let got2 = cache
        .cache_get(&"fresh_key".to_string())
        .expect("cache_get msgpack");
    assert_eq!(
        got2,
        Some("fresh_val".to_string()),
        "cache_get must read a store-written (msgpack) entry"
    );

    cache.cache_clear().expect("clean up");
}

// ─────────────────────────────── M7c: structured-value msgpack round-trip ────
//
// A derived struct value is set through a `RedisCache<String, MyStruct>` and
// retrieved by `cache_get`. Proves the msgpack encoding handles non-primitive
// value types correctly.

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
struct Point {
    x: i32,
    y: i32,
    label: String,
}

#[test]
fn redis_msgpack_round_trip_struct_value() {
    let _conn_url = conn_or_skip!();

    let prefix = "v3_msgpack_struct_rt";

    let cache = RedisCache::<String, Point>::builder(prefix)
        .namespace("")
        .ttl(Duration::from_secs(60))
        .build()
        .expect("build RedisCache<String, Point>");

    cache.cache_clear().expect("clear");

    let key = "origin".to_string();
    let point = Point {
        x: 42,
        y: -7,
        label: "test-point".to_string(),
    };

    // Miss: not yet in cache.
    assert_eq!(
        cache.cache_get(&key).expect("cache_get miss"),
        None,
        "first cache_get must return None"
    );

    // Set.
    cache
        .cache_set(key.clone(), point.clone())
        .expect("cache_set Point");

    // Hit: must deserialize back to the same value.
    let got = cache.cache_get(&key).expect("cache_get hit");
    assert_eq!(
        got,
        Some(point),
        "cache_get must return the exact struct that was set"
    );

    cache.cache_clear().expect("clean up");
}

// ─────────────────────────────── M7a: sub-second TTL precision (PTTL) ────────
//
// Set an entry with a sub-second TTL (750 ms), then query PTTL on the raw
// connection and assert the remaining TTL is in a plausible millisecond band:
//   > 0  (key has not already expired)
//   <= 750  (the key was NOT rounded up to a whole second)
// This certifies that the store writes with `PSETEX` (millisecond precision),
// not `SETEX` (which would round to 1000 ms).

#[test]
fn redis_subsecond_ttl_precision_via_pttl() {
    let _conn_url = conn_or_skip!();

    let prefix = "v3_subsecond_ttl_pttl";
    let ttl_ms = 750u64;

    let cache = RedisCache::<String, String>::builder(prefix)
        .namespace("")
        .ttl(Duration::from_millis(ttl_ms))
        .build()
        .expect("build RedisCache with 750ms TTL");

    cache.cache_clear().expect("clear");

    cache
        .cache_set("k".to_string(), "v".to_string())
        .expect("cache_set");

    // Query PTTL (millisecond TTL) on the raw key.
    let conn_str = cache.connection_string();
    let mut raw = redis::Client::open(conn_str.reveal())
        .expect("raw client")
        .get_connection()
        .expect("raw connection");

    let pttl: i64 = redis::cmd("PTTL")
        .arg(format!("{prefix}:k"))
        .query(&mut raw)
        .expect("PTTL query");

    assert!(
        pttl > 0,
        "PTTL must be positive (key must not have already expired); got {pttl}"
    );
    assert!(
        pttl <= ttl_ms as i64,
        "PTTL must be <= {ttl_ms} ms (certifies millisecond precision, not whole-second rounding); got {pttl}"
    );

    // Ensure the entry is also readable (store is consistent).
    let got = cache.cache_get(&"k".to_string()).expect("cache_get");
    assert_eq!(
        got,
        Some("v".to_string()),
        "cache_get must return the value"
    );

    cache.cache_clear().expect("clean up");
}
