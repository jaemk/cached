//! Tests for the bound-drop on `ConcurrentCached::cache_contains` and the inherent
//! `contains` on sharded stores.
//!
//! Three behavioral paths are pinned:
//! 1. Generic code over `C: ConcurrentCached<K, V>` can call `cache_contains` with no
//!    `V: Clone` bound -- compile proof that the bound was dropped.
//! 2. An external implementor with a NON-Clone value type can implement
//!    `ConcurrentCached` / `ConcurrentCachedAsync` and use `cache_contains` /
//!    `async_cache_contains` correctly.
//! 3. Inherent sharded `contains` resolves to `bool` (not `Result<bool, _>`) and
//!    agrees with `cache_contains`, including TTL expiry semantics.

// ── 1. Generic helper: no V: Clone bound ─────────────────────────────────────

use cached::{ConcurrentCached, ShardedLruCache, ShardedUnboundCache};

/// Compile-proof that `cache_contains` does not require `V: Clone`.
///
/// If a `V: Clone` bound crept back onto `ConcurrentCached::cache_contains`,
/// calling this function with `V = String` would still compile (String is Clone),
/// but calling it with a non-Clone value would not -- and the custom-store test
/// below (section 2) calls the equivalent path with `V = NoClone`. The type
/// annotation on the return makes the bound check explicit at this call site.
fn has<K, V, C: ConcurrentCached<K, V>>(c: &C, k: &K) -> bool {
    // No V: Clone bound anywhere in this signature or body.
    c.cache_contains(k).ok().unwrap_or(false)
}

#[test]
fn generic_has_compiles_and_returns_correct_result() {
    let cache: ShardedUnboundCache<u32, String> =
        ShardedUnboundCache::builder().build().expect("build");

    // Key 1 absent -- has must return false.
    assert!(!has(&cache, &1u32));

    cache.cache_set(1, "hello".to_string()).unwrap();

    // Key 1 present -- has must return true.
    assert!(has(&cache, &1u32));

    // Key 2 never inserted -- still false.
    assert!(!has(&cache, &2u32));

    cache.cache_remove(&1).unwrap();
    // After removal, absent again.
    assert!(!has(&cache, &1u32));
}

// ── 2. Custom store with NON-Clone value type ─────────────────────────────────

/// A value type that deliberately does NOT implement `Clone`.
/// Implementing `cache_contains` on a store holding `NoClone` values proves that
/// the required method works without any `V: Clone` in scope.
struct NoClone(#[allow(dead_code)] u32);

// Intentionally NOT `impl Clone for NoClone`.

mod custom_store {
    //! Minimal `ConcurrentCached` / `ConcurrentCachedAsync` implementation
    //! for a non-Clone value type.

    use super::NoClone;
    use cached::{ConcurrentCacheBase, ConcurrentCached};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::convert::Infallible;

    // A tiny hand-written concurrent store backed by a Mutex<HashMap>.
    pub struct NoCloneStore {
        inner: Mutex<HashMap<u32, NoClone>>,
    }

    impl NoCloneStore {
        pub fn new() -> Self {
            Self {
                inner: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ConcurrentCacheBase for NoCloneStore {
        type Error = Infallible;
    }

    impl ConcurrentCached<u32, NoClone> for NoCloneStore {
        fn cache_get(&self, k: &u32) -> Result<Option<NoClone>, Self::Error> {
            // Move the value out (remove) to return an owned copy -- semantics
            // don't matter for this test; we only need a correct `cache_contains`.
            Ok(self.inner.lock().remove(k))
        }

        fn cache_set(&self, k: u32, v: NoClone) -> Result<Option<NoClone>, Self::Error> {
            Ok(self.inner.lock().insert(k, v))
        }

        fn cache_remove(&self, k: &u32) -> Result<Option<NoClone>, Self::Error> {
            Ok(self.inner.lock().remove(k))
        }

        fn cache_remove_entry(&self, k: &u32) -> Result<Option<(u32, NoClone)>, Self::Error> {
            Ok(self.inner.lock().remove_entry(k))
        }

        fn cache_delete(&self, k: &u32) -> Result<bool, Self::Error> {
            Ok(self.inner.lock().remove(k).is_some())
        }

        /// Peek-based check: does NOT require V: Clone.
        fn cache_contains(&self, k: &u32) -> Result<bool, Self::Error>
        where
            Self: Sized,
        {
            Ok(self.inner.lock().contains_key(k))
        }

        fn cache_clear(&self) -> Result<(), Self::Error> {
            self.inner.lock().clear();
            Ok(())
        }

        fn cache_reset(&self) -> Result<(), Self::Error> {
            self.cache_clear()
        }
    }

    #[test]
    fn cache_contains_true_after_set_no_clone_bound() {
        let store = NoCloneStore::new();

        // Key absent initially.
        assert!(!store.cache_contains(&10).unwrap());

        store.cache_set(10, NoClone(42)).unwrap();

        // Key present -- cache_contains must return true without needing V: Clone.
        assert!(store.cache_contains(&10).unwrap());
    }

    #[test]
    fn cache_contains_false_after_removal() {
        let store = NoCloneStore::new();
        store.cache_set(7, NoClone(1)).unwrap();
        assert!(store.cache_contains(&7).unwrap());

        store.cache_remove(&7).unwrap();
        assert!(!store.cache_contains(&7).unwrap());
    }
}

// ── 2b. Async custom store with NON-Clone value type ─────────────────────────

#[cfg(feature = "async_core")]
mod custom_store_async {
    use super::NoClone;
    use cached::{ConcurrentCacheBase, ConcurrentCached, ConcurrentCachedAsync};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::convert::Infallible;

    pub struct NoCloneAsyncStore {
        inner: Mutex<HashMap<u32, NoClone>>,
    }

    impl NoCloneAsyncStore {
        pub fn new() -> Self {
            Self {
                inner: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ConcurrentCacheBase for NoCloneAsyncStore {
        type Error = Infallible;
    }

    // Keep the sync impl as well so we can delegate from the async one.
    impl ConcurrentCached<u32, NoClone> for NoCloneAsyncStore {
        fn cache_get(&self, k: &u32) -> Result<Option<NoClone>, Self::Error> {
            Ok(self.inner.lock().remove(k))
        }
        fn cache_set(&self, k: u32, v: NoClone) -> Result<Option<NoClone>, Self::Error> {
            Ok(self.inner.lock().insert(k, v))
        }
        fn cache_remove(&self, k: &u32) -> Result<Option<NoClone>, Self::Error> {
            Ok(self.inner.lock().remove(k))
        }
        fn cache_remove_entry(&self, k: &u32) -> Result<Option<(u32, NoClone)>, Self::Error> {
            Ok(self.inner.lock().remove_entry(k))
        }
        fn cache_delete(&self, k: &u32) -> Result<bool, Self::Error> {
            Ok(self.inner.lock().remove(k).is_some())
        }
        fn cache_contains(&self, k: &u32) -> Result<bool, Self::Error>
        where
            Self: Sized,
        {
            Ok(self.inner.lock().contains_key(k))
        }
        fn cache_clear(&self) -> Result<(), Self::Error> {
            self.inner.lock().clear();
            Ok(())
        }
        fn cache_reset(&self) -> Result<(), Self::Error> {
            self.cache_clear()
        }
    }

    impl ConcurrentCachedAsync<u32, NoClone> for NoCloneAsyncStore {
        fn async_cache_get(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<NoClone>, Self::Error>> + Send
        {
            let v = self.inner.lock().remove(k);
            async move { Ok(v) }
        }

        fn async_cache_set(
            &self,
            k: u32,
            v: NoClone,
        ) -> impl std::future::Future<Output = Result<Option<NoClone>, Self::Error>> + Send
        {
            let prev = self.inner.lock().insert(k, v);
            async move { Ok(prev) }
        }

        fn async_cache_remove(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<NoClone>, Self::Error>> + Send
        {
            let v = self.inner.lock().remove(k);
            async move { Ok(v) }
        }

        fn async_cache_remove_entry(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<(u32, NoClone)>, Self::Error>> + Send
        {
            let v = self.inner.lock().remove_entry(k);
            async move { Ok(v) }
        }

        fn async_cache_contains(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send
        where
            Self: Sized + Sync,
            u32: Sync,
        {
            let present = self.inner.lock().contains_key(k);
            async move { Ok(present) }
        }

        fn async_cache_clear(
            &self,
        ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send
        where
            Self: Sync,
        {
            self.inner.lock().clear();
            async move { Ok(()) }
        }

        fn async_cache_reset(
            &self,
        ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send
        where
            Self: Sync,
        {
            self.inner.lock().clear();
            async move { Ok(()) }
        }
    }

    #[tokio::test]
    async fn async_cache_contains_true_after_set_no_clone_bound() {
        let store = NoCloneAsyncStore::new();

        // Absent initially.
        assert!(!store.async_cache_contains(&10u32).await.unwrap());

        store.async_cache_set(10, NoClone(42)).await.unwrap();

        // Present after set -- no V: Clone needed.
        assert!(store.async_cache_contains(&10u32).await.unwrap());
    }

    #[tokio::test]
    async fn async_cache_contains_false_after_removal() {
        let store = NoCloneAsyncStore::new();
        store.async_cache_set(7, NoClone(1)).await.unwrap();
        assert!(store.async_cache_contains(&7u32).await.unwrap());

        store.async_cache_remove(&7).await.unwrap();
        assert!(!store.async_cache_contains(&7u32).await.unwrap());
    }
}

// ── 3. Inherent sharded `contains` returns plain bool ────────────────────────

/// The inherent `ShardedLruCache::contains` takes priority over the
/// `ConcurrentCachedExt::contains` (which returns `Result<bool, _>`).
/// The type annotation on `b` is the assertion: if the wrong method resolved,
/// the annotation would be `Result<bool, Infallible>` and this would fail to compile.
#[test]
fn sharded_lru_inherent_contains_returns_bool() {
    let cache: ShardedLruCache<u32, String> = ShardedLruCache::builder()
        .max_size(16)
        .build()
        .expect("build ShardedLruCache");

    cache.cache_set(1, "a".to_string()).unwrap();

    // Type annotation asserts the inherent method (-> bool) resolved, not the ext trait (-> Result).
    let b: bool = cache.contains(&1);
    assert!(b, "contains must return true after cache_set");

    let absent: bool = cache.contains(&999);
    assert!(!absent, "contains must return false for an absent key");

    // Inherent method agrees with trait method.
    assert_eq!(
        cache.contains(&1),
        cache.cache_contains(&1).unwrap(),
        "inherent contains must agree with trait cache_contains"
    );
}

/// Same check for `ShardedUnboundCache`.
#[test]
fn sharded_unbound_inherent_contains_returns_bool() {
    let cache: ShardedUnboundCache<u32, String> =
        ShardedUnboundCache::builder().build().expect("build");

    cache.cache_set(42, "x".to_string()).unwrap();

    let b: bool = cache.contains(&42);
    assert!(b);

    let absent: bool = cache.contains(&0);
    assert!(!absent);

    assert_eq!(cache.contains(&42), cache.cache_contains(&42).unwrap());
}

/// `ShardedTtlCache::contains` returns `bool`, respects TTL expiry,
/// and agrees with `cache_contains`.
#[cfg(feature = "time_stores")]
mod ttl_contains {
    use cached::{ConcurrentCached, ShardedTtlCache};
    use std::time::Duration;

    #[test]
    fn sharded_ttl_inherent_contains_returns_bool_and_expires() {
        let cache: ShardedTtlCache<u32, String> = ShardedTtlCache::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .expect("build ShardedTtlCache");

        cache.cache_set(1, "hello".to_string()).unwrap();

        // Immediately after insertion the key must be present.
        let before: bool = cache.contains(&1);
        assert!(before, "contains must return true before expiry");

        // Agree with trait method before expiry.
        assert_eq!(
            cache.contains(&1),
            cache.cache_contains(&1).unwrap(),
            "inherent and trait contains must agree before expiry"
        );

        // Sleep past the TTL with a comfortable margin.
        std::thread::sleep(Duration::from_millis(150));

        // Entry must be expired now.
        let after: bool = cache.contains(&1);
        assert!(!after, "contains must return false after TTL expiry");

        // Agree with trait method after expiry.
        assert_eq!(
            cache.contains(&1),
            cache.cache_contains(&1).unwrap(),
            "inherent and trait contains must agree after expiry"
        );
    }
}

// ── 3b. Remaining sharded stores: inherent contains ──────────────────────────

/// `ShardedLruTtlCache::contains` returns plain `bool`, agrees with
/// `cache_contains`, and returns false for expired entries.
#[cfg(feature = "time_stores")]
mod lru_ttl_contains {
    use cached::{ConcurrentCached, ShardedLruTtlCache};
    use std::time::Duration;

    #[test]
    fn sharded_lru_ttl_contains_true_and_false() {
        let cache: ShardedLruTtlCache<u32, String> = ShardedLruTtlCache::builder()
            .max_size(16)
            .ttl(Duration::from_millis(50))
            .build()
            .expect("build ShardedLruTtlCache");

        // Absent before insertion.
        let absent: bool = cache.contains(&1);
        assert!(!absent, "contains must be false before any insertion");

        cache.cache_set(1, "a".to_string()).unwrap();

        // Present immediately after insertion.
        let b: bool = cache.contains(&1);
        assert!(b, "contains must return true after cache_set");

        // Absent key.
        let b2: bool = cache.contains(&999);
        assert!(!b2, "contains must return false for absent key");

        // Agrees with trait method.
        assert_eq!(
            cache.contains(&1),
            cache.cache_contains(&1).unwrap(),
            "inherent contains must agree with trait cache_contains"
        );

        // Wait past TTL.
        std::thread::sleep(Duration::from_millis(150));

        let after: bool = cache.contains(&1);
        assert!(!after, "contains must return false after TTL expiry");

        // Agrees with trait method after expiry.
        assert_eq!(
            cache.contains(&1),
            cache.cache_contains(&1).unwrap(),
            "inherent and trait contains must agree after expiry"
        );
    }
}

/// `ShardedExpiringCache::contains` returns plain `bool`, is false for expired
/// entries (by value expiry), and agrees with `cache_contains`.
mod expiring_cache_contains {
    use cached::time::Instant;
    use cached::{ConcurrentCached, Expires, ShardedExpiringCache};

    #[derive(Clone)]
    struct MayExpire {
        expired: bool,
    }

    impl Expires for MayExpire {
        fn is_expired(&self) -> bool {
            self.expired
        }
        fn expires_at(&self) -> Option<Instant> {
            None
        }
    }

    #[test]
    fn sharded_expiring_cache_contains_live_and_expired() {
        let cache: ShardedExpiringCache<u32, MayExpire> = ShardedExpiringCache::new();

        // Absent before insertion.
        let absent: bool = cache.contains(&1);
        assert!(!absent, "contains must be false before insertion");

        // Live entry.
        cache.cache_set(1, MayExpire { expired: false }).unwrap();
        let b: bool = cache.contains(&1);
        assert!(b, "contains must return true for a live entry");

        // Agrees with trait method.
        assert_eq!(
            cache.contains(&1),
            cache.cache_contains(&1).unwrap(),
            "inherent and trait contains must agree"
        );

        // Expired entry.
        cache.cache_set(2, MayExpire { expired: true }).unwrap();
        let expired: bool = cache.contains(&2);
        assert!(!expired, "contains must return false for an expired entry");

        assert_eq!(
            cache.contains(&2),
            cache.cache_contains(&2).unwrap(),
            "inherent and trait contains must agree for expired entry"
        );
    }
}

/// `ShardedExpiringLruCache::contains` returns plain `bool`, is false for
/// expired entries, and agrees with `cache_contains`.
mod expiring_lru_cache_contains {
    use cached::time::Instant;
    use cached::{ConcurrentCached, Expires, ShardedExpiringLruCache};

    #[derive(Clone)]
    struct MayExpire {
        expired: bool,
    }

    impl Expires for MayExpire {
        fn is_expired(&self) -> bool {
            self.expired
        }
        fn expires_at(&self) -> Option<Instant> {
            None
        }
    }

    #[test]
    fn sharded_expiring_lru_cache_contains_live_and_expired() {
        let cache: ShardedExpiringLruCache<u32, MayExpire> = ShardedExpiringLruCache::builder()
            .max_size(16)
            .build()
            .expect("build ShardedExpiringLruCache");

        // Absent before insertion.
        let absent: bool = cache.contains(&1);
        assert!(!absent, "contains must be false before insertion");

        // Live entry.
        cache.cache_set(1, MayExpire { expired: false }).unwrap();
        let b: bool = cache.contains(&1);
        assert!(b, "contains must return true for a live entry");

        // Agrees with trait method.
        assert_eq!(
            cache.contains(&1),
            cache.cache_contains(&1).unwrap(),
            "inherent and trait contains must agree"
        );

        // Expired entry.
        cache.cache_set(2, MayExpire { expired: true }).unwrap();
        let expired: bool = cache.contains(&2);
        assert!(!expired, "contains must return false for an expired entry");

        assert_eq!(
            cache.contains(&2),
            cache.cache_contains(&2).unwrap(),
            "inherent and trait contains must agree for expired entry"
        );
    }
}

// ── 3c. ConcurrentCachedExt::contains via fully-qualified syntax ─────────────

/// Prove that `ConcurrentCachedExt::contains` still exists and compiles on a
/// sharded store when called via fully-qualified syntax.  The FQS path returns
/// `Result<bool, Infallible>`, not `bool`, which is distinct from the inherent
/// `contains` that returns plain `bool`.  This also proves the ext-trait alias
/// has no `V: Clone` bound (ShardedUnboundCache<u32, String> would compile
/// anyway, but the assertion is on the return type).
#[test]
fn ext_trait_contains_via_fqs_returns_result_bool() {
    use cached::ConcurrentCachedExt;

    let cache: ShardedUnboundCache<u32, String> =
        ShardedUnboundCache::builder().build().expect("build");

    cache.cache_set(7, "hello".to_string()).unwrap();

    // Fully-qualified call goes to the ext-trait impl, not the inherent method.
    let result: Result<bool, _> = ConcurrentCachedExt::contains(&cache, &7u32);
    assert!(
        result.unwrap(),
        "ext-trait contains must return true for present key"
    );

    let result_absent: Result<bool, _> = ConcurrentCachedExt::contains(&cache, &99u32);
    assert!(
        !result_absent.unwrap(),
        "ext-trait contains must return false for absent key"
    );
}

// ── 3d. Inherent contains does not promote LRU recency on ShardedLruCache ────

/// With a single shard and per_shard_max_size=2, inserting a third entry evicts
/// the LRU entry.  If `contains` promoted recency, the order would shift and a
/// different entry would survive.  We verify that calling `contains` on key 1
/// (the insertion-order LRU) before overflowing does NOT change which key is
/// evicted: key 1 should still be the LRU victim.
///
/// Setup (single shard, cap=2):
///   set(1), set(2)   → LRU order: 1 < 2
///   contains(1)      → must NOT move 1 to MRU
///   set(3)           → if no promotion: evicts 1; if promotion: evicts 2
///   assert 1 absent, 2 present, 3 present
#[test]
fn sharded_lru_contains_does_not_promote_recency() {
    let cache: ShardedLruCache<u32, String> = ShardedLruCache::builder()
        .shards(1)
        .per_shard_max_size(2)
        .build()
        .expect("build ShardedLruCache with single shard cap=2");

    cache.cache_set(1, "one".to_string()).unwrap();
    cache.cache_set(2, "two".to_string()).unwrap();

    // Call contains on key 1 (currently LRU). Must NOT promote it.
    let _: bool = cache.contains(&1);

    // Insert key 3: should evict key 1 (still LRU because contains did not promote).
    cache.cache_set(3, "three".to_string()).unwrap();

    assert!(
        !cache.contains(&1),
        "key 1 must have been evicted -- contains must not promote LRU recency"
    );
    assert!(cache.contains(&2), "key 2 must still be present");
    assert!(cache.contains(&3), "key 3 must be present");
}

// ── 4. Peek semantics: contains does not change metrics ──────────────────────

/// After calling `contains` on a key that is present, `metrics().hits` and
/// `metrics().misses` must be unchanged -- `contains` is peek-based and must not
/// record a hit or miss.
#[test]
fn sharded_lru_contains_does_not_affect_hit_miss_metrics() {
    let cache: ShardedLruCache<u32, String> = ShardedLruCache::builder()
        .max_size(16)
        .build()
        .expect("build ShardedLruCache");

    cache.cache_set(1, "a".to_string()).unwrap();

    let before = cache.metrics();

    // Call contains on a present key and on an absent key.
    let _present: bool = cache.contains(&1);
    let _absent: bool = cache.contains(&999);

    let after = cache.metrics();

    assert_eq!(after.hits, before.hits, "contains must not increment hits");
    assert_eq!(
        after.misses, before.misses,
        "contains must not increment misses"
    );
}
