use super::{Cached, DefaultHashBuilder};
use crate::lru_list::LRUList;
use crate::{CachedIter, CachedPeek};
use hashbrown::HashTable;
use std::borrow::Borrow;
use std::cmp::Eq;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Outcome of a get-or-set operation on the inner [`LruCache`]:
/// `(was_present, was_valid, displaced_entry, &mut current_value)`.
///
/// `displaced_entry` is the STORED `(K, V)` that was replaced when an existing-but-invalid
/// entry was overwritten (its own key, not the caller's lookup key), or `None` on a fresh
/// insert or a valid hit. Wrapper stores thread that key to their `on_evict` callback (C1/C8).
type GetOrSetOutcome<'a, K, V> = (bool, bool, Option<(K, V)>, &'a mut V);

/// Least Recently Used / `Sized` Cache
///
/// Stores up to a specified size before beginning
/// to evict the least recently used keys
///
/// Note: This cache is in-memory only
///
/// The optional type parameter `S` selects the hash builder. It defaults to
/// [`DefaultHashBuilder`] (ahash when the `ahash` feature is enabled, otherwise
/// `std::collections::hash_map::RandomState`). Supply a custom `S` via
/// [`LruCacheBuilder::hasher`] to use a different hasher.
#[doc(alias = "SizedCache")]
pub struct LruCache<K, V, S = DefaultHashBuilder> {
    // `store` contains a hash of K -> index of (K, V) tuple in `order`
    pub(super) store: HashTable<usize>,
    pub(super) hash_builder: S,
    pub(super) order: LRUList<(K, V)>,
    pub(super) capacity: usize,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
    /// When false, `get_if` / `get_mut_if` / `get_or_set_with_if` skip incrementing `hits` and
    /// `misses`. Used by wrapper stores that maintain their own counters and delegate to this
    /// cache solely for LRU ordering / storage — avoids a redundant atomic op per access.
    pub(crate) track_hit_miss: bool,
}

impl<K, V, S> Clone for LruCache<K, V, S>
where
    K: Clone + Hash + Eq,
    V: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            hash_builder: self.hash_builder.clone(),
            order: self.order.clone(),
            capacity: self.capacity,
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            on_evict: self.on_evict.clone(),
            track_hit_miss: self.track_hit_miss,
        }
    }
}

impl<K, V, S> fmt::Debug for LruCache<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LruCache")
            .field("capacity", &self.capacity)
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V, S> PartialEq for LruCache<K, V, S>
where
    K: Eq + Hash + Clone,
    V: PartialEq,
    S: BuildHasher,
{
    fn eq(&self, other: &LruCache<K, V, S>) -> bool {
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

impl<K, V, S> Eq for LruCache<K, V, S>
where
    K: Eq + Hash + Clone,
    V: Eq,
    S: BuildHasher,
{
}

/// Builder for [`LruCache`].
pub struct LruCacheBuilder<K, V, S = DefaultHashBuilder> {
    size: Option<usize>,
    on_evict: Option<super::OnEvict<K, V>>,
    hasher: S,
}

impl<K, V> Default for LruCacheBuilder<K, V, DefaultHashBuilder> {
    fn default() -> Self {
        Self {
            size: None,
            on_evict: None,
            hasher: super::new_default_hash_builder(),
        }
    }
}

impl<K, V, S> LruCacheBuilder<K, V, S> {
    /// Set the maximum number of entries. Required -- `build` returns `Err` if not set.
    #[doc(alias = "size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.size = Some(max_size);
        self
    }

    /// Set a callback to be invoked when an entry is evicted.
    ///
    /// Use [`cache_clear_with_on_evict`](LruCache::cache_clear_with_on_evict)
    /// instead of [`cache_clear`](crate::Cached::cache_clear) to opt into callback
    /// firing and eviction counter increments when clearing all entries.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Switch to a custom hash builder `S2`, returning a builder parameterized on `S2`.
    ///
    /// The hasher is used to hash keys in the internal `HashTable`. Calling this method
    /// changes the builder's type parameter so `build()` returns an `LruCache<K, V, S2>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{Cached, LruCache};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let mut cache = LruCache::<u32, u32>::builder()
    ///     .max_size(10)
    ///     .hasher(RandomState::new())
    ///     .build()
    ///     .unwrap();
    /// cache.cache_set(1, 100);
    /// assert_eq!(cache.cache_get(&1), Some(&100));
    /// ```
    #[doc(alias = "with_hasher")]
    #[must_use]
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> LruCacheBuilder<K, V, S2> {
        LruCacheBuilder {
            size: self.size,
            on_evict: self.on_evict,
            hasher,
        }
    }

    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::MissingRequired`](super::BuildError::MissingRequired) if `max_size` was not set,
    /// or [`BuildError::InvalidValue`](super::BuildError::InvalidValue) if `max_size` is `0` or capacity
    /// pre-allocation fails.
    pub fn build(self) -> Result<LruCache<K, V, S>, super::BuildError>
    where
        K: Hash + Eq + Clone,
        S: BuildHasher,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("max_size"))?;
        if size == 0 {
            return Err(super::BuildError::InvalidValue {
                field: "max_size",
                reason: "must be greater than zero",
            });
        }

        let mut store = HashTable::new();
        // Use a temporary hasher for pre-reservation; the actual hash_builder is stored on the cache.
        if let Err(_e) = store.try_reserve(size, |&index: &usize| {
            let hasher = &mut self.hasher.build_hasher();
            index.hash(hasher);
            hasher.finish()
        }) {
            return Err(super::BuildError::InvalidValue {
                field: "max_size",
                reason: "allocation failed",
            });
        }

        let mut cache = LruCache {
            store,
            hash_builder: self.hasher,
            order: LRUList::<(K, V)>::try_with_capacity(size)?,
            capacity: size,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            on_evict: None,
            track_hit_miss: true,
        };
        cache.on_evict = self.on_evict;
        Ok(cache)
    }
}

impl<K: Hash + Eq + Clone, V> LruCache<K, V> {
    /// Construct a ready-to-use [`LruCache`] holding up to `max_size` entries.
    ///
    /// For optional settings (`on_evict`) use [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is `0`, or if pre-allocating the backing store for
    /// `max_size` entries fails (e.g. `usize::MAX`). Use [`builder`](Self::builder)
    /// with [`build`](LruCacheBuilder::build) to handle those cases without panicking.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self::builder()
            .max_size(max_size)
            .build()
            .expect("LruCache::new requires a non-zero max_size with a valid allocation")
    }

    /// Return a builder for constructing a [`LruCache`].
    #[must_use]
    pub fn builder() -> LruCacheBuilder<K, V> {
        LruCacheBuilder::default()
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> LruCache<K, V, S> {
    /// Disable hit/miss counter increments on this cache.
    ///
    /// Called by wrapper stores (`LruTtlCache`, `ExpiringLruCache`, and the sharded equivalents)
    /// that maintain their own counters and use this cache solely for LRU ordering / storage.
    pub(crate) fn disable_hit_miss_tracking(&mut self) {
        self.track_hit_miss = false;
    }

    /// Returns the maximum number of entries this cache will hold before evicting.
    ///
    /// This is the bound set via [`LruCacheBuilder::max_size`],
    /// not the current number of entries — use [`cache_size`](crate::Cached::cache_size) for that.
    #[doc(alias = "size")]
    #[doc(alias = "max_size")]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Change the maximum number of entries, returning the previous bound as
    /// `Some(prev_capacity)`.
    ///
    /// Because `LruCache` is always bounded, this always returns `Some`. The
    /// `Option` wrapper aligns the return type with `TtlSortedCache::set_max_size`,
    /// which may have no prior bound and returns `None` in that case.
    ///
    /// Shrinking below the current entry count immediately evicts least-recently-used
    /// entries. Eviction fires `on_evict` and counts evictions until the cache fits.
    /// Growing the capacity does not pre-allocate; the backing stores grow on demand
    /// as entries are inserted.
    ///
    /// This is useful for sizing a `#[cached(create = "{ ... }")]` cache from a value
    /// loaded at startup (e.g. config), then adjusting it later as load changes.
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is 0. Use [`try_set_max_size`](LruCache::try_set_max_size)
    /// to validate first and avoid the panic.
    ///
    /// # See also
    ///
    /// [`LruTtlCache::set_max_size`](super::LruTtlCache::set_max_size),
    /// [`ExpiringLruCache::set_max_size`](super::ExpiringLruCache::set_max_size), and
    /// [`TtlSortedCache::set_max_size`](super::TtlSortedCache::set_max_size) are
    /// parallel methods on the other LRU-family stores.
    /// All stores also provide a fallible `try_set_max_size` counterpart.
    pub fn set_max_size(&mut self, max_size: usize) -> Option<usize> {
        assert!(max_size > 0, "max_size must be greater than zero");
        let prev = self.capacity;
        self.capacity = max_size;
        // `check_capacity` evicts at most one entry per call (it normally runs after
        // a single insert), so loop until the cache fits the new, smaller bound.
        while self.store.len() > self.capacity {
            self.check_capacity();
        }
        Some(prev)
    }

    /// Fallible counterpart of [`set_max_size`](LruCache::set_max_size): validates
    /// that `max_size` is non-zero and then delegates to `set_max_size`.
    /// Returns the previous capacity wrapped in `Some` on success.
    ///
    /// # Errors
    ///
    /// Returns [`SetMaxSizeError::ZeroMaxSize`](super::SetMaxSizeError) if `max_size` is 0.
    pub fn try_set_max_size(
        &mut self,
        max_size: usize,
    ) -> Result<Option<usize>, super::SetMaxSizeError> {
        if max_size == 0 {
            return Err(super::SetMaxSizeError::ZeroMaxSize);
        }
        Ok(self.set_max_size(max_size))
    }

    /// Return all entries in current LRU order (most-recently-used first) as a `Vec` of
    /// `(K, `[`CacheValue<V>`](super::CacheValue)`)` pairs. `LruCache` carries no per-entry
    /// metadata, so the wrapper's metadata type is `()`; the wrapper `Deref`s to `V`.
    #[must_use]
    pub fn iter_order(&self) -> Vec<(K, super::CacheValue<V>)>
    where
        K: Clone,
        V: Clone,
    {
        self.order
            .iter()
            .map(|(k, v)| (k.clone(), super::CacheValue::new(v.clone(), ())))
            .collect()
    }

    /// Internal tuple-form of [`iter_order`](Self::iter_order) for the wrapping
    /// stores and the sharded deep-clone paths.
    pub(crate) fn iter_order_raw(&self) -> Vec<(K, V)>
    where
        K: Clone,
        V: Clone,
    {
        self.order.iter().cloned().collect()
    }

    /// Return a `Vec` of keys in the current order from most
    /// to least recently used.
    #[must_use]
    pub fn key_order(&self) -> Vec<K>
    where
        K: Clone,
    {
        self.order.iter().map(|(k, _v)| k.clone()).collect()
    }

    /// Return a `Vec` of [`CacheValue`](super::CacheValue)-wrapped values in the
    /// current order from most to least recently used.
    #[must_use]
    pub fn value_order(&self) -> Vec<super::CacheValue<V>>
    where
        V: Clone,
    {
        self.order
            .iter()
            .map(|(_k, v)| super::CacheValue::new(v.clone(), ()))
            .collect()
    }

    pub(super) fn pop_raw<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hash(k);
        let key_borrow = k.borrow();
        let order = &self.order;
        match self
            .store
            .find_entry(hash, |&i| key_borrow == order.get(i).0.borrow())
        {
            Ok(entry) => {
                let index = entry.remove().0;
                Some(self.order.remove(index))
            }
            Err(_) => None,
        }
    }

    pub(super) fn hash<Q>(&self, key: &Q) -> u64
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hasher = &mut self.hash_builder.build_hasher();
        key.hash(hasher);
        hasher.finish()
    }

    fn insert_index(&mut self, hash: u64, index: usize) {
        let order = &self.order;
        let hash_builder = &self.hash_builder;
        self.store.insert_unique(hash, index, |&i| {
            let hasher = &mut hash_builder.build_hasher();
            order.get(i).0.hash(hasher);
            hasher.finish()
        });
    }

    pub(super) fn get_index<Q>(&self, hash: u64, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.store
            .find(hash, |&i| key == self.order.get(i).0.borrow())
            .copied()
    }

    fn check_capacity(&mut self) {
        // `while` (not `if`) plus pop-before-notify: remove the victim from both
        // the store and the LRU order BEFORE invoking `on_evict`, so a panicking
        // callback can never leave an entry behind over capacity, and the loop
        // self-heals `len <= capacity` after any earlier panic (SHARD-4).
        while self.store.len() > self.capacity {
            let index = self.order.back();
            let (key, _value) = self.order.get(index);
            let hasher = &mut self.hash_builder.build_hasher();
            key.hash(hasher);
            let hash = hasher.finish();

            let order = &self.order;
            match self.store.find_entry(hash, |&i| *key == order.get(i).0) {
                Ok(entry) => {
                    entry.remove();
                }
                Err(_) => unreachable!(
                    "LruCache internal invariant violated: LRU order and hash table out of sync"
                ),
            }
            // Take ownership of the evicted pair, then notify. If `on_evict`
            // panics here the victim is already gone, so the invariant holds.
            let (evicted_key, evicted_value) = self.order.remove(index);
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&evicted_key, &evicted_value);
            }
        }
    }

    pub(super) fn get_if<Q>(&mut self, key: &Q, is_valid: impl FnOnce(&V) -> bool) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(index) = self.get_index(self.hash(key), key)
            && is_valid(&self.order.get(index).1)
        {
            self.order.move_to_front(index);
            if self.track_hit_miss {
                self.hits.fetch_add(1, Ordering::Relaxed);
            }
            return Some(&self.order.get(index).1);
        }
        if self.track_hit_miss {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    pub(super) fn get_mut_if<Q>(
        &mut self,
        key: &Q,
        is_valid: impl FnOnce(&V) -> bool,
    ) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(index) = self.get_index(self.hash(key), key)
            && is_valid(&self.order.get(index).1)
        {
            self.order.move_to_front(index);
            if self.track_hit_miss {
                self.hits.fetch_add(1, Ordering::Relaxed);
            }
            return Some(&mut self.order.get_mut(index).1);
        }
        if self.track_hit_miss {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    pub(super) fn get_or_set_with_if<F: FnOnce() -> V, FC: FnOnce(&V) -> bool>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> GetOrSetOutcome<'_, K, V> {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            let replace_existing = {
                let v = &self.order.get(index).1;
                !is_valid(v)
            };
            if self.track_hit_miss {
                if replace_existing {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                }
            }
            let old_val = if replace_existing {
                self.order.set(index, (key, f()))
            } else {
                None
            };
            self.order.move_to_front(index);
            (
                true,
                !replace_existing,
                old_val,
                &mut self.order.get_mut(index).1,
            )
        } else {
            if self.track_hit_miss {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            let index = self.order.push_front((key, f()));
            self.insert_index(hash, index);
            self.check_capacity();
            (false, false, None, &mut self.order.get_mut(index).1)
        }
    }

    pub(super) fn try_get_or_set_with_if<E, F: FnOnce() -> Result<V, E>, FC: FnOnce(&V) -> bool>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> Result<GetOrSetOutcome<'_, K, V>, E> {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            let replace_existing = {
                let v = &self.order.get(index).1;
                !is_valid(v)
            };
            if self.track_hit_miss {
                if replace_existing {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                }
            }
            let old_val = if replace_existing {
                let new_val = f()?;
                self.order.set(index, (key, new_val))
            } else {
                None
            };
            self.order.move_to_front(index);
            Ok((
                true,
                !replace_existing,
                old_val,
                &mut self.order.get_mut(index).1,
            ))
        } else {
            if self.track_hit_miss {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            let index = self.order.push_front((key, f()?));
            self.insert_index(hash, index);
            self.check_capacity();
            Ok((false, false, None, &mut self.order.get_mut(index).1))
        }
    }

    /// Removes entries for which `keep` returns `false`.
    /// Each removed entry fires the configured `on_evict` callback and is counted in `evictions`,
    /// matching [`Cached::cache_remove`] semantics. The LRU recency order of the surviving
    /// entries is unchanged.
    ///
    /// The expiry-aware LRU stores also have `retain`, with one difference: their expired
    /// entries are removed regardless of the predicate. See
    /// [`LruTtlCache::retain`](crate::LruTtlCache::retain) and
    /// [`ExpiringLruCache::retain`](crate::ExpiringLruCache::retain).
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) {
        let remove_keys = {
            self.order
                .iter()
                .filter_map(|(k, v)| if keep(k, v) { None } else { Some(k.clone()) })
                .collect::<Vec<_>>()
        };
        for k in remove_keys {
            let _ = self.cache_remove(&k);
        }
    }

    /// Removes entries for which `keep` returns `false` without firing `on_evict` or
    /// incrementing `evictions`. Used internally by TTL/expiring wrapper stores to avoid
    /// double-counting when those wrappers handle eviction side effects themselves.
    pub(super) fn retain_silent<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) {
        let remove_keys = {
            self.order
                .iter()
                .filter_map(|(k, v)| if keep(k, v) { None } else { Some(k.clone()) })
                .collect::<Vec<_>>()
        };
        for k in remove_keys {
            self.pop_raw(&k);
        }
    }

    /// Insert or replace a cache entry, returning the **stored** key and value of the displaced
    /// entry as `Some((stored_key, stored_value))`, or `None` for a new insertion.
    ///
    /// Unlike [`Cached::cache_set`], which returns only `Option<V>`, this method preserves the
    /// full `(K, V)` pair of the entry that was actually stored. This matters when the key type
    /// has fields not covered by `Hash`/`Eq` (e.g. a struct with an `id` used for equality and a
    /// `tag` that is ignored): the caller's key and the stored key compare as equal but may
    /// differ in those extra fields. Used by `LruTtlCache::set_entry` to pass the correct stored
    /// key to `on_evict`.
    #[cfg(feature = "time_stores")]
    pub(super) fn cache_set_returning_entry(&mut self, key: K, val: V) -> Option<(K, V)> {
        let hash = self.hash(&key);
        let entry = if let Some(index) = self.get_index(hash, &key) {
            self.order.set(index, (key, val))
        } else {
            let index = self.order.push_front((key, val));
            self.insert_index(hash, index);
            None
        };
        self.check_capacity();
        entry
    }

    /// Remove all entries and fire the `on_evict` callback for each one, incrementing the
    /// evictions counter.
    ///
    /// Unlike [`cache_clear`](crate::Cached::cache_clear) (which removes entries silently),
    /// this method invokes `on_evict` for every removed entry and increments `evictions`. If no
    /// `on_evict` callback was configured, it falls back to the plain `cache_clear`.
    pub fn cache_clear_with_on_evict(&mut self) {
        if self.on_evict.is_none() {
            return self.cache_clear();
        }
        let keys = self.key_order();
        let mut removed = Vec::with_capacity(keys.len());
        for k in &keys {
            if let Some(pair) = self.pop_raw(k) {
                removed.push(pair);
            }
        }
        if !removed.is_empty() {
            self.evictions
                .fetch_add(removed.len() as u64, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (k, v) in &removed {
                on_evict(k, v);
            }
        }
    }
}

#[cfg(feature = "async_core")]
impl<K, V, S> LruCache<K, V, S>
where
    K: Hash + Eq + Clone + Send,
    S: BuildHasher,
{
    pub(super) async fn get_or_set_with_if_async<F, Fut, FC>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> GetOrSetOutcome<'_, K, V>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
        FC: FnOnce(&V) -> bool,
    {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            let replace_existing = { !is_valid(&self.order.get(index).1) };
            if self.track_hit_miss {
                if replace_existing {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                }
            }
            let old_val = if replace_existing {
                let new_val = f().await;
                self.order.set(index, (key, new_val))
            } else {
                None
            };
            self.order.move_to_front(index);
            (
                true,
                !replace_existing,
                old_val,
                &mut self.order.get_mut(index).1,
            )
        } else {
            if self.track_hit_miss {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            let new_val = f().await;
            let index = self.order.push_front((key, new_val));
            self.insert_index(hash, index);
            self.check_capacity();
            (false, false, None, &mut self.order.get_mut(index).1)
        }
    }

    pub(super) async fn try_get_or_set_with_if_async<E, F, Fut, FC>(
        &mut self,
        key: K,
        f: F,
        is_valid: FC,
    ) -> Result<GetOrSetOutcome<'_, K, V>, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
        FC: FnOnce(&V) -> bool,
    {
        let hash = self.hash(&key);
        let index = self.get_index(hash, &key);
        if let Some(index) = index {
            let replace_existing = { !is_valid(&self.order.get(index).1) };
            if self.track_hit_miss {
                if replace_existing {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                }
            }
            let old_val = if replace_existing {
                let new_val = f().await?;
                self.order.set(index, (key, new_val))
            } else {
                None
            };
            self.order.move_to_front(index);
            Ok((
                true,
                !replace_existing,
                old_val,
                &mut self.order.get_mut(index).1,
            ))
        } else {
            if self.track_hit_miss {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            let new_val = f().await?;
            let index = self.order.push_front((key, new_val));
            self.insert_index(hash, index);
            self.check_capacity();
            Ok((false, false, None, &mut self.order.get_mut(index).1))
        }
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> Cached<K, V> for LruCache<K, V, S> {
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_if(key, |_| true)
    }

    fn cache_get_mut<Q>(&mut self, key: &Q) -> std::option::Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_mut_if(key, |_| true)
    }

    /// Insert or replace a cache entry.
    ///
    /// Returns the previous value if the key already existed, or `None` for a
    /// new insertion.
    ///
    /// **Note:** overwriting an existing key replaces the value in-place
    /// **without** refreshing the key's LRU recency. The entry stays at its
    /// current position in the eviction order. Use `cache_get` before
    /// `cache_set` if you need to promote the entry to most-recently-used.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let hash = self.hash(&key);
        let v = if let Some(index) = self.get_index(hash, &key) {
            self.order.set(index, (key, val)).map(|(_, v)| v)
        } else {
            let index = self.order.push_front((key, val));
            self.insert_index(hash, index);
            None
        };
        self.check_capacity();
        v
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        let (_, _, _, v) = self.get_or_set_with_if(key, f, |_| true);
        v
    }

    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let (_, _, _, v) = self.try_get_or_set_with_if(key, f, |_| true)?;
        Ok(v)
    }

    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        <Self as Cached<K, V>>::cache_remove_entry(self, k).map(|(_, v)| v)
    }

    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let removed = self.pop_raw(k);
        if let Some((ref key, ref value)) = removed {
            if let Some(on_evict) = &self.on_evict {
                on_evict(key, value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        removed
    }

    fn cache_clear(&mut self) {
        self.store.clear();
        self.order.clear();
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        //
        // Pre-allocate up to the live entry count to avoid a large allocation when
        // `capacity` has been set to a very large value (e.g. `usize::MAX`). The
        // live count is a safe ceiling: we cannot have more entries than that right
        // now, and the backing stores grow on demand as new entries are inserted.
        let live = self.store.len();
        let mut new_store = HashTable::new();
        let _ = new_store.try_reserve(live, |&index: &usize| self.hash_builder.hash_one(index));
        let new_order = LRUList::<(K, V)>::try_with_capacity(live)
            .unwrap_or_else(|_| LRUList::<(K, V)>::with_capacity(0));
        self.store = new_store;
        self.order = new_order;
        self.cache_reset_metrics();
    }
    fn cache_reset_metrics(&mut self) {
        self.misses.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
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
    fn cache_evictions(&self) -> Option<u64> {
        Some(self.evictions.load(Ordering::Relaxed))
    }
    fn cache_capacity(&self) -> Option<usize> {
        Some(self.capacity)
    }

    /// Check whether the cache contains a live entry for `k`.
    ///
    /// Delegates to [`CachedPeek::cache_peek`], so it records no hit/miss
    /// metrics, performs no recency promotion, and reports absent/expired
    /// entries as `false`.
    fn cache_contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        crate::CachedPeek::cache_peek(self, k).is_some()
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> CachedIter<K, V> for LruCache<K, V, S> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.order.iter().map(|(k, v)| (k, v))
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> CachedPeek<K, V> for LruCache<K, V, S> {
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(index) = self.get_index(self.hash(k), k) {
            return Some(&self.order.get(index).1);
        }
        None
    }
}

#[cfg(feature = "async_core")]
impl<K, V, S> CachedGetOrSetAsync<K, V> for LruCache<K, V, S>
where
    K: Hash + Eq + Clone + Send,
    S: BuildHasher + Send,
{
    fn async_cache_get_or_set_with_mut<'a, F, Fut>(
        &'a mut self,
        k: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move {
            let (_, _, _, v) = self.get_or_set_with_if_async(k, f, |_| true).await;
            v
        }
    }

    fn async_cache_try_get_or_set_with_mut<'a, F, Fut, E>(
        &'a mut self,
        k: K,
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
            let (_, _, _, v) = self.try_get_or_set_with_if_async(k, f, |_| true).await?;
            Ok(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CachedExt;
    use crate::stores::Cached;

    #[test]
    fn new_returns_ready_cache_respecting_max_size() {
        let mut c: LruCache<u32, u32> = LruCache::new(2);
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.set(1, 10), None);
        assert_eq!(c.get(&1), Some(&10));
        c.set(2, 20);
        c.set(3, 30); // evicts LRU (1)
        assert_eq!(c.cache_size(), 2);
        assert_eq!(c.get(&1), None);
    }

    #[test]
    #[should_panic(expected = "non-zero max_size")]
    fn new_zero_max_size_panics() {
        let _c: LruCache<u32, u32> = LruCache::new(0);
    }

    #[test]
    fn sized_cache() {
        let mut c = LruCache::builder().max_size(5).build().unwrap();
        assert!(c.get(&1).is_none());
        assert_eq!(1, c.cache_misses().unwrap());

        assert_eq!(c.set(1, 100), None);
        assert!(c.get(&1).is_some());
        assert_eq!(1, c.cache_hits().unwrap());
        assert_eq!(1, c.cache_misses().unwrap());

        assert_eq!(c.set(2, 100), None);
        assert_eq!(c.set(3, 100), None);
        assert_eq!(c.set(4, 100), None);
        assert_eq!(c.set(5, 100), None);

        assert_eq!(c.key_order(), vec![5, 4, 3, 2, 1]);

        assert_eq!(c.set(6, 100), None);
        assert_eq!(c.set(7, 100), None);

        assert_eq!(c.key_order(), vec![7, 6, 5, 4, 3]);

        assert!(c.get(&2).is_none());
        assert!(c.get(&3).is_some());

        assert_eq!(c.key_order(), vec![3, 7, 6, 5, 4]);

        assert_eq!(2, c.cache_misses().unwrap());
        assert_eq!(5, c.cache_size());

        c.cache_reset_metrics();
        assert_eq!(0, c.cache_hits().unwrap());
        assert_eq!(0, c.cache_misses().unwrap());
        assert_eq!(5, c.cache_size());

        assert_eq!(c.set(7, 200), Some(100));

        #[derive(Hash, Clone, Eq, PartialEq)]
        struct MyKey {
            v: String,
        }
        let mut c = LruCache::builder().max_size(5).build().unwrap();
        assert_eq!(
            c.cache_set(
                MyKey {
                    v: String::from("s")
                },
                String::from("a")
            ),
            None
        );
        assert_eq!(
            c.cache_set(
                MyKey {
                    v: String::from("s")
                },
                String::from("a")
            ),
            Some(String::from("a"))
        );
        assert_eq!(
            c.cache_set(
                MyKey {
                    v: String::from("s2")
                },
                String::from("b")
            ),
            None
        );
        assert_eq!(
            c.cache_set(
                MyKey {
                    v: String::from("s2")
                },
                String::from("b")
            ),
            Some(String::from("b"))
        );
    }

    #[test]
    fn peek_does_not_update_recency_or_metrics() {
        let mut c = LruCache::builder().max_size(2).build().unwrap();
        c.set(1, 10);
        c.set(2, 20);
        c.cache_reset_metrics();

        assert_eq!(c.cache_peek(&1), Some(&10));
        assert_eq!(c.key_order(), vec![2, 1]);
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(0));

        c.set(3, 30);
        assert_eq!(c.cache_peek(&1), None);
        assert_eq!(c.cache_peek(&2), Some(&20));
        assert_eq!(c.cache_peek(&3), Some(&30));
    }

    #[test]
    fn try_new() {
        let c = LruCache::<i32, i32>::builder().max_size(0).build();
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue {
                field: "max_size",
                ..
            }
        ));

        let c = LruCache::<i32, i32>::builder().max_size(usize::MAX).build();
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue {
                field: "max_size",
                ..
            }
        ));
    }

    #[test]
    fn size_cache_racing_keys_eviction_regression() {
        // Regression: duplicate keys in the internal `order` caused wrong eviction. See issue #7.
        let mut c = LruCache::builder().max_size(2).build().unwrap();
        assert_eq!(c.set(1, 100), None);
        assert_eq!(c.set(1, 100), Some(100));
        // size would be 1, but internal order would be [1, 1] before the fix
        assert_eq!(c.set(2, 100), None);
        assert_eq!(c.set(3, 100), None);
        // this would fail if a duplicate key was evicted
        assert_eq!(c.set(4, 100), None);
    }

    #[test]
    fn clear() {
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        assert_eq!(c.set(1, 100), None);
        assert_eq!(c.set(2, 200), None);
        assert_eq!(c.set(3, 300), None);
        c.clear();
        assert_eq!(0, c.cache_size());
    }

    #[test]
    fn capacity_returns_bound_not_live_size() {
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        // The bound is fixed at construction and independent of live count.
        assert_eq!(c.capacity(), 3);
        assert_eq!(c.cache_size(), 0);

        c.set(1, 100);
        c.set(2, 200);
        assert_eq!(c.capacity(), 3);
        assert_eq!(c.cache_size(), 2);

        // Eviction past the bound keeps capacity fixed while live count stays capped.
        c.set(3, 300);
        c.set(4, 400);
        assert_eq!(c.capacity(), 3);
        assert_eq!(c.cache_size(), 3);
    }

    #[test]
    fn reset() {
        let init_capacity = 2;
        let mut c = LruCache::builder().max_size(init_capacity).build().unwrap();
        for i in 0..128 {
            assert_eq!(c.set(i, i), None);
        }
        c.cache_reset();
        assert_eq!(0, c.cache_size());
        assert!(init_capacity <= c.store.capacity());
    }

    #[test]
    fn remove() {
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        assert_eq!(c.set(1, 100), None);
        assert_eq!(c.set(2, 200), None);
        assert_eq!(c.set(3, 300), None);

        assert_eq!(Some(100), c.remove(&1));
        assert_eq!(2, c.cache_size());

        assert_eq!(Some(200), c.remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(None, c.remove(&2));
        assert_eq!(1, c.cache_size());

        assert_eq!(Some(300), c.remove(&3));
        assert_eq!(0, c.cache_size());
    }

    #[test]
    fn sized_cache_get_mut() {
        let mut c = LruCache::builder().max_size(5).build().unwrap();
        assert!(c.cache_get_mut(&1).is_none());
        assert_eq!(1, c.cache_misses().unwrap());

        assert_eq!(c.set(1, 100), None);
        assert_eq!(*c.cache_get_mut(&1).unwrap(), 100);
        assert_eq!(1, c.cache_hits().unwrap());
        assert_eq!(1, c.cache_misses().unwrap());

        let value = c.cache_get_mut(&1).unwrap();
        *value = 10;
        assert_eq!(2, c.cache_hits().unwrap());
        assert_eq!(1, c.cache_misses().unwrap());
        assert_eq!(*c.cache_get_mut(&1).unwrap(), 10);
    }

    #[test]
    fn sized_cache_eviction_fix() {
        let mut cache = LruCache::<u32, ()>::builder().max_size(3).build().unwrap();
        cache.set(1, ());
        cache.set(2, ());
        cache.set(3, ());

        assert!(cache.get(&1).is_some());
        assert!(cache.get(&2).is_some());
        assert!(cache.get(&3).is_some());
        assert!(cache.get(&4).is_none());

        // Inserting the same key multiple times must not evict extra entries
        cache.set(4, ());
        assert_eq!(cache.cache_size(), 3);
        cache.set(4, ());
        assert_eq!(cache.cache_size(), 3);

        assert!(cache.get(&1).is_none()); // evicted by first "4" insert
        assert!(cache.get(&2).is_some());
        assert!(cache.get(&3).is_some());
        assert!(cache.get(&4).is_some());
    }

    #[test]
    fn get_or_set_with() {
        let mut c = LruCache::builder().max_size(5).build().unwrap();
        for i in 0..=5usize {
            assert_eq!(c.cache_get_or_set_with(i, || i), &i);
        }
        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);
        assert_eq!(c.cache_misses(), Some(7)); // 0 was evicted (LRU), so re-miss

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);
        assert_eq!(c.cache_misses(), Some(7)); // now a hit

        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_misses(), Some(8)); // 1 was evicted

        c.cache_reset();
        fn _try_get(n: usize) -> Result<usize, String> {
            if n < 10 {
                Ok(n)
            } else {
                Err("dead".to_string())
            }
        }
        let res: Result<&usize, String> = c.cache_try_get_or_set_with(0, || _try_get(10));
        assert!(res.is_err());
        assert!(c.key_order().is_empty());

        let res: Result<&usize, String> = c.cache_try_get_or_set_with(0, || _try_get(1));
        assert_eq!(res.unwrap(), &1);
        let res: Result<&usize, String> = c.cache_try_get_or_set_with(0, || _try_get(5));
        assert_eq!(res.unwrap(), &1);
    }

    #[test]
    fn retain() {
        let mut c = LruCache::builder().max_size(5).build().unwrap();
        for i in 0i32..5 {
            c.set(i, i * 10);
        }
        assert_eq!(c.cache_size(), 5);
        c.retain(|k, _v| k % 2 == 0);
        assert_eq!(c.cache_size(), 3); // 0, 2, 4
        assert!(c.get(&0).is_some());
        assert!(c.get(&1).is_none());
        assert!(c.get(&2).is_some());
        assert!(c.get(&3).is_none());
        assert!(c.get(&4).is_some());
    }

    #[test]
    fn key_order_and_value_order() {
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.set(1, 10);
        c.set(2, 20);
        c.set(3, 30);
        // most-recently-used first
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        assert_eq!(c.value_order(), vec![30, 20, 10]);
        // access key 1, it moves to front
        c.cache_get(&1);
        assert_eq!(c.key_order(), vec![1, 3, 2]);
    }

    #[test]
    fn cache_set_over_existing_key_does_not_promote_recency() {
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.set(1, 10);
        c.set(2, 20);
        c.set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        // Overwriting the least-recently-used key updates the value in-place and
        // returns the old value, but must NOT move it to the front.
        assert_eq!(Cached::cache_set(&mut c, 1, 11), Some(10));
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        assert_eq!(c.value_order(), vec![30, 20, 11]);
    }

    #[test]
    fn sized_cache_clone_is_independent() {
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.set(1, 100);
        c.set(2, 200);
        let mut c2 = c.clone();
        c2.set(3, 300);
        // original unchanged
        assert_eq!(c.cache_size(), 2);
        assert_eq!(c2.cache_size(), 3);
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_async_trait() {
        use crate::CachedGetOrSetAsync;
        let mut c = LruCache::builder().max_size(5).build().unwrap();

        async fn _get(n: usize) -> usize {
            n
        }

        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 0, || async { _get(0).await })
                .await,
            &0
        );
        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 1, || async { _get(1).await })
                .await,
            &1
        );
        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 2, || async { _get(2).await })
                .await,
            &2
        );
        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 3, || async { _get(3).await })
                .await,
            &3
        );

        // hits — should not re-evaluate
        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 0, || async {
                _get(99).await
            })
            .await,
            &0
        );
        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 1, || async {
                _get(99).await
            })
            .await,
            &1
        );
        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 2, || async {
                _get(99).await
            })
            .await,
            &2
        );
        assert_eq!(
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 3, || async {
                _get(99).await
            })
            .await,
            &3
        );

        c.cache_reset();
        async fn _try_get(n: usize) -> Result<usize, String> {
            if n < 10 {
                Ok(n)
            } else {
                Err("dead".to_string())
            }
        }

        assert_eq!(
            CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 0, || async {
                _try_get(0).await
            })
            .await
            .unwrap(),
            &0
        );
        assert_eq!(
            CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 0, || async {
                _try_get(5).await
            })
            .await
            .unwrap(),
            &0 // cached value, 5 never evaluated
        );

        c.cache_reset();
        let res: Result<&usize, String> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 0, || async {
                _try_get(10).await
            })
            .await;
        assert!(res.is_err());
        assert!(c.key_order().is_empty());

        let res: Result<&usize, String> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 0, || async {
                _try_get(1).await
            })
            .await;
        assert_eq!(res.unwrap(), &1);
        let res: Result<&usize, String> =
            CachedGetOrSetAsync::async_cache_try_get_or_set_with(&mut c, 0, || async {
                _try_get(5).await
            })
            .await;
        assert_eq!(res.unwrap(), &1);
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(5)
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
        assert_eq!(c.cache_evictions(), Some(3));
    }

    #[test]
    fn cache_clear_does_not_fire_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(5)
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
    fn cache_reset_does_not_fire_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let evict_count = Arc::new(AtomicUsize::new(0));
        let evict_count2 = evict_count.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_k, _v| {
                evict_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        c.cache_reset();
        assert_eq!(
            evict_count.load(Ordering::Relaxed),
            0,
            "cache_reset must not fire on_evict"
        );
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn test_diagnostics_and_traits() {
        let mut cache = LruCache::builder().max_size(3).build().unwrap();
        cache.cache_set(1, 100);
        cache.cache_set(2, 200);

        // Debug
        let debug_str = format!("{:?}", cache);
        assert!(debug_str.contains("LruCache"));
        assert!(debug_str.contains("capacity"));
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

        // `Eq` requires `V: Eq`; it still applies for a value type that is `Eq`.
        fn assert_eq_impl<T: Eq>() {}
        assert_eq_impl::<LruCache<u32, u32>>();

        // build errors
        let builder = LruCache::<u32, u32>::builder();
        let built = builder.build();
        assert!(built.is_err()); // Missing required size

        let builder = LruCache::<u32, u32>::builder().max_size(0);
        let built = builder.build();
        assert!(built.is_err()); // Size 0 is invalid
    }

    #[test]
    fn cache_remove_entry_basic() {
        let mut c = LruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1u32, 100u32);
        c.cache_set(2u32, 200u32);

        // Returns None for absent key.
        assert_eq!(c.cache_remove_entry(&999u32), None);

        // Returns stored key and value.
        assert_eq!(c.cache_remove_entry(&1u32), Some((1u32, 100u32)));

        // Entry is gone.
        assert_eq!(c.cache_get(&1u32), None);
        assert_eq!(c.cache_size(), 1);
    }

    #[test]
    fn cache_remove_entry_fires_on_evict() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_, _| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        let _ = c.cache_remove_entry(&1u32);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        // No fire for absent key.
        let _ = c.cache_remove_entry(&999u32);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let mut c = LruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1u32, 10u32);
        let before = c.cache_evictions().expect("evictions are always tracked");
        let _ = c.cache_remove_entry(&1u32);
        let _ = c.cache_remove_entry(&999u32); // absent — must not increment
        assert_eq!(
            c.cache_evictions().expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }

    #[test]
    fn cache_delete_returns_true_for_present_entry() {
        let mut c = LruCache::builder().max_size(4).build().unwrap();
        c.cache_set(1u32, 10u32);
        assert!(c.cache_delete(&1u32));
        assert!(!c.cache_delete(&1u32));
    }

    #[test]
    fn set_max_size_grow_returns_previous_and_keeps_entries() {
        let mut c = LruCache::builder().max_size(2).build().unwrap();
        c.cache_set(1u32, 10u32);
        c.cache_set(2u32, 20u32);
        let prev = c.set_max_size(4);
        assert_eq!(prev, Some(2));
        assert_eq!(c.capacity(), 4);
        // Growing keeps existing entries.
        assert_eq!(c.cache_get(&1), Some(&10));
        assert_eq!(c.cache_get(&2), Some(&20));
        // Room for more before eviction.
        c.cache_set(3u32, 30u32);
        c.cache_set(4u32, 40u32);
        assert_eq!(c.cache_size(), 4);
    }

    #[test]
    fn set_max_size_shrink_evicts_lru_entries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted2 = evicted.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_k: &u32, _v: &u32| {
                evicted2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        c.cache_set(2u32, 20u32);
        c.cache_set(3u32, 30u32);
        c.cache_set(4u32, 40u32);
        // Touch 1 and 2 so 3 and 4 become the least-recently-used.
        assert_eq!(c.cache_get(&1), Some(&10));
        assert_eq!(c.cache_get(&2), Some(&20));

        let prev = c.set_max_size(2);
        assert_eq!(prev, Some(4));
        assert_eq!(c.capacity(), 2);
        assert_eq!(c.cache_size(), 2);
        // Shrinking fires on_evict for each evicted entry and counts evictions.
        assert_eq!(evicted.load(AOrdering::Relaxed), 2);
        assert_eq!(c.cache_evictions(), Some(2));
        // The two most-recently-used survive.
        assert_eq!(c.cache_get(&1), Some(&10));
        assert_eq!(c.cache_get(&2), Some(&20));
        assert_eq!(c.cache_get(&3), None);
        assert_eq!(c.cache_get(&4), None);
    }

    #[test]
    #[should_panic(expected = "max_size must be greater than zero")]
    fn set_max_size_zero_panics() {
        let mut c: LruCache<u32, u32> = LruCache::builder().max_size(2).build().unwrap();
        c.set_max_size(0);
    }

    #[test]
    fn try_set_max_size_rejects_zero() {
        let mut c: LruCache<u32, u32> = LruCache::builder().max_size(2).build().unwrap();
        assert_eq!(
            c.try_set_max_size(0),
            Err(super::super::SetMaxSizeError::ZeroMaxSize)
        );
        assert_eq!(c.try_set_max_size(8).unwrap(), Some(2));
        assert_eq!(c.capacity(), 8);
    }

    #[test]
    fn cache_reset_after_usize_max_capacity_does_not_panic() {
        // R1: after set_max_size(usize::MAX) the internal `capacity` field is huge,
        // but cache_reset must not attempt to pre-allocate that many bytes.
        // It should cap the pre-allocation to the live entry count and succeed.
        let mut c: LruCache<u32, u32> = LruCache::builder().max_size(2).build().unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.set_max_size(usize::MAX);
        // Must not panic/abort even though self.capacity == usize::MAX.
        c.cache_reset();
        assert_eq!(c.cache_size(), 0);
    }

    // --- custom hasher tests ---

    #[test]
    fn custom_hasher_get_set_round_trip() {
        use std::collections::hash_map::RandomState;
        let mut c = LruCache::<u32, u32>::builder()
            .max_size(10)
            .hasher(RandomState::new())
            .build()
            .unwrap();
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(c.cache_get(&2), Some(&200));
        assert_eq!(c.cache_hits(), Some(2));
        assert_eq!(c.cache_misses(), Some(0));
        assert_eq!(c.cache_get(&99), None);
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn default_constructor_still_works() {
        let mut c: LruCache<u32, u32> = LruCache::new(5);
        c.cache_set(1, 10);
        assert_eq!(c.cache_get(&1), Some(&10));

        let mut b = LruCache::<u32, u32>::builder().max_size(5).build().unwrap();
        b.cache_set(2, 20);
        assert_eq!(b.cache_get(&2), Some(&20));
    }

    #[test]
    fn custom_hasher_respects_lru_eviction() {
        use std::collections::hash_map::RandomState;
        let mut c = LruCache::<u32, u32>::builder()
            .max_size(2)
            .hasher(RandomState::new())
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_get(&1); // make 1 most-recently-used
        c.cache_set(3, 30); // should evict 2 (least-recently-used)
        assert_eq!(c.cache_get(&1), Some(&10));
        assert_eq!(c.cache_get(&2), None); // evicted
        assert_eq!(c.cache_get(&3), Some(&30));
    }

    // SHARD-4: a panicking `on_evict` during capacity eviction must not leave the
    // cache permanently over capacity. The victim is removed before the callback
    // runs, so `len <= capacity` holds across repeated panicking inserts.
    #[test]
    fn panicking_on_evict_keeps_cache_within_capacity() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: LruCache<u32, u32> = LruCache::builder()
            .max_size(2)
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1, 1);
        c.cache_set(2, 2);
        // The third insert overflows capacity and evicts, so `on_evict` panics.
        let r = catch_unwind(AssertUnwindSafe(|| c.cache_set(3, 3)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert!(
            c.cache_size() <= 2,
            "cache left over capacity: len {}",
            c.cache_size()
        );
        // Later inserts keep healing to <= capacity even as the callback panics.
        for i in 4..8 {
            let _ = catch_unwind(AssertUnwindSafe(|| c.cache_set(i, i)));
            assert!(
                c.cache_size() <= 2,
                "cache exceeded capacity after insert {i}: len {}",
                c.cache_size()
            );
        }
    }
}
