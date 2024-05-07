use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Eq)]
struct CacheArc<T>(Arc<T>);

impl<T> Clone for CacheArc<T> {
    fn clone(&self) -> Self {
        CacheArc(self.0.clone())
    }
}

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

impl Borrow<str> for CacheArc<String> {
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}

impl<T> Borrow<[T]> for CacheArc<Vec<T>> {
    fn borrow(&self) -> &[T] {
        self.0.as_slice()
    }
}

#[derive(Debug)]
pub enum Error {
    /// Calculating expiration `Instant`s resulted in a
    /// value outside of `Instant`s internal bounds
    TimeBounds,
}

#[derive(Hash, Eq, PartialEq)]
struct Stamped<K> {
    tombstone: bool,
    expiry: Instant,
    key: CacheArc<K>,
}

struct Entry<V> {
    stamp_index: usize,
    expiry: Instant,
    value: V,
}

macro_rules! impl_get {
    ($_self:expr, $key:expr) => {{
        let cutoff = Instant::now();
        $_self.map.get($key).and_then(|entry| {
            if entry.expiry < cutoff {
                None
            } else {
                Some(&entry.value)
            }
        })
    }};
}

/// A cache enforcing time expiration and an optional maximum size.
/// When a maximum size is specified, the values are dropped in the
/// order of expiration date, e.g. the next value to expire is dropped.
/// This cache is intended for high read scenarios to allow for concurrent
/// reads while still enforcing expiration and an optional maximum cache size.
///
/// To accomplish this, there are a few trade-offs:
///  - Maximum cache size logic cannot support "LRU", instead dropping the next value to expire
///  - The cache's size, reported by `.len` is only guaranteed to be accurate immediately
///    after a call to either `.evict` or `.retain_latest`
///  - Eviction must be explicitly requested, either on its own or while inserting
///  - Writing to existing keys, removing, evict, or dropping (with `.retain_latest`) will
///    generate tombstones that must eventually be cleared. Clearing tombstones requires
///    a full traversal (`O(n)`) to rewrite internal indices. This happens automatically
///    when the number of tombstones reaches a certain threshold.
pub struct ExpiringSizedCache<K, V> {
    // k/v where entry contains index into `key`
    map: HashMap<CacheArc<K>, Entry<V>>,

    // deque ordered in ascending expiration `Instant`s
    // to support retaining/evicting without full traversal
    keys: VecDeque<Stamped<K>>,

    pub ttl_millis: u64,
    pub size_limit: Option<usize>,
    pub(self) tombstone_count: usize,
    pub(self) max_tombstone_limit: usize,
}
impl<K: Hash + Eq, V> ExpiringSizedCache<K, V> {
    pub fn new(ttl_millis: u64) -> Self {
        Self {
            map: HashMap::new(),
            keys: VecDeque::new(),
            ttl_millis,
            size_limit: None,
            tombstone_count: 0,
            max_tombstone_limit: 50,
        }
    }

    pub fn with_capacity(ttl_millis: u64, size: usize) -> Self {
        let mut new = Self::new(ttl_millis);
        new.map.reserve(size);
        new.keys.reserve(size + new.max_tombstone_limit);
        new
    }

    /// Set a size limit. When reached, the next entries to expire are evicted.
    /// Returns the previous value if one was set.
    pub fn size_limit(&mut self, size: usize) -> Option<usize> {
        let prev = self.size_limit;
        self.size_limit = Some(size);
        prev
    }

    /// Set the max tombstone limit. When reached, tombstones will be cleared and
    /// a full traversal will occur (`O(n)`) to rewrite internal indices
    /// Returns the previous value that was set.
    pub fn max_tombstone_limit(&mut self, limit: usize) -> usize {
        let prev = self.max_tombstone_limit;
        self.max_tombstone_limit = limit;
        prev
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

    /// Evict values that have expired.
    /// Returns number of dropped items.
    pub fn evict(&mut self) -> usize {
        let cutoff = Instant::now();
        let remove = match self
            .keys
            .binary_search_by_key(&cutoff, |stamped| stamped.expiry)
        {
            Ok(mut i) => {
                // move past any duplicates
                while self.keys[i].expiry == cutoff {
                    i += 1;
                }
                i
            }
            Err(i) => {
                // index to insert at, drop those prior
                i
            }
        };
        let mut count = 0;
        for stamped in self.keys.iter() {
            if count >= remove {
                break;
            }
            if !stamped.tombstone {
                self.map.remove(&stamped.key);
                count += 1;
            }
        }
        self.entomb_head(remove);
        self.check_clear_tombstones();
        count
    }

    fn entomb_head(&mut self, remove: usize) {
        let mut stamp_index = 0;
        let mut count = 0;
        loop {
            if count >= remove || stamp_index >= self.keys.len() {
                break;
            }
            let stamped = self.keys.get_mut(stamp_index);
            match stamped {
                None => break,
                Some(stamped) => {
                    if !stamped.tombstone {
                        count += 1;
                        stamped.tombstone = true;
                        self.tombstone_count += 1;
                    }
                    stamp_index += 1;
                }
            }
        }
    }

    /// Retain only the latest `count` values, dropping the next values to expire.
    /// If `evict`, then also evict values that have expired.
    /// Returns number of dropped items.
    pub fn retain_latest(&mut self, count: usize, evict: bool) -> usize {
        let count_index = self.len().saturating_sub(count);

        let remove = if evict {
            let cutoff = Instant::now();
            match self
                .keys
                .binary_search_by_key(&cutoff, |stamped| stamped.expiry)
            {
                Ok(mut i) => {
                    while self.keys[i].expiry == cutoff {
                        i += 1;
                    }
                    count_index.max(i)
                }
                Err(i) => count_index.max(i),
            }
        } else {
            count_index
        };

        let mut count = 0;
        for stamped in self.keys.iter() {
            if count >= remove {
                break;
            }
            if !stamped.tombstone {
                self.map.remove(&stamped.key);
                count += 1;
            }
        }
        self.entomb_head(remove);
        self.check_clear_tombstones();
        count
    }

    fn should_clear_tombstones(&self) -> bool {
        // todo: consider some percentage of `self.size_limit`?
        self.tombstone_count > self.max_tombstone_limit
    }

    fn check_clear_tombstones(&mut self) -> usize {
        if !self.should_clear_tombstones() {
            return 0;
        }

        let mut cleared = 0;
        let mut stamp_index = 0;
        loop {
            if stamp_index >= self.keys.len() {
                break;
            }
            if self.keys[stamp_index].tombstone {
                self.keys
                    .remove(stamp_index)
                    .expect("already checked stamped key exists");
                cleared += 1;
                self.tombstone_count -= 1;
            } else {
                if let Some(entry) = self.map.get_mut(&self.keys[stamp_index].key) {
                    entry.stamp_index = stamp_index;
                }
                stamp_index += 1;
            }
        }
        cleared
    }

    /// Remove an entry, returning the value if it was present.
    /// Note, the value is not checked for expiry. If returning
    /// only non-expired values is desired, run `.evict` prior.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        match self.map.remove(key) {
            None => None,
            Some(removed) => {
                if let Some(stamped) = self.keys.get_mut(removed.stamp_index) {
                    stamped.tombstone = true;
                    self.tombstone_count += 1;
                }
                self.check_clear_tombstones();
                Some(removed.value)
            }
        }
    }

    /// Insert k/v pair without running eviction logic. If a `size_limit` was specified, the
    /// next entry to expire will be evicted to make space. Returns any existing value.
    /// Note, the existing value is not checked for expiry. If returning
    /// only non-expired values is desired, run `.evict` prior or use `.insert_evict(..., true)`
    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, Error> {
        self.insert_evict(key, value, false)
    }

    /// Optionally run eviction logic before inserting a k/v pair. If a `size_limit` was specified,
    /// next entry to expire will be evicted to make space. Returns any existing value.
    /// Note, the existing value is not checked for expiry. If returning
    /// only non-expired values is desired, run `.evict` prior or pass `evict = true`
    pub fn insert_evict(&mut self, key: K, value: V, evict: bool) -> Result<Option<V>, Error> {
        // todo: allow specifying ttl on individual entries, will require
        //       inserting stamped-keys in-place instead of pushing to end

        // optionally evict and retain to size
        if let Some(size_limit) = self.size_limit {
            if self.len() > size_limit - 1 {
                self.retain_latest(size_limit - 1, evict);
            }
        } else if evict {
            self.evict();
        }

        let key = CacheArc(Arc::new(key));
        let expiry = Instant::now()
            .checked_add(Duration::from_millis(self.ttl_millis))
            .ok_or(Error::TimeBounds)?;

        self.keys.push_back(Stamped {
            tombstone: false,
            expiry,
            key: key.clone(),
        });
        let stamp_index = self.keys.len() - 1;
        let old = self.map.insert(
            key,
            Entry {
                stamp_index,
                expiry,
                value,
            },
        );
        if let Some(old) = &old {
            if let Some(old_stamped) = self.keys.get_mut(old.stamp_index) {
                old_stamped.tombstone = true;
                self.tombstone_count += 1;
                self.check_clear_tombstones();
            }
        }
        Ok(old.map(|entry| entry.value))
    }

    /// Clear all cache entries. Does not release underlying containers
    pub fn clear(&mut self) {
        self.map.clear();
        self.keys.clear();
    }

    /// Return cache size. Note, this does not evict so may return
    /// a size that includes expired entries. Run `evict` or `retain_latest`
    /// first to ensure an accurate length.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Retrieve unexpired entry
    pub fn get(&self, key: &K) -> Option<&V> {
        // todo: generically support keys being borrowed types like the underlying map
        impl_get!(self, key)
    }
}

impl<V> ExpiringSizedCache<String, V> {
    /// Retrieve unexpired entry, accepting `&str` to check against `String` keys
    /// ```rust
    /// # use cached::stores::ExpiringSizedCache;
    /// let mut cache = ExpiringSizedCache::<String, &str>::new(2_000);
    /// cache.insert(String::from("a"), "a").unwrap();
    /// assert_eq!(cache.get_borrowed("a").unwrap(), &"a");
    /// ```
    pub fn get_borrowed(&self, key: &str) -> Option<&V> {
        impl_get!(self, key)
    }
}

impl<T: Hash + Eq + PartialEq, V> ExpiringSizedCache<Vec<T>, V> {
    /// Retrieve unexpired entry, accepting `&[T]` to check against `Vec<T>` keys
    /// ```rust
    /// # use cached::stores::ExpiringSizedCache;
    /// let mut cache = ExpiringSizedCache::<Vec<usize>, &str>::new(2_000);
    /// cache.insert(vec![0], "a").unwrap();
    /// assert_eq!(cache.get_borrowed(&[0]).unwrap(), &"a");
    /// ```
    pub fn get_borrowed(&self, key: &[T]) -> Option<&V> {
        impl_get!(self, key)
    }
}

#[cfg(test)]
mod test {
    use crate::stores::ExpiringSizedCache;
    use std::time::Duration;

    #[test]
    fn borrow_keys() {
        let mut cache = ExpiringSizedCache::with_capacity(100, 100);
        cache.insert(String::from("a"), "a").unwrap();
        assert_eq!(cache.get_borrowed("a").unwrap(), &"a");

        let mut cache = ExpiringSizedCache::with_capacity(100, 100);
        cache.insert(vec![0], "a").unwrap();
        assert_eq!(cache.get_borrowed(&[0]).unwrap(), &"a");
    }

    #[test]
    fn kitchen_sink() {
        let mut cache = ExpiringSizedCache::with_capacity(100, 100);
        assert_eq!(0, cache.evict());
        assert_eq!(0, cache.retain_latest(100, true));
        assert!(cache.get(&"a".into()).is_none());

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.get(&"a".into()), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(1, cache.evict());
        assert_eq!(1, cache.tombstone_count);
        assert!(cache.get(&"a".into()).is_none());
        assert_eq!(cache.len(), 0);

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.get(&"a".into()), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(0, cache.retain_latest(1, false));
        // expired
        assert_eq!(cache.get(&"a".into()), None);
        // in size until eviction
        assert_eq!(cache.len(), 1);
        assert_eq!(1, cache.retain_latest(1, true));
        assert_eq!(2, cache.tombstone_count);
        assert!(cache.get(&"a".into()).is_none());
        assert_eq!(cache.len(), 0);

        cache.insert("a".to_string(), "a".to_string()).unwrap();
        cache.insert("b".to_string(), "b".to_string()).unwrap();
        cache.insert("c".to_string(), "c".to_string()).unwrap();
        cache.insert("d".to_string(), "d".to_string()).unwrap();
        cache.insert("e".to_string(), "e".to_string()).unwrap();
        assert_eq!(3, cache.retain_latest(2, false));
        assert_eq!(2, cache.len());
        assert_eq!(5, cache.tombstone_count);
        assert_eq!(cache.get(&"a".into()), None);
        assert_eq!(cache.get(&"b".into()), None);
        assert_eq!(cache.get(&"c".into()), None);
        assert_eq!(cache.get(&"d".into()), Some("d".to_string()).as_ref());
        assert_eq!(cache.get(&"e".into()), Some("e".to_string()).as_ref());

        cache.insert("a".to_string(), "a".to_string()).unwrap();
        cache.insert("a".to_string(), "a".to_string()).unwrap();
        cache.insert("b".to_string(), "b".to_string()).unwrap();
        cache.insert("b".to_string(), "b".to_string()).unwrap();
        assert_eq!(4, cache.len());
        assert_eq!(7, cache.tombstone_count);

        assert_eq!(2, cache.retain_latest(2, false));
        assert_eq!(cache.get(&"d".into()), None);
        assert_eq!(cache.get(&"e".into()), None);
        assert_eq!(cache.get(&"a".into()), Some("a".to_string()).as_ref());
        assert_eq!(cache.get(&"b".into()), Some("b".to_string()).as_ref());
        assert_eq!(2, cache.len());
        assert_eq!(9, cache.tombstone_count);

        assert_eq!(cache.remove(&"a".into()), Some("a".to_string()));
        assert_eq!(10, cache.tombstone_count);
    }

    #[test]
    fn size_limit() {
        let mut cache = ExpiringSizedCache::with_capacity(100, 100);
        cache.size_limit(2);
        assert_eq!(0, cache.evict());
        assert_eq!(0, cache.retain_latest(100, true));
        assert!(cache.get(&"a".into()).is_none());

        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.get(&"a".into()), Some("A".to_string()).as_ref());
        assert_eq!(cache.len(), 1);
        cache.insert("b".to_string(), "B".to_string()).unwrap();
        assert_eq!(cache.get(&"b".into()), Some("B".to_string()).as_ref());
        assert_eq!(cache.len(), 2);
        cache.insert("c".to_string(), "C".to_string()).unwrap();
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&"b".into()), Some("B".to_string()).as_ref());
        assert_eq!(cache.get(&"c".into()), Some("C".to_string()).as_ref());
        assert_eq!(cache.get(&"a".into()), None);
    }

    #[test]
    fn tombstones() {
        let mut cache = ExpiringSizedCache::with_capacity(100, 100);
        cache.size_limit(2);
        for _ in 0..=cache.max_tombstone_limit {
            cache.insert("a".to_string(), "A".to_string()).unwrap();
        }
        assert_eq!(cache.tombstone_count, cache.max_tombstone_limit);
        cache.insert("a".to_string(), "A".to_string()).unwrap();
        assert_eq!(cache.tombstone_count, 0);
    }
}
