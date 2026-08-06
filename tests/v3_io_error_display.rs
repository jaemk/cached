//! Regression tests for REDB-5 / REDIS-5 (design/0005-store-error-consistency.md).
//!
//! Before the fix, `RedbCacheError`/`RedbCacheBuildError` and
//! `RedisCacheError`/`RedisCacheBuildError` variants that carry a `#[source]`
//! discarded the cause from their `Display` output (e.g. `Display` rendered the
//! bare string `"Storage error"` even though `Debug` -- and `source()` -- exposed
//! the real cause). Anyone logging `{e}` (or using `anyhow` without `{:#}`) saw
//! nothing but the variant label; since the boxed source type is documented as
//! NOT public API, the cause was unreachable by any means.
//!
//! Each test below drives a real failure where practical (the double-open
//! `redb` repro from the design doc, a blocked cache directory, a redis server
//! that rejects an oversized expire, a corrupt on-disk/on-wire entry, a value
//! that fails to serialize) and otherwise constructs the variant directly
//! (`RedbCacheError`/`RedisCacheError`'s fields are public even though the
//! enums are `#[non_exhaustive]` -- only exhaustive *matching* requires a
//! wildcard arm; constructing an existing variant with all-public fields from
//! downstream is still allowed). Every test asserts:
//! - `Display` contains the underlying cause's text, not just the variant label.
//! - `std::error::Error::source()` is still wired to the same cause.

#[cfg(feature = "redb_store")]
mod redb_tests {
    use cached::{ConcurrentCached, RedbCache, RedbCacheBuildError, RedbCacheError};
    use std::error::Error as _;
    use tempfile::TempDir;

    /// REDB-5: `RedbCacheBuildError::Storage`'s `Display` must include the
    /// underlying redb cause, not just "storage error".
    ///
    /// Real failure path (matches the design doc's exact repro): open the same
    /// redb database twice while the first handle is still alive. redb refuses
    /// the second open with `DatabaseAlreadyOpen` ("Database already open.
    /// Cannot acquire lock.").
    #[test]
    fn build_error_storage_display_contains_cause_and_source_is_wired() {
        let tmp = TempDir::new().expect("tempdir");

        // Keep the first cache alive: redb's in-process lock is only held while
        // the `Database` handle is live.
        let _first: RedbCache<u32, u32> = RedbCache::builder("v3-io-error-double-open")
            .disk_dir(tmp.path())
            .build()
            .expect("first build must succeed");

        let result = RedbCache::<u32, u32>::builder("v3-io-error-double-open")
            .disk_dir(tmp.path())
            .build();

        let err = result.expect_err("opening the same redb db twice must fail");
        let RedbCacheBuildError::Storage { ref source } = err else {
            panic!("expected RedbCacheBuildError::Storage, got: {err:?}");
        };
        let source_display = source.to_string();
        assert!(
            source_display.to_lowercase().contains("already open"),
            "expected the DatabaseAlreadyOpen cause; got source display: {source_display}"
        );

        let display = err.to_string();
        assert_ne!(
            display, "storage error",
            "Display must not be just the bare variant label"
        );
        assert!(
            display.contains(&source_display),
            "Display must include the underlying cause's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(
            src.to_string(),
            source_display,
            "source() chain must still be intact"
        );
    }

    /// REDB-5: `RedbCacheBuildError::Io`'s `Display` must include the
    /// underlying `std::io::Error` cause.
    ///
    /// Real failure path: point `disk_dir` at a path that already exists as a
    /// regular file, so directory creation fails. The expected substring is
    /// derived independently (via `std::fs::create_dir_all` on the same path)
    /// rather than hardcoded, so the assertion does not depend on guessing the
    /// OS's exact error text.
    #[test]
    fn build_error_io_display_contains_cause_and_source_is_wired() {
        let tmp = TempDir::new().expect("tempdir");
        let blocker_path = tmp.path().join("blocked-by-a-file");
        std::fs::write(&blocker_path, b"not a directory").expect("write blocker file");

        let independent_err = std::fs::create_dir_all(&blocker_path)
            .expect_err("a regular file must block directory creation at the same path");
        let expected_substring = independent_err.to_string();

        let result = RedbCache::<u32, u32>::builder("v3-io-error-blocked-dir")
            .disk_dir(&blocker_path)
            .build();

        let err = result.expect_err("build must fail when disk_dir is blocked by an existing file");
        let RedbCacheBuildError::Io { ref source } = err else {
            panic!("expected RedbCacheBuildError::Io, got: {err:?}");
        };
        let source_display = source.to_string();
        assert_eq!(
            source_display, expected_substring,
            "source() display should match the OS's own error for the same failure"
        );

        let display = err.to_string();
        assert!(
            display.contains(&expected_substring),
            "Display must include the underlying cause's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(src.to_string(), expected_substring);
    }

    /// REDB-5: `RedbCacheError::Storage`'s `Display` must include the
    /// underlying redb cause.
    ///
    /// A genuine runtime storage failure (as opposed to the build-time one
    /// above) is impractical to trigger hermetically once a `RedbCache` is
    /// already open and healthy, so the variant is constructed directly with a
    /// real `redb::Error` (its fields are public; only exhaustive *matching* on
    /// this `#[non_exhaustive]` enum requires a wildcard arm).
    #[test]
    fn cache_error_storage_display_contains_cause_and_source_is_wired() {
        let inner = redb::Error::Io(std::io::Error::other(
            "synthetic redb storage failure for display test",
        ));
        let inner_display = inner.to_string();
        let err = RedbCacheError::Storage {
            source: Box::new(inner),
        };

        let display = err.to_string();
        assert_ne!(display, "storage error");
        assert!(
            display.contains(&inner_display),
            "Display must include the underlying cause's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(src.to_string(), inner_display);
    }

    /// REDB-5: `RedbCacheError::CacheDeserialization`'s `Display` must include
    /// the underlying decode-error cause.
    ///
    /// Real failure path: build a cache, then reopen its on-disk file directly
    /// via `redb` and insert bytes that are not valid MessagePack at all (redb's
    /// table name and this being an invalid marker byte are both stable across
    /// the 3.x on-disk format). Reopening the cache with
    /// `strict_deserialization(true)` and reading the key back must surface
    /// `CacheDeserialization` instead of self-healing.
    #[test]
    fn cache_error_deserialization_display_contains_cause_and_source_is_wired() {
        let tmp = TempDir::new().expect("tempdir");
        let disk_path = {
            let cache: RedbCache<u32, u32> = RedbCache::builder("v3-io-error-deser")
                .disk_dir(tmp.path())
                .strict_deserialization(true)
                .build()
                .expect("build cache");
            cache.disk_path().to_path_buf()
        };

        // 0xc1 is msgpack's reserved/never-used marker byte: guaranteed to fail
        // decode regardless of the target type.
        let corrupt: Vec<u8> = vec![0xc1, 0xc1, 0xc1];
        {
            let db = redb::Database::open(&disk_path).expect("reopen db directly");
            let table_def: redb::TableDefinition<&str, &[u8]> =
                redb::TableDefinition::new("cached_disk_cache");
            let wtxn = db.begin_write().expect("begin write txn");
            {
                let mut table = wtxn.open_table(table_def).expect("open table");
                table
                    .insert("1", corrupt.as_slice())
                    .expect("insert corrupt bytes");
            }
            wtxn.commit().expect("commit corrupt bytes");
        }

        let cache: RedbCache<u32, u32> = RedbCache::builder("v3-io-error-deser")
            .disk_dir(tmp.path())
            .strict_deserialization(true)
            .build()
            .expect("reopen cache");

        // Ground truth for the expected cause text: decode the same corrupt
        // bytes independently (rmp_serde's marker-byte check runs before any
        // type-specific dispatch, so the error text does not depend on the
        // private `CachedDiskValue<V>` wrapper actually used internally).
        let expected_substring = rmp_serde::from_slice::<u32>(&corrupt)
            .expect_err("corrupt bytes must independently fail to decode")
            .to_string();

        let err = cache
            .cache_get(&1u32)
            .expect_err("cache_get must surface a deserialization error for corrupt bytes");

        let display = err.to_string();
        assert!(
            display.contains(&expected_substring),
            "Display must include the underlying decode error's text; got: {display}"
        );

        let source = err
            .source()
            .expect("CacheDeserialization must expose source()");
        assert_eq!(source.to_string(), expected_substring);

        match err {
            RedbCacheError::CacheDeserialization { cached_value, .. } => {
                assert_eq!(cached_value, corrupt);
            }
            other => panic!("expected CacheDeserialization, got: {other:?}"),
        }
    }

    #[derive(Debug)]
    struct Unserializable;

    impl serde::Serialize for Unserializable {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional serialize failure for display test",
            ))
        }
    }

    impl<'de> serde::Deserialize<'de> for Unserializable {
        fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Ok(Unserializable)
        }
    }

    /// REDB-5: `RedbCacheError::CacheSerialization`'s `Display` must include
    /// the underlying encode-error cause.
    ///
    /// Real failure path: `cache_set` a value whose `Serialize` impl always
    /// fails.
    #[test]
    fn cache_error_serialization_display_contains_cause_and_source_is_wired() {
        let tmp = TempDir::new().expect("tempdir");
        let cache: RedbCache<u32, Unserializable> = RedbCache::builder("v3-io-error-ser")
            .disk_dir(tmp.path())
            .build()
            .expect("build cache");

        let err = cache
            .cache_set(1u32, Unserializable)
            .expect_err("cache_set must fail to serialize Unserializable");

        let display = err.to_string();
        assert!(
            display.contains("intentional serialize failure for display test"),
            "Display must include the underlying encode error's text; got: {display}"
        );

        let source = err
            .source()
            .expect("CacheSerialization must expose source()");
        assert!(
            source
                .to_string()
                .contains("intentional serialize failure for display test"),
            "source() chain must expose the underlying encode error; got: {source}"
        );
        assert!(matches!(err, RedbCacheError::CacheSerialization { .. }));
    }
}

#[cfg(feature = "redis_store")]
mod redis_tests {
    use cached::time::Duration;
    use cached::{ConcurrentCached, RedisCache, RedisCacheBuildError, RedisCacheError};
    use std::error::Error as _;

    const ENV_KEY: &str = "CACHED_REDIS_CONNECTION_STRING";

    /// Return the connection string from the env var, or skip (return from the
    /// caller) if absent. Matches the pattern used by the other live-redis
    /// integration tests in this repo (see `tests/v3_redis_backward_read.rs`).
    macro_rules! conn_or_skip {
        () => {
            match std::env::var(ENV_KEY) {
                Ok(s) => s,
                Err(_) => return,
            }
        };
    }

    /// REDIS-5: `RedisCacheBuildError::Connection`'s `Display` must include the
    /// underlying cause, not just "redis connection error".
    ///
    /// Real failure path: an invalid connection scheme fails `Client::open`
    /// before any network I/O, so this needs no live redis server.
    #[test]
    fn build_error_connection_display_contains_cause_and_source_is_wired() {
        let bad_url = "not-redis://nonexistent-host:9999";
        let result = RedisCache::<String, String>::builder("v3_io_error_display_conn")
            .ttl(Duration::from_secs(1))
            .connection_string(bad_url)
            .build();

        let err = result.expect_err("build must fail for an invalid connection scheme");
        let RedisCacheBuildError::Connection { ref source } = err else {
            panic!("expected RedisCacheBuildError::Connection, got: {err:?}");
        };
        let source_display = source.to_string();
        assert!(!source_display.is_empty());

        let display = err.to_string();
        assert_ne!(display, "redis connection error");
        assert!(
            display.contains(&source_display),
            "Display must include the underlying cause's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(src.to_string(), source_display);
    }

    /// REDIS-5: `RedisCacheBuildError::Pool`'s `Display` must include the
    /// underlying cause, not just "redis pool error".
    ///
    /// Real failure path: a valid scheme pointed at a refused local port fails
    /// eager pool construction; no live redis server is needed (the point is
    /// that the connection is refused).
    #[test]
    fn build_error_pool_display_contains_cause_and_source_is_wired() {
        let refused_url = "redis://127.0.0.1:1";
        let result = RedisCache::<String, String>::builder("v3_io_error_display_pool")
            .ttl(Duration::from_secs(1))
            .connection_string(refused_url)
            .connection_pool_min_idle(1)
            .connection_pool_connection_timeout(Duration::from_millis(200))
            .build();

        let err = result.expect_err("build must fail against a refused port");
        let RedisCacheBuildError::Pool { ref source } = err else {
            panic!("expected RedisCacheBuildError::Pool, got: {err:?}");
        };
        let source_display = source.to_string();
        assert!(!source_display.is_empty());

        let display = err.to_string();
        assert_ne!(display, "redis pool error");
        assert!(
            display.contains(&source_display),
            "Display must include the underlying cause's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(src.to_string(), source_display);
    }

    /// REDIS-5: `RedisCacheError::Redis`'s `Display` must include the
    /// underlying cause, not just "redis error".
    ///
    /// Real failure path: `cache_set` with a TTL so large that, once converted
    /// to a millisecond expiry, the server's own `mstime() + ms` overflow check
    /// rejects the `PSETEX` (documented on `ttl_millis` in `src/stores/redis.rs`).
    /// Requires a live redis server (`CACHED_REDIS_CONNECTION_STRING`, matching
    /// the other live-redis tests in this repo); skips cleanly if unset.
    #[test]
    fn cache_error_redis_display_contains_cause_and_source_is_wired() {
        let _conn_url = conn_or_skip!();

        let cache = RedisCache::<String, String>::builder("v3_io_error_display_redis_overflow")
            .namespace("")
            .ttl_millis(u64::MAX)
            .build()
            .expect("build RedisCache");

        let err = cache
            .cache_set("k".to_string(), "v".to_string())
            .expect_err("the server must reject a PSETEX whose expiry overflows");
        let RedisCacheError::Redis { ref source } = err else {
            panic!("expected RedisCacheError::Redis, got: {err:?}");
        };
        let source_display = source.to_string();
        assert!(!source_display.is_empty());

        let display = err.to_string();
        assert_ne!(display, "redis error");
        assert!(
            display.contains(&source_display),
            "Display must include the underlying cause's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(src.to_string(), source_display);
    }

    /// REDIS-5: `RedisCacheError::Pool`'s `Display` must include the underlying
    /// cause, not just "redis pool error".
    ///
    /// Saturating a live `RedisCache`'s own internal r2d2 pool hermetically
    /// (without disrupting the shared test redis server used by concurrently
    /// running sibling test suites) is impractical, so the variant is
    /// constructed directly with a real `std::io::Error` cause (its fields are
    /// public; only exhaustive *matching* on this `#[non_exhaustive]` enum
    /// requires a wildcard arm).
    #[test]
    fn cache_error_pool_display_contains_cause_and_source_is_wired() {
        let inner = std::io::Error::other("synthetic redis pool failure for display test");
        let inner_display = inner.to_string();
        let err = RedisCacheError::Pool {
            source: Box::new(inner),
        };

        let display = err.to_string();
        assert_ne!(display, "redis pool error");
        assert!(
            display.contains(&inner_display),
            "Display must include the underlying cause's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(src.to_string(), inner_display);
    }

    /// REDIS-5: `RedisCacheError::CacheDeserialization`'s `Display` must
    /// include the underlying decode-error cause.
    ///
    /// Real failure path: write bytes that are neither valid MessagePack nor
    /// the legacy JSON fallback directly into redis at the key the cache would
    /// read, then read it back with `strict_deserialization(true)` so the
    /// failure surfaces instead of self-healing (mirrors the corrupt-bytes
    /// fixture used by `tests/v3_redis_selfheal.rs`). Requires a live redis
    /// server; skips cleanly if unset.
    #[test]
    fn cache_error_deserialization_display_contains_cause_and_source_is_wired() {
        let _conn_url = conn_or_skip!();

        let prefix = "v3_io_error_display_deser_redis";
        let cache = RedisCache::<String, String>::builder(prefix)
            .namespace("")
            .ttl(Duration::from_secs(30))
            .strict_deserialization(true)
            .build()
            .expect("build RedisCache");
        cache.cache_clear().expect("clear");

        let corrupt: &[u8] = b"\xff\xff not a valid cached value \x00\x01\x02";
        let full_key = format!(":{prefix}:k");
        let mut raw = redis::Client::open(cache.connection_string().reveal())
            .expect("open raw client")
            .get_connection()
            .expect("raw connection");
        redis::cmd("SET")
            .arg(&full_key)
            .arg(corrupt)
            .query::<()>(&mut raw)
            .expect("SET corrupt bytes");

        let err = cache
            .cache_get(&"k".to_string())
            .expect_err("cache_get must surface a deserialization error for corrupt bytes");
        let RedisCacheError::CacheDeserialization {
            ref source,
            ref cached_value,
        } = err
        else {
            panic!("expected RedisCacheError::CacheDeserialization, got: {err:?}");
        };
        assert_eq!(cached_value, corrupt);
        let source_display = source.to_string();
        assert!(!source_display.is_empty());

        let display = err.to_string();
        assert_ne!(display, "error deserializing cached value");
        assert!(
            display.contains(&source_display),
            "Display must include the underlying decode error's text; got: {display}"
        );

        let src = err
            .source()
            .expect("source() must expose the underlying cause");
        assert_eq!(src.to_string(), source_display);

        cache.cache_clear().expect("clean up");
    }

    #[derive(Debug)]
    struct Unserializable;

    impl serde::Serialize for Unserializable {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional serialize failure for display test",
            ))
        }
    }

    impl<'de> serde::Deserialize<'de> for Unserializable {
        fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Ok(Unserializable)
        }
    }

    /// REDIS-5: `RedisCacheError::CacheSerialization`'s `Display` must include
    /// the underlying encode-error cause.
    ///
    /// Real failure path: `cache_set` a value whose `Serialize` impl always
    /// fails. Requires a live redis server (the connection is acquired from the
    /// pool before serialization is attempted); skips cleanly if unset.
    #[test]
    fn cache_error_serialization_display_contains_cause_and_source_is_wired() {
        let _conn_url = conn_or_skip!();

        let cache = RedisCache::<String, Unserializable>::builder("v3_io_error_display_ser_redis")
            .namespace("")
            .ttl(Duration::from_secs(30))
            .build()
            .expect("build RedisCache");

        let err = cache
            .cache_set("k".to_string(), Unserializable)
            .expect_err("cache_set must fail to serialize Unserializable");
        let RedisCacheError::CacheSerialization { ref source } = err else {
            panic!("expected RedisCacheError::CacheSerialization, got: {err:?}");
        };

        let display = err.to_string();
        assert_ne!(display, "error serializing cached value");
        assert!(
            display.contains("intentional serialize failure for display test"),
            "Display must include the underlying encode error's text; got: {display}"
        );
        assert!(
            source
                .to_string()
                .contains("intentional serialize failure for display test"),
            "source() chain must expose the underlying encode error; got: {source}"
        );
    }
}
