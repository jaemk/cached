//! CI runs these phases in separate builds against the same file and Redis prefix.

use cached::prelude::*;
use cached::{AsyncRedisCache, RedbCache, RedisCache};
use std::path::Path;

struct Stores {
    disk: RedbCache<u32, String>,
    redis: RedisCache<u32, String>,
    async_redis: AsyncRedisCache<u32, String>,
}

fn value(key: u32) -> String {
    if key >= 9 {
        return format!("small value {key}");
    }
    format!("{key}: {}", "repeated cache payload ".repeat(256))
}

impl Stores {
    async fn open(path: &Path, strict: bool, compress: bool) -> Self {
        let prefix = path.file_name().unwrap().to_str().unwrap();
        let disk = RedbCache::builder("compression-compat")
            .disk_dir(path)
            .strict_deserialization(strict);
        let redis = RedisCache::builder(prefix)
            .ttl_secs(3600)
            .strict_deserialization(strict);
        let async_redis = AsyncRedisCache::builder(prefix)
            .ttl_secs(3600)
            .strict_deserialization(strict);

        #[cfg(feature = "compression_zstd")]
        let (disk, redis, async_redis) = if compress {
            let zstd = cached::Compression::Zstd { level: 3 };
            (
                disk.compression(zstd),
                redis.compression(zstd),
                async_redis.compression(zstd),
            )
        } else {
            (disk, redis, async_redis)
        };
        #[cfg(not(feature = "compression_zstd"))]
        assert!(!compress);

        Self {
            disk: disk.build().unwrap(),
            redis: redis.build().unwrap(),
            async_redis: async_redis.build().await.unwrap(),
        }
    }

    async fn write_pair(&self, key: u32) {
        self.disk.cache_set(key, value(key)).unwrap();
        self.disk
            .async_cache_set_ref(&(key + 1), &value(key + 1))
            .await
            .unwrap();
        self.redis.cache_set_ref(&key, &value(key)).unwrap();
        self.async_redis
            .async_cache_set(key + 1, value(key + 1))
            .await
            .unwrap();
    }

    async fn assert_read(&self, key: u32, expected: Option<String>) {
        assert_eq!(self.disk.cache_get(&key).unwrap(), expected);
        assert_eq!(self.disk.async_cache_get(&key).await.unwrap(), expected);
        assert_eq!(self.redis.cache_get(&key).unwrap(), expected);
        assert_eq!(
            self.async_redis.async_cache_get(&key).await.unwrap(),
            expected
        );
    }

    async fn assert_unsupported(&self, key: u32) {
        assert!(self.disk.cache_get(&key).unwrap_err().is_deserialization());
        assert!(
            self.disk
                .async_cache_get(&key)
                .await
                .unwrap_err()
                .is_deserialization()
        );
        assert!(self.redis.cache_get(&key).unwrap_err().is_deserialization());
        assert!(
            self.async_redis
                .async_cache_get(&key)
                .await
                .unwrap_err()
                .is_deserialization()
        );
    }
}

/// REDIS-8 and REDB-9. Redis and redb each get one sync and one async writer
/// per configuration. Run via `make tests/io-compression`.
#[tokio::test]
#[ignore = "requires a shared file and Redis namespace across three CI phases"]
async fn compression_compatibility() {
    let state = std::env::var("CACHED_COMPRESSION_TEST_DIR").unwrap();
    let path = Path::new(&state);
    let phase = std::env::var("CACHED_COMPRESSION_TEST_PHASE").unwrap();

    assert_eq!(
        cfg!(feature = "compression_zstd"),
        phase == "zstd-writers-and-readers"
    );
    match phase.as_str() {
        "raw-writer" => {
            Stores::open(path, true, false).await.write_pair(1).await;
        }
        "zstd-writers-and-readers" => {
            Stores::open(path, true, false).await.write_pair(3).await;
            let writer = Stores::open(path, true, true).await;
            writer.write_pair(5).await;
            writer.write_pair(7).await;
            writer.write_pair(9).await;
            drop(writer);
            for compress in [false, true] {
                let reader = Stores::open(path, true, compress).await;
                for key in 1..=10 {
                    reader.assert_read(key, Some(value(key))).await;
                }
            }
        }
        "raw-reader" => {
            let strict = Stores::open(path, true, false).await;
            for key in (1..=4).chain(9..=10) {
                strict.assert_read(key, Some(value(key))).await;
            }
            // The second reader for each backend also reports the frame as
            // undecodable, proving the first strict read preserved the entry.
            for key in 5..=8 {
                strict.assert_unsupported(key).await;
            }
            drop(strict);

            let healing = Stores::open(path, false, false).await;
            for key in 5..=6 {
                assert_eq!(healing.disk.cache_get(&key).unwrap(), None);
                assert_eq!(healing.redis.cache_get(&key).unwrap(), None);
            }
            for key in 7..=8 {
                assert_eq!(healing.disk.async_cache_get(&key).await.unwrap(), None);
                assert_eq!(
                    healing.async_redis.async_cache_get(&key).await.unwrap(),
                    None
                );
            }
            drop(healing);

            let strict = Stores::open(path, true, false).await;
            for key in 5..=8 {
                strict.assert_read(key, None).await;
            }
            for key in (1..=4).chain(9..=10) {
                strict.assert_read(key, Some(value(key))).await;
            }
            strict.redis.cache_clear().unwrap();
        }
        _ => panic!("unknown compression compatibility phase: {phase}"),
    }
}
