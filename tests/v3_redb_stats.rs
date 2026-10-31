//! `RedbCache::scan_stats` from outside the crate (#336).
//!
//! Pins that `RedbCacheStats` is reachable from both the crate root and
//! `cached::stores`, that its fields are readable downstream, and that the scan
//! sees entries written through the public API while `cache_size` stays `None`.

#![cfg(feature = "redb_store")]

use cached::{ConcurrentCacheBase, ConcurrentCached, RedbCache, RedbCacheStats};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

#[test]
fn scan_stats_counts_public_writes() {
    let dir = TempDir::new().unwrap();
    let cache: RedbCache<u32, String> = RedbCache::builder("scan-stats-public")
        .disk_dir(dir.path())
        .ttl(Duration::from_secs(60))
        .durable(false)
        .build()
        .unwrap();
    let before = SystemTime::now();
    cache.cache_set(1, "a".to_string()).unwrap();
    cache.cache_set(2, "b".to_string()).unwrap();
    let after = SystemTime::now();

    let stats: cached::stores::RedbCacheStats = cache.scan_stats().unwrap();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.expired, 0);
    assert_eq!(stats.undecodable, 0);
    let (oldest, newest) = (stats.oldest.unwrap(), stats.newest.unwrap());
    assert!(before <= oldest && oldest <= newest && newest <= after);

    // The count is opt-in: the cheap accessor still declines to scan.
    assert_eq!(cache.cache_size().unwrap(), None);
}

#[test]
fn scan_stats_on_empty_cache_is_default() {
    let dir = TempDir::new().unwrap();
    let cache: RedbCache<u32, u32> = RedbCache::builder("scan-stats-public-empty")
        .disk_dir(dir.path())
        .build()
        .unwrap();
    assert_eq!(cache.scan_stats().unwrap(), RedbCacheStats::default());
}
