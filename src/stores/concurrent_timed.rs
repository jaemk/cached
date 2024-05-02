use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Eq)]
struct CacheArc<T>(Arc<T>);

impl<T: PartialEq> PartialEq for CacheArc<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq(&other.0)
    }
}

impl<T: Hash> Hash for CacheArc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<T> Borrow<T> for CacheArc<T> {
    fn borrow(&self) -> &T {
        &self.0
    }
}

#[derive(Debug)]
pub enum Error {
    Time,
}

pub struct ConcurrentTimedCache<K, V> {
    map: HashMap<CacheArc<K>, (Instant, V)>,
    keys: VecDeque<(Instant, CacheArc<K>)>,
    pub ttl_millis: u64,
}
impl<K: Hash + Eq + Clone, V> ConcurrentTimedCache<K, V> {
    pub fn new(ttl_millis: u64) -> Self {
        Self {
            map: HashMap::new(),
            keys: VecDeque::new(),
            ttl_millis,
        }
    }

    pub fn with_capacity(ttl_millis: u64, size: usize) -> Self {
        let mut new = Self::new(ttl_millis);
        new.reserve(size);
        new
    }

    /// Increase backing stores with enough capacity to store `more`
    pub fn reserve(&mut self, more: usize) {
        self.map.reserve(more);
        self.keys.reserve(more);
    }

    /// Set ttl millis, return previous value
    pub fn ttl_millis(&mut self, ttl_millis: u64) -> u64 {
        let prev = self.ttl_millis;
        self.ttl_millis = ttl_millis;
        prev
    }

    /// Evict values that have expired
    pub fn evict(&mut self) -> Result<usize, Error> {
        let cutoff = Instant::now()
            .checked_sub(Duration::from_millis(self.ttl_millis))
            .ok_or(Error::Time)?;
        let remove = match self.keys.binary_search_by_key(&cutoff, |&(inst, _)| inst) {
            Ok(mut i) => {
                // move past any duplicates
                while self.keys[i].0 == cutoff {
                    i += 1;
                }
                i
            }
            Err(i) => {
                // index to insert at, drop those prior
                i
            }
        };
        for (_inst, arc) in self.keys.range(..remove) {
            self.map.remove(arc);
        }
        self.remove_head(remove);
        Ok(remove)
    }

    fn remove_head(&mut self, count: usize) {
        for _ in 0..count {
            self.keys.pop_front();
        }
    }

    /// Retain only the latest `count` values. If `evict`, then also evict values that have expired
    pub fn retain_latest(&mut self, count: usize, evict: bool) -> Result<usize, Error> {
        let cutoff = Instant::now()
            .checked_sub(Duration::from_millis(self.ttl_millis))
            .ok_or(Error::Time)?;

        let count_index = self.keys.len().saturating_sub(count);

        let remove = if evict {
            match self.keys.binary_search_by_key(&cutoff, |&(inst, _)| inst) {
                Ok(mut i) => {
                    while self.keys[i].0 == cutoff {
                        i += 1;
                    }
                    count_index.max(i)
                }
                Err(i) => count_index.max(i),
            }
        } else {
            count_index
        };
        for (_inst, arc) in self.keys.range(..remove) {
            self.map.remove(arc);
        }
        self.remove_head(remove);
        Ok(remove)
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        Instant::now()
            .checked_sub(Duration::from_millis(self.ttl_millis))
            .and_then(|cutoff| {
                self.map
                    .get(key)
                    .and_then(|(inst, v)| if *inst < cutoff { None } else { Some(v) })
            })
    }

    pub fn insert(&mut self, key: K, value: V) -> Result<(), Error> {
        // optionally evict and retain to size
        let arc = CacheArc(Arc::new(key));
        let expiry = Instant::now()
            .checked_add(Duration::from_millis(self.ttl_millis))
            .ok_or(Error::Time)?;
        self.keys.push_back((expiry, arc.clone()));
        self.map.insert(arc, (expiry, value));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

#[cfg(test)]
mod test {
    use crate::stores::ConcurrentTimedCache;
    use std::time::Duration;

    #[test]
    fn kitchen_sink() {
        let mut cache = ConcurrentTimedCache::with_capacity(100, 100);
        assert_eq!(0, cache.evict().unwrap());
        assert_eq!(0, cache.retain_latest(100, true).unwrap());
        assert!(cache.get(&"a".into()).is_none());

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.get(&"a".into()), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(1, cache.evict().unwrap());
        assert!(cache.get(&"a".into()).is_none());
        assert_eq!(cache.len(), 0);

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.get(&"a".into()), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(0, cache.retain_latest(1, false).unwrap());
        // assert_eq!(cache.get(&"a".into()), Some("A".to_string()).as_ref());
        // assert_eq!(cache.len(), 1);
        // assert_eq!(1, cache.retain_latest(1, true).unwrap());
        // assert!(cache.get(&"a".into()).is_none());
        // assert_eq!(cache.len(), 0);
    }
}
