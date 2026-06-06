use super::Cached;
use crate::{CachedIter, CachedPeek, CachedRead};

use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "ahash")]
use ahash::RandomState;

#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::{HashMap, hash_map::Entry};

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

use super::StripedCounter;

/// Default unbounded cache
///
/// This cache has no size limit or eviction policy.
///
/// Note: This cache is in-memory only
pub struct UnboundCache<K, V> {
    pub(super) store: HashMap<K, V, RandomState>,
    pub(super) hits: StripedCounter,
    pub(super) misses: StripedCounter,
    pub(super) initial_capacity: Option<usize>,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> std::fmt::Debug for UnboundCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnboundCache")
            .field("hits", &self.hits.load())
            .field("misses", &self.misses.load())
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
            hits: self.hits.snapshot(),
            misses: self.misses.snapshot(),
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
    /// Use [`cache_clear_with_on_evict`](UnboundCache::cache_clear_with_on_evict)
    /// instead of [`cache_clear`](crate::Cached::cache_clear) to opt into callback
    /// firing when clearing all entries.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(std::sync::Arc::new(on_evict));
        self
    }

    /// Build the cache.
    ///
    /// `UnboundCache` has no required fields and this always succeeds.
    ///
    /// # Errors
    ///
    /// This method currently never returns an error.
    pub fn build(self) -> Result<UnboundCache<K, V>, super::BuildError>
    where
        K: Hash + Eq,
    {
        let store = UnboundCache::<K, V>::new_store(self.capacity);
        Ok(UnboundCache {
            store,
            hits: StripedCounter::new(),
            misses: StripedCounter::new(),
            initial_capacity: self.capacity,
            on_evict: self.on_evict,
        })
    }
}

impl<K: Hash + Eq, V> UnboundCache<K, V> {
    /// Return a builder for constructing an [`UnboundCache`].
    #[must_use]
    pub fn builder() -> UnboundCacheBuilder<K, V> {
        UnboundCacheBuilder::default()
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

    /// Remove all entries and fire the `on_evict` callback for each one.
    ///
    /// Unlike [`cache_clear`](crate::Cached::cache_clear) (which removes entries silently),
    /// this method invokes `on_evict` for every removed entry. If no `on_evict` callback was
    /// configured, it falls back to the plain `cache_clear`.
    pub fn cache_clear_with_on_evict(&mut self) {
        if self.on_evict.is_none() {
            return self.cache_clear();
        }
        let entries: Vec<(K, V)> = self.store.drain().collect();
        if let Some(on_evict) = &self.on_evict {
            for (k, v) in &entries {
                on_evict(k, v);
            }
        }
    }
}

impl<K: Hash + Eq, V> Cached<K, V> for UnboundCache<K, V> {
    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(v) = self.store.get(key) {
            self.hits.increment();
            Some(v)
        } else {
            self.misses.increment();
            None
        }
    }
    fn cache_get_mut<Q>(&mut self, key: &Q) -> std::option::Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(v) = self.store.get_mut(key) {
            self.hits.increment();
            Some(v)
        } else {
            self.misses.increment();
            None
        }
    }
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        self.store.insert(key, val)
    }
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        match self.store.entry(key) {
            Entry::Occupied(occupied) => {
                self.hits.increment();
                occupied.into_mut()
            }

            Entry::Vacant(vacant) => {
                self.misses.increment();
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
                self.hits.increment();
                Ok(occupied.into_mut())
            }

            Entry::Vacant(vacant) => {
                self.misses.increment();
                Ok(vacant.insert(f()?))
            }
        }
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.cache_remove_entry(k).map(|(_, v)| v)
    }

    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let removed = self.store.remove_entry(k);
        if let Some((ref stored_k, ref v)) = removed {
            if let Some(on_evict) = &self.on_evict {
                on_evict(stored_k, v);
            }
        }
        removed
    }

    fn cache_clear(&mut self) {
        self.store.clear();
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is not called during reset.
        self.store = Self::new_store(self.initial_capacity);
        self.cache_reset_metrics();
    }
    fn cache_reset_metrics(&mut self) {
        self.misses.reset();
        self.hits.reset();
    }
    fn cache_size(&self) -> usize {
        self.store.len()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load())
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load())
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
            self.hits.increment();
            Some(value)
        } else {
            self.misses.increment();
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
                    self.hits.increment();
                    occupied.into_mut()
                }
                Entry::Vacant(vacant) => {
                    self.misses.increment();
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
                    self.hits.increment();
                    occupied.into_mut()
                }
                Entry::Vacant(vacant) => {
                    self.misses.increment();
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
    use crate::Cached;

    #[test]
    fn basic_cache() {
        let mut c = UnboundCache::builder().build().unwrap();
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
        let mut c = UnboundCache::builder().build().unwrap();

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
        let mut c = UnboundCache::builder().capacity(capacity).build().unwrap();
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
        let mut c = UnboundCache::builder().build().unwrap();
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(3 <= c.store.capacity());

        c.cache_reset();

        assert_eq!(0, c.store.capacity());

        let init_capacity = 1;
        let mut c = UnboundCache::builder()
            .capacity(init_capacity)
            .build()
            .unwrap();
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(3 <= c.store.capacity());

        c.cache_reset();

        assert!(init_capacity <= c.store.capacity());
    }

    #[test]
    fn remove() {
        let mut c = UnboundCache::builder().build().unwrap();

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
        let mut c = UnboundCache::builder().build().unwrap();

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

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = UnboundCache::builder()
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(count.load(AOrdering::Relaxed), 3);
    }

    #[test]
    fn cache_clear_does_not_fire_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = UnboundCache::builder()
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            count.load(AOrdering::Relaxed),
            0,
            "cache_clear must not fire on_evict"
        );
    }

    #[test]
    fn test_diagnostics_and_traits() {
        let mut cache = UnboundCache::builder().capacity(10).build().unwrap();
        cache.cache_set(1, 100);
        cache.cache_set(2, 200);

        // Debug
        let debug_str = format!("{:?}", cache);
        assert!(debug_str.contains("UnboundCache"));
        assert!(debug_str.contains("hits"));
        assert!(debug_str.contains("misses"));

        // Clone
        let mut cloned = cache.clone();
        assert_eq!(cloned.cache_get(&1), Some(&100));
        assert_eq!(cloned.cache_get(&2), Some(&200));

        // PartialEq/Eq
        assert_eq!(cache, cloned);
        cloned.cache_set(3, 300);
        assert_ne!(cache, cloned);

        // Builder build always succeeds for UnboundCache
        let builder = UnboundCache::<u32, u32>::builder().on_evict(|_, _| {});
        let built = builder.build();
        assert!(built.is_ok());
    }

    #[test]
    fn cache_remove_entry_basic() {
        let mut c = UnboundCache::builder().build().unwrap();
        c.cache_set(1u32, 100u32);

        // Returns None when key absent.
        assert_eq!(c.cache_remove_entry(&999u32), None);

        // Returns stored key and value.
        let removed = c.cache_remove_entry(&1u32);
        assert_eq!(removed, Some((1u32, 100u32)));

        // Entry is gone.
        assert_eq!(c.cache_get(&1u32), None);
    }

    #[test]
    fn cache_remove_entry_fires_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count2 = count.clone();
        let mut c = UnboundCache::builder()
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        c.cache_remove_entry(&1u32);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        // No fire for absent key.
        c.cache_remove_entry(&999u32);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cache_delete_uses_cache_remove_entry() {
        let mut c = UnboundCache::<u32, u32>::builder().build().unwrap();
        c.cache_set(1, 10);
        assert!(
            c.cache_delete(&1),
            "cache_delete must return true for existing entry"
        );
        assert!(
            !c.cache_delete(&1),
            "cache_delete must return false for absent entry"
        );
    }

    #[test]
    fn cache_remove_entry_returns_stored_key_not_lookup_key() {
        // Verify the doc promise: cache_remove_entry returns the *stored* key,
        // not the lookup key. Uses a key type where Hash+Eq only check `lower`
        // so two instances can be "equal" but have different `original` fields.
        use std::hash::{Hash, Hasher};
        #[derive(Clone, Debug)]
        struct CaseKey {
            lower: String,
            original: String,
        }
        impl PartialEq for CaseKey {
            fn eq(&self, other: &Self) -> bool {
                self.lower == other.lower
            }
        }
        impl Eq for CaseKey {}
        impl Hash for CaseKey {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.lower.hash(state);
            }
        }

        let stored = CaseKey {
            lower: "hello".to_string(),
            original: "Hello".to_string(),
        };
        let lookup = CaseKey {
            lower: "hello".to_string(),
            original: "HELLO".to_string(),
        };

        let mut c = UnboundCache::<CaseKey, u32>::builder().build().unwrap();
        c.cache_set(stored, 42);

        let (returned_key, returned_val) =
            c.cache_remove_entry(&lookup).expect("key must be found");
        assert_eq!(returned_val, 42);
        // The *stored* original casing must come back, not the lookup's casing.
        assert_eq!(
            returned_key.original, "Hello",
            "cache_remove_entry must return the stored key instance"
        );
    }
}
