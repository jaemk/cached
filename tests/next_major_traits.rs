//! Integration tests for the next-major trait additions:
//! - `*_mut` get-or-set variants (#179)
//! - `SerializeCached`/`SerializeCachedAsync` borrowed set (#196)

use cached::{Cached, LruCache, UnboundCache};

/// The `_mut` variants return a mutable reference that callers can mutate
/// in place; the resulting change is observable on the next read.
#[test]
fn cache_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: UnboundCache<u32, u32> =
        UnboundCache::builder().build().expect("build UnboundCache");

    // Insert via the mutable variant and mutate the returned `&mut V`.
    let v: &mut u32 = cache.cache_get_or_set_with_mut(1, || 10);
    assert_eq!(*v, 10);
    *v += 5;
    assert_eq!(cache.cache_get(&1), Some(&15));

    // The shared-reference variant returns `&V` (it sees the mutated value on hit).
    let shared: &u32 = cache.cache_get_or_set_with(1, || 999);
    assert_eq!(*shared, 15);
}

#[test]
fn cache_try_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: UnboundCache<u32, u32> =
        UnboundCache::builder().build().expect("build UnboundCache");

    let v: &mut u32 = cache
        .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(10))
        .unwrap();
    assert_eq!(*v, 10);
    *v *= 2;
    assert_eq!(cache.cache_get(&1), Some(&20));

    // Shared-ref fallible variant returns `Result<&V, E>`.
    let shared: &u32 = cache
        .cache_try_get_or_set_with(1, || Ok::<u32, ()>(999))
        .unwrap();
    assert_eq!(*shared, 20);
}

#[test]
fn lru_cache_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: LruCache<u32, u32> = LruCache::builder()
        .max_size(10)
        .build()
        .expect("build LruCache");

    // Miss: body runs, value inserted; mutate through the returned `&mut V`.
    let v: &mut u32 = cache.cache_get_or_set_with_mut(1, || 10);
    assert_eq!(*v, 10);
    *v += 5;
    assert_eq!(cache.cache_get(&1), Some(&15));

    // Hit: body does not run; returns the mutated value.
    let hit: &mut u32 = cache.cache_get_or_set_with_mut(1, || 999);
    assert_eq!(*hit, 15);
}

#[test]
fn lru_cache_try_get_or_set_with_mut_returns_mutable_ref() {
    let mut cache: LruCache<u32, u32> = LruCache::builder()
        .max_size(10)
        .build()
        .expect("build LruCache");

    // Err: propagated, nothing cached.
    let result: Result<&mut u32, ()> = cache.cache_try_get_or_set_with_mut(1, || Err(()));
    assert!(result.is_err());
    assert_eq!(cache.cache_get(&1), None);

    // Ok miss: value inserted; mutate through the returned `&mut V`.
    let v: &mut u32 = cache
        .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(10))
        .unwrap();
    assert_eq!(*v, 10);
    *v *= 2;
    assert_eq!(cache.cache_get(&1), Some(&20));

    // Hit: body does not run; stored value returned.
    let hit: &mut u32 = cache
        .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(999))
        .unwrap();
    assert_eq!(*hit, 20);
}

// ExpiringCache values must implement Expires; use a simple never-expiring wrapper.
mod expiring_cache_mut {
    use cached::{Cached, Expires, ExpiringCache};

    // A value that never expires. ExpiringCache requires V: Expires.
    #[derive(Debug, PartialEq, Clone)]
    struct Never(u32);

    impl Expires for Never {
        fn is_expired(&self) -> bool {
            false
        }
    }

    /// `cache_get_or_set_with_mut` on ExpiringCache: value computed once on miss,
    /// returned from cache on hit (body does not run again).
    #[test]
    fn expiring_cache_get_or_set_with_mut() {
        let mut cache: ExpiringCache<u32, Never> = ExpiringCache::builder()
            .build()
            .expect("build ExpiringCache");

        // Miss: body runs, value inserted.
        let v: &mut Never = cache.cache_get_or_set_with_mut(1, || Never(10));
        assert_eq!(*v, Never(10));

        // Mutate in place and confirm the change is visible on a subsequent get.
        v.0 += 5;
        assert_eq!(cache.cache_get(&1), Some(&Never(15)));

        // Hit: body does not run; returns the previously stored (mutated) value.
        let hit: &mut Never = cache.cache_get_or_set_with_mut(1, || Never(999));
        assert_eq!(*hit, Never(15));
    }

    /// `cache_try_get_or_set_with_mut` on ExpiringCache: Err from setter is propagated
    /// and the key is not inserted; Ok path stores and returns a mutable ref.
    #[test]
    fn expiring_cache_try_get_or_set_with_mut() {
        let mut cache: ExpiringCache<u32, Never> = ExpiringCache::builder()
            .build()
            .expect("build ExpiringCache");

        // Err: propagated, nothing cached.
        let result: Result<&mut Never, ()> = cache.cache_try_get_or_set_with_mut(1, || Err(()));
        assert!(result.is_err());
        assert_eq!(cache.cache_get(&1), None);

        // Ok miss: value inserted.
        let v: &mut Never = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<Never, ()>(Never(20)))
            .unwrap();
        assert_eq!(*v, Never(20));
        v.0 *= 2;
        assert_eq!(cache.cache_get(&1), Some(&Never(40)));

        // Hit: body does not run; stored value returned.
        let hit: &mut Never = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<Never, ()>(Never(999)))
            .unwrap();
        assert_eq!(*hit, Never(40));
    }
}

#[cfg(feature = "time_stores")]
mod ttl_sorted_cache_mut {
    use cached::{Cached, TtlSortedCache};
    use std::time::Duration;

    /// `cache_get_or_set_with_mut` on TtlSortedCache: value computed once on miss,
    /// returned from cache on hit (body does not run again).
    #[test]
    fn ttl_sorted_cache_get_or_set_with_mut() {
        let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build TtlSortedCache");

        // Miss: body runs, value inserted.
        let v: &mut u32 = cache.cache_get_or_set_with_mut(1, || 10);
        assert_eq!(*v, 10);

        // Mutate in place and confirm the change persists.
        *v += 5;
        assert_eq!(cache.cache_get(&1), Some(&15));

        // Hit: body does not run; stored (mutated) value returned.
        let hit: &mut u32 = cache.cache_get_or_set_with_mut(1, || 999);
        assert_eq!(*hit, 15);
    }

    /// `cache_try_get_or_set_with_mut` on TtlSortedCache: Err from setter is propagated
    /// and the key is not inserted; Ok path stores and returns a mutable ref.
    #[test]
    fn ttl_sorted_cache_try_get_or_set_with_mut() {
        let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("build TtlSortedCache");

        // Err: propagated, nothing cached.
        let result: Result<&mut u32, ()> = cache.cache_try_get_or_set_with_mut(1, || Err(()));
        assert!(result.is_err());
        assert_eq!(cache.cache_get(&1), None);

        // Ok miss: value inserted.
        let v: &mut u32 = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(20))
            .unwrap();
        assert_eq!(*v, 20);
        *v *= 2;
        assert_eq!(cache.cache_get(&1), Some(&40));

        // Hit: body does not run; stored value returned.
        let hit: &mut u32 = cache
            .cache_try_get_or_set_with_mut(1, || Ok::<u32, ()>(999))
            .unwrap();
        assert_eq!(*hit, 40);
    }
}

#[cfg(feature = "disk_store")]
mod redb_serialize_cached {
    use cached::stores::RedbCache;
    use cached::time::Duration;
    use cached::{ConcurrentCached, SerializeCached};
    use tempfile::TempDir;

    fn build_cache(dir: &TempDir, name: &str) -> RedbCache<u32, String> {
        RedbCache::<u32, String>::builder(name)
            .disk_directory(dir.path())
            .build()
            .expect("error building redb cache")
    }

    /// `cache_set_ref` takes `&K, &V` (no clone needed at the call site) and
    /// round-trips through the same store as `cache_set`.
    #[test]
    fn cache_set_ref_round_trip() {
        let dir = TempDir::new().unwrap();
        let cache = build_cache(&dir, "serialize_cached_round_trip");

        let key: u32 = 42;
        let value: String = "hello".to_string();

        // Borrowed set: `key` and `value` are still owned by the caller afterward.
        let prev = cache
            .cache_set_ref(&key, &value)
            .expect("cache_set_ref failed");
        assert_eq!(prev, None);
        assert_eq!(key, 42);
        assert_eq!(value, "hello");

        // Read back the value written via the borrowed setter.
        assert_eq!(cache.cache_get(&key).unwrap(), Some("hello".to_string()));

        // Overwriting returns the previous value (proving same storage as cache_set).
        let prev = cache
            .cache_set_ref(&key, &"world".to_string())
            .expect("cache_set_ref overwrite failed");
        assert_eq!(prev, Some("hello".to_string()));
        assert_eq!(cache.cache_get(&key).unwrap(), Some("world".to_string()));
    }

    /// A value written via `cache_set` reads back identically to one written via
    /// `cache_set_ref` — the borrowed serialize path is byte-compatible.
    #[test]
    fn cache_set_ref_matches_cache_set() {
        let dir = TempDir::new().unwrap();
        let cache = build_cache(&dir, "serialize_cached_compat");

        cache.cache_set(1, "owned".to_string()).unwrap();
        cache.cache_set_ref(&2, &"owned".to_string()).unwrap();

        assert_eq!(cache.cache_get(&1).unwrap(), cache.cache_get(&2).unwrap());
    }

    /// A value written via `cache_set_ref` carries a `created_at` timestamp that the
    /// expiry check reads. After sleeping past the TTL the entry must be absent.
    #[test]
    fn cache_set_ref_ttl_expiry() {
        let dir = TempDir::new().unwrap();
        let cache: RedbCache<u32, String> = RedbCache::builder("serialize_cached_ttl_expiry")
            .disk_directory(dir.path())
            .ttl(Duration::from_millis(100))
            .build()
            .expect("error building redb cache");

        let key: u32 = 1;
        let value: String = "expires".to_string();

        let prev = cache
            .cache_set_ref(&key, &value)
            .expect("cache_set_ref failed");
        assert_eq!(prev, None);

        // Entry is present immediately after insertion.
        assert_eq!(cache.cache_get(&key).unwrap(), Some("expires".to_string()));

        // Sleep past the TTL; the entry must now be treated as expired (absent).
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(cache.cache_get(&key).unwrap(), None);
    }
}

#[cfg(all(feature = "disk_store", feature = "async"))]
mod redb_serialize_cached_async {
    use cached::stores::RedbCache;
    use cached::{ConcurrentCachedAsync, SerializeCachedAsync};
    use tempfile::TempDir;

    #[tokio::test]
    async fn async_cache_set_ref_round_trip() {
        let dir = TempDir::new().unwrap();
        let cache: RedbCache<u32, String> = RedbCache::builder("serialize_cached_async_round_trip")
            .disk_directory(dir.path())
            .build()
            .expect("error building redb cache");

        let key: u32 = 7;
        let value: String = "async".to_string();

        let prev = cache
            .async_cache_set_ref(&key, &value)
            .await
            .expect("async_cache_set_ref failed");
        assert_eq!(prev, None);
        // Caller still owns the borrowed inputs.
        assert_eq!(key, 7);
        assert_eq!(value, "async");

        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some("async".to_string())
        );
    }

    /// Overwriting an existing entry via `async_cache_set_ref` returns the previous value
    /// and the store reflects the new value on the next read.
    #[tokio::test]
    async fn async_cache_set_ref_overwrite() {
        let dir = TempDir::new().unwrap();
        let cache: RedbCache<u32, String> = RedbCache::builder("serialize_cached_async_overwrite")
            .disk_directory(dir.path())
            .build()
            .expect("error building redb cache");

        let key: u32 = 99;

        // First insert: no previous value.
        let prev = cache
            .async_cache_set_ref(&key, &"first".to_string())
            .await
            .expect("async_cache_set_ref first failed");
        assert_eq!(prev, None);

        // Overwrite: previous value is returned.
        let prev = cache
            .async_cache_set_ref(&key, &"second".to_string())
            .await
            .expect("async_cache_set_ref overwrite failed");
        assert_eq!(prev, Some("first".to_string()));

        // Store reflects the new value.
        assert_eq!(
            cache.async_cache_get(&key).await.unwrap(),
            Some("second".to_string())
        );
    }
}
