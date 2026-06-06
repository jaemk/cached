use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

use crate::{CachedIter, CachedPeek, CloneCached};

use super::{CacheEvict, Cached, LruCache, TimedEntry};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
/// When this marker is active, [`LruTtlCacheBuilder::build`] requires
/// `K: 'static` and `V: 'static` because the callback must be wired into
/// the inner LRU store.
pub struct HasEvict;

/// Builder for [`LruTtlCache`].
///
/// Obtain one via [`LruTtlCache::builder`].
///
/// The `E` type parameter is a compile-time marker:
/// - [`NoEvict`] (the default): no eviction callback has been set; `build`
///   does **not** require `K: 'static` or `V: 'static`.
/// - [`HasEvict`]: an eviction callback was registered via [`on_evict`](LruTtlCacheBuilder::on_evict);
///   `build` requires `K: 'static + V: 'static` so the callback
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
    #[doc(alias = "size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.size = Some(max_size);
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
    pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Alias for [`refresh_on_hit`](Self::refresh_on_hit).
    #[must_use]
    pub fn refresh(self, refresh: bool) -> Self {
        self.refresh_on_hit(refresh)
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
    ///
    /// Use [`cache_clear_with_on_evict`](LruTtlCache::cache_clear_with_on_evict)
    /// instead of [`cache_clear`](crate::Cached::cache_clear) to opt into callback
    /// firing and eviction counter increments when clearing all entries.
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

// build without an eviction callback — no 'static required
impl<K, V> LruTtlCacheBuilder<K, V, NoEvict> {
    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `max_size` or `ttl` was not set, if `ttl` is zero, or if `max_size` is `0`.
    pub fn build(self) -> Result<LruTtlCache<K, V>, super::BuildError>
    where
        K: Hash + Eq + Clone,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("max_size"))?;
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        super::validate_ttl(ttl)?;
        LruTtlCache::new_internal(size, ttl, self.refresh)
    }
}

// build with an eviction callback — 'static required for sync_on_evict
impl<K, V> LruTtlCacheBuilder<K, V, HasEvict> {
    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `max_size` or `ttl` was not set, if `ttl` is zero, or if `max_size` is `0`.
    pub fn build(self) -> Result<LruTtlCache<K, V>, super::BuildError>
    where
        K: Hash + Eq + Clone + 'static,
        V: 'static,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("max_size"))?;
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        super::validate_ttl(ttl)?;
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
        let mut store = LruCache::builder().max_size(size).build()?;
        store.disable_hit_miss_tracking();
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

    /// Returns the maximum number of entries this cache will hold before evicting.
    ///
    /// This is the bound set via [`LruTtlCacheBuilder::max_size`], not the current number
    /// of entries — use [`cache_size`](crate::Cached::cache_size) for that.
    #[doc(alias = "size")]
    #[doc(alias = "max_size")]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.size
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
        self.store.retain_silent(|key, entry| {
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
        self.store.retain_silent(|key, entry| {
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

    /// Remove all entries and fire the `on_evict` callback for each one, incrementing the
    /// evictions counter.
    ///
    /// Unlike [`cache_clear`](crate::Cached::cache_clear) (which removes entries silently),
    /// this method invokes `on_evict` for every removed entry (whether or not they had expired)
    /// and increments `evictions`. If no `on_evict` callback was configured, it falls back to
    /// the plain `cache_clear`.
    pub fn cache_clear_with_on_evict(&mut self) {
        if self.on_evict.is_none() {
            return self.cache_clear();
        }
        let keys = self.store.key_order();
        let mut removed = Vec::with_capacity(keys.len());
        for k in &keys {
            if let Some(pair) = self.store.pop_raw(k) {
                removed.push(pair);
            }
        }
        let count = removed.len() as u64;
        if count > 0 {
            self.evictions.fetch_add(count, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (k, entry) in &removed {
                on_evict(k, &entry.value);
            }
        }
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
                if let Some((k, entry)) = self.store.pop_raw(key) {
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
                if let Some((k, entry)) = self.store.pop_raw(key) {
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
        if let Some((stored_k, entry)) = self.store.pop_raw(k) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if entry.instant.elapsed() < self.ttl {
                Some(entry.value)
            } else {
                None
            }
        } else {
            None
        }
    }

    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, entry)) = self.store.pop_raw(k) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
            Some((stored_k, entry.value))
        } else {
            None
        }
    }

    fn cache_clear(&mut self) {
        self.store.clear();
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        let on_evict = self.store.on_evict.clone();
        self.store = LruCache::builder()
            .max_size(self.size)
            .build()
            .expect("LruCache build failed");
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
        let mut cache = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
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
    fn capacity_returns_bound_not_live_size() {
        let mut cache = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(cache.capacity(), 3);
        assert_eq!(cache.cache_size(), 0);

        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        assert_eq!(cache.capacity(), 3);
        assert_eq!(cache.cache_size(), 2);

        // Eviction past the bound keeps capacity fixed while live count stays capped.
        cache.cache_set(3, 30);
        cache.cache_set(4, 40);
        assert_eq!(cache.capacity(), 3);
        assert_eq!(cache.cache_size(), 3);
    }

    #[test]
    fn reset_rebuilds_store_and_preserves_on_evict() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_for_callback = evicted.clone();
        let mut cache = LruTtlCache::builder()
            .max_size(1)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_key: &u8, _value: &u8| {
                evicted_for_callback.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();

        cache.set(1, 10);
        cache.cache_reset();
        assert_eq!(cache.cache_size(), 0);

        cache.set(2, 20);
        cache.set(3, 30);
        assert_eq!(evicted.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn try_new() {
        let c = LruTtlCache::<i32, i32>::builder()
            .max_size(0)
            .ttl(Duration::from_secs(1))
            .build();
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue {
                field: "max_size",
                ..
            }
        ));

        let c = LruTtlCache::<i32, i32>::builder()
            .max_size(usize::MAX)
            .ttl(Duration::from_secs(1))
            .build();
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue {
                field: "max_size",
                ..
            }
        ));
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruTtlCache::builder()
            .max_size(5)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(count.load(AtomicOrdering::Relaxed), 3);
        assert_eq!(c.evictions.load(AtomicOrdering::Relaxed), 3);
    }

    #[test]
    fn cache_clear_does_not_fire_on_evict() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c = LruTtlCache::builder()
            .max_size(5)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            fired.load(AtomicOrdering::Relaxed),
            0,
            "cache_clear must not fire on_evict"
        );
    }

    #[test]
    fn cache_reset_does_not_fire_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let evict_count = Arc::new(AtomicUsize::new(0));
        let evict_count2 = evict_count.clone();
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k, _v| {
                evict_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
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
        // LruTtlCacheBuilder::build must not impose K: 'static or V: 'static
        // when no on_evict callback is configured.
        fn build_with_borrowed<'a>(_k: &'a str, _v: &'a str) -> LruTtlCache<&'a str, &'a str> {
            LruTtlCache::builder()
                .max_size(4)
                .ttl(Duration::from_secs(60))
                .build()
                .unwrap()
        }
        let mut cache = build_with_borrowed("key", "val");
        cache.cache_set("key", "val");
        assert_eq!(cache.cache_get(&"key"), Some(&"val"));
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_async_trait() {
        use crate::CachedAsync;
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();

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
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
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

        // Builder build errors
        let builder = LruTtlCache::<u32, u32>::builder();
        let built = builder.build();
        assert!(built.is_err()); // Missing both size and ttl

        let builder = LruTtlCache::<u32, u32>::builder().max_size(3);
        let built = builder.build();
        assert!(built.is_err()); // Missing ttl

        let builder = LruTtlCache::<u32, u32>::builder().ttl(Duration::from_secs(60));
        let built = builder.build();
        assert!(built.is_err()); // Missing size

        let builder = LruTtlCache::<u32, u32>::builder()
            .max_size(0)
            .ttl(Duration::from_secs(60));
        let built = builder.build();
        assert!(built.is_err()); // Size 0 is invalid

        let builder = LruTtlCache::<u32, u32>::builder()
            .max_size(3)
            .ttl(Duration::ZERO);
        let built = builder.build();
        assert!(built.is_err()); // Zero ttl is invalid
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        assert_eq!(c.cache_remove_entry(&999u32), None); // absent
        assert_eq!(c.cache_remove_entry(&1u32), Some((1u32, 100u32)));
        assert_eq!(c.cache_get(&1u32), None);
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(100));

        // cache_remove returns None for expired.
        assert_eq!(c.cache_remove(&1u32), None);

        // cache_remove_entry returns Some even for expired.
        c.cache_set(2u32, 200u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let removed = c.cache_remove_entry(&2u32);
        assert!(removed.is_some());
        assert_eq!(
            removed.expect("cache_remove_entry returns Some for expired"),
            (2u32, 200u32)
        );
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            c.cache_delete(&1u32),
            "cache_delete must be true even for expired entry"
        );
        assert!(!c.cache_delete(&1u32), "cache_delete false when absent");
    }

    #[test]
    fn cache_remove_entry_fires_on_evict_for_expired() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(50))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(100));

        c.cache_remove_entry(&1u32);
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "on_evict fires for expired entries"
        );

        c.cache_remove_entry(&999u32);
        assert_eq!(count.load(Ordering::Relaxed), 1, "no fire for absent key");
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let before = c.cache_evictions().expect("evictions are always tracked");
        c.cache_remove_entry(&1u32); // expired but present — must increment
        c.cache_remove_entry(&999u32); // absent — must not increment
        assert_eq!(
            c.cache_evictions().expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }
}
