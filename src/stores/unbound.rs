use super::Cached;
use crate::{CachedIter, CachedPeek, CachedRead};

use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "ahash")]
use ahash::RandomState;

#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::{hash_map::Entry, HashMap};

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

use std::sync::atomic::{AtomicU64, Ordering};

/// Default unbounded cache
///
/// This cache has no size limit or eviction policy.
///
/// Note: This cache is in-memory only
pub struct UnboundCache<K, V> {
    pub(super) store: HashMap<K, V, RandomState>,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) initial_capacity: Option<usize>,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> std::fmt::Debug for UnboundCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnboundCache")
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V> Clone for UnboundCache<K, V>
where
    K: Clone + Hash + Eq,
    V: Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            initial_capacity: self.initial_capacity,
            on_evict: self.on_evict.clone(),
        }
    }
}

impl<K, V> PartialEq for UnboundCache<K, V>
where
    K: Eq + Hash,
    V: PartialEq,
{
    fn eq(&self, other: &UnboundCache<K, V>) -> bool {
        self.store.eq(&other.store)
    }
}

impl<K, V> Eq for UnboundCache<K, V>
where
    K: Eq + Hash,
    V: PartialEq,
{
}

/// Builder for [`UnboundCache`].
pub struct UnboundCacheBuilder<K, V> {
    capacity: Option<usize>,
    on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> Default for UnboundCacheBuilder<K, V> {
    fn default() -> Self {
        Self {
            capacity: None,
            on_evict: None,
        }
    }
}

impl<K, V> UnboundCacheBuilder<K, V> {
    /// Set the initial allocation capacity (optional, purely a hint).
    #[must_use]
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Set a callback invoked when an entry is explicitly removed via
    /// [`cache_remove`](crate::Cached::cache_remove).
    ///
    /// Note: because `UnboundCache` has no eviction policy, `on_evict` will
    /// not fire during normal cache operations — only on explicit removal.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(std::sync::Arc::new(on_evict));
        self
    }

    /// Build the cache.
    #[must_use]
    pub fn build(self) -> UnboundCache<K, V>
    where
        K: Hash + Eq,
    {
        let mut cache = match self.capacity {
            Some(cap) => UnboundCache::with_capacity(cap),
            None => UnboundCache::new(),
        };
        cache.on_evict = self.on_evict;
        cache
    }

    /// Build the cache, returning an error instead of panicking.
    ///
    /// `UnboundCache` has no required fields, so this always succeeds.
    /// Provided for API consistency with other builders.
    ///
    /// # Errors
    ///
    /// This method currently never returns an error.
    pub fn try_build(self) -> Result<UnboundCache<K, V>, super::BuildError>
    where
        K: Hash + Eq,
    {
        Ok(self.build())
    }
}

impl<K: Hash + Eq, V> UnboundCache<K, V> {
    /// Return a builder for constructing an [`UnboundCache`].
    #[must_use]
    pub fn builder() -> UnboundCacheBuilder<K, V> {
        UnboundCacheBuilder::default()
    }

    /// Creates an empty `UnboundCache`
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> UnboundCache<K, V> {
        UnboundCache {
            store: Self::new_store(None),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            initial_capacity: None,
            on_evict: None,
        }
    }

    /// Creates an empty `UnboundCache` with a given pre-allocated capacity
    #[must_use]
    pub fn with_capacity(size: usize) -> UnboundCache<K, V> {
        UnboundCache {
            store: Self::new_store(Some(size)),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            initial_capacity: Some(size),
            on_evict: None,
        }
    }

    fn new_store(capacity: Option<usize>) -> HashMap<K, V, RandomState> {
        capacity.map_or_else(
            || HashMap::with_hasher(RandomState::new()),
            |cap| HashMap::with_capacity_and_hasher(cap, RandomState::new()),
        )
    }

    /// Returns a reference to the cache's `store`
    #[must_use]
    pub fn store(&self) -> &HashMap<K, V, RandomState> {
        &self.store
    }
}

impl<K: Hash + Eq, V> Cached<K, V> for UnboundCache<K, V> {
    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(v) = self.store.get(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(v)
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
        if let Some(v) = self.store.get_mut(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(v)
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        self.store.insert(key, val)
    }
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        match self.store.entry(key) {
            Entry::Occupied(occupied) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                occupied.into_mut()
            }

            Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                vacant.insert(f())
            }
        }
    }
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        match self.store.entry(key) {
            Entry::Occupied(occupied) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Ok(occupied.into_mut())
            }

            Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Ok(vacant.insert(f()?))
            }
        }
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let removed = self.store.remove_entry(k);
        if let Some((ref k, ref v)) = removed {
            if let Some(on_evict) = &self.on_evict {
                on_evict(k, v);
            }
        }
        removed.map(|(_, v)| v)
    }
    fn cache_clear(&mut self) {
        self.store.clear();
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place. UnboundCache has no `on_evict` callback.
        self.store = Self::new_store(self.initial_capacity);
        self.cache_reset_metrics();
    }
    fn cache_reset_metrics(&mut self) {
        self.misses.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
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
}

impl<K: Hash + Eq, V> CachedIter<K, V> for UnboundCache<K, V> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.store.iter()
    }
}

impl<K: Hash + Eq, V> CachedPeek<K, V> for UnboundCache<K, V> {
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.get(k)
    }
}

impl<K: Hash + Eq, V> CachedRead<K, V> for UnboundCache<K, V> {
    fn cache_get_read<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(value) = self.cache_peek(k) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(value)
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

#[cfg(feature = "async_core")]
impl<K, V> CachedAsync<K, V> for UnboundCache<K, V>
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
            match self.store.entry(key) {
                Entry::Occupied(occupied) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    occupied.into_mut()
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    vacant.insert(f().await)
                }
            }
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
            let v = match self.store.entry(key) {
                Entry::Occupied(occupied) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    occupied.into_mut()
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    vacant.insert(f().await?)
                }
            };
            Ok(v)
        }
    }
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use super::*;

    #[test]
    fn basic_cache() {
        let mut c = UnboundCache::new();
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);
    }

    #[test]
    fn metrics_preserve_untracked_state_in_helpers() {
        let c = std::collections::HashMap::<u8, u8>::new();
        let metrics = c.metrics();
        assert_eq!(metrics.hits, None);
        assert_eq!(metrics.misses, None);
        assert_eq!(metrics.evictions, None);
        assert_eq!(metrics.hit_ratio(), None);
    }

    #[test]
    fn clear() {
        let mut c = UnboundCache::new();

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        // register some hits and misses
        c.cache_get(&1);
        c.cache_get(&2);
        c.cache_get(&3);
        c.cache_get(&10);
        c.cache_get(&20);
        c.cache_get(&30);

        assert_eq!(3, c.cache_size());
        assert_eq!(3, c.cache_hits().unwrap());
        assert_eq!(3, c.cache_misses().unwrap());
        assert!(3 <= c.store.capacity());

        // clear the cache, should have no more elements
        // hits and misses will still be kept
        c.cache_clear();

        assert_eq!(0, c.cache_size());
        assert_eq!(3, c.cache_hits().unwrap());
        assert_eq!(3, c.cache_misses().unwrap());
        assert!(3 <= c.store.capacity()); // Keeps the allocated memory for reuse.

        let capacity = 1;
        let mut c = UnboundCache::with_capacity(capacity);
        assert!(capacity <= c.store.capacity());

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        assert!(3 <= c.store.capacity());

        c.cache_clear();

        assert!(3 <= c.store.capacity()); // Keeps the allocated memory for reuse.
    }

    #[test]
    fn reset() {
        let mut c = UnboundCache::new();
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(3 <= c.store.capacity());

        c.cache_reset();

        assert_eq!(0, c.store.capacity());

        let init_capacity = 1;
        let mut c = UnboundCache::with_capacity(init_capacity);
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(3 <= c.store.capacity());

        c.cache_reset();

        assert!(init_capacity <= c.store.capacity());
    }

    #[test]
    fn remove() {
        let mut c = UnboundCache::new();

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        // register some hits and misses
        c.cache_get(&1);
        c.cache_get(&2);
        c.cache_get(&3);
        c.cache_get(&10);
        c.cache_get(&20);
        c.cache_get(&30);

        assert_eq!(3, c.cache_size());
        assert_eq!(3, c.cache_hits().unwrap());
        assert_eq!(3, c.cache_misses().unwrap());

        // remove some items from cache
        // hits and misses will still be kept
        assert_eq!(Some(100), c.cache_remove(&1));

        assert_eq!(2, c.cache_size());
        assert_eq!(3, c.cache_hits().unwrap());
        assert_eq!(3, c.cache_misses().unwrap());

        assert_eq!(Some(200), c.cache_remove(&2));

        assert_eq!(1, c.cache_size());

        // removing extra is ok
        assert_eq!(None, c.cache_remove(&2));

        assert_eq!(1, c.cache_size());
    }

    #[test]
    fn get_or_set_with() {
        let mut c = UnboundCache::new();

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);
        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_get_or_set_with(2, || 2), &2);
        assert_eq!(c.cache_get_or_set_with(3, || 3), &3);
        assert_eq!(c.cache_get_or_set_with(4, || 4), &4);
        assert_eq!(c.cache_get_or_set_with(5, || 5), &5);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);

        assert_eq!(c.cache_misses(), Some(6));

        c.cache_reset();
        fn _try_get(n: usize) -> Result<usize, String> {
            if n < 10 {
                Ok(n)
            } else {
                Err("dead".to_string())
            }
        }
        let res: Result<&mut usize, String> = c.cache_try_get_or_set_with(0, || _try_get(10));
        assert!(res.is_err());

        let res: Result<&mut usize, String> = c.cache_try_get_or_set_with(0, || _try_get(1));
        assert_eq!(res.unwrap(), &1);
        let res: Result<&mut usize, String> = c.cache_try_get_or_set_with(0, || _try_get(5));
        assert_eq!(res.unwrap(), &1);
    }
}
