//! Runtime certification for the redis `cache_set_ref` rewrite.
//!
//! `SerializeCached::cache_set_ref` (and its async counterpart) was rewritten from
//! a `GET`+`SET`/`PSETEX` pipeline that read back the displaced value into a single
//! direct write with NO read-back: a plain `SET` when the TTL is unset (no expiry)
//! and a `PSETEX` when a TTL is configured. The signature also changed to return
//! `Result<(), _>` instead of `Result<Option<V>, _>`.
//!
//! `tests/serialize_set.rs` deliberately covers only the redb path at runtime and
//! states the redis path is "covered at compile time by the feature builds (no live
//! redis server in CI here)". These tests close that gap against a live redis: they
//! assert the raw redis `TTL` so the `SET` (ttl-unset, TTL == -1) vs `PSETEX`
//! (ttl-set, TTL > 0) branch is actually exercised, that the value round-trips from
//! a borrow, and that a corrupt pre-existing value never surfaces as an error (the
//! write no longer reads the previous value back).
//!
//! Requires a live redis; each test skips (returns early) when none is reachable.

#![cfg(feature = "redis_store")]

use cached::time::Duration;

// Undecodable bytes: not valid MessagePack and not the legacy JSON fallback, so a
// read-back (if the code still did one) would have to decode-fail on these.
const CORRUPT: &[u8] = b"\xff\xff not a valid cached value \x00\x01\x02";

// ── sync ─────────────────────────────────────────────────────────────────────

mod sync_tests {
    use super::*;
    use cached::{ConcurrentCached, RedisCache, SerializeCached};

    fn try_build(prefix: &str, ttl: Option<Duration>) -> Option<RedisCache<String, String>> {
        let mut b = RedisCache::<String, String>::builder(prefix).namespace("");
        if let Some(t) = ttl {
            b = b.ttl(t);
        }
        b.build().ok()
    }

    fn raw_conn(cache: &RedisCache<String, String>) -> redis::Connection {
        redis::Client::open(cache.connection_string().reveal())
            .expect("open redis client")
            .get_connection()
            .expect("redis connection")
    }

    fn raw_ttl_secs(cache: &RedisCache<String, String>, full_key: &str) -> i64 {
        let mut conn = raw_conn(cache);
        redis::cmd("TTL")
            .arg(full_key)
            .query(&mut conn)
            .expect("TTL query")
    }

    // ttl-unset: cache_set_ref must take the plain `SET` path (no expiry, raw
    // TTL == -1). A regression to PSETEX (or a stray expiry) would make TTL > 0.
    #[test]
    fn cache_set_ref_ttl_unset_uses_plain_set_no_expiry() {
        let prefix = "v3_setref_sync_unset";
        let Some(cache) = try_build(prefix, None) else {
            eprintln!("skipping cache_set_ref_ttl_unset_uses_plain_set_no_expiry: no live redis");
            return;
        };
        cache.cache_clear().expect("clear");

        let key = "k".to_string();
        // The setter returns `()` -- pin the unit shape at the call site.
        let out: () = cache
            .cache_set_ref(&key, &"v".to_string())
            .expect("set_ref");
        assert_eq!(out, ());

        let ttl = raw_ttl_secs(&cache, "v3_setref_sync_unset:k");
        assert_eq!(
            ttl, -1,
            "ttl-unset cache_set_ref must store the key without expiry (raw TTL -1), got {ttl}"
        );
        assert_eq!(
            cache.cache_get(&key).unwrap(),
            Some("v".to_string()),
            "the value written from a borrow must be readable"
        );

        cache.cache_clear().expect("cleanup");
    }

    // ttl-set: cache_set_ref must take the `PSETEX` path (raw TTL > 0). A
    // regression to a plain SET would leave TTL == -1.
    #[test]
    fn cache_set_ref_ttl_set_uses_psetex_expiry() {
        let prefix = "v3_setref_sync_set";
        let Some(cache) = try_build(prefix, Some(Duration::from_secs(60))) else {
            eprintln!("skipping cache_set_ref_ttl_set_uses_psetex_expiry: no live redis");
            return;
        };
        cache.cache_clear().expect("clear");

        let key = "k".to_string();
        cache
            .cache_set_ref(&key, &"v".to_string())
            .expect("set_ref");

        let ttl = raw_ttl_secs(&cache, "v3_setref_sync_set:k");
        assert!(
            ttl > 0,
            "ttl-set cache_set_ref must apply a positive expiry via PSETEX, got {ttl}"
        );
        assert_eq!(
            cache.cache_get(&key).unwrap(),
            Some("v".to_string()),
            "the value written from a borrow must be readable"
        );

        cache.cache_clear().expect("cleanup");
    }

    // cache_set_ref does not read the previous value back, so an undecodable
    // pre-existing value can never surface as an error: the write is Ok(()) and
    // the new value is readable. (Sync counterpart of the in-source async test.)
    #[test]
    fn cache_set_ref_over_corrupt_previous_returns_ok_unit() {
        let prefix = "v3_setref_sync_corrupt";
        let Some(cache) = try_build(prefix, Some(Duration::from_secs(60))) else {
            eprintln!(
                "skipping cache_set_ref_over_corrupt_previous_returns_ok_unit: no live redis"
            );
            return;
        };
        cache.cache_clear().expect("clear");

        let key = "k".to_string();
        let full_key = "v3_setref_sync_corrupt:k";
        // Plant an undecodable value directly.
        let mut conn = raw_conn(&cache);
        let _: () = redis::cmd("SET")
            .arg(full_key)
            .arg(CORRUPT)
            .query(&mut conn)
            .unwrap();

        // Must succeed with unit despite the corrupt previous value.
        cache
            .cache_set_ref(&key, &"fresh".to_string())
            .expect("set_ref over corrupt previous must be Ok(())");

        assert_eq!(
            cache.cache_get(&key).unwrap(),
            Some("fresh".to_string()),
            "the fresh value written over the corrupt one must be readable"
        );

        cache.cache_clear().expect("cleanup");
    }
}

// ── async ────────────────────────────────────────────────────────────────────

#[cfg(feature = "redis_tokio")]
mod async_tests {
    use super::*;
    use cached::{AsyncRedisCache, ConcurrentCachedAsync, SerializeCachedAsync};

    async fn try_build(
        prefix: &str,
        ttl: Option<Duration>,
    ) -> Option<AsyncRedisCache<String, String>> {
        let mut b = AsyncRedisCache::<String, String>::builder(prefix).namespace("");
        if let Some(t) = ttl {
            b = b.ttl(t);
        }
        b.build().await.ok()
    }

    fn raw_ttl_secs(cache: &AsyncRedisCache<String, String>, full_key: &str) -> i64 {
        let mut conn = redis::Client::open(cache.connection_string().reveal())
            .expect("open redis client")
            .get_connection()
            .expect("redis connection");
        redis::cmd("TTL")
            .arg(full_key)
            .query(&mut conn)
            .expect("TTL query")
    }

    #[tokio::test]
    async fn async_cache_set_ref_ttl_unset_uses_plain_set_no_expiry() {
        let prefix = "v3_setref_async_unset";
        let Some(cache) = try_build(prefix, None).await else {
            eprintln!(
                "skipping async_cache_set_ref_ttl_unset_uses_plain_set_no_expiry: no live redis"
            );
            return;
        };
        cache.async_cache_clear().await.expect("clear");

        let key = "k".to_string();
        let out: () = cache
            .async_cache_set_ref(&key, &"v".to_string())
            .await
            .expect("set_ref");
        assert_eq!(out, ());

        let ttl = raw_ttl_secs(&cache, "v3_setref_async_unset:k");
        assert_eq!(
            ttl, -1,
            "ttl-unset async_cache_set_ref must store without expiry (raw TTL -1), got {ttl}"
        );
        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some("v".to_string())
        );

        cache.async_cache_clear().await.expect("cleanup");
    }

    #[tokio::test]
    async fn async_cache_set_ref_ttl_set_uses_psetex_expiry() {
        let prefix = "v3_setref_async_set";
        let Some(cache) = try_build(prefix, Some(Duration::from_secs(60))).await else {
            eprintln!("skipping async_cache_set_ref_ttl_set_uses_psetex_expiry: no live redis");
            return;
        };
        cache.async_cache_clear().await.expect("clear");

        let key = "k".to_string();
        cache
            .async_cache_set_ref(&key, &"v".to_string())
            .await
            .expect("set_ref");

        let ttl = raw_ttl_secs(&cache, "v3_setref_async_set:k");
        assert!(
            ttl > 0,
            "ttl-set async_cache_set_ref must apply a positive expiry via PSETEX, got {ttl}"
        );
        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some("v".to_string())
        );

        cache.async_cache_clear().await.expect("cleanup");
    }
}
