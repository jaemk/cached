/*!
Implementation of various caches

*/

use std::collections::{HashMap, LinkedList};
use std::hash::Hash;
use std::cmp::Eq;

use super::Cached;


/// Default unbounded cache
pub struct Cache<K: Hash + Eq, V> {
    store: HashMap<K, V>,
    hits: u32,
    misses: u32,
}
impl <K: Hash + Eq, V> Cache<K, V> {
    pub fn new() -> Cache<K, V> {
        let store = HashMap::new();
        Cache {
            store: store,
            hits: 0,
            misses: 0,
        }
    }
}
impl <K: Hash + Eq, V> Cached<K, V> for Cache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        match self.store.get(key) {
            Some(v) => {
                self.hits += 1;
                Some(v)
            }
            None =>  {
                self.misses += 1;
                None
            }
        }
    }
    fn cache_set(&mut self, key: K, val: V) {
        self.store.insert(key, val);
    }
    fn cache_size(&self) -> usize { self.store.len() }
    fn cache_hits(&self) -> Option<u32> { Some(self.hits) }
    fn cache_misses(&self) -> Option<u32> { Some(self.misses) }
}


/// Least Recently Used / `Sized` Cache
/// - Stores up to a specified sized before beginning
///   to evict the least recently used values
pub struct SizedCache<K: Hash + Eq, V> {
    store: HashMap<K, V>,
    order: LinkedList<K>,
    capacity: usize,
    hits: u32,
    misses: u32,
}
impl<K: Hash + Eq, V> SizedCache<K, V> {
    pub fn new(size: usize) -> SizedCache<K, V> {
        if size == 0 { panic!("`size` of `SizedCache` must be greater than zero.") }
        SizedCache {
            store: HashMap::with_capacity(size),
            order: LinkedList::new(),
            capacity: size,
            hits: 0,
            misses: 0,
        }
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}
impl<K: Hash + Eq + Clone, V> Cached<K, V> for SizedCache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        let val = self.store.get(key);
        if val.is_some() {
            // if there's something in `self.store`, then `self.order`
            // cannot be empty, and `key` must be present
            let index = self.order.iter().enumerate()
                            .find(|&(_, e)| { key == e })
                            .unwrap().0;
            let mut tail = self.order.split_off(index);
            let used = tail.pop_front().unwrap();
            self.order.push_front(used);
            self.order.append(&mut tail);
            self.hits += 1;
        } else { self.misses += 1; }
        val
    }
    fn cache_set(&mut self, key: K, val: V) {
        if self.store.len() < self.capacity {
            self.store.insert(key.clone(), val);
            self.order.push_front(key);
        } else {
            // store capacity cannot be zero, so there must be content in `self.order`
            let lru_key = self.order.pop_back().unwrap();
            self.store.remove(&lru_key).unwrap();
            self.store.insert(key.clone(), val);
            self.order.push_front(key);
        }
    }
    fn cache_size(&self) -> usize { self.store.len() }
    fn cache_hits(&self) -> Option<u32> { Some(self.hits) }
    fn cache_misses(&self) -> Option<u32> { Some(self.misses) }
}


#[cfg(test)]
mod tests {
    use super::Cached;

    use super::Cache;
    use super::SizedCache;

    #[test]
    fn basic_cache() {
        let mut c = Cache::new();
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        c.cache_set(1, 100);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);
    }

    #[test]
    fn sized_cache() {
        let mut c = SizedCache::new(5);
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        c.cache_set(1, 100);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        c.cache_set(2, 100);
        c.cache_set(3, 100);
        c.cache_set(4, 100);
        c.cache_set(5, 100);
        c.cache_set(6, 100);
        c.cache_set(7, 100);
        let size = c.cache_size();
        assert_eq!(5, size);
    }
}
