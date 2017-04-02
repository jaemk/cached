use std::collections::HashMap;
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
    fn cache_get(&mut self, k: &K) -> Option<&V> {
        match self.store.get(k) {
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
    fn cache_set(&mut self, k: K, v: V) {
        self.store.insert(k, v);
    }
    fn cache_size(&self) -> usize { self.store.len() }
    fn cache_hits(&self) -> Option<u32> { Some(self.hits) }
    fn cache_misses(&self) -> Option<u32> { Some(self.misses) }
}


#[cfg(test)]
mod tests {
    use super::Cached;
    use super::Cache;

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
}
