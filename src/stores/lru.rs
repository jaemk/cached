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

impl<K, V> LruCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`LruCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        // `LRUListIterator` has no `size_hint`, so `collect` would grow the Vec from
        // zero. The live entry count is known here, so pre-size instead.
        let mut out = Vec::with_capacity(self.store.len());
        out.extend(
            self.order
                .iter()
                .map(|(k, v)| (k.clone(), super::CacheValue::new(v.clone(), ()))),
        );
        out
    }

    /// Internal tuple-form of [`iter_order`](Self::iter_order) for the wrapping
    /// stores and the sharded deep-clone paths.
    pub(crate) fn iter_order_raw(&self) -> Vec<(K, V)>
    where
        K: Clone,
        V: Clone,
    {
        let mut out = Vec::with_capacity(self.store.len());
        out.extend(self.order.iter().cloned());
        out
    }

    /// Return a `Vec` of keys in the current order from most
    /// to least recently used.
    #[must_use]
    pub fn key_order(&self) -> Vec<K>
    where
        K: Clone,
    {
        let mut out = Vec::with_capacity(self.store.len());
        out.extend(self.order.iter().map(|(k, _v)| k.clone()));
        out
    }

    /// Return a `Vec` of [`CacheValue`](super::CacheValue)-wrapped values in the
    /// current order from most to least recently used.
    #[must_use]
    pub fn value_order(&self) -> Vec<super::CacheValue<V>>
    where
        V: Clone,
    {
        let mut out = Vec::with_capacity(self.store.len());
        out.extend(
            self.order
                .iter()
                .map(|(_k, v)| super::CacheValue::new(v.clone(), ())),
        );
        out
    }

    pub(super) fn pop_raw<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hash(k);
        self.pop_raw_with_hash(hash, k)
    }

    /// [`pop_raw`](Self::pop_raw) for callers that already hold the key's hash.
    ///
    /// `hash` MUST be `self.hash(k)` (i.e. produced by this cache's own hash builder for
    /// this key); passing any other value makes the lookup miss and the entry is left in
    /// place. Use this when a caller has computed the hash for an earlier probe on the
    /// same key -- it skips only the re-hash, the `K: Eq` bucket probe still runs.
    pub(super) fn pop_raw_with_hash<Q>(&mut self, hash: u64, k: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let order = &self.order;
        match self
            .store
            .find_entry(hash, |&i| k == order.get(i).0.borrow())
        {
            Ok(entry) => {
                let index = entry.remove().0;
                Some(self.order.remove(index))
            }
            Err(_) => None,
        }
    }

    /// Remove the entry occupying LRU slot `index`, returning the **stored** `(K, V)`.
    ///
    /// Fires no `on_evict` callback and touches no counters -- callers own those side
    /// effects. Slot indices come from [`LRUList::iter_indices`] (or `order.back()`), and
    /// stay valid across removals of *other* slots, so a sweep can collect a
    /// `Vec<usize>` up front and remove from it.
    ///
    /// Compared to [`pop_raw`](Self::pop_raw), this removes the key **clone** and the
    /// `K: Eq` comparisons (the table entry is located by comparing the stored slot
    /// index, which is unique). It does **not** remove the hash: the table is keyed by
    /// key-hash, so the stored key must still be hashed once to find its bucket. That is
    /// unavoidable without a second index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is not an occupied slot, or if the hash table and the LRU order
    /// have drifted out of sync (an internal invariant violation).
    pub(super) fn remove_index(&mut self, index: usize) -> (K, V) {
        // Hash the STORED key -- the caller may never have had a key at all.
        let hash = {
            let (key, _value) = self.order.get(index);
            self.hash(key)
        };
        match self.store.find_entry(hash, |&i| i == index) {
            Ok(entry) => {
                entry.remove();
            }
            Err(_) => unreachable!(
                "LruCache internal invariant violated: LRU order and hash table out of sync"
            ),
        }
        self.order.remove(index)
    }

    /// Remove every entry, returning the stored `(K, V)` pairs in MRU -> LRU order.
    ///
    /// Fires no `on_evict` callback and touches no counters -- callers own those side
    /// effects (see [`cache_clear_with_on_evict`](Self::cache_clear_with_on_evict)).
    /// Unlike a key-by-key drain this performs **zero hashing and zero clones**: the LRU
    /// chain is walked once taking owned values and the hash table is cleared wholesale.
    /// The cache is empty and immediately reusable afterwards.
    pub(super) fn drain_all(&mut self) -> Vec<(K, V)> {
        let mut drained = Vec::with_capacity(self.store.len());
        self.order.drain_into(&mut drained);
        self.store.clear();
        drained
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
    /// Returns the number of entries removed, i.e. the number of times `keep` returned `false`.
    /// `retain` is deliberately not `#[must_use]`: discarding the count is a legitimate and
    /// common use, matching existing bare `cache.retain(...);` call sites.
    ///
    /// The expiry-aware LRU stores also have `retain`, with one difference: their expired
    /// entries are removed regardless of the predicate, so their returned count folds together
    /// predicate rejections and expired sweeps. See
    /// [`LruTtlCache::retain`](crate::LruTtlCache::retain) and
    /// [`ExpiringLruCache::retain`](crate::ExpiringLruCache::retain).
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) -> usize {
        // Collect doomed *slot indices*, not cloned keys: `remove_index` then removes
        // each entry without a key clone or an `Eq` probe. Indices stay valid because
        // nothing is inserted between the scan and the removals.
        let doomed = self.doomed_indices(&mut keep);
        let removed = doomed.len();
        for index in doomed {
            let (key, value) = self.remove_index(index);
            // Count BEFORE notifying: a panicking callback must never leave an
            // entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&key, &value);
            }
        }
        removed
    }

    /// Slot indices (MRU -> LRU) of the entries for which `keep` returns `false`.
    /// Shared by [`retain`](Self::retain) and the TTL/expiring wrapper stores, which run their
    /// own two-phase sweep so a panicking predicate cannot remove an entry uncounted.
    fn doomed_indices<F: FnMut(&K, &V) -> bool>(&self, keep: &mut F) -> Vec<usize> {
        // The live entry count is an upper bound on the number of doomed indices;
        // pre-size instead of growing the Vec from zero.
        let mut doomed = Vec::with_capacity(self.store.len());
        doomed.extend(self.order.iter_indices().filter(|&i| {
            let (k, v) = self.order.get(i);
            !keep(k, v)
        }));
        doomed
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
    ///
    /// # Recency
    ///
    /// Like [`Cached::cache_set`], writing over an existing key promotes that entry to
    /// most-recently-used: a write counts as an access, so an overwrite is equivalent to
    /// inserting a fresh value. This matches a remove-then-insert (`pop_raw` +
    /// `cache_set`), whose insert goes through `push_front`.
    // Deliberately NOT `#[cfg(feature = "time_stores")]`: nothing in the body is
    // time-related, and non-time_stores callers need it too.
    pub(super) fn cache_set_returning_entry(&mut self, key: K, val: V) -> Option<(K, V)> {
        let hash = self.hash(&key);
        let entry = if let Some(index) = self.get_index(hash, &key) {
            let displaced = self.order.set(index, (key, val));
            self.order.move_to_front(index);
            displaced
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
    /// this method invokes `on_evict` for every removed entry and increments `evictions`.
    /// The eviction count does not depend on whether an `on_evict` callback is configured.
    pub fn cache_clear_with_on_evict(&mut self) {
        // `drain_all` walks the LRU chain once taking owned pairs (MRU -> LRU, the same
        // order the old key-by-key drain fired in) -- no key clones, no re-hashing.
        let removed = self.drain_all();
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
    /// **Note:** overwriting an existing key promotes that key to
    /// most-recently-used. A write counts as an access, so an overwrite moves
    /// the entry to the front of the eviction order exactly as a fresh
    /// insertion would. Use [`CachedPeek::cache_peek`](crate::CachedPeek::cache_peek)
    /// if you need to inspect an entry without touching recency.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let hash = self.hash(&key);
        let v = if let Some(index) = self.get_index(hash, &key) {
            let displaced = self.order.set(index, (key, val)).map(|(_, v)| v);
            self.order.move_to_front(index);
            displaced
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
            // Count BEFORE notifying: a panicking callback must never leave an
            // entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(key, value);
            }
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

impl<K: Hash + Eq + Clone, V, S: BuildHasher> crate::CacheSetMaxSize for LruCache<K, V, S> {
    fn set_max_size(&mut self, max_size: usize) -> Option<usize> {
        LruCache::set_max_size(self, max_size)
    }

    fn try_set_max_size(
        &mut self,
        max_size: usize,
    ) -> Result<Option<usize>, super::SetMaxSizeError> {
        LruCache::try_set_max_size(self, max_size)
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> crate::CacheClearWithOnEvict for LruCache<K, V, S> {
    fn cache_clear_with_on_evict(&mut self) {
        LruCache::cache_clear_with_on_evict(self);
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
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
        let removed = c.retain(|k, _v| k % 2 == 0);
        assert_eq!(c.cache_size(), 3); // 0, 2, 4
        assert_eq!(removed, 2);
        assert!(c.get(&0).is_some());
        assert!(c.get(&1).is_none());
        assert!(c.get(&2).is_some());
        assert!(c.get(&3).is_none());
        assert!(c.get(&4).is_some());
    }

    #[test]
    fn retain_fires_on_evict_in_mru_to_lru_order_and_counts_evictions() {
        // Pins the side effects of the index-based `retain` rewrite: every doomed
        // entry fires `on_evict` exactly once, MRU -> LRU, and is counted.
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(5)
            .on_evict(move |k: &i32, v: &i32| seen2.lock().unwrap().push((*k, *v)))
            .build()
            .unwrap();
        for i in 0i32..5 {
            c.cache_set(i, i * 10);
        }
        // MRU -> LRU is 4, 3, 2, 1, 0; drop the odd keys.
        c.retain(|k, _v| k % 2 == 0);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(3, 30), (1, 10)],
            "on_evict must fire once per removed entry, MRU -> LRU"
        );
        assert_eq!(c.cache_evictions(), Some(2));
        // Survivors keep their relative recency order.
        assert_eq!(c.key_order(), vec![4, 2, 0]);
        assert_eq!(c.cache_size(), 3);
        // The hash table entries are really gone, and the freed slots are reusable.
        assert!(c.cache_peek(&1).is_none());
        c.cache_set(7, 70);
        assert_eq!(c.cache_peek(&7), Some(&70));
        assert_eq!(c.key_order(), vec![7, 4, 2, 0]);
    }

    #[test]
    fn remove_index_returns_stored_key_and_unlinks() {
        #[derive(Debug, Clone)]
        struct MyKey {
            id: u32,
            tag: &'static str,
        }
        impl PartialEq for MyKey {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }
        impl Eq for MyKey {}
        impl Hash for MyKey {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }

        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_k: &MyKey, _v: &u32| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        for id in 1..=3u32 {
            c.cache_set(MyKey { id, tag: "stored" }, id * 10);
        }
        // MRU -> LRU: 3, 2, 1
        let indices: Vec<usize> = c.order.iter_indices().collect();
        assert_eq!(indices.len(), 3);

        // Removing the middle slot yields the STORED key (tag "stored"), not a
        // caller-supplied lookup key.
        let (key, value) = c.remove_index(indices[1]);
        assert_eq!(key.id, 2);
        assert_eq!(key.tag, "stored");
        assert_eq!(value, 20);

        // Unlinked from both the order and the hash table.
        assert_eq!(c.cache_size(), 2);
        assert!(
            c.cache_peek(&MyKey {
                id: 2,
                tag: "lookup"
            })
            .is_none()
        );
        assert_eq!(
            c.key_order().iter().map(|k| k.id).collect::<Vec<_>>(),
            vec![3, 1]
        );
        // Untouched slots keep their indices valid.
        assert_eq!(c.order.get(indices[0]).1, 30);
        assert_eq!(c.order.get(indices[2]).1, 10);

        // Silent: no callback, no counter.
        assert_eq!(count.load(AOrdering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));

        // Re-inserting the removed key works (its table entry was really freed).
        assert_eq!(
            c.cache_set(
                MyKey {
                    id: 2,
                    tag: "again"
                },
                22
            ),
            None
        );
        assert_eq!(
            c.cache_peek(&MyKey {
                id: 2,
                tag: "lookup"
            }),
            Some(&22)
        );
        assert_eq!(
            c.key_order().iter().map(|k| k.id).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn drain_all_returns_mru_to_lru_and_leaves_cache_reusable() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        c.cache_set(2u32, 20u32);
        c.cache_set(3u32, 30u32);
        c.cache_get(&1); // promote: MRU -> LRU is now 1, 3, 2

        let drained = c.drain_all();
        assert_eq!(drained, vec![(1, 10), (3, 30), (2, 20)]);
        assert_eq!(c.cache_size(), 0);
        assert!(c.key_order().is_empty());
        assert!(c.cache_peek(&1).is_none());
        // `drain_all` is silent: callers own the side effects.
        assert_eq!(count.load(AOrdering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));

        // Reusable afterwards, with correct ordering and eviction behavior.
        c.cache_set(5u32, 50u32);
        c.cache_set(6u32, 60u32);
        assert_eq!(c.cache_get(&5), Some(&50));
        assert_eq!(c.key_order(), vec![5, 6]);
        assert_eq!(c.cache_size(), 2);

        // Draining an empty cache is a no-op.
        let mut empty: LruCache<u32, u32> = LruCache::new(2);
        assert!(empty.drain_all().is_empty());
        empty.cache_set(1, 1);
        assert_eq!(empty.cache_get(&1), Some(&1));
    }

    #[test]
    fn pop_raw_with_hash_agrees_with_pop_raw() {
        let mut a = LruCache::builder().max_size(4).build().unwrap();
        let mut b = LruCache::builder().max_size(4).build().unwrap();
        for i in 1..=3u32 {
            a.cache_set(i, i * 10);
            b.cache_set(i, i * 10);
        }
        let hash = a.hash(&2u32);
        assert_eq!(a.pop_raw_with_hash(hash, &2u32), b.pop_raw(&2u32));
        assert_eq!(a.key_order(), b.key_order());
        assert_eq!(a.cache_size(), b.cache_size());

        // Absent key: both report a miss and leave the cache untouched.
        let missing = a.hash(&99u32);
        assert_eq!(a.pop_raw_with_hash(missing, &99u32), None);
        assert_eq!(b.pop_raw(&99u32), None);
        assert_eq!(a.key_order(), b.key_order());

        // Borrowed lookup form (String key probed by &str).
        let mut s: LruCache<String, u32> = LruCache::new(4);
        s.cache_set("a".to_string(), 1);
        s.cache_set("b".to_string(), 2);
        let hash = s.hash("a");
        assert_eq!(s.pop_raw_with_hash(hash, "a"), Some(("a".to_string(), 1)));
        assert_eq!(s.key_order(), vec!["b".to_string()]);
    }

    #[test]
    fn cache_set_returning_entry_promotes_replaced_entry_to_mru() {
        // `cache_set_returning_entry` matches `Cached::cache_set`: a write over an
        // existing key promotes it to MRU, exactly as remove-then-insert would.
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.cache_set(1u32, 10u32);
        c.cache_set(2u32, 20u32);
        c.cache_set(3u32, 30u32);
        assert_eq!(c.key_order(), vec![3, 2, 1]);

        // Replacing the least-recently-used entry returns the stored pair and moves
        // the entry to the front.
        assert_eq!(c.cache_set_returning_entry(1, 11), Some((1, 10)));
        assert_eq!(
            c.key_order(),
            vec![1, 3, 2],
            "cache_set_returning_entry must promote the replaced entry to MRU"
        );
        assert_eq!(c.value_order(), vec![11, 30, 20]);

        // Remove-then-insert agrees.
        assert_eq!(c.pop_raw(&2u32), Some((2, 20)));
        assert_eq!(Cached::cache_set(&mut c, 2u32, 22u32), None);
        assert_eq!(c.key_order(), vec![2, 1, 3]);

        // A fresh key inserts at the front and returns None.
        let mut d = LruCache::builder().max_size(3).build().unwrap();
        assert_eq!(d.cache_set_returning_entry(1u32, 10u32), None);
        assert_eq!(d.cache_set_returning_entry(2u32, 20u32), None);
        assert_eq!(d.key_order(), vec![2, 1]);
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
    fn cache_set_over_existing_key_promotes_to_mru() {
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.set(1, 10);
        c.set(2, 20);
        c.set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        // Overwriting the least-recently-used key returns the old value and promotes
        // the entry to most-recently-used: a write is an access.
        assert_eq!(Cached::cache_set(&mut c, 1, 11), Some(10));
        assert_eq!(c.key_order(), vec![1, 3, 2]);
        assert_eq!(c.value_order(), vec![11, 30, 20]);
    }

    #[test]
    fn cache_set_over_current_mru_keeps_it_at_the_front() {
        // Exercises `move_to_front` on a slot that is already the head: `unlink` +
        // `link_after` must be a no-op there, not corrupt the chain.
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.set(1, 10);
        c.set(2, 20);
        c.set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);

        assert_eq!(Cached::cache_set(&mut c, 3, 33), Some(30));
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        assert_eq!(c.value_order(), vec![33, 20, 10]);

        // The chain is still intact in both directions: the LRU victim is still 1,
        // and every entry is reachable.
        assert_eq!(c.cache_size(), 3);
        let entries: Vec<(u32, u32)> = c.iter_order().into_iter().map(|(k, v)| (k, *v)).collect();
        assert_eq!(entries, vec![(3, 33), (2, 20), (1, 10)]);
        c.set(4, 40);
        assert_eq!(c.key_order(), vec![4, 3, 2]);
        assert_eq!(c.cache_get(&1), None);
    }

    #[test]
    fn cache_set_over_sole_entry_of_capacity_one_cache() {
        let mut c: LruCache<u32, u32> = LruCache::builder().max_size(1).build().unwrap();
        c.set(1, 10);
        assert_eq!(Cached::cache_set(&mut c, 1, 11), Some(10));
        assert_eq!(c.key_order(), vec![1]);
        assert_eq!(c.value_order(), vec![11]);
        assert_eq!(c.cache_size(), 1);
        // Still evicts correctly afterwards.
        c.set(2, 20);
        assert_eq!(c.key_order(), vec![2]);
        assert_eq!(c.cache_size(), 1);
    }

    #[test]
    fn cache_set_promotion_changes_the_capacity_eviction_victim() {
        // The user-visible consequence of promote-on-set: after overwriting the LRU
        // entry, the next insertion evicts the *other* old entry instead.
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.set(1, 10);
        c.set(2, 20);
        c.set(3, 30);
        // 1 is the LRU victim, but overwriting it makes 2 the victim.
        assert_eq!(Cached::cache_set(&mut c, 1, 11), Some(10));
        c.set(4, 40);
        assert_eq!(c.key_order(), vec![4, 1, 3]);
        assert_eq!(c.cache_get(&2), None, "2 became the LRU victim");
        assert_eq!(c.cache_peek(&1), Some(&11));
    }

    #[test]
    fn cache_peek_still_does_not_promote_after_set_does() {
        // `cache_set` and `cache_peek` must remain distinguishable: only the write
        // touches recency.
        let mut c = LruCache::builder().max_size(3).build().unwrap();
        c.set(1, 10);
        c.set(2, 20);
        c.set(3, 30);
        assert_eq!(c.cache_peek(&1), Some(&10));
        assert_eq!(c.key_order(), vec![3, 2, 1], "peek must not promote");
        assert!(c.cache_contains(&1), "contains must not promote");
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        // The write on the same key does promote.
        assert_eq!(Cached::cache_set(&mut c, 1, 11), Some(10));
        assert_eq!(c.key_order(), vec![1, 3, 2]);
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
    fn cache_remove_entry_with_panicking_on_evict_still_counts_eviction() {
        // The entry is popped and counted BEFORE `on_evict` runs, so a panicking
        // callback must not leave the removed entry uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(|_k: &u32, _v: &u32| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        let r = catch_unwind(AssertUnwindSafe(|| c.cache_remove_entry(&1u32)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(c.cache_get(&1u32), None, "entry must still be removed");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
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
        assert_store_and_order_agree(&c);
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

    // ---------------------------------------------------------------------
    // Certification coverage for the index/drain rewrite of `retain`,
    // `cache_clear_with_on_evict` and the `*_order`
    // collectors. The bar is that observable behavior is UNCHANGED, so these
    // pin side-effect ordering, counters, panic paths, and the store/order
    // invariant the pre-sizing depends on.
    // ---------------------------------------------------------------------

    /// The hash table and the LRU chain must describe exactly the same entries, and
    /// every order collector must agree with the chain. `store.len() == chain length`
    /// is precisely the assumption the `Vec::with_capacity(self.store.len())` pre-size
    /// in `iter_order`/`iter_order_raw`/`key_order`/`value_order` relies on.
    fn assert_store_and_order_agree<K, V, S>(c: &LruCache<K, V, S>)
    where
        K: Hash + Eq + Clone + std::fmt::Debug,
        V: Clone + PartialEq + std::fmt::Debug,
        S: BuildHasher,
    {
        let n = c.cache_size();
        assert_eq!(c.store.len(), n, "cache_size must equal hash table length");
        let chain: Vec<usize> = c.order.iter_indices().collect();
        assert_eq!(
            chain.len(),
            n,
            "live LRU chain length must equal store.len() (the *_order pre-size assumption)"
        );
        let keys = c.key_order();
        let vals = c.value_order();
        let entries = c.iter_order();
        let raw = c.iter_order_raw();
        assert_eq!(keys.len(), n, "key_order length");
        assert_eq!(vals.len(), n, "value_order length");
        assert_eq!(entries.len(), n, "iter_order length");
        assert_eq!(raw.len(), n, "iter_order_raw length");
        for (pos, &i) in chain.iter().enumerate() {
            let (k, v) = c.order.get(i);
            assert_eq!(k, &keys[pos], "key_order disagrees with the chain");
            assert_eq!(
                c.get_index(c.hash(k), k),
                Some(i),
                "hash table must resolve {k:?} back to its chain slot (no orphan/stale entries)"
            );
            assert_eq!(v, &*vals[pos], "value_order disagrees with the chain");
            assert_eq!(k, &raw[pos].0);
            assert_eq!(v, &raw[pos].1);
            assert_eq!(k, &entries[pos].0);
            assert_eq!(v, &*entries[pos].1);
        }
    }

    #[test]
    #[should_panic(expected = "live LRU chain length must equal store.len()")]
    fn invariant_helper_detects_a_desynced_store() {
        // Self-test: the certification helper must actually fail on a divergence,
        // otherwise every assertion built on it is vacuous.
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        c.store.clear(); // chain still holds the entry
        assert_store_and_order_agree(&c);
    }

    #[test]
    #[should_panic(expected = "hash table must resolve")]
    fn invariant_helper_detects_a_stale_table_pointer() {
        // The other direction: a chain entry whose table entry points at the wrong
        // slot. Lengths still agree, so only the per-entry resolution check catches it.
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        let slot_of_1 = c.order.iter_indices().next().unwrap();
        let _unlinked = c.order.push_front((2, 20)); // chain entry with no table entry
        let hash = c.hash(&2u32);
        c.insert_index(hash, slot_of_1); // table entry pointing at the wrong slot
        assert_store_and_order_agree(&c);
    }

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn remove_index_on_back_walks_the_cache_down_to_empty() {
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);

        // `order.back()` is the LRU slot -- the documented alternative index source.
        assert_eq!(c.remove_index(c.order.back()), (1, 10));
        assert_eq!(c.key_order(), vec![3, 2]);
        assert_store_and_order_agree(&c);

        assert_eq!(c.remove_index(c.order.back()), (2, 20));
        // Sole remaining entry: back() and the MRU slot are the same slot.
        assert_eq!(c.order.back(), c.order.iter_indices().next().unwrap());
        assert_eq!(c.remove_index(c.order.back()), (3, 30));
        assert_eq!(c.cache_size(), 0);
        assert!(c.key_order().is_empty());
        assert_store_and_order_agree(&c);

        // Empty and immediately reusable, with eviction still correct afterwards.
        for i in 1..=5u32 {
            c.cache_set(i, i * 10);
        }
        assert_eq!(c.key_order(), vec![5, 4, 3, 2]);
        assert_store_and_order_agree(&c);
    }

    #[test]
    #[should_panic(expected = "invalid index")]
    fn remove_index_on_empty_cache_back_panics() {
        // `back()` of an empty cache is the sentinel slot, which holds no value.
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        let _ = c.remove_index(c.order.back());
    }

    #[test]
    #[should_panic(expected = "invalid index")]
    fn remove_index_on_vacant_slot_panics() {
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        let slots: Vec<usize> = c.order.iter_indices().collect();
        // Free the slot through the normal path, then replay its index.
        assert_eq!(c.cache_remove(&2u32), Some(20));
        let _ = c.remove_index(slots[0]);
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn remove_index_out_of_range_panics() {
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        let _ = c.remove_index(9_999);
    }

    #[test]
    #[should_panic(expected = "out of sync")]
    fn remove_index_on_desynced_store_hits_the_unreachable_arm() {
        // The `unreachable!` arm fires when the LRU chain still holds an entry the hash
        // table has lost. Simulate that drift directly -- there is no public way to
        // reach it, which is exactly why it is `unreachable!` and not an `Option`.
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        let slot = c.order.iter_indices().next().unwrap();
        c.store.clear();
        let _ = c.remove_index(slot);
    }

    #[test]
    fn stale_slot_index_after_insert_targets_the_recycled_entry() {
        // Pins the contract documented on `remove_index` / `iter_indices`: collected
        // indices stay valid across removals of OTHER slots, but an insert recycles a
        // freed slot, so replaying a stale index silently hits the new occupant. Any
        // consuming shard that interleaves inserts into an index sweep is buggy.
        let mut c: LruCache<u32, u32> = LruCache::new(8);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        let snapshot: Vec<usize> = c.order.iter_indices().collect(); // MRU -> LRU: 3, 2, 1

        // Removing the middle entry leaves the other two indices valid.
        assert_eq!(c.remove_index(snapshot[1]), (2, 20));
        assert_eq!(c.order.get(snapshot[0]).1, 30);
        assert_eq!(c.order.get(snapshot[2]).1, 10);

        // The insert recycles slot `snapshot[1]` ...
        c.cache_set(4, 40);
        assert_eq!(
            c.order.iter_indices().next(),
            Some(snapshot[1]),
            "the new MRU entry must occupy the recycled slot"
        );
        // ... so replaying the stale index removes key 4, not key 2.
        assert_eq!(
            c.remove_index(snapshot[1]),
            (4, 40),
            "a stale index replayed after an insert removes the recycled entry"
        );
        assert_eq!(c.key_order(), vec![3, 1]);
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_predicate_visits_entries_mru_to_lru() {
        // The predicate visit order is observable through `FnMut` and must match the
        // pre-rewrite `order.iter()` walk: MRU -> LRU, recency order (not insertion).
        let mut c: LruCache<u32, u32> = LruCache::new(5);
        for i in 1..=4u32 {
            c.cache_set(i, i * 10);
        }
        assert_eq!(c.cache_get(&2), Some(&20)); // promote: MRU -> LRU is 2, 4, 3, 1
        let mut seen = Vec::new();
        c.retain(|k, v| {
            seen.push((*k, *v));
            true
        });
        assert_eq!(seen, vec![(2, 20), (4, 40), (3, 30), (1, 10)]);
    }

    #[test]
    fn retain_on_evict_order_follows_recency_not_insertion() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(6)
            .on_evict(move |k: &u32, v: &u32| seen2.lock().unwrap().push((*k, *v)))
            .build()
            .unwrap();
        for i in 1..=5u32 {
            c.cache_set(i, i * 10);
        }
        // Promote the two entries that will be removed to the extremes of the chain.
        assert_eq!(c.cache_get(&1), Some(&10)); // MRU -> LRU: 1, 5, 4, 3, 2
        let removed = c.retain(|k, _v| *k != 1 && *k != 2);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(1, 10), (2, 20)],
            "on_evict must fire in MRU -> LRU chain order, not insertion order"
        );
        assert_eq!(c.key_order(), vec![5, 4, 3]);
        assert_eq!(c.cache_evictions(), Some(2));
        assert_eq!(
            removed, 2,
            "retain must return the number of entries removed"
        );
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_returns_count_of_removed_entries() {
        // The return value must equal the number of entries actually removed, and that
        // must agree with both the `cache_size()` delta and the number of `on_evict`
        // invocations. `LruCache` has no expiry dimension, so the count is exactly the
        // number of predicate rejections.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c = LruCache::builder()
            .max_size(10)
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..6u32 {
            c.cache_set(i, i * 10);
        }
        let size_before = c.cache_size();
        let removed = c.retain(|k, _v| k % 2 == 0);
        let size_after = c.cache_size();

        assert_eq!(removed, 3, "keys 1, 3, 5 rejected by the predicate");
        assert_eq!(size_before - size_after, removed);
        assert_eq!(fired.load(AOrdering::Relaxed), removed);
    }

    #[test]
    fn retain_removes_all_entries() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |k: &u32, _v: &u32| seen2.lock().unwrap().push(*k))
            .build()
            .unwrap();
        for i in 0..4u32 {
            c.cache_set(i, i * 10);
        }
        let removed = c.retain(|_k, _v| false);
        assert_eq!(c.cache_size(), 0);
        assert!(c.key_order().is_empty());
        assert_eq!(*seen.lock().unwrap(), vec![3, 2, 1, 0]);
        assert_eq!(c.cache_evictions(), Some(4));
        assert_eq!(removed, 4, "all four entries were removed");
        assert_store_and_order_agree(&c);

        // Reusable: every slot was freed, and refilling past capacity still evicts LRU.
        for i in 10..15u32 {
            c.cache_set(i, i);
        }
        assert_eq!(c.key_order(), vec![14, 13, 12, 11]);
        assert_eq!(c.cache_evictions(), Some(5));
        assert_eq!(*seen.lock().unwrap(), vec![3, 2, 1, 0, 10]);
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_removing_nothing_is_a_no_op() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..3u32 {
            c.cache_set(i, i * 10);
        }
        let before = c.key_order();
        let removed = c.retain(|_k, _v| true);
        assert_eq!(
            c.key_order(),
            before,
            "retain must not perturb recency order"
        );
        assert_eq!(c.cache_size(), 3);
        assert_eq!(count.load(AOrdering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(removed, 0, "nothing was removed");
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_on_empty_cache_never_calls_the_predicate() {
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        let mut calls = 0usize;
        let removed = c.retain(|_k, _v| {
            calls += 1;
            false
        });
        assert_eq!(calls, 0);
        assert_eq!(c.cache_size(), 0);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(removed, 0);
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_without_on_evict_still_counts_evictions() {
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        for i in 0..4u32 {
            c.cache_set(i, i * 10);
        }
        let (hits, misses) = (c.cache_hits(), c.cache_misses());
        let removed = c.retain(|k, _v| *k >= 2);
        assert_eq!(c.cache_evictions(), Some(2));
        assert_eq!(c.key_order(), vec![3, 2]);
        assert_eq!(removed, 2);
        // The removal path must not look like a lookup: hit/miss counters are untouched.
        assert_eq!(c.cache_hits(), hits);
        assert_eq!(c.cache_misses(), misses);
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_removes_all_and_none_and_frees_slots() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        for i in 0..4u32 {
            c.cache_set(i, i * 10);
        }
        let removed = c.retain(|_k, _v| true);
        assert_eq!(removed, 0);
        assert_eq!(c.key_order(), vec![3, 2, 1, 0]);
        assert_eq!(count.load(AOrdering::Relaxed), 0);
        assert_store_and_order_agree(&c);

        let removed = c.retain(|_k, _v| false);
        assert_eq!(removed, 4);
        assert_eq!(c.cache_size(), 0);
        assert!(c.key_order().is_empty());
        assert_eq!(count.load(AOrdering::Relaxed), 4);
        assert_eq!(c.cache_evictions(), Some(4));
        assert_store_and_order_agree(&c);

        // Freed slots are reusable and capacity eviction still fires normally.
        for i in 10..15u32 {
            c.cache_set(i, i);
        }
        assert_eq!(c.key_order(), vec![14, 13, 12, 11]);
        assert_eq!(count.load(AOrdering::Relaxed), 5);
        assert_eq!(c.cache_evictions(), Some(5));
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_then_capacity_eviction_picks_the_right_victim() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(3)
            .on_evict(move |k: &u32, _v: &u32| seen2.lock().unwrap().push(*k))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        c.retain(|k, _v| *k != 2);
        assert_eq!(c.key_order(), vec![3, 1]);

        c.cache_set(4, 40); // fits exactly
        assert_eq!(c.key_order(), vec![4, 3, 1]);
        assert_eq!(c.cache_evictions(), Some(1));

        c.cache_set(5, 50); // overflows: LRU (1) is evicted
        assert_eq!(c.key_order(), vec![5, 4, 3]);
        assert_eq!(*seen.lock().unwrap(), vec![2, 1]);
        assert_eq!(c.cache_evictions(), Some(2));
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_with_panicking_on_evict_leaves_cache_consistent() {
        // The entry is removed AND counted BEFORE notifying, so a panicking callback
        // can never leave an entry half-removed or removed-but-uncounted. The
        // remaining doomed entries stay (the loop unwinds), but `evictions` IS
        // credited for the panicking entry since counting happens before the call.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(5)
            .on_evict(move |k: &i32, v: &i32| {
                seen2.lock().unwrap().push((*k, *v));
                assert_ne!(*k, 3, "boom");
            })
            .build()
            .unwrap();
        for i in 0i32..5 {
            c.cache_set(i, i * 10);
        }
        // MRU -> LRU: 4, 3, 2, 1, 0; doomed are 3 then 1, and 3 panics first.
        let r = catch_unwind(AssertUnwindSafe(|| c.retain(|k, _v| k % 2 == 0)));
        assert!(r.is_err(), "on_evict should have panicked");

        assert_eq!(*seen.lock().unwrap(), vec![(3, 30)]);
        // Key 3 is fully gone (removed before the callback ran); key 1 was never reached.
        assert!(c.cache_peek(&3).is_none());
        assert_eq!(c.cache_peek(&1), Some(&10));
        assert_eq!(c.cache_size(), 4);
        assert_eq!(c.key_order(), vec![4, 2, 1, 0]);
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "evictions is credited BEFORE the callback, so a panicking callback still counts"
        );
        // No entry is both removed and still present.
        assert_store_and_order_agree(&c);

        // The cache still works: a second retain finishes the job for the survivor.
        c.retain(|k, _v| k % 2 == 0);
        assert_eq!(*seen.lock().unwrap(), vec![(3, 30), (1, 10)]);
        assert_eq!(c.key_order(), vec![4, 2, 0]);
        assert_eq!(c.cache_evictions(), Some(2));
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn retain_with_panicking_predicate_leaves_cache_untouched() {
        // The predicate runs during the index scan, before any removal, so an unwind
        // out of it must not have removed anything.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: LruCache<u32, u32> = LruCache::new(5);
        for i in 0..5u32 {
            c.cache_set(i, i * 10);
        }
        let before = c.key_order();
        let r = catch_unwind(AssertUnwindSafe(|| {
            c.retain(|k, _v| {
                assert_ne!(*k, 2, "boom");
                false
            })
        }));
        assert!(r.is_err(), "the predicate should have panicked");
        assert_eq!(c.key_order(), before);
        assert_eq!(c.cache_size(), 5);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn cache_clear_with_on_evict_fires_mru_to_lru() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(5)
            .on_evict(move |k: &u32, v: &u32| seen2.lock().unwrap().push((*k, *v)))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        assert_eq!(c.cache_get(&1), Some(&10)); // MRU -> LRU: 1, 3, 2
        let (hits, misses) = (c.cache_hits(), c.cache_misses());
        c.cache_clear_with_on_evict();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(1, 10), (3, 30), (2, 20)],
            "callbacks must fire MRU -> LRU, the same order the key-by-key drain used"
        );
        assert_eq!(c.cache_evictions(), Some(3));
        // The wholesale drain must not look like a lookup either.
        assert_eq!(c.cache_hits(), hits);
        assert_eq!(c.cache_misses(), misses);
        assert_store_and_order_agree(&c);

        // A second clear on the now-empty cache fires nothing further.
        c.cache_clear_with_on_evict();
        assert_eq!(seen.lock().unwrap().len(), 3);
        assert_eq!(c.cache_evictions(), Some(3));
    }

    #[test]
    fn cache_clear_with_on_evict_on_empty_cache_is_a_no_op() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruCache::builder()
            .max_size(4)
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_clear_with_on_evict();
        assert_eq!(count.load(AOrdering::Relaxed), 0);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(c.cache_size(), 0);

        // Still usable, and the cleared-then-populated cache behaves normally.
        c.cache_set(1, 10);
        assert_eq!(c.cache_get(&1), Some(&10));
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn cache_clear_with_on_evict_counts_without_a_callback() {
        // The eviction count must not depend on whether a callback is configured:
        // attaching a purely observational `on_evict` cannot change `evictions`.
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(c.cache_evictions(), Some(2));
        assert!(c.key_order().is_empty());
        c.cache_set(3, 30);
        assert_eq!(c.cache_get(&3), Some(&30));
        assert_store_and_order_agree(&c);

        // Same sequence, with a no-op callback: identical count.
        let mut with_cb: LruCache<u32, u32> = LruCache::builder()
            .max_size(4)
            .on_evict(|_: &u32, _: &u32| {})
            .build()
            .unwrap();
        with_cb.cache_set(1, 10);
        with_cb.cache_set(2, 20);
        with_cb.cache_clear_with_on_evict();
        assert_eq!(with_cb.cache_evictions(), c.cache_evictions());
    }

    #[test]
    fn cache_clear_with_panicking_on_evict_leaves_cache_empty_and_counted() {
        // `drain_all` empties the cache before any callback runs, and `evictions` is
        // credited for the whole batch up front, so a panic mid-callback still leaves
        // an empty, consistent, reusable cache.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(5)
            .on_evict(move |k: &u32, v: &u32| {
                seen2.lock().unwrap().push((*k, *v));
                assert_ne!(*k, 2, "boom");
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);

        let r = catch_unwind(AssertUnwindSafe(|| c.cache_clear_with_on_evict()));
        assert!(r.is_err(), "on_evict should have panicked");
        // MRU -> LRU is 3, 2, 1; the callback for 1 never ran.
        assert_eq!(*seen.lock().unwrap(), vec![(3, 30), (2, 20)]);
        assert_eq!(c.cache_size(), 0);
        assert!(c.key_order().is_empty());
        assert_eq!(
            c.cache_evictions(),
            Some(3),
            "evictions is credited for the whole batch before callbacks run"
        );
        assert_store_and_order_agree(&c);

        // Reusable after the unwind.
        c.cache_set(7, 70);
        assert_eq!(c.cache_get(&7), Some(&70));
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn drain_all_with_holes_and_repeated_cycles() {
        // `drain_all` walks the live chain only, so slabs full of freed slots (from
        // retain/remove churn) must not resurrect removed entries.
        let mut c: LruCache<u32, u32> = LruCache::new(8);
        for i in 0..6u32 {
            c.cache_set(i, i * 10);
        }
        c.retain(|k, _v| k % 2 == 0); // frees three slots
        assert_eq!(c.cache_remove(&0u32), Some(0));
        let drained = c.drain_all();
        assert_eq!(drained, vec![(4, 40), (2, 20)]);
        assert_eq!(c.cache_size(), 0);
        assert_store_and_order_agree(&c);

        // Repeated fill/drain cycles keep both structures in step.
        for round in 0..3u32 {
            for i in 0..5u32 {
                c.cache_set(round * 100 + i, i);
            }
            assert_store_and_order_agree(&c);
            let drained = c.drain_all();
            assert_eq!(drained.len(), 5);
            assert_eq!(c.cache_size(), 0);
            assert_store_and_order_agree(&c);
        }
    }

    #[test]
    fn pop_raw_with_hash_wrong_hash_misses_and_leaves_the_entry() {
        // Documented misuse contract: `hash` MUST be `self.hash(k)`. Flipping the top
        // bit changes hashbrown's control tag deterministically, so the probe cannot
        // match -- the lookup misses and the entry stays put.
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        let before = c.key_order();

        let wrong = c.hash(&2u32) ^ (1u64 << 63);
        assert_eq!(
            c.pop_raw_with_hash(wrong, &2u32),
            None,
            "a hash that is not self.hash(k) must silently miss"
        );
        assert_eq!(c.cache_size(), 3);
        assert_eq!(c.key_order(), before);
        assert_eq!(c.cache_peek(&2), Some(&20));
        assert_store_and_order_agree(&c);

        // The correct hash still finds it, and the miss left no damage behind.
        let right = c.hash(&2u32);
        assert_eq!(c.pop_raw_with_hash(right, &2u32), Some((2, 20)));
        assert_eq!(c.key_order(), vec![3, 1]);
        assert_store_and_order_agree(&c);

        // A correct hash paired with an absent key misses without side effects, and
        // does not fire callbacks or counters (pop_raw* is silent).
        assert_eq!(c.pop_raw_with_hash(c.hash(&99u32), &99u32), None);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn pop_raw_with_hash_does_not_promote_or_touch_hit_miss_counters() {
        let mut c: LruCache<u32, u32> = LruCache::new(4);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        let hits = c.cache_hits();
        let misses = c.cache_misses();
        assert_eq!(c.pop_raw_with_hash(c.hash(&3u32), &3u32), Some((3, 30)));
        assert_eq!(c.cache_hits(), hits);
        assert_eq!(c.cache_misses(), misses);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(c.key_order(), vec![2, 1]);
    }

    #[test]
    fn cache_set_returning_entry_returns_the_stored_key_and_evicts_at_capacity() {
        #[derive(Debug, Clone)]
        struct TaggedKey {
            id: u32,
            tag: &'static str,
        }
        impl PartialEq for TaggedKey {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }
        impl Eq for TaggedKey {}
        impl Hash for TaggedKey {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }

        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(2)
            .on_evict(move |k: &TaggedKey, v: &u32| seen2.lock().unwrap().push((k.id, *v)))
            .build()
            .unwrap();
        c.cache_set(
            TaggedKey {
                id: 1,
                tag: "stored",
            },
            10,
        );
        c.cache_set(
            TaggedKey {
                id: 2,
                tag: "stored",
            },
            20,
        );

        // Replacing returns the STORED key (with its ignored field), not the caller's.
        let displaced = c
            .cache_set_returning_entry(
                TaggedKey {
                    id: 1,
                    tag: "caller",
                },
                11,
            )
            .expect("existing key must return the displaced entry");
        assert_eq!(displaced.0.id, 1);
        assert_eq!(
            displaced.0.tag, "stored",
            "the displaced pair must carry the stored key, not the caller's"
        );
        assert_eq!(displaced.1, 10);
        // No eviction: a replace is not an eviction.
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(c.cache_evictions(), Some(0));
        // The write promoted 1 to MRU, so 2 is now the LRU victim.
        assert_eq!(
            c.key_order().iter().map(|k| k.id).collect::<Vec<_>>(),
            vec![1, 2]
        );

        // A fresh key over capacity evicts the LRU entry through `check_capacity`.
        assert_eq!(
            c.cache_set_returning_entry(
                TaggedKey {
                    id: 3,
                    tag: "stored"
                },
                30
            ),
            None
        );
        assert_eq!(*seen.lock().unwrap(), vec![(2, 20)]);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(
            c.key_order().iter().map(|k| k.id).collect::<Vec<_>>(),
            vec![3, 1]
        );
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn cache_set_over_an_existing_key_rebinds_the_stored_key() {
        // The native LRU fast path replaces the whole `(K, V)` slot, so an overwrite
        // rebinds the entry to the caller's key and drops the previously stored one.
        // This is unconditional: there is no key-replacement policy knob.
        #[derive(Debug, Clone)]
        struct TaggedKey {
            id: u32,
            tag: &'static str,
        }
        impl PartialEq for TaggedKey {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }
        impl Eq for TaggedKey {}
        impl Hash for TaggedKey {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }

        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(2)
            .on_evict(move |k: &TaggedKey, _v: &u32| seen2.lock().unwrap().push(k.tag))
            .build()
            .unwrap();

        c.cache_set(
            TaggedKey {
                id: 1,
                tag: "first",
            },
            10,
        );
        assert_eq!(
            c.cache_set(
                TaggedKey {
                    id: 1,
                    tag: "second"
                },
                20
            ),
            Some(10)
        );

        // The stored key is now the caller's most recent key.
        assert_eq!(
            c.key_order().iter().map(|k| k.tag).collect::<Vec<_>>(),
            vec!["second"]
        );

        // A further overwrite hands the *stored* key back through
        // `cache_set_returning_entry`, and `on_evict` sees the stored key on removal.
        let displaced = c
            .cache_set_returning_entry(
                TaggedKey {
                    id: 1,
                    tag: "third",
                },
                30,
            )
            .expect("existing key must return the displaced entry");
        assert_eq!(displaced.0.tag, "second");
        assert_eq!(displaced.1, 20);

        let (removed_key, removed_val) = c
            .cache_remove_entry(&TaggedKey {
                id: 1,
                tag: "probe",
            })
            .expect("the entry must still be present");
        assert_eq!(removed_key.tag, "third");
        assert_eq!(removed_val, 30);
        assert_eq!(*seen.lock().unwrap(), vec!["third"]);
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn order_collectors_presize_from_live_count_not_capacity() {
        // The pre-size must come from `store.len()`, never from the configured
        // capacity: a large `max_size` with two live entries must not allocate large
        // vectors.
        let mut c: LruCache<u32, u32> = LruCache::new(65_536);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        assert!(
            c.key_order().capacity() < 1_000,
            "key_order pre-sized from capacity instead of the live count"
        );
        assert!(c.value_order().capacity() < 1_000);
        assert!(c.iter_order().capacity() < 1_000);
        assert!(c.iter_order_raw().capacity() < 1_000);

        // An empty cache pre-sizes to zero and allocates nothing.
        let empty: LruCache<u32, u32> = LruCache::new(65_536);
        assert_eq!(empty.key_order().capacity(), 0);
        assert!(empty.key_order().is_empty());
        assert!(empty.iter_order().is_empty());
        assert!(empty.value_order().is_empty());
        assert!(empty.iter_order_raw().is_empty());
    }

    #[test]
    fn store_len_and_live_chain_never_diverge_under_mixed_operations() {
        // The `*_order` pre-sizing assumes `store.len()` equals the live chain length.
        // Hammer every mutating path that touches the slab -- capacity eviction,
        // retain, remove_index, drain_all, clear-with-callback, replace
        // -- and assert the two views agree after every single step.
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let mut c = LruCache::builder()
            .max_size(8)
            .on_evict(move |k: &u32, _v: &u32| seen2.lock().unwrap().push(*k))
            .build()
            .unwrap();

        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for step in 0..600u32 {
            let r = xorshift(&mut state);
            let key = (r >> 32) as u32 % 12;
            match r % 11 {
                0..=3 => {
                    c.cache_set(key, key * 10);
                }
                4 => {
                    let _ = c.cache_get(&key);
                }
                5 => {
                    let _ = c.cache_remove(&key);
                }
                6 => {
                    c.retain(|k, _v| k % 3 != 0);
                }
                7 => {
                    c.retain(|k, _v| *k != key);
                }
                8 => {
                    if c.cache_size() > 0 {
                        let _ = c.remove_index(c.order.back());
                    }
                }
                9 => {
                    let before = c.cache_size();
                    let drained = c.drain_all();
                    assert_eq!(drained.len(), before, "drain_all must return every entry");
                    assert_eq!(c.cache_size(), 0, "drain_all must leave the cache empty");
                }
                _ => c.cache_clear_with_on_evict(),
            }
            assert_eq!(
                c.store.len(),
                c.order.iter_indices().count(),
                "store/chain diverged at step {step} (op {})",
                r % 11
            );
            assert!(
                c.cache_size() <= c.capacity(),
                "capacity exceeded at step {step}"
            );
            assert_store_and_order_agree(&c);
        }
    }

    // `set_max_size` / `try_set_max_size` / `cache_clear_with_on_evict` are also inherent
    // methods, and inherent methods win at a concrete call site. These helpers take a generic
    // bound, so they can only reach the trait method: they are the reachability the traits add.
    fn resize_through_trait<T: crate::CacheSetMaxSize>(
        cache: &mut T,
        max_size: usize,
    ) -> Option<usize> {
        cache.set_max_size(max_size)
    }

    fn try_resize_through_trait<T: crate::CacheSetMaxSize>(
        cache: &mut T,
        max_size: usize,
    ) -> Result<Option<usize>, crate::SetMaxSizeError> {
        cache.try_set_max_size(max_size)
    }

    fn clear_with_on_evict_through_trait<T: crate::CacheClearWithOnEvict>(cache: &mut T) {
        cache.cache_clear_with_on_evict();
    }

    #[test]
    fn set_max_size_through_trait_grows_like_the_inherent_method() {
        let mut c: LruCache<u32, u32> = LruCache::new(2);
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        assert_eq!(resize_through_trait(&mut c, 4), Some(2));
        assert_eq!(c.capacity(), 4);
        assert_eq!(c.cache_size(), 2);
        assert_eq!(c.cache_get(&1), Some(&10));
    }

    #[test]
    fn set_max_size_through_trait_shrinks_eagerly_and_fires_on_evict() {
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
        assert_eq!(c.cache_get(&3), Some(&30));
        assert_eq!(c.cache_get(&4), Some(&40));

        assert_eq!(resize_through_trait(&mut c, 2), Some(4));
        assert_eq!(c.capacity(), 2);
        // Eviction happens before the call returns, not on the next insert.
        assert_eq!(c.cache_size(), 2);
        assert_eq!(evicted.load(AOrdering::Relaxed), 2);
        assert_eq!(c.cache_evictions(), Some(2));
        assert_eq!(c.cache_get(&3), Some(&30));
        assert_eq!(c.cache_get(&4), Some(&40));
        assert_eq!(c.cache_get(&1), None);
        assert_eq!(c.cache_get(&2), None);
        assert_store_and_order_agree(&c);
    }

    #[test]
    fn try_set_max_size_through_trait_rejects_zero() {
        let mut c: LruCache<u32, u32> = LruCache::new(2);
        assert_eq!(
            try_resize_through_trait(&mut c, 0),
            Err(crate::SetMaxSizeError::ZeroMaxSize)
        );
        assert_eq!(c.capacity(), 2);
        assert_eq!(try_resize_through_trait(&mut c, 3), Ok(Some(2)));
        assert_eq!(c.capacity(), 3);
    }

    #[test]
    fn cache_clear_with_on_evict_through_trait_fires_for_all_entries() {
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

        clear_with_on_evict_through_trait(&mut c);
        assert_eq!(c.cache_size(), 0);
        assert_eq!(evicted.load(AOrdering::Relaxed), 3);
        assert_eq!(c.cache_evictions(), Some(3));
    }
}
