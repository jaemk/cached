use super::Cached;
use std::cmp::Eq;
use std::collections::HashMap;
use std::hash::Hash;

use std::collections::hash_map::Entry;

#[cfg(feature = "async")]
use {super::CachedAsync, async_trait::async_trait, futures_lite::Future};

/// Default unbounded cache
///
/// This cache has no size limit or eviction policy.
///
/// Note: This cache is in-memory only
#[derive(Clone, Debug)]
pub struct UnboundCache<K, V> {
    pub(super) store: HashMap<K, V>,
    pub(super) hits: u64,
    pub(super) misses: u64,
    pub(super) initial_capacity: Option<usize>,
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

impl<K: Hash + Eq, V> UnboundCache<K, V> {
    /// Creates an empty `UnboundCache`
    #[allow(clippy::new_without_default)]
    pub fn new() -> UnboundCache<K, V> {
        UnboundCache {
            store: Self::new_store(None),
            hits: 0,
            misses: 0,
            initial_capacity: None,
        }
    }

    /// Creates an empty `UnboundCache` with a given pre-allocated capacity
    pub fn with_capacity(size: usize) -> UnboundCache<K, V> {
        UnboundCache {
            store: Self::new_store(Some(size)),
            hits: 0,
            misses: 0,
            initial_capacity: Some(size),
        }
    }

    fn new_store(capacity: Option<usize>) -> HashMap<K, V> {
        capacity.map_or_else(HashMap::new, HashMap::with_capacity)
    }
}

impl<K: Hash + Eq, V> Cached<K, V> for UnboundCache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        match self.store.get(key) {
            Some(v) => {
                self.hits += 1;
                Some(v)
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }
    fn cache_get_mut(&mut self, key: &K) -> std::option::Option<&mut V> {
        match self.store.get_mut(key) {
            Some(v) => {
                self.hits += 1;
                Some(v)
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        self.store.insert(key, val)
    }
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        match self.store.entry(key) {
            Entry::Occupied(occupied) => {
                self.hits += 1;
                occupied.into_mut()
            }

            Entry::Vacant(vacant) => {
                self.misses += 1;
                vacant.insert(f())
            }
        }
    }
    fn cache_remove(&mut self, k: &K) -> Option<V> {
        self.store.remove(k)
    }
    fn cache_clear(&mut self) {
        self.store.clear();
    }
    fn cache_reset(&mut self) {
        self.store = Self::new_store(self.initial_capacity);
    }
    fn cache_size(&self) -> usize {
        self.store.len()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits)
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses)
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for UnboundCache<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    async fn get_or_set_with<F, Fut>(&mut self, key: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        match self.store.entry(key) {
            Entry::Occupied(occupied) => {
                self.hits += 1;
                occupied.into_mut()
            }

            Entry::Vacant(vacant) => {
                self.misses += 1;
                vacant.insert(f().await)
            }
        }
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, key: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let v = match self.store.entry(key) {
            Entry::Occupied(occupied) => {
                self.hits += 1;
                occupied.into_mut()
            }

            Entry::Vacant(vacant) => {
                self.misses += 1;
                vacant.insert(f().await?)
            }
        };
        Ok(v)
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
    }
}
