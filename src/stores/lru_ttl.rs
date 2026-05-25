use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

use crate::{CachedIter, CachedPeek, CloneCached};

use super::{CacheEvict, Cached, LruCache, TimedEntry};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Timed LRU Cache
///
/// Stores a limited number of values,
/// evicting expired and least-used entries.
/// Time expiration is determined based on entry insertion time.
/// By default, the TTL of an entry is not refreshed on retrieval.
/// Set `refresh = true` to refresh the TTL on cache hits.
///
/// Note: This cache is in-memory only
pub struct LruTtlCache<K, V> {
    pub(super) store: LruCache<K, TimedEntry<V>>,
    pub(super) size: usize,
    pub(super) ttl: Duration,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) refresh: bool,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> std::fmt::Debug for LruTtlCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LruTtlCache")
            .field("size", &self.size)
            .field("ttl", &self.ttl)
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("refresh", &self.refresh)
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V> Clone for LruTtlCache<K, V>
where
    K: Clone + Hash + Eq,
    V: Clone,
{
    fn clone(&self) -> Self {
        let store = self.store.clone();
        Self {
            store,
            size: self.size,
            ttl: self.ttl,
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            refresh: self.refresh,
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Typestate marker for [`LruTtlCacheBuilder`]: no eviction callback set.
pub struct NoEvict;

/// Typestate marker for [`LruTtlCacheBuilder`]: eviction callback has been set.
///
/// When this marker is active, [`LruTtlCacheBuilder::build`] and
/// [`LruTtlCacheBuilder::try_build`] require `K: 'static` and `V: 'static`
/// because the callback must be wired into the inner LRU store.
pub struct HasEvict;

/// Builder for [`LruTtlCache`].
///
/// Obtain one via [`LruTtlCache::builder`].
///
/// The `E` type parameter is a compile-time marker:
/// - [`NoEvict`] (the default): no eviction callback has been set; `build` /
///   `try_build` do **not** require `K: 'static` or `V: 'static`.
/// - [`HasEvict`]: an eviction callback was registered via [`on_evict`](LruTtlCacheBuilder::on_evict);
///   `build` / `try_build` require `K: 'static + V: 'static` so the callback
///   can be wired into the inner LRU eviction path.
pub struct LruTtlCacheBuilder<K, V, E = NoEvict> {
    size: Option<usize>,
    ttl: Option<Duration>,
    refresh: bool,
    on_evict: Option<super::OnEvict<K, V>>,
    _evict: PhantomData<E>,
}

// size / ttl / refresh work regardless of eviction state
impl<K, V, E> LruTtlCacheBuilder<K, V, E> {
    /// Set the maximum number of entries. Required.
    #[doc(alias = "max_size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn size(mut self, size: usize) -> Self {
        self.size = Some(size);
        self
    }

    /// Set the TTL for cache entries. Required.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set whether cache hits refresh the TTL of the accessed entry.
    #[must_use]
    pub fn refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }
}

// on_evict transitions the builder from NoEvict → HasEvict
impl<K, V> LruTtlCacheBuilder<K, V, NoEvict> {
    /// Set a callback to be invoked when an entry is evicted.
    ///
    /// Calling this method changes the builder's type to
    /// `LruTtlCacheBuilder<K, V, `[`HasEvict`]`>`, which requires `K: 'static`
    /// and `V: 'static` at [`build`](LruTtlCacheBuilder::build) time so the
    /// callback can be wired into the inner LRU eviction path.
    #[must_use]
    pub fn on_evict(
        self,
        on_evict: impl Fn(&K, &V) + Send + Sync + 'static,
    ) -> LruTtlCacheBuilder<K, V, HasEvict> {
        LruTtlCacheBuilder {
            size: self.size,
            ttl: self.ttl,
            refresh: self.refresh,
            on_evict: Some(Arc::new(on_evict)),
            _evict: PhantomData,
        }
    }
}

// build / try_build without an eviction callback — no 'static required
impl<K, V> LruTtlCacheBuilder<K, V, NoEvict> {
    /// Build the cache.
    ///
    /// # Panics
    ///
    /// Panics if `size` or `ttl` was not set, or if `size` is `0`.
    #[must_use]
    pub fn build(self) -> LruTtlCache<K, V>
    where
        K: Hash + Eq + Clone,
    {
        let size = self
            .size
            .expect("`LruTtlCacheBuilder` requires `size` to be set");
        let ttl = self
            .ttl
            .expect("`LruTtlCacheBuilder` requires `ttl` to be set");
        LruTtlCache::with_size_and_ttl_and_refresh(size, ttl, self.refresh)
    }

    /// Build the cache, returning an error instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `size` or `ttl` was not set, or `size` is `0`.
    pub fn try_build(self) -> Result<LruTtlCache<K, V>, super::BuildError>
    where
        K: Hash + Eq + Clone,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("size"))?;
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        LruTtlCache::new_internal(size, ttl, self.refresh)
    }
}

// build / try_build with an eviction callback — 'static required for sync_on_evict
impl<K, V> LruTtlCacheBuilder<K, V, HasEvict> {
    /// Build the cache.
    ///
    /// # Panics
    ///
    /// Panics if `size` or `ttl` was not set, or if `size` is `0`.
    #[must_use]
    pub fn build(self) -> LruTtlCache<K, V>
    where
        K: Hash + Eq + Clone + 'static,
        V: 'static,
    {
        let size = self
            .size
            .expect("`LruTtlCacheBuilder` requires `size` to be set");
        let ttl = self
            .ttl
            .expect("`LruTtlCacheBuilder` requires `ttl` to be set");
        let mut cache = LruTtlCache::with_size_and_ttl_and_refresh(size, ttl, self.refresh);
        cache.on_evict = self.on_evict;
        cache.sync_on_evict();
        cache
    }

    /// Build the cache, returning an error instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `size` or `ttl` was not set, or `size` is `0`.
    pub fn try_build(self) -> Result<LruTtlCache<K, V>, super::BuildError>
    where
        K: Hash + Eq + Clone + 'static,
        V: 'static,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("size"))?;
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        let mut cache = LruTtlCache::new_internal(size, ttl, self.refresh)?;
        cache.on_evict = self.on_evict;
        cache.sync_on_evict();
        Ok(cache)
    }
}

impl<K: Hash + Eq + Clone, V> LruTtlCache<K, V> {
    /// Return a builder for constructing a [`LruTtlCache`].
    #[must_use]
    pub fn builder() -> LruTtlCacheBuilder<K, V> {
        LruTtlCacheBuilder {
            size: None,
            ttl: None,
            refresh: false,
            on_evict: None,
            _evict: PhantomData,
        }
    }

    pub(super) fn sync_on_evict(&mut self)
    where
        K: 'static,
        V: 'static,
    {
        if self.on_evict.is_some() {
            let on_evict_ext = self.on_evict.clone();
            self.store.on_evict = Some(Arc::new(move |k, entry| {
                if let Some(on_evict) = &on_evict_ext {
                    on_evict(k, &entry.value);
                }
            }));
        }
    }

    fn new_internal(size: usize, ttl: Duration, refresh: bool) -> Result<Self, super::BuildError> {
        let store = LruCache::try_with_size(size)?;
        Ok(LruTtlCache {
            store,
            size,
            ttl,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            refresh,
            on_evict: None,
        })
    }

    /// Creates a new `LruTtlCache` with a given size limit and TTL
    #[must_use]
    pub fn with_size_and_ttl(size: usize, ttl: Duration) -> LruTtlCache<K, V> {
        Self::with_size_and_ttl_and_refresh(size, ttl, false)
    }

    /// Creates a new `LruTtlCache` with a given size limit, TTL, and refresh-on-hit flag.
    ///
    /// # Panics
    ///
    /// Will panic if size is 0
    #[must_use]
    pub fn with_size_and_ttl_and_refresh(
        size: usize,
        ttl: Duration,
        refresh: bool,
    ) -> LruTtlCache<K, V> {
        Self::new_internal(size, ttl, refresh).unwrap_or_else(|e| panic!("{}", e))
    }

    /// Creates a new `LruTtlCache` with a specified ttl and a given size limit and pre-allocated backing data
    ///
    /// # Errors
    ///
    /// Will return a [`BuildError`](super::BuildError) if size is 0 or memory allocation fails.
    pub fn try_with_size_and_ttl(
        size: usize,
        ttl: Duration,
    ) -> Result<LruTtlCache<K, V>, super::BuildError> {
        Self::new_internal(size, ttl, false)
    }

    pub fn iter_order(&self) -> Vec<(K, (Instant, V))>
    where
        K: Clone,
        V: Clone,
    {
        let max_ttl = self.ttl;
        self.store
            .iter_order()
            .into_iter()
            .filter_map(|(k, entry)| {
                let instant = entry.instant;
                if instant.elapsed() < max_ttl {
                    Some((k.clone(), (instant, entry.value.clone())))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Return an iterator of keys in the current order from most
    /// to least recently used.
    /// Items passed their expiration seconds will be excluded.
    pub fn key_order(&self) -> Vec<K>
    where
        K: Clone,
    {
        let max_ttl = self.ttl;
        self.store
            .order
            .iter()
            .filter_map(|(k, entry)| {
                if entry.instant.elapsed() < max_ttl {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Return an iterator of timestamped values in the current order
    /// from most to least recently used.
    /// Items passed their expiration seconds will be excluded.
    pub fn value_order(&self) -> Vec<(Instant, V)>
    where
        V: Clone,
    {
        let max_ttl = self.ttl;
        self.store
            .order
            .iter()
            .filter_map(|(_k, entry)| {
                let instant = entry.instant;
                if instant.elapsed() < max_ttl {
                    Some((instant, entry.value.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns whether the ttl is refreshed when the value is retrieved.
    #[must_use]
    pub fn refresh_on_hit(&self) -> bool {
        self.refresh
    }

    /// Sets whether the ttl is refreshed when the value is retrieved.
    pub fn set_refresh_on_hit(&mut self, refresh: bool) {
        self.refresh = refresh;
    }

    /// Returns a reference to the cache's `store`
    #[must_use]
    pub fn store(&self) -> &LruCache<K, TimedEntry<V>> {
        &self.store
    }

    /// Evict expired values from the cache.
    pub fn evict(&mut self) -> usize {
        let ttl = self.ttl;
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        let mut removed = 0;
        self.store.retain(|key, entry| {
            if entry.instant.elapsed() < ttl {
                true
            } else {
                if let Some(on_evict) = on_evict {
                    on_evict(key, &entry.value);
                }
                evictions.fetch_add(1, Ordering::Relaxed);
                removed += 1;
                false
            }
        });
        removed
    }

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Entries where `keep` returns `false` (or that are already expired)
    /// are removed. `on_evict` is called and the eviction counter incremented
    /// for each removed entry.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) {
        let ttl = self.ttl;
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        self.store.retain(|key, entry| {
            let expired = entry.instant.elapsed() >= ttl;
            if expired || !keep(key, &entry.value) {
                if let Some(on_evict) = on_evict {
                    on_evict(key, &entry.value);
                }
                evictions.fetch_add(1, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }
}

impl<K: Hash + Eq + Clone, V> Cached<K, V> for LruTtlCache<K, V> {
    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(key);
        if let Some(index) = self.store.get_index(hash, key) {
            let entry = &self.store.order.get(index).1;
            if entry.instant.elapsed() < self.ttl {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    self.store.order.get_mut(index).1.instant = Instant::now();
                }
                Some(&self.store.order.get(index).1.value)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((k, entry)) = self.store.cache_remove_entry(key) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &entry.value);
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

    fn cache_get_mut<Q>(&mut self, key: &Q) -> std::option::Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(key);
        if let Some(index) = self.store.get_index(hash, key) {
            let entry = &self.store.order.get(index).1;
            if entry.instant.elapsed() < self.ttl {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    self.store.order.get_mut(index).1.instant = Instant::now();
                }
                Some(&mut self.store.order.get_mut(index).1.value)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((k, entry)) = self.store.cache_remove_entry(key) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &entry.value);
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

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        let key_for_evict = key.clone();
        let setter = || TimedEntry {
            instant: Instant::now(),
            value: f(),
        };
        let max_ttl = self.ttl;
        let (was_present, was_valid, old_entry, entry) =
            self.store
                .get_or_set_with_if(key, setter, |entry| entry.instant.elapsed() < max_ttl);
        if was_present && was_valid {
            if self.refresh {
                entry.instant = Instant::now();
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            if let Some(old) = old_entry {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old.value);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        &mut entry.value
    }

    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let key_for_evict = key.clone();
        let setter = || {
            Ok(TimedEntry {
                instant: Instant::now(),
                value: f()?,
            })
        };
        let max_ttl = self.ttl;
        let (was_present, was_valid, old_entry, entry) =
            self.store
                .try_get_or_set_with_if(key, setter, |entry| entry.instant.elapsed() < max_ttl)?;
        if was_present && was_valid {
            if self.refresh {
                entry.instant = Instant::now();
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            if let Some(old) = old_entry {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old.value);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        Ok(&mut entry.value)
    }

    /// Insert a key-value pair. Returns the previous value only if it had not yet expired.
    /// Expired previous values are silently discarded.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let entry = TimedEntry {
            instant: Instant::now(),
            value: val,
        };
        let stamped = self.store.set(key, entry);
        stamped.and_then(|entry| {
            if entry.instant.elapsed() < self.ttl {
                Some(entry.value)
            } else {
                None
            }
        })
    }

    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let stamped = self.store.remove(k);
        stamped.and_then(|entry| {
            if entry.instant.elapsed() < self.ttl {
                Some(entry.value)
            } else {
                None
            }
        })
    }
    fn cache_clear(&mut self) {
        self.store.clear();
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        let on_evict = self.store.on_evict.clone();
        self.store = LruCache::with_size(self.size);
        self.store.on_evict = on_evict;
        self.cache_reset_metrics();
    }
    fn cache_reset_metrics(&mut self) {
        self.misses.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.store.cache_reset_metrics();
    }
    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load(Ordering::Relaxed))
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load(Ordering::Relaxed))
    }
    fn cache_evictions(&self) -> Option<u64> {
        // Combined evictions from underlying store and our time-based removals
        Some(self.evictions.load(Ordering::Relaxed) + self.store.cache_evictions().unwrap_or(0))
    }
    fn cache_capacity(&self) -> Option<usize> {
        Some(self.size)
    }
}

impl<K: Hash + Eq + Clone, V> CachedIter<K, V> for LruTtlCache<K, V> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        let max_ttl = self.ttl;
        CachedIter::iter(&self.store).filter_map(move |(k, entry)| {
            if entry.instant.elapsed() < max_ttl {
                Some((k, &entry.value))
            } else {
                None
            }
        })
    }
}

impl<K: Hash + Eq + Clone, V> CachedPeek<K, V> for LruTtlCache<K, V> {
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.cache_peek(k) {
            if entry.instant.elapsed() < self.ttl {
                return Some(&entry.value);
            }
        }
        None
    }
}

impl<K: Hash + Eq + Clone, V> crate::CacheTtl for LruTtlCache<K, V> {
    fn ttl(&self) -> Option<Duration> {
        Some(self.ttl)
    }
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration> {
        let old = self.ttl;
        self.ttl = ttl;
        Some(old)
    }
    fn unset_ttl(&mut self) -> Option<Duration> {
        None
    }
    fn refresh_on_hit(&self) -> bool {
        self.refresh
    }
    fn set_refresh_on_hit(&mut self, refresh: bool) -> bool {
        let old = self.refresh;
        self.refresh = refresh;
        old
    }
}

impl<K: Hash + Eq + Clone, V: Clone> CloneCached<K, V> for LruTtlCache<K, V> {
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(k);
        if let Some(index) = self.store.get_index(hash, k) {
            let entry = &self.store.order.get(index).1;
            let expired = entry.instant.elapsed() >= self.ttl;
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(self.store.order.get(index).1.value.clone()), true)
            } else {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    self.store.order.get_mut(index).1.instant = Instant::now();
                }
                (Some(self.store.order.get(index).1.value.clone()), false)
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            (None, false)
        }
    }
}

#[cfg(feature = "async_core")]
impl<K, V> CachedAsync<K, V> for LruTtlCache<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    fn async_get_or_set_with<'a, F, Fut>(
        &'a mut self,
        key: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move {
            let key_for_evict = key.clone();
            let setter = || async {
                TimedEntry {
                    instant: Instant::now(),
                    value: f().await,
                }
            };
            let max_ttl = self.ttl;
            let (was_present, was_valid, old_entry, entry) = self
                .store
                .get_or_set_with_if_async(key, setter, |entry| entry.instant.elapsed() < max_ttl)
                .await;
            if was_present && was_valid {
                if self.refresh {
                    entry.instant = Instant::now();
                }
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Some(old) = old_entry {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key_for_evict, &old.value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            &mut entry.value
        }
    }

    fn async_try_get_or_set_with<'a, F, Fut, E>(
        &'a mut self,
        key: K,
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
            let key_for_evict = key.clone();
            let setter = || async {
                let new_val = f().await?;
                Ok(TimedEntry {
                    instant: Instant::now(),
                    value: new_val,
                })
            };
            let max_ttl = self.ttl;
            let (was_present, was_valid, old_entry, entry) = self
                .store
                .try_get_or_set_with_if_async(key, setter, |entry| {
                    entry.instant.elapsed() < max_ttl
                })
                .await?;
            if was_present && was_valid {
                if self.refresh {
                    entry.instant = Instant::now();
                }
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Some(old) = old_entry {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key_for_evict, &old.value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            Ok(&mut entry.value)
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V> CacheEvict for LruTtlCache<K, V> {
    fn evict(&mut self) -> usize {
        LruTtlCache::evict(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cached;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[test]
    fn status_does_not_inflate_inner_store_hits() {
        let mut cache = LruTtlCache::with_size_and_ttl(4, Duration::from_secs(60));
        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        cache.store.cache_reset_metrics();

        // cache_get calls status() internally
        assert_eq!(cache.cache_get(&1), Some(&10));
        assert_eq!(
            cache.store.cache_hits(),
            Some(0),
            "inner LruCache must not record hits from status() promotion"
        );
        assert_eq!(
            cache.store.cache_misses(),
            Some(0),
            "inner LruCache must not record misses from status() promotion"
        );
    }

    #[test]
    fn reset_rebuilds_store_and_preserves_on_evict() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_for_callback = evicted.clone();
        let mut cache = LruTtlCache::builder()
            .size(1)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_key: &u8, _value: &u8| {
                evicted_for_callback.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build();

        cache.set(1, 10);
        cache.cache_reset();
        assert_eq!(cache.cache_size(), 0);

        cache.set(2, 20);
        cache.set(3, 30);
        assert_eq!(evicted.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn try_new() {
        let c = LruTtlCache::<i32, i32>::try_with_size_and_ttl(0, Duration::from_secs(1));
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue { field: "size", .. }
        ));

        let c = LruTtlCache::<i32, i32>::try_with_size_and_ttl(usize::MAX, Duration::from_secs(1));
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue { field: "size", .. }
        ));
    }

    #[test]
    fn cache_reset_does_not_fire_on_evict() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let evict_count = Arc::new(AtomicUsize::new(0));
        let evict_count2 = evict_count.clone();
        let mut c = LruTtlCache::builder()
            .size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k, _v| {
                evict_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        c.cache_reset();
        assert_eq!(
            evict_count.load(Ordering::Relaxed),
            0,
            "cache_reset must not fire on_evict"
        );
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn builder_does_not_require_static_without_on_evict() {
        // LruTtlCacheBuilder::build / try_build must not impose K: 'static or V: 'static
        // when no on_evict callback is configured.
        fn build_with_borrowed<'a>(_k: &'a str, _v: &'a str) -> LruTtlCache<&'a str, &'a str> {
            LruTtlCache::builder()
                .size(4)
                .ttl(Duration::from_secs(60))
                .build()
        }
        let mut cache = build_with_borrowed("key", "val");
        cache.cache_set("key", "val");
        assert_eq!(cache.cache_get(&"key"), Some(&"val"));
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_async_trait() {
        use crate::CachedAsync;
        let mut c = LruTtlCache::with_size_and_ttl(4, Duration::from_secs(60));

        async fn _get(n: usize) -> usize {
            n
        }

        assert_eq!(
            CachedAsync::async_get_or_set_with(&mut c, 0, || async { _get(0).await }).await,
            &0
        );
        assert_eq!(
            CachedAsync::async_get_or_set_with(&mut c, 1, || async { _get(1).await }).await,
            &1
        );
        assert_eq!(
            CachedAsync::async_get_or_set_with(&mut c, 0, || async { _get(99).await }).await,
            &0
        );
    }

    #[test]
    fn test_diagnostics_and_traits() {
        let mut cache = LruTtlCache::builder()
            .size(3)
            .ttl(Duration::from_secs(60))
            .build();
        cache.cache_set(1, 100);
        cache.cache_set(2, 200);

        // Debug
        let debug_str = format!("{:?}", cache);
        assert!(debug_str.contains("LruTtlCache"));
        assert!(debug_str.contains("size"));
        assert!(debug_str.contains("ttl"));
        assert!(debug_str.contains("hits"));
        assert!(debug_str.contains("misses"));

        // Clone
        let mut cloned = cache.clone();
        assert_eq!(cloned.cache_get(&1), Some(&100));
        assert_eq!(cloned.cache_get(&2), Some(&200));

        // Builder try_build errors
        let builder = LruTtlCache::<u32, u32>::builder();
        let try_built = builder.try_build();
        assert!(try_built.is_err()); // Missing both size and ttl

        let builder = LruTtlCache::<u32, u32>::builder().size(3);
        let try_built = builder.try_build();
        assert!(try_built.is_err()); // Missing ttl

        let builder = LruTtlCache::<u32, u32>::builder().ttl(Duration::from_secs(60));
        let try_built = builder.try_build();
        assert!(try_built.is_err()); // Missing size

        let builder = LruTtlCache::<u32, u32>::builder()
            .size(0)
            .ttl(Duration::from_secs(60));
        let try_built = builder.try_build();
        assert!(try_built.is_err()); // Size 0 is invalid
    }
}
