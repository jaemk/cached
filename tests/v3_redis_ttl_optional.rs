//! Redis TTL is optional: a `RedisCache` built without a TTL stores entries with
//! no expiry (a plain `SET`, raw redis `TTL` == -1), while a cache built with a
//! TTL still applies it (raw `TTL` > 0) and entries expire.
//!
//! Requires a live redis; the tests skip (return early) when none is reachable.

#![cfg(feature = "redis_store")]

use cached::time::Duration;
use cached::{ConcurrentCached, RedisCache};

fn try_build(prefix: &str, ttl: Option<Duration>) -> Option<RedisCache<String, String>> {
    let mut b = RedisCache::<String, String>::builder(prefix).namespace("");
    if let Some(t) = ttl {
        b = b.ttl(t);
    }
    b.build().ok()
}

// Raw redis `TTL` (seconds) for the namespace-less key `{prefix}:{key}`.
// -1 == persistent (no expiry), -2 == absent, otherwise remaining seconds.
fn raw_ttl_secs(cache: &RedisCache<String, String>, prefix: &str, key: &str) -> i64 {
    let client =
        redis::Client::open(cache.connection_string().reveal()).expect("open redis client");
    let mut conn = client.get_connection().expect("redis connection");
    redis::cmd("TTL")
        .arg(format!("{prefix}:{key}"))
        .query(&mut conn)
        .expect("TTL query")
}

#[test]
fn ttl_unset_stores_entry_without_expiry() {
    let prefix = "v3_ttl_optional_unset";
    let Some(cache) = try_build(prefix, None) else {
        eprintln!("skipping ttl_unset_stores_entry_without_expiry: no live redis");
        return;
    };
    cache.cache_clear().expect("clear");

    cache
        .cache_set("k".to_string(), "v".to_string())
        .expect("set");

    // Persistent key: raw redis TTL is -1 (set with no expiry).
    let ttl = raw_ttl_secs(&cache, prefix, "k");
    assert_eq!(
        ttl, -1,
        "an unset TTL must store the key without expiry (raw TTL -1), got {ttl}"
    );

    assert_eq!(
        cache.cache_get(&"k".to_string()).unwrap(),
        Some("v".to_string()),
        "the persistent entry must remain readable"
    );

    cache.cache_clear().expect("cleanup");
}

#[test]
fn ttl_set_applies_expiry() {
    let prefix = "v3_ttl_optional_set";
    let Some(cache) = try_build(prefix, Some(Duration::from_secs(30))) else {
        eprintln!("skipping ttl_set_applies_expiry: no live redis");
        return;
    };
    cache.cache_clear().expect("clear");

    cache
        .cache_set("k".to_string(), "v".to_string())
        .expect("set");

    let ttl = raw_ttl_secs(&cache, prefix, "k");
    assert!(
        ttl > 0,
        "an explicit TTL must store the key with a positive expiry, got {ttl}"
    );

    cache.cache_clear().expect("cleanup");
}

#[test]
fn ttl_set_short_entry_expires() {
    let prefix = "v3_ttl_optional_short";
    let Some(cache) = try_build(prefix, Some(Duration::from_millis(300))) else {
        eprintln!("skipping ttl_set_short_entry_expires: no live redis");
        return;
    };
    cache.cache_clear().expect("clear");

    cache
        .cache_set("k".to_string(), "v".to_string())
        .expect("set");
    assert_eq!(
        cache.cache_get(&"k".to_string()).unwrap(),
        Some("v".to_string())
    );

    std::thread::sleep(Duration::from_millis(500));

    assert_eq!(
        cache.cache_get(&"k".to_string()).unwrap(),
        None,
        "the entry must expire after its TTL"
    );

    cache.cache_clear().expect("cleanup");
}
