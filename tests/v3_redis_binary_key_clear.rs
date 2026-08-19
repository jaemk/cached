//! Regression tests: `cache_clear` / `async_cache_clear` must tolerate keys in
//! their own scope that are not valid UTF-8.
//!
//! Redis keys are binary-safe. Both clear loops decoded the `SCAN` reply as
//! `(u64, Vec<String>)`, so a single non-UTF-8 key anywhere in
//! `{namespace}:{prefix}:*` made redis-rs' conversion fail:
//!
//! ```text
//! Err(Redis { source: Incompatible type - Cannot convert from UTF-8:
//!             invalid utf-8 sequence of 1 bytes from index 11 })
//! ```
//!
//! The whole clear aborted and the cache's *own* entries survived. It never
//! self-healed either, because the offending key was never deleted, so every
//! later `cache_clear` failed the same way. On a populated keyspace it was
//! worse: earlier `SCAN` batches were already `DEL`eted before the failing
//! batch, i.e. a partially applied clear reported as an error.
//!
//! The fix decodes the batch as `(u64, Vec<Vec<u8>>)` and hands the raw bytes
//! to `DEL` unchanged. `cache_reset` / `async_cache_reset` delegate to the
//! clears, so they are covered here too.
//!
//! Requires a live redis; every test skips (returns early) when
//! `CACHED_REDIS_CONNECTION_STRING` is unset, matching the existing live redis
//! tests.

#![cfg(feature = "redis_store")]

macro_rules! skip_without_redis {
    () => {
        if std::env::var("CACHED_REDIS_CONNECTION_STRING").is_err() {
            return;
        }
    };
}

// ── sync ─────────────────────────────────────────────────────────────────────

mod sync_tests {
    use cached::{ConcurrentCached, RedisCache};
    use std::time::Duration;

    fn build(prefix: &str) -> RedisCache<String, String> {
        RedisCache::<String, String>::builder(prefix)
            .namespace("")
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build RedisCache")
    }

    fn raw_conn(cache: &RedisCache<String, String>) -> redis::Connection {
        redis::Client::open(cache.connection_string().reveal())
            .expect("raw client")
            .get_connection()
            .expect("raw connection")
    }

    fn raw_set(cache: &RedisCache<String, String>, key: &[u8]) {
        let mut conn = raw_conn(cache);
        redis::cmd("SET")
            .arg(key)
            .arg("planted")
            .query::<()>(&mut conn)
            .expect("raw SET");
    }

    fn raw_del(cache: &RedisCache<String, String>, key: &[u8]) {
        let mut conn = raw_conn(cache);
        redis::cmd("DEL")
            .arg(key)
            .query::<i64>(&mut conn)
            .expect("raw DEL");
    }

    fn raw_exists(cache: &RedisCache<String, String>, key: &[u8]) -> bool {
        let mut conn = raw_conn(cache);
        redis::cmd("EXISTS")
            .arg(key)
            .query::<i64>(&mut conn)
            .expect("raw EXISTS")
            == 1
    }

    /// A single non-UTF-8 key inside this cache's own scope must not stop
    /// `cache_clear` from removing the cache's own entries, and must not turn
    /// the clear into an error.
    #[test]
    fn clear_removes_own_keys_despite_a_non_utf8_key_in_scope() {
        skip_without_redis!();

        let cache = build("v3binclear_sync");
        // `namespace("")` gives the `:{prefix}:{key}` layout, so this lands
        // squarely inside the `:v3binclear_sync:*` clear scope.
        let binary_key: &[u8] = b":v3binclear_sync:\xff";

        raw_del(&cache, binary_key);
        cache.cache_clear().expect("initial clear");

        cache
            .cache_set("k1".to_string(), "v1".to_string())
            .expect("set k1");
        cache
            .cache_set("k2".to_string(), "v2".to_string())
            .expect("set k2");
        raw_set(&cache, binary_key);

        let cleared = cache.cache_clear();
        // Clean up before asserting so a failure does not leave the poisoned
        // key behind for the next run (pre-fix, nothing ever removed it).
        raw_del(&cache, binary_key);

        cleared.expect("cache_clear must succeed with a non-UTF-8 key in scope");

        assert_eq!(
            cache.cache_get(&"k1".to_string()).expect("get k1"),
            None,
            "cache_clear must remove its own entries even when a non-UTF-8 key \
             shares the scope"
        );
        assert_eq!(
            cache.cache_get(&"k2".to_string()).expect("get k2"),
            None,
            "cache_clear must remove every one of its own entries"
        );
    }

    /// The non-UTF-8 key is itself inside the scope, so the clear must delete
    /// it as well: that is what makes the failure self-healing rather than
    /// permanently wedging every future `cache_clear`.
    #[test]
    fn clear_deletes_the_non_utf8_key_itself() {
        skip_without_redis!();

        let cache = build("v3binclear_sync_del");
        let binary_key: &[u8] = b":v3binclear_sync_del:\xfe\xff";

        raw_del(&cache, binary_key);
        cache.cache_clear().expect("initial clear");

        cache
            .cache_set("k".to_string(), "v".to_string())
            .expect("set k");
        raw_set(&cache, binary_key);

        let cleared = cache.cache_clear();
        let still_there = raw_exists(&cache, binary_key);
        raw_del(&cache, binary_key);

        cleared.expect("cache_clear must succeed");
        assert!(
            !still_there,
            "a non-UTF-8 key inside the cache's own scope must be deleted by \
             cache_clear, otherwise the failure never self-heals"
        );
    }

    /// More own keys than the `SCAN COUNT` batch size (100), so the poisoned
    /// key is reached after at least one batch has already been `DEL`eted.
    /// Pre-fix this was a partially applied clear reported as an error; the
    /// clear must now be both `Ok` and complete.
    #[test]
    fn clear_over_many_batches_is_not_partially_applied() {
        skip_without_redis!();

        const KEYS: usize = 250;
        let cache = build("v3binclear_sync_batched");
        let binary_key: &[u8] = b":v3binclear_sync_batched:\x80bad";

        raw_del(&cache, binary_key);
        cache.cache_clear().expect("initial clear");

        for i in 0..KEYS {
            cache
                .cache_set(format!("k{i}"), format!("v{i}"))
                .expect("cache_set");
        }
        raw_set(&cache, binary_key);

        let cleared = cache.cache_clear();
        raw_del(&cache, binary_key);

        cleared.expect("cache_clear must succeed across multiple SCAN batches");

        for i in 0..KEYS {
            assert_eq!(
                cache.cache_get(&format!("k{i}")).expect("cache_get"),
                None,
                "key k{i} survived a clear that spanned several SCAN batches"
            );
        }
    }

    /// `cache_reset` delegates to `cache_clear`, so it inherits the fix.
    #[test]
    fn reset_removes_own_keys_despite_a_non_utf8_key_in_scope() {
        skip_without_redis!();

        let cache = build("v3binclear_sync_reset");
        let binary_key: &[u8] = b":v3binclear_sync_reset:\xff\xfe";

        raw_del(&cache, binary_key);
        cache.cache_reset().expect("initial reset");

        cache
            .cache_set("k".to_string(), "v".to_string())
            .expect("set k");
        raw_set(&cache, binary_key);

        let reset = cache.cache_reset();
        raw_del(&cache, binary_key);

        reset.expect("cache_reset must succeed with a non-UTF-8 key in scope");
        assert_eq!(
            cache.cache_get(&"k".to_string()).expect("get k"),
            None,
            "cache_reset must remove its own entries (it delegates to cache_clear)"
        );
    }

    /// The clear stays scoped: a non-UTF-8 key *outside* this cache's
    /// namespace/prefix is neither decoded nor deleted.
    #[test]
    fn clear_spares_a_non_utf8_key_outside_the_scope() {
        skip_without_redis!();

        let cache = build("v3binclear_sync_scope");
        let outside_key: &[u8] = b":v3binclear_sync_scope_other:\xff";

        cache.cache_clear().expect("initial clear");
        cache
            .cache_set("k".to_string(), "v".to_string())
            .expect("set k");
        raw_set(&cache, outside_key);

        let cleared = cache.cache_clear();
        let survived = raw_exists(&cache, outside_key);
        raw_del(&cache, outside_key);

        cleared.expect("cache_clear must succeed");
        assert!(
            survived,
            "cache_clear must not reach a non-UTF-8 key outside its own scope"
        );
        assert_eq!(
            cache.cache_get(&"k".to_string()).expect("get k"),
            None,
            "cache_clear must still remove its own entry"
        );
    }
}

// ── async ────────────────────────────────────────────────────────────────────

#[cfg(feature = "redis_tokio")]
mod async_tests {
    use cached::time::Duration;
    use cached::{AsyncRedisCache, ConcurrentCachedAsync};

    async fn build(prefix: &str) -> AsyncRedisCache<String, String> {
        AsyncRedisCache::<String, String>::builder(prefix)
            .namespace("")
            .ttl(Duration::from_secs(60))
            .build()
            .await
            .expect("build AsyncRedisCache")
    }

    fn raw_conn(cache: &AsyncRedisCache<String, String>) -> redis::Connection {
        redis::Client::open(cache.connection_string().reveal())
            .expect("raw client")
            .get_connection()
            .expect("raw connection")
    }

    fn raw_set(cache: &AsyncRedisCache<String, String>, key: &[u8]) {
        let mut conn = raw_conn(cache);
        redis::cmd("SET")
            .arg(key)
            .arg("planted")
            .query::<()>(&mut conn)
            .expect("raw SET");
    }

    fn raw_del(cache: &AsyncRedisCache<String, String>, key: &[u8]) {
        let mut conn = raw_conn(cache);
        redis::cmd("DEL")
            .arg(key)
            .query::<i64>(&mut conn)
            .expect("raw DEL");
    }

    fn raw_exists(cache: &AsyncRedisCache<String, String>, key: &[u8]) -> bool {
        let mut conn = raw_conn(cache);
        redis::cmd("EXISTS")
            .arg(key)
            .query::<i64>(&mut conn)
            .expect("raw EXISTS")
            == 1
    }

    /// Async counterpart of
    /// `clear_removes_own_keys_despite_a_non_utf8_key_in_scope`.
    #[tokio::test]
    async fn async_clear_removes_own_keys_despite_a_non_utf8_key_in_scope() {
        skip_without_redis!();

        let cache = build("v3binclear_async").await;
        let binary_key: &[u8] = b":v3binclear_async:\xff";

        raw_del(&cache, binary_key);
        cache.async_cache_clear().await.expect("initial clear");

        cache
            .async_cache_set("k1".to_string(), "v1".to_string())
            .await
            .expect("set k1");
        cache
            .async_cache_set("k2".to_string(), "v2".to_string())
            .await
            .expect("set k2");
        raw_set(&cache, binary_key);

        let cleared = cache.async_cache_clear().await;
        let still_there = raw_exists(&cache, binary_key);
        raw_del(&cache, binary_key);

        cleared.expect("async_cache_clear must succeed with a non-UTF-8 key in scope");
        assert!(
            !still_there,
            "async_cache_clear must delete the non-UTF-8 key in its own scope"
        );
        assert_eq!(
            cache
                .async_cache_get(&"k1".to_string())
                .await
                .expect("get k1"),
            None,
            "async_cache_clear must remove its own entries even when a non-UTF-8 \
             key shares the scope"
        );
        assert_eq!(
            cache
                .async_cache_get(&"k2".to_string())
                .await
                .expect("get k2"),
            None,
            "async_cache_clear must remove every one of its own entries"
        );
    }

    /// Async counterpart of `clear_over_many_batches_is_not_partially_applied`.
    #[tokio::test]
    async fn async_clear_over_many_batches_is_not_partially_applied() {
        skip_without_redis!();

        const KEYS: usize = 250;
        let cache = build("v3binclear_async_batched").await;
        let binary_key: &[u8] = b":v3binclear_async_batched:\x80bad";

        raw_del(&cache, binary_key);
        cache.async_cache_clear().await.expect("initial clear");

        for i in 0..KEYS {
            cache
                .async_cache_set(format!("k{i}"), format!("v{i}"))
                .await
                .expect("async_cache_set");
        }
        raw_set(&cache, binary_key);

        let cleared = cache.async_cache_clear().await;
        raw_del(&cache, binary_key);

        cleared.expect("async_cache_clear must succeed across multiple SCAN batches");

        for i in 0..KEYS {
            assert_eq!(
                cache
                    .async_cache_get(&format!("k{i}"))
                    .await
                    .expect("async_cache_get"),
                None,
                "key k{i} survived an async clear that spanned several SCAN batches"
            );
        }
    }

    /// `async_cache_reset` delegates to `async_cache_clear`, so it inherits the
    /// fix.
    #[tokio::test]
    async fn async_reset_removes_own_keys_despite_a_non_utf8_key_in_scope() {
        skip_without_redis!();

        let cache = build("v3binclear_async_reset").await;
        let binary_key: &[u8] = b":v3binclear_async_reset:\xff\xfe";

        raw_del(&cache, binary_key);
        cache.async_cache_reset().await.expect("initial reset");

        cache
            .async_cache_set("k".to_string(), "v".to_string())
            .await
            .expect("set k");
        raw_set(&cache, binary_key);

        let reset = cache.async_cache_reset().await;
        raw_del(&cache, binary_key);

        reset.expect("async_cache_reset must succeed with a non-UTF-8 key in scope");
        assert_eq!(
            cache
                .async_cache_get(&"k".to_string())
                .await
                .expect("get k"),
            None,
            "async_cache_reset must remove its own entries (it delegates to \
             async_cache_clear)"
        );
    }

    /// The async clear stays scoped: a non-UTF-8 key outside this cache's
    /// namespace/prefix survives.
    #[tokio::test]
    async fn async_clear_spares_a_non_utf8_key_outside_the_scope() {
        skip_without_redis!();

        let cache = build("v3binclear_async_scope").await;
        let outside_key: &[u8] = b":v3binclear_async_scope_other:\xff";

        cache.async_cache_clear().await.expect("initial clear");
        cache
            .async_cache_set("k".to_string(), "v".to_string())
            .await
            .expect("set k");
        raw_set(&cache, outside_key);

        let cleared = cache.async_cache_clear().await;
        let survived = raw_exists(&cache, outside_key);
        raw_del(&cache, outside_key);

        cleared.expect("async_cache_clear must succeed");
        assert!(
            survived,
            "async_cache_clear must not reach a non-UTF-8 key outside its own scope"
        );
        assert_eq!(
            cache
                .async_cache_get(&"k".to_string())
                .await
                .expect("get k"),
            None,
            "async_cache_clear must still remove its own entry"
        );
    }
}
