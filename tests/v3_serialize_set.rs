//! (#196): the `#[concurrent_cached]` redis/disk set paths use the borrowed
//! setter (`SerializeCached::cache_set_ref` / `SerializeCachedAsync::async_cache_set_ref`)
//! instead of cloning the value before an owned `cache_set`. These tests prove the
//! set/get round-trip still works through the new call path on the disk (redb) store,
//! across the plain-`Result`, `with_cached_flag`, and async arms. The redis path is
//! covered at compile time by the feature builds (no live redis server in CI here).
//!
//! The borrowed setter now applies to ANY store that implements `SerializeCached` via the
//! autoref shim, including custom `ty`/`create` stores like `RedbCache`. The `*_custom_redb`
//! test exercises that path.

#![cfg(all(feature = "redb_store", feature = "proc_macro"))]

use cached::RedbCache;
use cached::macros::concurrent_cached;
use std::sync::atomic::{AtomicU32, Ordering};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum SerializeSetError {
    #[error("disk error `{0}`")]
    Disk(String),
}

static PLAIN_CALLS: AtomicU32 = AtomicU32::new(0);

#[concurrent_cached(
    disk = true,
    map_error = r##"|e| SerializeSetError::Disk(format!("{e:?}"))"##
)]
fn disk_plain(n: u32) -> Result<u32, SerializeSetError> {
    PLAIN_CALLS.fetch_add(1, Ordering::SeqCst);
    Ok(n * 2)
}

#[test]
fn disk_plain_result_round_trips_via_cache_set_ref() {
    use cached::ConcurrentCached;
    // redb disk caches persist on disk across test runs; clear first so the
    // call-count assertion is deterministic.
    DISK_PLAIN.cache_clear().expect("clear disk cache");
    PLAIN_CALLS.store(0, Ordering::SeqCst);
    // First call computes and stores via the borrowed setter.
    assert_eq!(disk_plain(21), Ok(42));
    // Second call is served from the cache (body not re-run).
    assert_eq!(disk_plain(21), Ok(42));
    assert_eq!(PLAIN_CALLS.load(Ordering::SeqCst), 1);
}

static FLAG_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[concurrent_cached(
    disk = true,
    with_cached_flag = true,
    map_error = r##"|e| SerializeSetError::Disk(format!("{e:?}"))"##
)]
fn disk_flag(n: u32) -> Result<cached::Return<u32>, SerializeSetError> {
    FLAG_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(cached::Return::new(n + 1))
}

#[test]
fn disk_with_cached_flag_round_trips_via_cache_set_ref() {
    use cached::ConcurrentCached;
    DISK_FLAG.cache_clear().expect("clear disk cache");
    FLAG_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    let first = disk_flag(100).unwrap();
    assert_eq!(*first, 101);
    assert!(!first.was_cached());
    let second = disk_flag(100).unwrap();
    assert_eq!(*second, 101);
    assert!(second.was_cached());
    // Body must have run exactly once: the second call is a cache hit.
    assert_eq!(FLAG_CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
}

// Custom `ty`/`create` redb store: `RedbCache` implements `SerializeCached`, so the
// autoref shim picks the borrowed `cache_set_ref` path (no value clone) even for a
// `ty`/`create` store. Still round-trips correctly.
static CUSTOM_CALLS: AtomicU32 = AtomicU32::new(0);

#[concurrent_cached(
    map_error = r##"|e| SerializeSetError::Disk(format!("{e:?}"))"##,
    ty = "cached::RedbCache<u32, u32>",
    create = r##" { RedbCache::builder("serialize_set_custom_redb").build().expect("build redb") } "##
)]
fn custom_redb(n: u32) -> Result<u32, SerializeSetError> {
    CUSTOM_CALLS.fetch_add(1, Ordering::SeqCst);
    Ok(n + 7)
}

#[test]
fn custom_redb_round_trips_via_cache_set_ref() {
    use cached::ConcurrentCached;
    // Clear the persisted store so the borrowed-set path actually runs (a re-run
    // would otherwise be a pure cache hit and skip the path under test).
    CUSTOM_REDB.cache_clear().expect("clear disk cache");
    CUSTOM_CALLS.store(0, Ordering::SeqCst);
    assert_eq!(custom_redb(3), Ok(10));
    assert_eq!(custom_redb(3), Ok(10));
    assert_eq!(CUSTOM_CALLS.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "async")]
mod async_disk {
    use super::*;

    static ASYNC_CALLS: AtomicU32 = AtomicU32::new(0);

    #[concurrent_cached(
        disk = true,
        map_error = r##"|e| SerializeSetError::Disk(format!("{e:?}"))"##
    )]
    async fn disk_async(n: u32) -> Result<u32, SerializeSetError> {
        ASYNC_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(n * 3)
    }

    #[tokio::test]
    async fn disk_async_round_trips_via_async_cache_set_ref() {
        use cached::ConcurrentCached;
        // The async cache static is a lazily-initialized `OnceCell`: make one
        // call to initialize it, then clear the persisted store so the
        // borrowed-set path actually runs (a re-run would otherwise be a pure
        // cache hit and skip the path under test).
        let _ = disk_async(4).await;
        DISK_ASYNC
            .get()
            .expect("cache initialized by the call above")
            .cache_clear()
            .expect("clear disk cache");
        ASYNC_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(disk_async(4).await, Ok(12));
        assert_eq!(disk_async(4).await, Ok(12));
        assert_eq!(ASYNC_CALLS.load(Ordering::SeqCst), 1);
    }
}
