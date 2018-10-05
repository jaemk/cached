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
///
/// This cache has no size limit or eviction policy.
///
/// Note: This cache is in-memory only
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
    fn cache_remove(&mut self, k: &K) -> Option<V> { self.store.remove(k) }
    fn cache_clear(&mut self) { self.store.clear(); }
    fn cache_size(&self) -> usize { self.store.len() }
    fn cache_hits(&self) -> Option<u32> { Some(self.hits) }
    fn cache_misses(&self) -> Option<u32> { Some(self.misses) }
}


enum Slot<T> {
    Occupied(T),
    Empty,
}
impl<T> Slot<T> {
    fn get(&self) -> Option<&T> {
        match self {
            &Slot::Occupied(ref v) => Some(v),
            &Slot::Empty => None,
        }
    }

    fn take(self) -> Option<T> {
        match self {
            Slot::Occupied(v) => Some(v),
            Slot::Empty => None,
        }
    }
}


/// Least Recently Used / `Sized` Cache
///
/// Stores up to a specified size before beginning
/// to evict the least recently used keys
///
/// Note: This cache is in-memory only
pub struct SizedCache<K, V> {
    store: HashMap<K, Slot<V>>,
    order: LinkedList<K>,
    capacity: usize,
    hits: u32,
    misses: u32,
}

impl<K: Hash + Eq, V> SizedCache<K, V> {
    #[deprecated(since="0.5.1", note="method renamed to `with_size`")]
    pub fn with_capacity(size: usize) -> SizedCache<K, V> {
        Self::with_size(size)
    }

    /// Creates a new `SizedCache` with a given size limit and pre-allocated backing data
    pub fn with_size(size: usize) -> SizedCache<K, V> {
        if size == 0 { panic!("`size` of `SizedCache` must be greater than zero.") }
        SizedCache {
            store: HashMap::with_capacity(size),
            order: LinkedList::new(),
            capacity: size,
            hits: 0,
            misses: 0,
        }
    }

    /// Return an iterator of keys in the current order from most
    /// to least recently used.
    pub fn key_order(&self) -> Iter<K> {
        self.order.iter()
    }
}

impl<K: Hash + Eq + Clone, V> Cached<K, V> for SizedCache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        let val = self.store.get(key);
        match val {
            Some(slot) => {
                // if there's something in `self.store`, then `self.order`
                // cannot be empty, and `key` must be present
                let index = self.order.iter().enumerate()
                                .find(|&(_, e)| { key == e })
                                .expect("SizedCache::cache_get key not found in ordering").0;
                let mut tail = self.order.split_off(index);
                let used = tail.pop_front().expect("SizedCache::cache_get ordering is empty");
                self.order.push_front(used);
                self.order.append(&mut tail);
                self.hits += 1;
                Some(slot.get().expect("SizedCache::cache_get slots should never be empty"))
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }
    fn cache_set(&mut self, key: K, val: V) {
        if self.store.len() >= self.capacity {
            // store has reached capacity, evict the oldest item.
            // store capacity cannot be zero, so there must be content in `self.order`.
            let lru_key = self.order.pop_back().expect("SizedCache::cache_set ordering is empty");
            self.store.remove(&lru_key).expect("SizedCache::cache_set failed evicting cache key");
        }
        let slot = self.store.entry(key.clone()).or_insert(Slot::Empty);
        match slot {
            Slot::Empty => self.order.push_front(key),
            _ => (),
        }
        *slot = Slot::Occupied(val);
    }
    fn cache_remove(&mut self, k: &K) -> Option<V> {
        // try and remove item from mapping, and then from order list if it was in mapping
        let removed = self.store.remove(k);
        if removed.is_some() {
            // need to remove the key in the order list
            let index = self.order.iter().enumerate()
                    .find(|&(_, e)| { k == e })
                    .expect("SizedCache::cache_remove key not found in ordering").0;
            let mut tail = self.order.split_off(index);
            tail.pop_front().expect("SizedCache::cache_remove ordering is empty");
            self.order.append(&mut tail);

            let slot = removed.expect("SizedCache::cache_remove slot is empty");

            slot.take()
        }
        else {
            None
        }
    }
    fn cache_clear(&mut self) {
        // clear both the store and the order list
        self.store.clear();
        self.order.clear();
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
///
/// Values are timestamped when inserted and are
/// evicted if expired at time of retrieval.
///
/// Note: This cache is in-memory only
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
    fn cache_remove(&mut self, k: &K) -> Option<V> { self.store.remove(k).map(|(_, v)| v) }
    fn cache_clear(&mut self) { self.store.clear(); }
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
        let mut c = SizedCache::with_size(5);
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
    /// This is a regression test to confirm that racing cache sets on a SizedCache
    /// do not cause duplicates to exist in the internal `order`. See issue #7
    fn size_cache_racing_keys_eviction_regression() {
        let mut c = SizedCache::with_size(2);
        c.cache_set(1, 100);
        c.cache_set(1, 100);
        // size would be 1, but internal ordered would be [1, 1]
        c.cache_set(2, 100);
        c.cache_set(3, 100);
        // this next set would fail because a duplicate key would be evicted
        c.cache_set(4, 100);
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

    #[test]
    fn clear() {
        let mut c = UnboundCache::new();

        c.cache_set(1, 100);
        c.cache_set(2, 200);
        c.cache_set(3, 300);

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

        // clear the cache, should have no more elements
        // hits and misses will still be kept
        c.cache_clear();

        assert_eq!(0, c.cache_size());
        assert_eq!(3, c.cache_hits().unwrap());
        assert_eq!(3, c.cache_misses().unwrap());

        let mut c = SizedCache::with_size(3);

        c.cache_set(1, 100);
        c.cache_set(2, 200);
        c.cache_set(3, 300);
        c.cache_clear();

        assert_eq!(0, c.cache_size());

        let mut c = TimedCache::with_lifespan(3600);

        c.cache_set(1, 100);
        c.cache_set(2, 200);
        c.cache_set(3, 300);
        c.cache_clear();

        assert_eq!(0, c.cache_size());
    }

    #[test]
    fn remove() {
        let mut c = UnboundCache::new();

        c.cache_set(1, 100);
        c.cache_set(2, 200);
        c.cache_set(3, 300);

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

        let mut c = SizedCache::with_size(3);

        c.cache_set(1, 100);
        c.cache_set(2, 200);
        c.cache_set(3, 300);

        assert_eq!(Some(100), c.cache_remove(&1));
        assert_eq!(2, c.cache_size());

        assert_eq!(Some(200), c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(None, c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(Some(300), c.cache_remove(&3));
        assert_eq!(0, c.cache_size());

        let mut c = TimedCache::with_lifespan(3600);

        c.cache_set(1, 100);
        c.cache_set(2, 200);
        c.cache_set(3, 300);

        assert_eq!(Some(100), c.cache_remove(&1));
        assert_eq!(2, c.cache_size());
    }
}
