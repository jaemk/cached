use std::borrow::Cow;
use std::io;

/// Compression algorithm used when writing values to an I/O cache.
#[cfg(feature = "compression_zstd")]
#[cfg_attr(docsrs, doc(cfg(feature = "compression_zstd")))]
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Compression {
    /// Write zstd frames. Level 0 uses the zstd default (currently 3).
    Zstd {
        /// Compression level passed to zstd.
        level: i32,
    },
}

#[cfg(feature = "compression_zstd")]
#[derive(Clone, Copy)]
struct CompressionSettings {
    algorithm: Option<Compression>,
    min_compress_size: usize,
    max_decompressed_size: usize,
}

#[cfg(feature = "compression_zstd")]
impl Default for CompressionSettings {
    fn default() -> Self {
        Self {
            algorithm: None,
            min_compress_size: 256,
            max_decompressed_size: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct IoValueCompression {
    #[cfg(feature = "compression_zstd")]
    settings: CompressionSettings,
    #[cfg(all(test, feature = "compression_zstd"))]
    pub(super) fail_decode_allocation: bool,
}

#[cfg(feature = "compression_zstd")]
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

#[cfg(feature = "compression_zstd")]
fn decompress_zstd(bytes: &[u8], size: usize) -> io::Result<Vec<u8>> {
    use zstd::zstd_safe::{DCtx, get_error_name};

    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(size)
        .map_err(|_| io::ErrorKind::OutOfMemory)?;
    DCtx::try_create()
        .ok_or(io::ErrorKind::OutOfMemory)?
        .decompress(&mut decoded, bytes)
        .map_err(|code| io::Error::new(io::ErrorKind::InvalidData, get_error_name(code)))?;
    Ok(decoded)
}

// The codec could not allocate resources to decode the value. This says nothing
// about the stored bytes, so callers must not treat it as corruption.
pub(super) fn is_decompression_allocation_failure(
    error: &(dyn std::error::Error + 'static),
) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::OutOfMemory)
}

impl IoValueCompression {
    #[cfg(feature = "compression_zstd")]
    pub(super) fn set_algorithm(&mut self, algorithm: Compression) {
        self.settings.algorithm = Some(algorithm);
    }

    #[cfg(feature = "compression_zstd")]
    pub(super) fn set_min_compress_size(&mut self, size: usize) {
        self.settings.min_compress_size = size;
    }

    #[cfg(feature = "compression_zstd")]
    pub(super) fn set_max_decompressed_size(&mut self, size: usize) {
        self.settings.max_decompressed_size = size;
    }

    pub(super) fn encode(self, bytes: Vec<u8>) -> io::Result<Vec<u8>> {
        #[cfg(feature = "compression_zstd")]
        if let Some(Compression::Zstd { level }) = self.settings.algorithm
            && bytes.len() >= self.settings.min_compress_size
            && bytes.len() <= self.settings.max_decompressed_size
        {
            let frame = zstd::bulk::compress(&bytes, level)?;
            if frame.len() < bytes.len() {
                return Ok(frame);
            }
        }
        Ok(bytes)
    }

    pub(super) fn decode(self, bytes: &[u8]) -> io::Result<Cow<'_, [u8]>> {
        #[cfg(feature = "compression_zstd")]
        if bytes.starts_with(&ZSTD_MAGIC) {
            let size = zstd::zstd_safe::get_frame_content_size(bytes)
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid zstd frame header")
                })?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "missing zstd frame content size",
                    )
                })?;
            let size = usize::try_from(size)
                .ok()
                .filter(|size| *size <= self.settings.max_decompressed_size)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "zstd frame exceeds decompression limit",
                    )
                })?;
            #[cfg(test)]
            if self.fail_decode_allocation {
                return Err(io::ErrorKind::OutOfMemory.into());
            }
            return decompress_zstd(bytes, size).map(Cow::Owned);
        }
        Ok(Cow::Borrowed(bytes))
    }
}

#[cfg(all(test, feature = "compression_zstd"))]
mod tests {
    use super::*;

    #[test]
    fn compression_threshold_and_limit_are_inclusive() {
        let mut settings = IoValueCompression::default();
        let bytes = vec![b'a'; 256];

        assert_eq!(settings.encode(bytes.clone()).unwrap(), bytes);
        settings.set_algorithm(Compression::Zstd { level: 0 });
        settings.set_min_compress_size(256);
        settings.set_max_decompressed_size(256);

        let below_threshold = vec![b'a'; 255];
        assert_eq!(
            settings.encode(below_threshold.clone()).unwrap(),
            below_threshold
        );
        let above_limit = vec![b'a'; 257];
        assert_eq!(settings.encode(above_limit.clone()).unwrap(), above_limit);

        let frame = settings.encode(bytes.clone()).unwrap();
        assert_eq!(frame[..4], [0x28, 0xb5, 0x2f, 0xfd]);
        assert!(frame.len() < bytes.len());
        assert_eq!(zstd::bulk::decompress(&frame, 256).unwrap(), bytes);
        assert_eq!(settings.decode(&frame).unwrap().as_ref(), bytes);
        settings.set_max_decompressed_size(255);
        assert!(settings.decode(&frame).is_err());
        assert!(matches!(settings.decode(&bytes).unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn decompression_reports_unrepresentable_buffer_capacity() {
        let frame = zstd::bulk::compress(b"valid cache data", 3).unwrap();
        let error = decompress_zstd(&frame, usize::MAX).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn compression_keeps_raw_bytes_when_framing_would_expand_them() {
        let mut settings = IoValueCompression::default();
        settings.set_algorithm(Compression::Zstd { level: 3 });
        settings.set_min_compress_size(0);
        assert_eq!(settings.encode(vec![42]).unwrap(), [42]);
    }

    #[test]
    fn decompression_rejects_invalid_headers_unknown_sizes_and_truncated_frames() {
        let settings = IoValueCompression::default();
        assert!(settings.decode(&ZSTD_MAGIC).is_err());

        let bytes = vec![b'a'; 256];
        let streaming_frame = zstd::stream::encode_all(bytes.as_slice(), 3).unwrap();
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&streaming_frame).unwrap(),
            None
        );
        assert!(settings.decode(&streaming_frame).is_err());

        let mut truncated = zstd::bulk::compress(&bytes, 3).unwrap();
        truncated.pop().unwrap();
        assert!(settings.decode(&truncated).is_err());
    }

    #[cfg(all(feature = "redb_store", feature = "redis_tokio"))]
    #[tokio::test]
    async fn serialization_errors_preserve_existing_compressed_values() {
        use crate::{
            AsyncRedisCache, RedbCache, RedisCache, SerializeCached, SerializeCachedAsync,
        };
        use std::error::Error;

        #[derive(Debug, serde::Deserialize)]
        #[serde(transparent)]
        struct Value(Option<String>);

        impl serde::Serialize for Value {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match &self.0 {
                    Some(value) => serializer.serialize_str(value),
                    None => Err(serde::ser::Error::custom("test serialization failure")),
                }
            }
        }

        fn assert_msgpack_error(error: impl Error) {
            assert!(error.source().unwrap().is::<rmp_serde::encode::Error>());
        }

        fn check_sync(cache: &impl SerializeCached<u32, Value>, payload: &str) {
            cache
                .cache_set_ref(&1, &Value(Some(payload.into())))
                .unwrap();
            assert_msgpack_error(cache.cache_set(1, Value(None)).unwrap_err());
            assert_msgpack_error(cache.cache_set_ref(&1, &Value(None)).unwrap_err());
            assert_eq!(
                cache.cache_remove(&1).unwrap().unwrap().0.as_deref(),
                Some(payload)
            );
        }

        async fn check_async(cache: &impl SerializeCachedAsync<u32, Value>, payload: &str) {
            cache
                .async_cache_set_ref(&1, &Value(Some(payload.into())))
                .await
                .unwrap();
            assert_msgpack_error(cache.async_cache_set(1, Value(None)).await.unwrap_err());
            assert_msgpack_error(
                cache
                    .async_cache_set_ref(&1, &Value(None))
                    .await
                    .unwrap_err(),
            );
            assert_eq!(
                cache
                    .async_cache_remove(&1)
                    .await
                    .unwrap()
                    .unwrap()
                    .0
                    .as_deref(),
                Some(payload)
            );
        }

        let compression = Compression::Zstd { level: 3 };
        let payload = "valid cache contents".repeat(256);
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().file_name().unwrap().to_str().unwrap();
        let disk = RedbCache::builder("serialization-error")
            .disk_dir(dir.path())
            .compression(compression)
            .build()
            .unwrap();
        let redis = RedisCache::builder(prefix)
            .compression(compression)
            .build()
            .unwrap();
        let async_redis = AsyncRedisCache::builder(prefix)
            .compression(compression)
            .build()
            .await
            .unwrap();

        check_sync(&disk, &payload);
        check_sync(&redis, &payload);
        check_async(&disk, &payload).await;
        check_async(&async_redis, &payload).await;
    }
}
