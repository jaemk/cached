//! `RedbCache::builder` takes the required cache `name` positionally.
//!
//! These pin the positional-builder API: `RedbCache::builder(name)` supplies the
//! name (this file would fail to compile if the method regressed to no-arg), and
//! `RedbCacheBuilder::new()` (no positional) still reports the missing name at
//! build time without touching disk.

#![cfg(feature = "redb_store")]

use cached::{BuildError, ConcurrentCached, RedbCache, RedbCacheBuildError, RedbCacheBuilder};
use tempfile::TempDir;

#[test]
fn builder_positional_name_builds() {
    let dir = TempDir::new().unwrap();
    let cache: RedbCache<u32, u32> = RedbCache::builder("positional-name")
        .disk_dir(dir.path())
        .durable(false)
        .build()
        .expect("build with positional name");

    cache.cache_set(1, 10).unwrap();
    assert_eq!(cache.cache_get(&1).unwrap(), Some(10));
}

#[test]
fn builder_positional_name_can_be_overridden() {
    let dir = TempDir::new().unwrap();
    // A later `.name(...)` overrides the positional argument.
    let cache: RedbCache<u32, u32> = RedbCache::builder("initial")
        .name("overridden")
        .disk_dir(dir.path())
        .durable(false)
        .build()
        .expect("build with overridden name");

    let file = cache.disk_path().to_string_lossy().to_string();
    assert!(
        file.contains("overridden") && !file.contains("initial"),
        "on-disk file must use the overriding name, got {file}"
    );
}

#[test]
fn builder_new_without_name_is_server_free_missing_required() {
    // `RedbCacheBuilder::new()` omits the positional name; build reports it
    // without any disk IO.
    let result = RedbCacheBuilder::<u32, u32>::new().build();
    assert!(
        matches!(
            result,
            Err(RedbCacheBuildError::Build(BuildError::MissingRequired(
                "name"
            )))
        ),
        "expected Build(MissingRequired(\"name\"))"
    );
}
