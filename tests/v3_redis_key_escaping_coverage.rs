//! Independent certification coverage for the redis key-escaping rewrite in
//! `src/stores/redis.rs` (`escape_key_field` / `canonical_namespace` /
//! `join_key_fields`, shared by `generate_redis_key` and `clear_match_pattern`).
//!
//! `src/stores/redis.rs`'s own `#[cfg(test)]` modules and `tests/frozen_format_golden.rs`
//! already cover: the escaped key layout, triple-injectivity, `clear_match_pattern`
//! agreement with `generate_redis_key`, and a handful of live `RedisCache` (sync)
//! `cache_clear` scoping cases through `unescape_glob`/pattern-comparison helpers
//! rather than a real glob engine, plus one live glob-metacharacter case
//! (`n*s:x` namespace) in `frozen_format_golden.rs`.
//!
//! This file closes the gaps the implementor flagged as unexercised:
//!  1. Real redis-glob semantics (not a hand-rolled unescape helper) driving
//!     `cache_clear` scoping against a live server, with glob metacharacters and
//!     colons split across keys as well as namespace/prefix.
//!  2. `AsyncRedisCache` has no live end-to-end key test anywhere: every existing
//!     live key test uses the sync `RedisCache`. Covered here for key generation,
//!     collision-freedom, and `async_cache_clear` scoping.
//!  3. Multi-byte UTF-8 fields, and a field whose literal text is `%3A` (which must
//!     not be confused with an escaped colon), proven end-to-end against live redis.
//!  4. Scope isolation between a namespace-less cache and a cache whose namespace
//!     equals that cache's prefix. The layout's fixed arity (every key carries both
//!     separators, so an empty namespace is an empty leading field) is what keeps the
//!     two keyspaces disjoint; before it, clearing the namespace-less cache deleted
//!     the other's entries whatever prefix it used.
//!
//! Requires a live redis; every test module-level macro skips (returns early) when
//! `CACHED_REDIS_CONNECTION_STRING` is unset, matching the existing live redis tests.

#![cfg(feature = "redis_store")]

macro_rules! skip_without_redis {
    () => {
        if std::env::var("CACHED_REDIS_CONNECTION_STRING").is_err() {
            return;
        }
    };
}

// ── real redis-glob semantics against a live server (sync) ────────────────────

mod live_glob_semantics {
    use cached::{ConcurrentCached, RedisCache};
    use std::time::Duration;

    fn build(namespace: &str, prefix: &str) -> RedisCache<String, String> {
        RedisCache::<String, String>::builder(prefix)
            .namespace(namespace)
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

    fn raw_exists(cache: &RedisCache<String, String>, full_key: &str) -> bool {
        let mut conn = raw_conn(cache);
        redis::cmd("EXISTS")
            .arg(full_key)
            .query::<i64>(&mut conn)
            .expect("raw EXISTS")
            == 1
    }

    /// A prefix carrying every glob metacharacter redis' `SCAN MATCH` understands
    /// (`*`, `?`, `[`, `]`) plus a literal backslash, all glob-escaped on top of the
    /// percent-escaping. A neighbour whose *unescaped* prefix would satisfy the raw
    /// glob (`p[1]?x` matches literal `p1yx`) must survive `cache_clear` on the
    /// glob-laden cache: this drives the real server's `SCAN MATCH`, not a hand-rolled
    /// unescape helper, so it catches a regression where glob-escaping is dropped or
    /// applied in the wrong order relative to percent-escaping.
    #[test]
    fn clear_with_full_glob_metacharacter_set_in_prefix_spares_a_matching_neighbour() {
        skip_without_redis!();

        let globby = build("", "v3glob_p[1]?x");
        // Would satisfy the *unescaped* glob `p[1]?x` (p, char-class [1], any-char ?, x).
        let neighbour = build("", "v3glob_p1yx");
        globby.cache_clear().expect("clear globby");
        neighbour.cache_clear().expect("clear neighbour");

        globby
            .cache_set("k*key".to_string(), "globby".to_string())
            .expect("set globby");
        neighbour
            .cache_set("k".to_string(), "neighbour".to_string())
            .expect("set neighbour");

        // The metacharacters are literal in the actual key; only the SCAN pattern
        // quotes them. The leading `:` is the empty namespace field.
        assert!(
            raw_exists(&globby, ":v3glob_p[1]?x:k*key"),
            "glob metacharacters must be stored literally in the key"
        );

        globby.cache_clear().expect("clear globby again");

        assert_eq!(
            globby.cache_get(&"k*key".to_string()).expect("get globby"),
            None,
            "cache_clear must remove this cache's own glob-laden key"
        );
        assert_eq!(
            neighbour
                .cache_get(&"k".to_string())
                .expect("get neighbour"),
            Some("neighbour".to_string()),
            "an unescaped glob-metacharacter prefix must not sweep a neighbour whose \
             literal prefix would satisfy the unescaped pattern"
        );

        neighbour.cache_clear().expect("cleanup neighbour");
    }

    /// A literal backslash in both the prefix and the key, driven against a live
    /// `SCAN MATCH`: backslash is itself a glob metacharacter (redis' escape
    /// introducer), so an unescaped backslash could desynchronize the pattern from
    /// the literal key bytes that follow it.
    #[test]
    fn clear_with_literal_backslash_in_prefix_and_key_matches_only_its_own_keys() {
        skip_without_redis!();

        let backslashy = build("", "v3glob_back\\slash");
        let neighbour = build("", "v3glob_backslash");
        backslashy.cache_clear().expect("clear backslashy");
        neighbour.cache_clear().expect("clear neighbour");

        backslashy
            .cache_set("k\\ey".to_string(), "backslashy".to_string())
            .expect("set backslashy");
        neighbour
            .cache_set("k".to_string(), "neighbour".to_string())
            .expect("set neighbour");

        backslashy.cache_clear().expect("clear backslashy again");

        assert_eq!(
            backslashy
                .cache_get(&"k\\ey".to_string())
                .expect("get backslashy"),
            None,
            "cache_clear must remove this cache's own backslash-laden key"
        );
        assert_eq!(
            neighbour
                .cache_get(&"k".to_string())
                .expect("get neighbour"),
            Some("neighbour".to_string()),
            "a literal backslash in the prefix must not desynchronize the scan \
             pattern and sweep an unrelated neighbour"
        );

        neighbour.cache_clear().expect("cleanup neighbour");
    }

    /// Glob metacharacters AND a field separator together, split differently across
    /// namespace/prefix between two caches, exercised against a real `SCAN`. Combines
    /// the percent-escape (colon) and glob-escape (metacharacter) passes in one
    /// pattern, on the live server rather than the offline `unescape_glob` helper.
    #[test]
    fn clear_with_colon_and_glob_metacharacter_together_does_not_cross_the_split() {
        skip_without_redis!();

        // namespace="n*s:x", prefix="p" vs. the differently-split
        // namespace="n*s", prefix="x:p" -- both contain the `*` metacharacter and a
        // `:` that must not let one cache's clear reach the other's keys.
        let left = build("n*s:x", "v3glob_split_p");
        let right = build("n*s", "x:v3glob_split_p");
        left.cache_clear().expect("clear left");
        right.cache_clear().expect("clear right");

        left.cache_set("k".to_string(), "left".to_string())
            .expect("set left");
        right
            .cache_set("k".to_string(), "right".to_string())
            .expect("set right");

        left.cache_clear().expect("clear left again");

        assert_eq!(
            left.cache_get(&"k".to_string()).expect("get left"),
            None,
            "cache_clear must remove this cache's own key"
        );
        assert_eq!(
            right.cache_get(&"k".to_string()).expect("get right"),
            Some("right".to_string()),
            "a differently-split namespace/prefix pair sharing the same \
             metacharacters and separator must not be swept"
        );

        right.cache_clear().expect("cleanup right");
    }
}

// ── AsyncRedisCache has no live key test anywhere else ─────────────────────────

#[cfg(feature = "redis_tokio")]
mod async_key_generation {
    use cached::time::Duration;
    use cached::{AsyncRedisCache, ConcurrentCachedAsync};

    async fn build(namespace: &str, prefix: &str) -> AsyncRedisCache<String, String> {
        AsyncRedisCache::<String, String>::builder(prefix)
            .namespace(namespace)
            .ttl(Duration::from_secs(60))
            .build()
            .await
            .expect("build AsyncRedisCache")
    }

    fn raw_exists(cache: &AsyncRedisCache<String, String>, full_key: &str) -> bool {
        let mut conn = redis::Client::open(cache.connection_string().reveal())
            .expect("raw client")
            .get_connection()
            .expect("raw connection");
        redis::cmd("EXISTS")
            .arg(full_key)
            .query::<i64>(&mut conn)
            .expect("raw EXISTS")
            == 1
    }

    /// `AsyncRedisCache` writes the same escaped `{namespace}:{prefix}:{key}` layout
    /// as the sync store: this is the async store's only live key-format test.
    #[tokio::test]
    async fn async_written_keys_use_the_escaped_layout() {
        skip_without_redis!();

        let cache = build("a:b", "v3async_key_escaped").await;
        cache.async_cache_clear().await.expect("clear");
        cache
            .async_cache_set("x:y".to_string(), "v".to_string())
            .await
            .expect("cache_set");

        assert!(
            raw_exists(&cache, "a%3Ab:v3async_key_escaped:x%3Ay"),
            "async store must percent-escape separators inside a field"
        );
        assert!(
            !raw_exists(&cache, "a:b:v3async_key_escaped:x:y"),
            "the unescaped pre-3.0 key must not be written by the async store"
        );

        cache.async_cache_clear().await.expect("cleanup");
    }

    /// Two async caches whose namespace/prefix split the same characters
    /// differently must not collide -- the async builder delegates to the same
    /// `generate_redis_key`, but this is the only live test that proves it for
    /// `AsyncRedisCache` specifically.
    #[tokio::test]
    async fn async_differently_split_namespace_and_prefix_do_not_collide() {
        skip_without_redis!();

        let left = build("a:b", "v3async_key_split").await;
        let right = build("a", "b:v3async_key_split").await;
        left.async_cache_clear().await.expect("clear left");
        right.async_cache_clear().await.expect("clear right");

        left.async_cache_set("k".to_string(), "left".to_string())
            .await
            .expect("set left");
        right
            .async_cache_set("k".to_string(), "right".to_string())
            .await
            .expect("set right");

        assert_eq!(
            left.async_cache_get(&"k".to_string())
                .await
                .expect("get left"),
            Some("left".to_string()),
            "the second async cache must not have overwritten the first's entry"
        );
        assert_eq!(
            right
                .async_cache_get(&"k".to_string())
                .await
                .expect("get right"),
            Some("right".to_string())
        );

        left.async_cache_clear().await.expect("cleanup left");
        right.async_cache_clear().await.expect("cleanup right");
    }

    /// `async_cache_clear` is scoped to this async cache's own escaped keyspace and
    /// spares a differently-split neighbour, mirroring the sync `cache_clear` golden
    /// test but exercised through the async store's own scan/delete path.
    #[tokio::test]
    async fn async_clear_covers_own_escaped_keys_and_spares_the_neighbour() {
        skip_without_redis!();

        let left = build("a:b", "v3async_key_clear").await;
        let right = build("a", "b:v3async_key_clear").await;
        left.async_cache_clear().await.expect("clear left");
        right.async_cache_clear().await.expect("clear right");

        left.async_cache_set("k:1".to_string(), "left".to_string())
            .await
            .expect("set left");
        right
            .async_cache_set("k:1".to_string(), "right".to_string())
            .await
            .expect("set right");

        left.async_cache_clear().await.expect("clear left");

        assert_eq!(
            left.async_cache_get(&"k:1".to_string())
                .await
                .expect("get left"),
            None,
            "async_cache_clear must match this cache's own escaped keys"
        );
        assert_eq!(
            right
                .async_cache_get(&"k:1".to_string())
                .await
                .expect("get right"),
            Some("right".to_string()),
            "async_cache_clear must not reach a differently-split neighbouring cache"
        );

        right.async_cache_clear().await.expect("cleanup right");
    }
}

// ── multi-byte UTF-8 fields and a literal "%3A" field text ─────────────────────

mod utf8_and_literal_percent_escape_text {
    use cached::{ConcurrentCached, RedisCache};
    use std::time::Duration;

    fn build(prefix: &str) -> RedisCache<String, String> {
        RedisCache::<String, String>::builder(prefix)
            .namespace("")
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build RedisCache")
    }

    fn raw_exists(cache: &RedisCache<String, String>, full_key: &str) -> bool {
        let mut conn = redis::Client::open(cache.connection_string().reveal())
            .expect("raw client")
            .get_connection()
            .expect("raw connection");
        redis::cmd("EXISTS")
            .arg(full_key)
            .query::<i64>(&mut conn)
            .expect("raw EXISTS")
            == 1
    }

    /// A multi-byte UTF-8 key round-trips through the percent-escaping (which
    /// operates on `char`s, not bytes) end-to-end against live redis: written,
    /// read back, and `cache_clear`-scoped correctly.
    #[test]
    fn multibyte_utf8_key_round_trips_and_is_cleared() {
        skip_without_redis!();

        let cache = build("v3utf8_key");
        cache.cache_clear().expect("clear");

        // Mixes multi-byte CJK, an accented Latin char, and an emoji (a
        // multi-codepoint grapheme under the hood) with an embedded colon that
        // must still be escaped.
        let key = "héllo:世界🎉".to_string();
        cache
            .cache_set(key.clone(), "utf8-value".to_string())
            .expect("cache_set");

        assert!(
            raw_exists(&cache, ":v3utf8_key:héllo%3A世界🎉"),
            "the colon inside a multi-byte field must still be percent-escaped; \
             the surrounding UTF-8 text must be untouched"
        );
        assert_eq!(
            cache.cache_get(&key).expect("cache_get"),
            Some("utf8-value".to_string()),
            "a multi-byte UTF-8 key must round-trip through cache_get"
        );

        cache.cache_clear().expect("clear again");
        assert_eq!(
            cache.cache_get(&key).expect("cache_get after clear"),
            None,
            "cache_clear must remove a multi-byte UTF-8 key"
        );
    }

    /// A field whose literal text IS `%3A` (three ASCII characters: `%`, `3`, `A`)
    /// must not be confused with a field containing an actual colon. The literal
    /// `%` gets percent-escaped to `%25`, so the field is stored as `%253A`, never
    /// as the bare `%3A` an escaped colon would produce.
    #[test]
    fn literal_percent_3a_text_is_distinct_from_an_escaped_colon() {
        skip_without_redis!();

        let cache = build("v3utf8_literal_pct");
        cache.cache_clear().expect("clear");

        let literal_key = "%3A".to_string();
        let colon_key = ":".to_string();

        cache
            .cache_set(literal_key.clone(), "literal".to_string())
            .expect("set literal");
        cache
            .cache_set(colon_key.clone(), "colon".to_string())
            .expect("set colon");

        // Distinct raw keys: the literal-text field escapes its own `%` to
        // `%25`, producing `%253A`; the colon field escapes `:` to `%3A`.
        assert!(
            raw_exists(&cache, ":v3utf8_literal_pct:%253A"),
            "a field whose literal text is `%3A` must store the `%` escaped to `%25`"
        );
        assert!(
            raw_exists(&cache, ":v3utf8_literal_pct:%3A"),
            "a field containing an actual colon must store it escaped to `%3A`"
        );

        // Round trip: each key reads back its own value, never the other's.
        assert_eq!(
            cache.cache_get(&literal_key).expect("get literal"),
            Some("literal".to_string()),
            "the literal `%3A` text key must read back its own value"
        );
        assert_eq!(
            cache.cache_get(&colon_key).expect("get colon"),
            Some("colon".to_string()),
            "the `:` key must read back its own value, not the literal-text one"
        );

        cache.cache_clear().expect("cleanup");
    }
}

// ── empty namespace vs a cache whose namespace equals that prefix ─────────────

mod empty_namespace_scope_isolation {
    use cached::{ConcurrentCached, RedisCache};
    use std::time::Duration;

    fn build(namespace: &str, prefix: &str) -> RedisCache<String, String> {
        RedisCache::<String, String>::builder(prefix)
            .namespace(namespace)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build RedisCache")
    }

    /// Cache A has an empty namespace and prefix `"v3isolate_ns"`; cache B has
    /// namespace `"v3isolate_ns"` (equal to A's prefix) and an unrelated prefix.
    ///
    /// While the layout dropped the namespace field when it was empty, A's clear
    /// scope was `v3isolate_ns:*`, and every key B wrote
    /// (`v3isolate_ns:<B's escaped prefix>:<key>`) started with `v3isolate_ns:`.
    /// Clearing A therefore deleted all of B's entries, whatever prefix B used.
    /// Under the fixed arity A's keys are `:v3isolate_ns:<key>` and its scope is
    /// `:v3isolate_ns:*`, which no cache with a non-empty namespace can produce,
    /// so the two keyspaces are structurally disjoint.
    #[test]
    fn empty_namespace_clear_spares_a_cache_whose_namespace_equals_its_prefix() {
        skip_without_redis!();

        let a = build("", "v3isolate_ns");
        let b = build("v3isolate_ns", "v3isolate_unrelated_prefix");
        a.cache_clear().expect("clear a");
        b.cache_clear().expect("clear b");

        a.cache_set("k".to_string(), "a-value".to_string())
            .expect("set a");
        b.cache_set("k".to_string(), "b-value".to_string())
            .expect("set b");

        assert_eq!(
            a.cache_get(&"k".to_string()).expect("get a before clear"),
            Some("a-value".to_string())
        );
        assert_eq!(
            b.cache_get(&"k".to_string()).expect("get b before clear"),
            Some("b-value".to_string())
        );

        a.cache_clear().expect("clear a again");

        assert_eq!(
            a.cache_get(&"k".to_string()).expect("get a after clear"),
            None,
            "clearing a must remove a's own entry"
        );
        assert_eq!(
            b.cache_get(&"k".to_string()).expect("get b after clear"),
            Some("b-value".to_string()),
            "clearing a namespace-less cache must not reach a cache whose namespace \
             equals that prefix"
        );

        b.cache_clear().expect("cleanup b");
    }

    /// The same isolation in the other direction: clearing B (the cache whose
    /// namespace equals A's prefix) must not reach A's namespace-less keys.
    #[test]
    fn clear_of_the_namespaced_cache_spares_the_namespace_less_one() {
        skip_without_redis!();

        let a = build("", "v3isolate_rev");
        let b = build("v3isolate_rev", "v3isolate_rev_prefix");
        a.cache_clear().expect("clear a");
        b.cache_clear().expect("clear b");

        a.cache_set("k".to_string(), "a-value".to_string())
            .expect("set a");
        b.cache_set("k".to_string(), "b-value".to_string())
            .expect("set b");

        b.cache_clear().expect("clear b again");

        assert_eq!(
            b.cache_get(&"k".to_string()).expect("get b after clear"),
            None,
            "clearing b must remove b's own entry"
        );
        assert_eq!(
            a.cache_get(&"k".to_string()).expect("get a after clear"),
            Some("a-value".to_string()),
            "b's scope must not reach the namespace-less cache's keys"
        );

        a.cache_clear().expect("cleanup a");
    }
}
