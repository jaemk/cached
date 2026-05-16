use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "ahash")]
use ahash::RandomState;

#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::{hash_map::Entry, HashMap};

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

use crate::{CachedIter, CachedPeek, CloneCached};

use super::{CacheEvict, Cached, TimedEntry};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Cache store bound by time
///
/// Values are timestamped when inserted and are
/// evicted if expired at time of retrieval.
///
/// Note: This cache is in-memory only
pub struct TtlCache<K, V> {
    pub(super) store: HashMap<K, TimedEntry<V>, RandomState>,
    pub(super) ttl: Duration,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) initial_capacity: Option<usize>,
    pub(super) refresh: bool,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> std::fmt::Debug for TtlCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtlCache")
            .field("ttl", &self.ttl)
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("initial_capacity", &self.initial_capacity)
            .field("refresh", &self.refresh)
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V> Clone for TtlCache<K, V>
where
    K: Clone + Hash + Eq,
    V: Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            ttl: self.ttl,
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            initial_capacity: self.initial_capacity,
            refresh: self.refresh,
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Builder for [`TtlCache`].
pub struct TtlCacheBuilder<K, V> {
    ttl: Option<Duration>,
    capacity: Option<usize>,
    refresh: bool,
    on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> TtlCacheBuilder<K, V> {
    /// Set the TTL for cache entries. Required — panics at build time if not set.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set the initial allocation capacity (optional).
    #[must_use]
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Set whether cache hits refresh the TTL of the accessed entry.
    #[must_use]
    pub fn refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
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
    /// Panics if `ttl` was not set.
    #[must_use]
    pub fn build(self) -> TtlCache<K, V>
    where
        K: Hash + Eq,
    {
        let ttl = self
            .ttl
            .expect("`TtlCacheBuilder` requires `ttl` to be set");
        TtlCache {
            store: TtlCache::<K, V>::new_store(self.capacity),
            ttl,
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
            evictions: std::sync::atomic::AtomicU64::new(0),
            initial_capacity: self.capacity,
            refresh: self.refresh,
            on_evict: self.on_evict,
        }
    }

    /// Build the cache, returning an error instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `ttl` was not set.
    pub fn try_build(self) -> Result<TtlCache<K, V>, super::BuildError>
    where
        K: Hash + Eq,
    {
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        Ok(TtlCache {
            store: TtlCache::<K, V>::new_store(self.capacity),
            ttl,
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
            evictions: std::sync::atomic::AtomicU64::new(0),
            initial_capacity: self.capacity,
            refresh: self.refresh,
            on_evict: self.on_evict,
        })
    }
}

impl<K: Hash + Eq, V> TtlCache<K, V> {
    /// Return a builder for constructing a [`TtlCache`].
    #[must_use]
    pub fn builder() -> TtlCacheBuilder<K, V> {
        TtlCacheBuilder {
            ttl: None,
            capacity: None,
            refresh: false,
            on_evict: None,
        }
    }

    /// Creates a new `TtlCache` with a specified ttl
    #[must_use]
    pub fn with_ttl(ttl: Duration) -> TtlCache<K, V> {
        Self::with_ttl_and_refresh(ttl, false)
    }

    /// Creates a new `TtlCache` with a specified ttl and
    /// cache-store with the specified pre-allocated capacity
    #[must_use]
    pub fn with_ttl_and_capacity(ttl: Duration, size: usize) -> TtlCache<K, V> {
        TtlCache {
            store: Self::new_store(Some(size)),
            ttl,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            initial_capacity: Some(size),
            refresh: false,
            on_evict: None,
        }
    }

    /// Creates a new `TtlCache` with a specified ttl which
    /// refreshes the ttl when the entry is retrieved
    #[must_use]
    pub fn with_ttl_and_refresh(ttl: Duration, refresh: bool) -> TtlCache<K, V> {
        TtlCache {
            store: Self::new_store(None),
            ttl,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            initial_capacity: None,
            refresh,
            on_evict: None,
        }
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

    fn new_store(capacity: Option<usize>) -> HashMap<K, TimedEntry<V>, RandomState> {
        capacity.map_or_else(
            || HashMap::with_hasher(RandomState::new()),
            |cap| HashMap::with_capacity_and_hasher(cap, RandomState::new()),
        )
    }

    /// Returns a reference to the cache's `store`
    #[must_use]
    pub fn store(&self) -> &HashMap<K, TimedEntry<V>, RandomState> {
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
}

impl<K: Hash + Eq, V> Cached<K, V> for TtlCache<K, V> {
    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get_mut(key) {
            if entry.instant.elapsed() < self.ttl {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    entry.instant = Instant::now();
                }
                // SAFETY: `ptr` points into a HashMap entry obtained from `get_mut`.
                // We return immediately without modifying the map, so the entry is
                // not moved while the returned reference is live. The raw pointer is
                // needed because the borrow checker cannot see that the `&mut entry`
                // borrow ends here when `refresh` mutated `entry.instant` above.
                let ptr = &entry.value as *const V;
                return Some(unsafe { &*ptr });
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        if let Some((k, entry)) = self.store.remove_entry(key) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    fn cache_get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get_mut(key) {
            if entry.instant.elapsed() < self.ttl {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    entry.instant = Instant::now();
                }
                // SAFETY: same as `cache_get` — entry is not moved between obtaining
                // the pointer and returning, and `&mut self` prevents concurrent access.
                let ptr = &mut entry.value as *mut V;
                return Some(unsafe { &mut *ptr });
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        if let Some((k, entry)) = self.store.remove_entry(key) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                if occupied.get().instant.elapsed() < self.ttl {
                    if self.refresh {
                        occupied.get_mut().instant = Instant::now();
                    }
                    self.hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &occupied.get().value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    let val = f();
                    occupied.insert(TimedEntry {
                        instant: Instant::now(),
                        value: val,
                    });
                }
                &mut occupied.into_mut().value
            }
            Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                let val = f();
                &mut vacant
                    .insert(TimedEntry {
                        instant: Instant::now(),
                        value: val,
                    })
                    .value
            }
        }
    }

    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                if occupied.get().instant.elapsed() < self.ttl {
                    if self.refresh {
                        occupied.get_mut().instant = Instant::now();
                    }
                    self.hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &occupied.get().value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    let val = f()?;
                    occupied.insert(TimedEntry {
                        instant: Instant::now(),
                        value: val,
                    });
                }
                Ok(&mut occupied.into_mut().value)
            }
            Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                let val = f()?;
                Ok(&mut vacant
                    .insert(TimedEntry {
                        instant: Instant::now(),
                        value: val,
                    })
                    .value)
            }
        }
    }

    /// Insert a key-value pair. Returns the previous value only if it had not yet expired.
    /// Expired previous values are silently discarded.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let entry = TimedEntry {
            instant: Instant::now(),
            value: val,
        };
        self.store.insert(key, entry).and_then(|entry| {
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
        self.store.remove(k).and_then(|entry| {
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
    fn cache_reset_metrics(&mut self) {
        self.misses.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        self.store = Self::new_store(self.initial_capacity);
        self.cache_reset_metrics();
    }
    fn cache_size(&self) -> usize {
        self.store.len()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load(Ordering::Relaxed))
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load(Ordering::Relaxed))
    }
    fn cache_evictions(&self) -> Option<u64> {
        Some(self.evictions.load(Ordering::Relaxed))
    }
}

impl<K: Hash + Eq, V> CachedIter<K, V> for TtlCache<K, V> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        let ttl = self.ttl;
        self.store.iter().filter_map(move |(k, entry)| {
            if entry.instant.elapsed() < ttl {
                Some((k, &entry.value))
            } else {
                None
            }
        })
    }
}

impl<K: Hash + Eq, V> CachedPeek<K, V> for TtlCache<K, V> {
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get(k) {
            if entry.instant.elapsed() < self.ttl {
                return Some(&entry.value);
            }
        }
        None
    }
}

impl<K: Hash + Eq, V> crate::CacheTtl for TtlCache<K, V> {
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

impl<K: Hash + Eq + Clone, V: Clone> CloneCached<K, V> for TtlCache<K, V> {
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get_mut(k) {
            let expired = entry.instant.elapsed() >= self.ttl;
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(entry.value.clone()), true)
            } else {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    entry.instant = Instant::now();
                }
                (Some(entry.value.clone()), false)
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            (None, false)
        }
    }
}

#[cfg(feature = "async_core")]
impl<K, V> CachedAsync<K, V> for TtlCache<K, V>
where
    K: Hash + Eq + Clone + Send,
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
            match self.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if occupied.get().instant.elapsed() < self.ttl {
                        if self.refresh {
                            occupied.get_mut().instant = Instant::now();
                        }
                        self.hits.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &occupied.get().value);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        occupied.insert(TimedEntry {
                            instant: Instant::now(),
                            value: f().await,
                        });
                    }
                    &mut occupied.into_mut().value
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    &mut vacant
                        .insert(TimedEntry {
                            instant: Instant::now(),
                            value: f().await,
                        })
                        .value
                }
            }
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
            let v = match self.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if occupied.get().instant.elapsed() < self.ttl {
                        if self.refresh {
                            occupied.get_mut().instant = Instant::now();
                        }
                        self.hits.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &occupied.get().value);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        occupied.insert(TimedEntry {
                            instant: Instant::now(),
                            value: f().await?,
                        });
                    }
                    &mut occupied.into_mut().value
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    &mut vacant
                        .insert(TimedEntry {
                            instant: Instant::now(),
                            value: f().await?,
                        })
                        .value
                }
            };
            Ok(v)
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V> CacheEvict for TtlCache<K, V> {
    fn evict(&mut self) -> usize {
        TtlCache::evict(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::Cached;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn cache_reset_does_not_fire_on_evict() {
        let evict_count = Arc::new(AtomicUsize::new(0));
        let evict_count2 = evict_count.clone();
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
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
}
