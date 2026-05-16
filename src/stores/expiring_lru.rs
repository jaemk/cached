use super::{CacheEvict, Cached, LruCache};
use crate::{CachedIter, CachedPeek, CloneCached};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

/// Implemented by values stored in [`ExpiringLruCache`] so the value itself
/// decides when it is stale (renamed from `CanExpire` in 1.0). Expired values
/// are not returned by lookups and are removed on access:
///
/// ```rust
/// use cached::{Cached, Expires, ExpiringLruCache};
///
/// struct Token {
///     #[allow(dead_code)]
///     value: String,
///     expired: bool,
/// }
/// impl Expires for Token {
///     fn is_expired(&self) -> bool {
///         self.expired
///     }
/// }
///
/// let mut cache: ExpiringLruCache<u32, Token> = ExpiringLruCache::with_size(8);
/// cache.cache_set(1, Token { value: "live".into(), expired: false });
/// assert!(cache.cache_get(&1).is_some());
/// cache.cache_set(2, Token { value: "stale".into(), expired: true });
/// assert!(cache.cache_get(&2).is_none()); // expired -> not returned
/// ```
pub trait Expires {
    /// `is_expired` returns whether the value has expired.
    fn is_expired(&self) -> bool;
}

/// Expiring Value Cache
///
/// Stores values that implement the `Expires` trait so that expiration
/// is determined by the values themselves. This is useful for caching
/// values which themselves contain an expiry timestamp.
///
/// Note: This cache is in-memory only.
///
/// Note: once specialization is stable (`#[feature(specialization)]`), the expiry-checking
/// behavior here could be folded into [`LruCache`] via a specialized `Cached<K, V>` impl
/// for `V: Expires`, eliminating this separate type. Until then, the two must remain
/// distinct because overlapping blanket impls are not allowed on stable Rust.
pub struct ExpiringLruCache<K: Hash + Eq, V: Expires> {
    pub(super) store: LruCache<K, V>,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K: Hash + Eq, V: Expires> std::fmt::Debug for ExpiringLruCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpiringLruCache")
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V> Clone for ExpiringLruCache<K, V>
where
    K: Clone + Hash + Eq,
    V: Expires + Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Builder for [`ExpiringLruCache`].
pub struct ExpiringLruCacheBuilder<K, V: Expires> {
    size: Option<usize>,
    on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V: Expires> ExpiringLruCacheBuilder<K, V> {
    /// Set the maximum number of entries.
    #[must_use]
    pub fn size(mut self, size: usize) -> Self {
        self.size = Some(size);
        self
    }

    /// Set a callback to be invoked when an entry is evicted.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Build the cache.
    ///
    /// # Panics
    ///
    /// Panics if `size` was not set or is `0`.
    #[must_use]
    pub fn build(self) -> ExpiringLruCache<K, V>
    where
        K: Hash + Eq + Clone,
    {
        let size = self
            .size
            .expect("`ExpiringLruCacheBuilder` requires `size` to be set");
        let mut cache = ExpiringLruCache::with_size(size);
        // Two separate callbacks for two separate eviction causes:
        //   cache.on_evict    — fires when ExpiringLruCache itself removes an expired entry
        //   cache.store.on_evict — fires when LruCache::check_capacity evicts for capacity
        // Both must be registered independently so neither path is silently skipped.
        cache.on_evict = self.on_evict.clone();
        if let Some(on_evict) = self.on_evict {
            cache.store.on_evict = Some(on_evict);
        }
        cache
    }

    /// Build the cache, returning an error instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `size` was not set or is `0`.
    pub fn try_build(self) -> Result<ExpiringLruCache<K, V>, super::BuildError>
    where
        K: Hash + Eq + Clone,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("size"))?;
        let mut cache = ExpiringLruCache {
            store: LruCache::try_with_size(size)?,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            on_evict: self.on_evict.clone(),
        };
        if let Some(on_evict) = self.on_evict {
            cache.store.on_evict = Some(on_evict);
        }
        Ok(cache)
    }
}

impl<K: Clone + Hash + Eq, V: Expires> ExpiringLruCache<K, V> {
    /// Return a builder for constructing an [`ExpiringLruCache`].
    #[must_use]
    pub fn builder() -> ExpiringLruCacheBuilder<K, V> {
        ExpiringLruCacheBuilder {
            size: None,
            on_evict: None,
        }
    }

    /// Creates a new `ExpiringLruCache` with a given size limit and
    /// pre-allocated backing data.
    #[must_use]
    pub fn with_size(size: usize) -> ExpiringLruCache<K, V> {
        ExpiringLruCache {
            store: LruCache::with_size(size),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            on_evict: None,
        }
    }

    /// Returns a reference to the inner [`LruCache`].
    #[must_use]
    pub fn store(&self) -> &LruCache<K, V> {
        &self.store
    }

    /// Evict expired values from the cache.
    pub fn evict(&mut self) -> usize {
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        let mut removed = 0;
        self.store.retain(|key, value| {
            if value.is_expired() {
                if let Some(on_evict) = on_evict {
                    on_evict(key, value);
                }
                evictions.fetch_add(1, Ordering::Relaxed);
                removed += 1;
                false
            } else {
                true
            }
        });
        removed
    }
}

// https://docs.rs/cached/latest/cached/trait.Cached.html
impl<K: Hash + Eq + Clone, V: Expires> Cached<K, V> for ExpiringLruCache<K, V> {
    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(k);
        if let Some(index) = self.store.get_index(hash, k) {
            let value = &self.store.order.get(index).1;
            if !value.is_expired() {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(&self.store.order.get(index).1)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.cache_remove_entry(k) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    fn cache_get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(key);
        if let Some(index) = self.store.get_index(hash, key) {
            let value = &self.store.order.get(index).1;
            if !value.is_expired() {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(&mut self.store.order.get_mut(index).1)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((k, old)) = self.store.cache_remove_entry(key) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V {
        let key_for_evict = k.clone();
        // get_or_set_with_if will set the value in the cache if an existing
        // value is not valid, which, in our case, is if the value has expired.
        let (was_present, was_valid, old_val, v) =
            self.store.get_or_set_with_if(k, f, |v| !v.is_expired());
        if was_present && was_valid {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            if let Some(old) = old_val {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        v
    }
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let key_for_evict = key.clone();
        let (was_present, was_valid, old_val, v) =
            self.store
                .try_get_or_set_with_if(key, f, |v| !v.is_expired())?;
        if was_present && was_valid {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            if let Some(old) = old_val {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        Ok(v)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        self.store.set(k, v)
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.remove(k)
    }
    fn cache_clear(&mut self) {
        self.store.clear();
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        let on_evict = self.store.on_evict.clone();
        let capacity = self.store.capacity;
        self.store = LruCache::with_size(capacity);
        self.store.on_evict = on_evict;
        self.cache_reset_metrics();
    }
    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }
    fn cache_capacity(&self) -> Option<usize> {
        // Bounded by the inner `LruCache`; report it like the other bounded
        // stores so `metrics().capacity` is accurate.
        self.store.cache_capacity()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load(Ordering::Relaxed))
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load(Ordering::Relaxed))
    }
    fn cache_evictions(&self) -> Option<u64> {
        Some(self.evictions.load(Ordering::Relaxed) + self.store.cache_evictions().unwrap_or(0))
    }
    fn cache_reset_metrics(&mut self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.store.cache_reset_metrics();
    }
}

impl<K: Hash + Eq + Clone, V: Expires> CachedIter<K, V> for ExpiringLruCache<K, V> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.store
            .iter()
            .filter_map(|(k, v)| if v.is_expired() { None } else { Some((k, v)) })
    }
}

impl<K: Hash + Eq + Clone, V: Expires> CachedPeek<K, V> for ExpiringLruCache<K, V> {
    fn cache_peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.cache_peek(key).and_then(|value| {
            if value.is_expired() {
                None
            } else {
                Some(value)
            }
        })
    }
}

#[cfg(feature = "async_core")]
impl<K, V> CachedAsync<K, V> for ExpiringLruCache<K, V>
where
    K: Hash + Eq + Clone + Send,
    V: Expires + Send,
{
    fn async_get_or_set_with<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move {
            let key_for_evict = k.clone();
            let (was_present, was_valid, old_val, v) = self
                .store
                .get_or_set_with_if_async(k, f, |v| !v.is_expired())
                .await;
            if was_present && was_valid {
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Some(old) = old_val {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key_for_evict, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            v
        }
    }

    fn async_try_get_or_set_with<'a, F, Fut, E>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = Result<&'a mut V, E>> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        E: 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = Result<V, E>> + Send + 'a,
    {
        async move {
            let key_for_evict = k.clone();
            let (was_present, was_valid, old_val, v) = self
                .store
                .try_get_or_set_with_if_async(k, f, |v| !v.is_expired())
                .await?;
            if was_present && was_valid {
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Some(old) = old_val {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key_for_evict, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            Ok(v)
        }
    }
}

impl<K: Hash + Eq + Clone, V: Expires + Clone> CloneCached<K, V> for ExpiringLruCache<K, V> {
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(k);
        if let Some(index) = self.store.get_index(hash, k) {
            let value = &self.store.order.get(index).1;
            let expired = value.is_expired();
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                // Don't move to front — expired entries must not be promoted.
                // Return the stale value so callers using `result_fallback` can
                // use it during revalidation.
                (Some(self.store.order.get(index).1.clone()), true)
            } else {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                (Some(self.store.order.get(index).1.clone()), false)
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            (None, false)
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V: Expires> CacheEvict for ExpiringLruCache<K, V> {
    fn evict(&mut self) -> usize {
        ExpiringLruCache::evict(self)
    }
}

#[cfg(test)]
/// Expiring Value Cache tests
mod tests {
    use super::*;

    type ExpiredU8 = u8;

    impl Expires for ExpiredU8 {
        fn is_expired(&self) -> bool {
            *self > 10
        }
    }

    #[test]
    fn expiring_value_cache_get_miss() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        // Getting a non-existent cache key.
        assert!(c.get(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_reports_capacity() {
        // Regression: `ExpiringLruCache` is size-bounded, so it must report a
        // capacity like the other bounded stores (was falling through to the
        // `Cached` default `None`, making `metrics().capacity` inaccurate).
        let c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(7);
        assert_eq!(c.cache_capacity(), Some(7));
        assert_eq!(c.metrics().capacity, Some(7));
    }

    #[test]
    fn expiring_value_cache_get_hit() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        // Getting a cached value.
        assert!(c.set(1, 2).is_none());
        assert_eq!(c.get(&1), Some(&2));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_value_cache_get_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        assert!(c.set(2, 12).is_none());

        assert!(c.get(&2).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_mut_miss() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        // Getting a non-existent cache key.
        assert!(c.cache_get_mut(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_mut_hit() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        // Getting a cached value.
        assert!(c.set(1, 2).is_none());
        assert_eq!(c.cache_get_mut(&1), Some(&mut 2));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_value_cache_get_mut_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        assert!(c.set(2, 12).is_none());

        assert!(c.get(&2).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_or_set_with_missing() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_get_or_set_with_present() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);
        assert!(c.set(1, 5).is_none());

        // Existing value is returned rather than setting new value.
        assert_eq!(c.cache_get_or_set_with(1, || 1), &5);
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_value_cache_get_or_set_with_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);
        assert!(c.set(1, 11).is_none());

        // New value is returned as existing had expired.
        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_value_cache_try_get_or_set_with_missing() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        assert_eq!(
            c.cache_try_get_or_set_with(1, || Ok::<_, ()>(1)),
            Ok(&mut 1)
        );
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));

        assert_eq!(c.cache_try_get_or_set_with(1, || Err(())), Ok(&mut 1));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(1));

        assert_eq!(
            c.cache_try_get_or_set_with(2, || Ok::<_, ()>(2)),
            Ok(&mut 2)
        );
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(2));
    }

    #[test]
    fn evict_expired() {
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(3);

        assert_eq!(c.set(1, 100), None);
        assert_eq!(c.set(1, 200), Some(100));
        assert_eq!(c.set(2, 1), None);
        assert_eq!(c.cache_size(), 2);

        // It should only evict n > 10
        assert_eq!(2, c.cache_size());
        c.evict();
        assert_eq!(1, c.cache_size());
    }

    #[test]
    fn reset_rebuilds_store_and_preserves_on_evict() {
        let evicted = Arc::new(AtomicU64::new(0));
        let evicted_for_callback = evicted.clone();
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .size(1)
            .on_evict(move |_key: &u8, _value: &ExpiredU8| {
                evicted_for_callback.fetch_add(1, Ordering::Relaxed);
            })
            .build();

        c.set(1, 1);
        c.cache_reset();
        assert_eq!(0, c.cache_size());

        // Inserting two values into a capacity-1 cache should evict exactly one.
        c.set(2, 2);
        c.set(3, 3);
        assert_eq!(1, evicted.load(Ordering::Relaxed));

        // Insert a third value — eviction count should now be exactly 2, not more.
        c.set(4, 4);
        assert_eq!(2, evicted.load(Ordering::Relaxed));
    }

    #[test]
    fn cache_get_with_expiry_status_does_not_promote_expired_entry() {
        // Build a capacity-2 cache. Insert A then B, making B the MRU entry.
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::with_size(2);
        c.set(1, 100); // A — value 100 > 10, so it is expired
        c.set(2, 100); // B — also expired

        // Calling cache_get_with_expiry_status on A must NOT promote A to MRU.
        let (val, expired) = c.cache_get_with_expiry_status(&1u8);
        assert!(val.is_some(), "expired entry should still be returned");
        assert!(expired, "entry should be flagged as expired");

        // Now insert a third key C to force a capacity eviction.
        // If A was wrongly promoted it would be MRU and B would be evicted instead.
        // Correct behaviour: B is still MRU → A (LRU) is evicted first.
        c.set(3, 1); // C — value 1 <= 10, live
        assert_eq!(c.cache_size(), 2);
        // A should have been evicted (LRU), B and C should still be present.
        assert!(
            c.get(&1u8).is_none(),
            "key 1 (A) should have been evicted as LRU"
        );
        assert!(
            c.get(&2u8).is_none(),
            "key 2 (B) is expired — none after get"
        );
        assert!(c.get(&3u8).is_some(), "key 3 (C) should be live");
    }

    #[test]
    fn cache_reset_does_not_fire_on_evict() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let evict_count = Arc::new(AtomicUsize::new(0));
        let evict_count2 = evict_count.clone();
        let mut c: ExpiringLruCache<u8, ExpiredU8> = ExpiringLruCache::builder()
            .size(4)
            .on_evict(move |_k, _v| {
                evict_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build();
        c.cache_set(1, 5);
        c.cache_set(2, 5);
        c.cache_set(3, 5);
        c.cache_reset();
        assert_eq!(
            evict_count.load(Ordering::Relaxed),
            0,
            "cache_reset must not fire on_evict"
        );
        assert_eq!(c.cache_size(), 0);
    }
}
