/*!
Implementation of various caches

*/

use std::cmp::Eq;
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::time::Instant;

use super::Cached;

use std::collections::hash_map::{Entry, RandomState};

use hashbrown::raw::RawTable;

#[cfg(feature = "async")]
use {super::CachedAsync, async_trait::async_trait, futures::Future};

/// Default unbounded cache
///
/// This cache has no size limit or eviction policy.
///
/// Note: This cache is in-memory only
#[derive(Clone, Debug)]
pub struct UnboundCache<K, V> {
    store: HashMap<K, V>,
    hits: u64,
    misses: u64,
    initial_capacity: Option<usize>,
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

/// Limited functionality doubly linked list using Vec as storage.
#[derive(Clone, Debug)]
struct LRUList<T> {
    values: Vec<ListEntry<T>>,
}

#[derive(Clone, Debug)]
struct ListEntry<T> {
    value: Option<T>,
    next: usize,
    prev: usize,
}

/// Free and occupied cells are each linked into a cyclic list with one auxiliary cell.
/// Cell #0 is on the list of free cells, element #1 is on the list of occupied cells.
///
impl<T> LRUList<T> {
    const FREE: usize = 0;
    const OCCUPIED: usize = 1;

    fn with_capacity(capacity: usize) -> LRUList<T> {
        let mut values = Vec::with_capacity(capacity + 2);
        values.push(ListEntry::<T> {
            value: None,
            next: 0,
            prev: 0,
        });
        values.push(ListEntry::<T> {
            value: None,
            next: 1,
            prev: 1,
        });
        LRUList { values }
    }

    fn unlink(&mut self, index: usize) {
        let prev = self.values[index].prev;
        let next = self.values[index].next;
        self.values[prev].next = next;
        self.values[next].prev = prev;
    }

    fn link_after(&mut self, index: usize, prev: usize) {
        let next = self.values[prev].next;
        self.values[index].prev = prev;
        self.values[index].next = next;
        self.values[prev].next = index;
        self.values[next].prev = index;
    }

    fn move_to_front(&mut self, index: usize) {
        self.unlink(index);
        self.link_after(index, Self::OCCUPIED);
    }

    fn push_front(&mut self, value: T) -> usize {
        if self.values[Self::FREE].next == Self::FREE {
            self.values.push(ListEntry::<T> {
                value: None,
                next: Self::FREE,
                prev: Self::FREE,
            });
            self.values[Self::FREE].next = self.values.len() - 1;
        }
        let index = self.values[Self::FREE].next;
        self.values[index].value = Some(value);
        self.unlink(index);
        self.link_after(index, Self::OCCUPIED);
        index
    }

    fn remove(&mut self, index: usize) -> T {
        self.unlink(index);
        self.link_after(index, Self::FREE);
        self.values[index].value.take().expect("invalid index")
    }

    fn back(&self) -> usize {
        self.values[Self::OCCUPIED].prev
    }

    fn get(&self, index: usize) -> &T {
        self.values[index].value.as_ref().expect("invalid index")
    }

    fn get_mut(&mut self, index: usize) -> &mut T {
        self.values[index].value.as_mut().expect("invalid index")
    }

    fn set(&mut self, index: usize, value: T) -> Option<T> {
        self.values[index].value.replace(value)
    }

    fn clear(&mut self) {
        self.values.clear();
        self.values.push(ListEntry::<T> {
            value: None,
            next: 0,
            prev: 0,
        });
        self.values.push(ListEntry::<T> {
            value: None,
            next: 1,
            prev: 1,
        });
    }

    fn iter(&self) -> LRUListIterator<T> {
        LRUListIterator::<T> {
            list: self,
            index: Self::OCCUPIED,
        }
    }
}

#[derive(Debug)]
struct LRUListIterator<'a, T> {
    list: &'a LRUList<T>,
    index: usize,
}

impl<'a, T> Iterator for LRUListIterator<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.list.values[self.index].next;
        if next == LRUList::<T>::OCCUPIED {
            None
        } else {
            let value = self.list.values[next].value.as_ref();
            self.index = next;
            value
        }
    }
}

/// Least Recently Used / `Sized` Cache
///
/// Stores up to a specified size before beginning
/// to evict the least recently used keys
///
/// Note: This cache is in-memory only
#[derive(Clone)]
pub struct SizedCache<K, V> {
    store: RawTable<usize>,
    hash_builder: RandomState,
    order: LRUList<(K, V)>,
    capacity: usize,
    hits: u64,
    misses: u64,
}

impl<K, V> fmt::Debug for SizedCache<K, V>
where
    K: fmt::Debug,
    V: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SizedCache")
            .field("order", &self.order)
            .field("capacity", &self.capacity)
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish()
    }
}

impl<K, V> PartialEq for SizedCache<K, V>
where
    K: Eq + Hash + Clone,
    V: PartialEq,
{
    fn eq(&self, other: &SizedCache<K, V>) -> bool {
        self.store.len() == other.store.len() && {
            self.order
                .iter()
                .all(|(key, value)| match other.get_index(other.hash(key), key) {
                    Some(i) => value == &other.order.get(i).1,
                    None => false,
                })
        }
    }
}

impl<K, V> Eq for SizedCache<K, V>
where
    K: Eq + Hash + Clone,
    V: PartialEq,
{
}

impl<K: Hash + Eq + Clone, V> SizedCache<K, V> {
    #[deprecated(since = "0.5.1", note = "method renamed to `with_size`")]
    pub fn with_capacity(size: usize) -> SizedCache<K, V> {
        Self::with_size(size)
    }

    /// Creates a new `SizedCache` with a given size limit and pre-allocated backing data
    pub fn with_size(size: usize) -> SizedCache<K, V> {
        if size == 0 {
            panic!("`size` of `SizedCache` must be greater than zero.")
        }
        SizedCache {
            store: RawTable::with_capacity(size),
            hash_builder: RandomState::new(),
            order: LRUList::<(K, V)>::with_capacity(size),
            capacity: size,
            hits: 0,
            misses: 0,
        }
    }

    fn iter_order(&self) -> impl Iterator<Item = &(K, V)> {
        self.order.iter()
    }

    /// Return an iterator of keys in the current order from most
    /// to least recently used.
    pub fn key_order(&self) -> impl Iterator<Item = &K> {
        self.order.iter().map(|(k, _v)| k)
    }

    /// Return an iterator of values in the current order from most
    /// to least recently used.
    pub fn value_order(&self) -> impl Iterator<Item = &V> {
        self.order.iter().map(|(_k, v)| v)
    }

    fn hash(&self, key: &K) -> u64 {
        let hasher = &mut self.hash_builder.build_hasher();
        key.hash(hasher);
        hasher.finish()
    }

    fn insert_index(&mut self, hash: u64, index: usize) {
        let Self {
            ref mut store,
            ref order,
            ref hash_builder,
            ..
        } = *self;
        store.insert(hash, index, move |&i| {
            let hasher = &mut hash_builder.build_hasher();
            order.get(i).0.hash(hasher);
            hasher.finish()
        });
    }

    fn get_index(&self, hash: u64, key: &K) -> Option<usize> {
        let Self { store, order, .. } = self;
        store.get(hash, |&i| *key == order.get(i).0).copied()
    }

    fn remove_index(&mut self, hash: u64, key: &K) -> Option<usize> {
        let Self { store, order, .. } = self;
        store.remove_entry(hash, |&i| *key == order.get(i).0)
    }

    fn check_capacity(&mut self) {
        if self.store.len() >= self.capacity {
            // store has reached capacity, evict the oldest item.
            // store capacity cannot be zero, so there must be content in `self.order`.
            let index = self.order.back();
            let key = &self.order.get(index).0;
            let hash = self.hash(key);

            let order = &self.order;
            let erased = self.store.erase_entry(hash, |&i| *key == order.get(i).0);
            assert!(erased, "SizedCache::cache_set failed evicting cache key");
            self.order.remove(index);
        }
    }

    fn get_if<F: FnOnce(&V) -> bool>(&mut self, key: &K, is_valid: F) -> Option<&V> {
        if let Some(index) = self.get_index(self.hash(key), key) {
            if is_valid(&self.order.get(index).1) {
                self.order.move_to_front(index);
                self.hits += 1;
                return Some(&self.order.get(index).1);
            }
        }
        self.misses += 1;
        None
    }

    fn get_mut_if<F: FnOnce(&V) -> bool>(&mut self, key: &K, is_valid: F) -> Option<&mut V> {
        if let Some(index) = self.get_index(self.hash(key), key) {
            if is_valid(&self.order.get(index).1) {
                self.order.move_to_front(index);
                self.hits += 1;
                return Some(&mut self.order.get_mut(index).1);
            }
        }
        self.misses += 1;
        None
    }

    /// Get the cached value, or set it using `f` if the value
    /// is either not-set or if `is_valid` returns `false` for
    /// the set value.
    ///
    /// Returns (was_present, was_valid, mut ref to set value)
    /// `was_valid` will be false when `was_present` is false
    fn get_or_set_with_if<F: FnOnce() -> V, FC: FnOnce(&V) -> bool>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> (bool, bool, &mut V) {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            self.hits += 1;
            let replace_existing = {
                let v = &self.order.get(index).1;
                !is_valid(v)
            };
            if replace_existing {
                self.order.set(index, (key, f()));
            }
            self.order.move_to_front(index);
            (true, !replace_existing, &mut self.order.get_mut(index).1)
        } else {
            self.check_capacity();
            self.misses += 1;
            let index = self.order.push_front((key, f()));
            self.insert_index(hash, index);
            (false, false, &mut self.order.get_mut(index).1)
        }
    }

    #[allow(dead_code)]
    fn try_get_or_set_with_if<E, F: FnOnce() -> Result<V, E>, FC: FnOnce(&V) -> bool>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> Result<(bool, bool, &mut V), E> {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            self.hits += 1;
            let replace_existing = {
                let v = &self.order.get(index).1;
                !is_valid(v)
            };
            if replace_existing {
                self.order.set(index, (key, f()?));
            }
            self.order.move_to_front(index);
            Ok((true, !replace_existing, &mut self.order.get_mut(index).1))
        } else {
            self.check_capacity();
            self.misses += 1;
            let index = self.order.push_front((key, f()?));
            self.insert_index(hash, index);
            Ok((false, false, &mut self.order.get_mut(index).1))
        }
    }
}

#[cfg(feature = "async")]
impl<K, V> SizedCache<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    /// Get the cached value, or set it using `f` if the value
    /// is either not-set or if `is_valid` returns `false` for
    /// the set value.
    ///
    /// Returns (was_present, was_valid, mut ref to set value)
    /// `was_valid` will be false when `was_present` is false
    async fn get_or_set_with_if_async<F, Fut, FC>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> (bool, bool, &mut V)
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
        FC: FnOnce(&V) -> bool,
    {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            self.hits += 1;
            let replace_existing = {
                let v = &self.order.get(index).1;
                !is_valid(v)
            };
            if replace_existing {
                self.order.set(index, (key, f().await));
            }
            self.order.move_to_front(index);
            (true, !replace_existing, &mut self.order.get_mut(index).1)
        } else {
            self.check_capacity();
            self.misses += 1;
            let index = self.order.push_front((key, f().await));
            self.insert_index(hash, index);
            (false, false, &mut self.order.get_mut(index).1)
        }
    }

    async fn try_get_or_set_with_if_async<E, F, Fut, FC>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> Result<(bool, bool, &mut V), E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
        FC: FnOnce(&V) -> bool,
    {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            self.hits += 1;
            let replace_existing = {
                let v = &self.order.get(index).1;
                !is_valid(v)
            };
            if replace_existing {
                self.order.set(index, (key, f().await?));
            }
            self.order.move_to_front(index);
            Ok((true, !replace_existing, &mut self.order.get_mut(index).1))
        } else {
            self.check_capacity();
            self.misses += 1;
            let index = self.order.push_front((key, f().await?));
            self.insert_index(hash, index);
            Ok((false, false, &mut self.order.get_mut(index).1))
        }
    }
}

impl<K: Hash + Eq + Clone, V> Cached<K, V> for SizedCache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        self.get_if(key, |_| true)
    }

    fn cache_get_mut(&mut self, key: &K) -> std::option::Option<&mut V> {
        self.get_mut_if(key, |_| true)
    }

    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        self.check_capacity();
        let hash = self.hash(&key);
        if let Some(index) = self.get_index(hash, &key) {
            self.order.set(index, (key, val)).map(|(_, v)| v)
        } else {
            let index = self.order.push_front((key, val));
            self.insert_index(hash, index);
            None
        }
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        let (_, _, v) = self.get_or_set_with_if(key, f, |_| true);
        v
    }

    fn cache_remove(&mut self, k: &K) -> Option<V> {
        // try and remove item from mapping, and then from order list if it was in mapping
        let hash = self.hash(&k);
        if let Some(index) = self.remove_index(hash, k) {
            // need to remove the key in the order list
            let (_key, value) = self.order.remove(index);
            Some(value)
        } else {
            None
        }
    }
    fn cache_clear(&mut self) {
        // clear both the store and the order list
        self.store.clear();
        self.order.clear();
    }
    fn cache_reset(&mut self) {
        // SizedCache uses cache_clear because capacity is fixed.
        self.cache_clear();
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
    fn cache_capacity(&self) -> Option<usize> {
        Some(self.capacity)
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for SizedCache<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    async fn get_or_set_with<F, Fut>(&mut self, k: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        let (_, _, v) = self.get_or_set_with_if_async(k, f, |_| true).await;
        v
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, k: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let (_, _, v) = self.try_get_or_set_with_if_async(k, f, |_| true).await?;
        Ok(v)
    }
}

/// Enum used for defining the status of time-cached values
#[derive(Debug)]
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
#[derive(Clone, Debug)]
pub struct TimedCache<K, V> {
    store: HashMap<K, (Instant, V)>,
    seconds: u64,
    hits: u64,
    misses: u64,
    initial_capacity: Option<usize>,
}

impl<K: Hash + Eq, V> TimedCache<K, V> {
    /// Creates a new `TimedCache` with a specified lifespan
    pub fn with_lifespan(seconds: u64) -> TimedCache<K, V> {
        TimedCache {
            store: Self::new_store(None),
            seconds,
            hits: 0,
            misses: 0,
            initial_capacity: None,
        }
    }

    /// Creates a new `TimedCache` with a specified lifespan and
    /// cache-store with the specified pre-allocated capacity
    pub fn with_lifespan_and_capacity(seconds: u64, size: usize) -> TimedCache<K, V> {
        TimedCache {
            store: Self::new_store(Some(size)),
            seconds,
            hits: 0,
            misses: 0,
            initial_capacity: Some(size),
        }
    }

    fn new_store(capacity: Option<usize>) -> HashMap<K, (Instant, V)> {
        capacity.map_or_else(HashMap::new, HashMap::with_capacity)
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
            Status::Found => {
                self.hits += 1;
                self.store.get(key).map(|stamped| &stamped.1)
            }
            Status::Expired => {
                self.misses += 1;
                self.store.remove(key).unwrap();
                None
            }
        }
    }

    fn cache_get_mut(&mut self, key: &K) -> Option<&mut V> {
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
            Status::Found => {
                self.hits += 1;
                self.store.get_mut(key).map(|stamped| &mut stamped.1)
            }
            Status::Expired => {
                self.misses += 1;
                self.store.remove(key).unwrap();
                None
            }
        }
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                if occupied.get().0.elapsed().as_secs() < self.seconds {
                    self.hits += 1;
                } else {
                    self.misses += 1;
                    let val = f();
                    occupied.insert((Instant::now(), val));
                }
                &mut occupied.into_mut().1
            }
            Entry::Vacant(vacant) => {
                self.misses += 1;
                let val = f();
                &mut vacant.insert((Instant::now(), val)).1
            }
        }
    }

    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let stamped = (Instant::now(), val);
        self.store.insert(key, stamped).map(|(_, v)| v)
    }
    fn cache_remove(&mut self, k: &K) -> Option<V> {
        self.store.remove(k).map(|(_, v)| v)
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
    fn cache_lifespan(&self) -> Option<u64> {
        Some(self.seconds)
    }

    fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
        let old = self.seconds;
        self.seconds = seconds;
        Some(old)
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for TimedCache<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    async fn get_or_set_with<F, Fut>(&mut self, k: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        match self.store.entry(k) {
            Entry::Occupied(mut occupied) => {
                if occupied.get().0.elapsed().as_secs() < self.seconds {
                    self.hits += 1;
                } else {
                    self.misses += 1;
                    occupied.insert((Instant::now(), f().await));
                }
                &mut occupied.into_mut().1
            }
            Entry::Vacant(vacant) => {
                self.misses += 1;
                &mut vacant.insert((Instant::now(), f().await)).1
            }
        }
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, k: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let v = match self.store.entry(k) {
            Entry::Occupied(mut occupied) => {
                if occupied.get().0.elapsed().as_secs() < self.seconds {
                    self.hits += 1;
                } else {
                    self.misses += 1;
                    occupied.insert((Instant::now(), f().await?));
                }
                &mut occupied.into_mut().1
            }
            Entry::Vacant(vacant) => {
                self.misses += 1;
                &mut vacant.insert((Instant::now(), f().await?)).1
            }
        };

        Ok(v)
    }
}

/// Timed LRU Cache
///
/// Stores a limited number of values,
/// evicting expired and least-used entries.
/// Time expiration is determined based on entry insertion time..
/// The TTL of an entry is not updated when retrieved.
///
/// Note: This cache is in-memory only
#[derive(Clone, Debug)]
pub struct TimedSizedCache<K, V> {
    store: SizedCache<K, (Instant, V)>,
    size: usize,
    seconds: u64,
    hits: u64,
    misses: u64,
}

impl<K: Hash + Eq + Clone, V> TimedSizedCache<K, V> {
    /// Creates a new `SizedCache` with a given size limit and pre-allocated backing data
    pub fn with_size_and_lifespan(size: usize, seconds: u64) -> TimedSizedCache<K, V> {
        if size == 0 {
            panic!("`size` of `TimedSizedCache` must be greater than zero.")
        }
        TimedSizedCache {
            store: SizedCache::with_size(size),
            size,
            seconds,
            hits: 0,
            misses: 0,
        }
    }

    fn iter_order(&self) -> impl Iterator<Item = &(K, (Instant, V))> {
        let max_seconds = self.seconds;
        self.store
            .iter_order()
            .filter(move |(_k, stamped)| stamped.0.elapsed().as_secs() < max_seconds)
    }

    /// Return an iterator of keys in the current order from most
    /// to least recently used.
    /// Items passed their expiration seconds will be excluded.
    pub fn key_order(&self) -> impl Iterator<Item = &K> {
        self.iter_order().map(|(k, _v)| k)
    }

    /// Return an iterator of timestamped values in the current order
    /// from most to least recently used.
    /// Items passed their expiration seconds will be excluded.
    pub fn value_order(&self) -> impl Iterator<Item = &(Instant, V)> {
        self.iter_order().map(|(_k, v)| v)
    }
}

impl<K: Hash + Eq + Clone, V> Cached<K, V> for TimedSizedCache<K, V> {
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        let max_seconds = self.seconds;
        let val = self
            .store
            .get_if(key, |stamped| stamped.0.elapsed().as_secs() < max_seconds);
        match val {
            None => {
                self.misses += 1;
                None
            }
            Some(stamped) => {
                self.hits += 1;
                Some(&stamped.1)
            }
        }
    }

    fn cache_get_mut(&mut self, key: &K) -> std::option::Option<&mut V> {
        let max_seconds = self.seconds;
        let val = self
            .store
            .get_mut_if(key, |stamped| stamped.0.elapsed().as_secs() < max_seconds);
        match val {
            None => {
                self.misses += 1;
                None
            }
            Some(stamped) => {
                self.hits += 1;
                Some(&mut stamped.1)
            }
        }
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        let setter = || (Instant::now(), f());
        let max_seconds = self.seconds;
        let (was_present, was_valid, stamped) =
            self.store.get_or_set_with_if(key, setter, |stamped| {
                stamped.0.elapsed().as_secs() < max_seconds
            });
        if was_present && was_valid {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        &mut stamped.1
    }

    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let stamped = self.store.cache_set(key, (Instant::now(), val));
        stamped.map(|stamped| stamped.1)
    }

    fn cache_remove(&mut self, k: &K) -> Option<V> {
        let stamped = self.store.cache_remove(k);
        stamped.map(|stamped| stamped.1)
    }
    fn cache_clear(&mut self) {
        self.store.cache_clear();
    }
    fn cache_reset(&mut self) {
        self.cache_clear();
    }
    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits)
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses)
    }
    fn cache_capacity(&self) -> Option<usize> {
        Some(self.size)
    }
    fn cache_lifespan(&self) -> Option<u64> {
        Some(self.seconds)
    }
    fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
        let old = self.seconds;
        self.seconds = seconds;
        Some(old)
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for TimedSizedCache<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    async fn get_or_set_with<F, Fut>(&mut self, key: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        let setter = || async { (Instant::now(), f().await) };
        let max_seconds = self.seconds;
        let (was_present, was_valid, stamped) = self
            .store
            .get_or_set_with_if_async(key, setter, |stamped| {
                stamped.0.elapsed().as_secs() < max_seconds
            })
            .await;
        if was_present && was_valid {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        &mut stamped.1
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, key: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let setter = || async {
            let new_val = f().await?;
            Ok((Instant::now(), new_val))
        };
        let max_seconds = self.seconds;
        let (was_present, was_valid, stamped) = self
            .store
            .try_get_or_set_with_if_async(key, setter, |stamped| {
                stamped.0.elapsed().as_secs() < max_seconds
            })
            .await?;
        if was_present && was_valid {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        Ok(&mut stamped.1)
    }
}

impl<K: Hash + Eq, V> Cached<K, V> for HashMap<K, V> {
    fn cache_get(&mut self, k: &K) -> Option<&V> {
        self.get(k)
    }
    fn cache_get_mut(&mut self, k: &K) -> Option<&mut V> {
        self.get_mut(k)
    }
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.entry(key).or_insert_with(f)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        self.insert(k, v)
    }
    fn cache_remove(&mut self, k: &K) -> Option<V> {
        self.remove(k)
    }
    fn cache_clear(&mut self) {
        self.clear();
    }
    fn cache_reset(&mut self) {
        *self = HashMap::new();
    }
    fn cache_size(&self) -> usize {
        self.len()
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for HashMap<K, V>
where
    K: Hash + Eq + Clone + Send,
{
    async fn get_or_set_with<F, Fut>(&mut self, k: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        match self.entry(k) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(f().await),
        }
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, k: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let v = match self.entry(k) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(f().await?),
        };

        Ok(v)
    }
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::Cached;

    use super::SizedCache;
    use super::TimedCache;
    use super::TimedSizedCache;
    use super::UnboundCache;

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
    fn sized_cache() {
        let mut c = SizedCache::with_size(5);
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(2, 100), None);
        assert_eq!(c.cache_set(3, 100), None);
        assert_eq!(c.cache_set(4, 100), None);
        assert_eq!(c.cache_set(5, 100), None);

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [5, 4, 3, 2, 1]);

        assert_eq!(c.cache_set(6, 100), None);
        assert_eq!(c.cache_set(7, 100), None);

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
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(1, 100), Some(100));
        // size would be 1, but internal ordered would be [1, 1]
        assert_eq!(c.cache_set(2, 100), None);
        assert_eq!(c.cache_set(3, 100), None);
        // this next set would fail because a duplicate key would be evicted
        assert_eq!(c.cache_set(4, 100), None);
    }

    #[test]
    fn timed_cache() {
        let mut c = TimedCache::with_lifespan(2);
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        sleep(Duration::new(2, 0));
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(2, misses);

        let old = c.cache_set_lifespan(1).unwrap();
        assert_eq!(2, old);
        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(2, hits);
        assert_eq!(2, misses);

        sleep(Duration::new(1, 0));
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(3, misses);
    }

    #[test]
    fn timed_sized_cache() {
        let mut c = TimedSizedCache::with_size_and_lifespan(5, 2);
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(2, 100), None);
        assert_eq!(c.cache_set(3, 100), None);
        assert_eq!(c.cache_set(4, 100), None);
        assert_eq!(c.cache_set(5, 100), None);

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [5, 4, 3, 2, 1]);

        sleep(Duration::new(1, 0));

        assert_eq!(c.cache_set(6, 100), None);
        assert_eq!(c.cache_set(7, 100), None);

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [7, 6, 5, 4, 3]);

        assert!(c.cache_get(&2).is_none());
        assert!(c.cache_get(&3).is_some());

        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [3, 7, 6, 5, 4]);

        assert_eq!(2, c.cache_misses().unwrap());
        assert_eq!(5, c.cache_size());

        sleep(Duration::new(1, 0));

        assert!(c.cache_get(&1).is_none());
        assert!(c.cache_get(&2).is_none());
        assert!(c.cache_get(&3).is_none());
        assert!(c.cache_get(&4).is_none());
        assert!(c.cache_get(&5).is_none());
        assert!(c.cache_get(&6).is_some());
        assert!(c.cache_get(&7).is_some());

        assert_eq!(7, c.cache_misses().unwrap());

        assert!(c.cache_set(1, 100).is_none());
        assert!(c.cache_set(2, 100).is_none());
        assert!(c.cache_set(3, 100).is_none());
        assert_eq!(c.key_order().cloned().collect::<Vec<_>>(), [3, 2, 1, 7, 6]);

        sleep(Duration::new(1, 0));

        assert!(c.cache_get(&1).is_some());
        assert!(c.cache_get(&2).is_some());
        assert!(c.cache_get(&3).is_some());
        assert!(c.cache_get(&4).is_none());
        assert!(c.cache_get(&5).is_none());
        assert!(c.cache_get(&6).is_none());
        assert!(c.cache_get(&7).is_none());

        assert_eq!(11, c.cache_misses().unwrap());

        let mut c = TimedSizedCache::with_size_and_lifespan(5, 0);
        let mut ticker = 0;
        let setter = || {
            let v = ticker;
            ticker += 1;
            v
        };
        assert_eq!(c.cache_get_or_set_with(1, setter), &0);
        let setter = || {
            let v = ticker;
            ticker += 1;
            v
        };
        assert_eq!(c.cache_get_or_set_with(1, setter), &1);
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

        let mut c = SizedCache::with_size(3);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        c.cache_clear();

        assert_eq!(0, c.cache_size());

        let mut c = TimedCache::with_lifespan(3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        c.cache_clear();

        assert_eq!(0, c.cache_size());

        let mut c = TimedSizedCache::with_size_and_lifespan(3, 3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        c.cache_clear();

        assert_eq!(0, c.cache_size());
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

        let mut c = SizedCache::with_size(init_capacity);
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(init_capacity <= c.store.capacity());

        c.cache_reset();

        assert!(init_capacity <= c.store.capacity());

        let mut c = TimedCache::with_lifespan(100);
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(3 <= c.store.capacity());

        c.cache_reset();

        assert_eq!(0, c.store.capacity());

        let mut c = TimedCache::with_lifespan_and_capacity(100, init_capacity);
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(3 <= c.store.capacity());

        c.cache_reset();

        assert!(init_capacity <= c.store.capacity());

        let mut c = TimedSizedCache::with_size_and_lifespan(init_capacity, 100);
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert!(init_capacity <= c.store.capacity);

        c.cache_reset();
        assert!(init_capacity <= c.store.capacity);
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

        let mut c = SizedCache::with_size(3);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        assert_eq!(Some(100), c.cache_remove(&1));
        assert_eq!(2, c.cache_size());

        assert_eq!(Some(200), c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(None, c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(Some(300), c.cache_remove(&3));
        assert_eq!(0, c.cache_size());

        let mut c = TimedCache::with_lifespan(3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        assert_eq!(Some(100), c.cache_remove(&1));
        assert_eq!(2, c.cache_size());

        let mut c = TimedSizedCache::with_size_and_lifespan(3, 3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        assert_eq!(Some(100), c.cache_remove(&1));
        assert_eq!(2, c.cache_size());

        assert_eq!(Some(200), c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(None, c.cache_remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(Some(300), c.cache_remove(&3));
        assert_eq!(0, c.cache_size());
    }

    #[test]
    fn sized_cache_get_mut() {
        let mut c = SizedCache::with_size(5);
        assert!(c.cache_get_mut(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(*c.cache_get_mut(&1).unwrap(), 100);
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        let value = c.cache_get_mut(&1).unwrap();
        *value = 10;

        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(2, hits);
        assert_eq!(1, misses);
        assert_eq!(*c.cache_get_mut(&1).unwrap(), 10);
    }

    #[test]
    fn hashmap() {
        let mut c = std::collections::HashMap::new();
        assert!(c.cache_get(&1).is_none());
        assert_eq!(c.cache_misses(), None);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(c.cache_hits(), None);
        assert_eq!(c.cache_misses(), None);
    }

    #[test]
    fn get_or_set_with() {
        let mut c = SizedCache::with_size(5);

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);
        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_get_or_set_with(2, || 2), &2);
        assert_eq!(c.cache_get_or_set_with(3, || 3), &3);
        assert_eq!(c.cache_get_or_set_with(4, || 4), &4);
        assert_eq!(c.cache_get_or_set_with(5, || 5), &5);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);

        assert_eq!(c.cache_misses(), Some(7));

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);

        assert_eq!(c.cache_misses(), Some(7));

        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);

        assert_eq!(c.cache_misses(), Some(8));

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

        let mut c = TimedCache::with_lifespan(2);

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

        sleep(Duration::new(2, 0));

        assert_eq!(c.cache_get_or_set_with(1, || 42), &42);

        assert_eq!(c.cache_misses(), Some(7));

        let mut c = TimedSizedCache::with_size_and_lifespan(5, 2);

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);
        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_get_or_set_with(2, || 2), &2);
        assert_eq!(c.cache_get_or_set_with(3, || 3), &3);
        assert_eq!(c.cache_get_or_set_with(4, || 4), &4);
        assert_eq!(c.cache_get_or_set_with(5, || 5), &5);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);

        assert_eq!(c.cache_misses(), Some(7));

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);

        sleep(Duration::new(1, 0));

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);

        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);

        assert_eq!(c.cache_get_or_set_with(4, || 42), &4);

        assert_eq!(c.cache_get_or_set_with(5, || 42), &5);

        assert_eq!(c.cache_get_or_set_with(6, || 6), &6);

        assert_eq!(c.cache_misses(), Some(9));

        sleep(Duration::new(1, 0));

        assert_eq!(c.cache_get_or_set_with(4, || 42), &42);

        assert_eq!(c.cache_get_or_set_with(5, || 42), &42);

        assert_eq!(c.cache_get_or_set_with(6, || 42), &6);

        assert_eq!(c.cache_misses(), Some(11));
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_async_trait() {
        use crate::CachedAsync;
        let mut c = SizedCache::with_size(5);

        async fn _get(n: usize) -> usize {
            n
        }

        assert_eq!(c.get_or_set_with(0, || async { _get(0).await }).await, &0);
        assert_eq!(c.get_or_set_with(1, || async { _get(1).await }).await, &1);
        assert_eq!(c.get_or_set_with(2, || async { _get(2).await }).await, &2);
        assert_eq!(c.get_or_set_with(3, || async { _get(3).await }).await, &3);

        assert_eq!(c.get_or_set_with(0, || async { _get(3).await }).await, &0);
        assert_eq!(c.get_or_set_with(1, || async { _get(3).await }).await, &1);
        assert_eq!(c.get_or_set_with(2, || async { _get(3).await }).await, &2);
        assert_eq!(c.get_or_set_with(3, || async { _get(1).await }).await, &3);

        c.cache_reset();
        async fn _try_get(n: usize) -> Result<usize, String> {
            if n < 10 {
                Ok(n)
            } else {
                Err("dead".to_string())
            }
        }

        assert_eq!(
            c.try_get_or_set_with(0, || async {
                match _try_get(0).await {
                    Ok(n) => Ok(n),
                    Err(_) => Err("err".to_string()),
                }
            })
            .await
            .unwrap(),
            &0
        );
        assert_eq!(
            c.try_get_or_set_with(0, || async {
                match _try_get(5).await {
                    Ok(n) => Ok(n),
                    Err(_) => Err("err".to_string()),
                }
            })
            .await
            .unwrap(),
            &0
        );

        c.cache_reset();
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(10).await?) })
            .await;
        assert!(res.is_err());
        assert!(c.key_order().next().is_none());

        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(1).await?) })
            .await;
        assert_eq!(res.unwrap(), &1);
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(5).await?) })
            .await;
        assert_eq!(res.unwrap(), &1);
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_async_trait_timed_sized() {
        use crate::CachedAsync;
        let mut c = TimedSizedCache::with_size_and_lifespan(5, 1);

        async fn _get(n: usize) -> usize {
            n
        }

        assert_eq!(c.get_or_set_with(0, || async { _get(0).await }).await, &0);
        assert_eq!(c.get_or_set_with(1, || async { _get(1).await }).await, &1);
        assert_eq!(c.get_or_set_with(2, || async { _get(2).await }).await, &2);
        assert_eq!(c.get_or_set_with(3, || async { _get(3).await }).await, &3);

        assert_eq!(c.get_or_set_with(0, || async { _get(3).await }).await, &0);
        assert_eq!(c.get_or_set_with(1, || async { _get(3).await }).await, &1);
        assert_eq!(c.get_or_set_with(2, || async { _get(3).await }).await, &2);
        assert_eq!(c.get_or_set_with(3, || async { _get(1).await }).await, &3);

        sleep(Duration::new(1, 0));
        // after sleeping, the original val should have expired
        assert_eq!(c.get_or_set_with(0, || async { _get(3).await }).await, &3);

        c.cache_reset();
        async fn _try_get(n: usize) -> Result<usize, String> {
            if n < 10 {
                Ok(n)
            } else {
                Err("dead".to_string())
            }
        }

        assert_eq!(
            c.try_get_or_set_with(0, || async {
                match _try_get(0).await {
                    Ok(n) => Ok(n),
                    Err(_) => Err("err".to_string()),
                }
            })
            .await
            .unwrap(),
            &0
        );
        assert_eq!(
            c.try_get_or_set_with(0, || async {
                match _try_get(5).await {
                    Ok(n) => Ok(n),
                    Err(_) => Err("err".to_string()),
                }
            })
            .await
            .unwrap(),
            &0
        );

        c.cache_reset();
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(10).await?) })
            .await;
        assert!(res.is_err());
        assert!(c.key_order().next().is_none());

        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(1).await?) })
            .await;
        assert_eq!(res.unwrap(), &1);
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(5).await?) })
            .await;
        assert_eq!(res.unwrap(), &1);
        sleep(Duration::new(1, 0));
        let res: Result<&mut usize, String> = c
            .try_get_or_set_with(0, || async { Ok(_try_get(5).await?) })
            .await;
        assert_eq!(res.unwrap(), &5);
    }
}
