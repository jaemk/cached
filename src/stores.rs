/*!
Implementation of various caches

*/

use std::collections::{HashMap, LinkedList};
use std::collections::linked_list::Iter;
use std::time::Instant;
use std::hash::Hash;
use std::cmp::Eq;

use super::Cached;


/// Default unbounded cache
pub struct UnboundCache<K, V> {
    store: HashMap<K, V>,
    hits: u32,
    misses: u32,
}

impl <K: Hash + Eq, V> UnboundCache<K, V> {
    /// Creates an empty `UnboundCache`
    pub fn new() -> UnboundCache<K, V> {
        UnboundCache {
            store: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Creates an empty `UnboundCache` with a given pre-allocated capacity
    pub fn with_capacity(size: usize) -> UnboundCache<K, V> {
        UnboundCache {
            store: HashMap::with_capacity(size),
            hits: 0,
            misses: 0,
        }
    }
}

impl <K: Hash + Eq, V> Cached<K, V> for UnboundCache<K, V> {
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
///   to evict the least recently used keys
pub struct SizedCache<K, V> {
    store: HashMap<K, V>,
    order: LinkedList<K>,
    capacity: usize,
    hits: u32,
    misses: u32,
}

impl<K: Hash + Eq, V> SizedCache<K, V> {
    /// Creates a new `SizedCache` with a given capacity
    pub fn with_capacity(size: usize) -> SizedCache<K, V> {
        if size == 0 { panic!("`size` of `SizedCache` must be greater than zero.") }
        SizedCache {
            store: HashMap::with_capacity(size),
            order: LinkedList::new(),
            capacity: size,
            hits: 0,
            misses: 0,
        }
    }
    pub fn key_order(&self) -> Iter<K> {
        self.order.iter()
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
    fn cache_capacity(&self) -> Option<usize> { Some(self.capacity) }
}


/// Enum used for defining the status of time-cached values
enum Status {
    NotFound,
    Found,
    Expired,
}


/// Cache store bound by time
/// - Values are timestamped when inserted and are
///   expired on attempted retrieval.
pub struct TimedCache<K, V> {
    store: HashMap<K, (Instant, V)>,
    seconds: u64,
    hits: u32,
    misses: u32,
}

impl<K: Hash + Eq, V> TimedCache<K, V> {
    /// Creates a new `TimedCache` with a specified lifespan
    pub fn with_lifespan(seconds: u64) -> TimedCache<K, V> {
        TimedCache {
            store: HashMap::new(),
            seconds: seconds,
            hits: 0,
            misses: 0,
        }
    }

    /// Creates a new `TimedCache` with a specified lifespan and
    /// cache-store with the specified pre-allocated capacity
    pub fn with_lifespan_and_capacity(seconds: u64, size: usize) -> TimedCache<K, V> {
        TimedCache {
            store: HashMap::with_capacity(size),
            seconds: seconds,
            hits: 0,
            misses: 0,
        }
    }
}

impl<K: Hash + Eq, V> Cached<K, V> for TimedCache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        let status = {
            let val = self.store.get(key);
            if let Some(&(instant, _)) = val {
                if instant.elapsed().as_secs() < self.seconds {
                    Status::Found
                } else {
                    Status::Expired
                }
            } else {
                 Status::NotFound
            }
        };
        match status {
            Status::NotFound => {
                self.misses += 1;
                None
            }
            Status::Found    => {
                self.hits += 1;
                self.store.get(key).map(|stamped| &stamped.1)
            }
            Status::Expired  => {
                self.misses += 1;
                self.store.remove(key).unwrap();
                None
            }
        }
    }
    fn cache_set(&mut self, key: K, val: V) {
        let stamped = (Instant::now(), val);
        self.store.insert(key, stamped);
    }
    fn cache_size(&self) -> usize {
        self.store.len()
    }
    fn cache_hits(&self) -> Option<u32> { Some(self.hits) }
    fn cache_misses(&self) -> Option<u32> { Some(self.misses) }
    fn cache_lifespan(&self) -> Option<u64> { Some(self.seconds) }
}


#[cfg(test)]
/// Cache store tests
mod tests {
    use std::time::Duration;
    use std::thread::sleep;

    use super::Cached;

    use super::UnboundCache;
    use super::SizedCache;
    use super::TimedCache;

    #[test]
    fn basic_cache() {
        let mut c = UnboundCache::new();
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
        let mut c = SizedCache::with_capacity(5);
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

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [5, 4, 3, 2, 1]);

        c.cache_set(6, 100);
        c.cache_set(7, 100);

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [7, 6, 5, 4, 3]);

        assert!(c.cache_get(&2).is_none());
        assert!(c.cache_get(&3).is_some());

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [3, 7, 6, 5, 4]);

        assert_eq!(2, c.cache_misses().unwrap());
        let size = c.cache_size();
        assert_eq!(5, size);
    }

    #[test]
    fn timed_cache() {
        let mut c = TimedCache::with_lifespan(2);
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        c.cache_set(1, 100);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        sleep(Duration::new(2, 0));
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(2, misses);
    }
}
