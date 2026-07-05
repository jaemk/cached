use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

use crate::{CachedIter, CachedPeek, CloneCached};

use super::{CacheEvict, Cached, DefaultHashBuilder, LruCache, TimedEntry};
use std::hash::BuildHasher;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Timed LRU Cache
///
/// Stores a limited number of values,
/// evicting expired and least-used entries.
/// Time expiration is determined based on entry insertion time.
/// By default, the TTL of an entry is not refreshed on retrieval.
/// Set `refresh = true` to refresh the TTL on cache hits.
///
/// Note: This cache is in-memory only
///
/// **`len` / `iter` / `evict` contract**: `len()` returns the raw stored entry count
/// and may include expired-but-not-yet-swept entries. `iter()` omits expired entries
/// from the view but does not remove them. Call `evict()` (via [`CacheEvict`](crate::CacheEvict))
/// to physically remove expired entries and obtain an accurate live count.
///
/// The optional type parameter `S` selects the hash builder. It defaults to
/// [`DefaultHashBuilder`] (ahash when the `ahash` feature is enabled, otherwise
/// `std::collections::hash_map::RandomState`). Supply a custom `S` via
/// [`LruTtlCacheBuilder::hasher`] to use a different hasher.
pub struct LruTtlCache<K, V, S = DefaultHashBuilder> {
    pub(super) store: LruCache<K, TimedEntry<V>, S>,
    pub(super) size: usize,
    pub(super) ttl: Duration,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) refresh: bool,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V, S> std::fmt::Debug for LruTtlCache<K, V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LruTtlCache")
            .field("size", &self.size)
            .field("ttl", &self.ttl)
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("refresh", &self.refresh)
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V, S> Clone for LruTtlCache<K, V, S>
where
    K: Clone + Hash + Eq,
    V: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        let store = self.store.clone();
        Self {
            store,
            size: self.size,
            ttl: self.ttl,
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            refresh: self.refresh,
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Typestate marker for [`LruTtlCacheBuilder`]: no eviction callback set.
pub struct NoEvict;

/// Typestate marker for [`LruTtlCacheBuilder`]: eviction callback has been set.
///
/// When this marker is active, [`LruTtlCacheBuilder::build`] requires
/// `K: 'static` and `V: 'static` because the callback must be wired into
/// the inner LRU store.
pub struct HasEvict;

/// Builder for [`LruTtlCache`].
///
/// Obtain one via [`LruTtlCache::builder`].
///
/// The `E` type parameter is a compile-time marker:
/// - [`NoEvict`] (the default): no eviction callback has been set; `build`
///   does **not** require `K: 'static` or `V: 'static`.
/// - [`HasEvict`]: an eviction callback was registered via [`on_evict`](LruTtlCacheBuilder::on_evict);
///   `build` requires `K: 'static + V: 'static` so the callback
///   can be wired into the inner LRU eviction path.
///
/// The `S` type parameter selects the hash builder; it defaults to [`DefaultHashBuilder`].
/// Call [`.hasher()`](LruTtlCacheBuilder::hasher) to use a custom hasher.
pub struct LruTtlCacheBuilder<K, V, E = NoEvict, S = DefaultHashBuilder> {
    size: Option<usize>,
    ttl: Option<Duration>,
    refresh: bool,
    on_evict: Option<super::OnEvict<K, V>>,
    hasher: S,
    _evict: PhantomData<E>,
}

// size / ttl / refresh work regardless of eviction state or hasher
impl<K, V, E, S> LruTtlCacheBuilder<K, V, E, S> {
    /// Set the maximum number of entries. Required.
    #[doc(alias = "size")]
    #[doc(alias = "capacity")]
    #[must_use]
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.size = Some(max_size);
        self
    }

    /// Set the TTL for cache entries. Required.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set the TTL for cache entries in whole seconds. Equivalent to
    /// `ttl(Duration::from_secs(secs))`.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl_secs(self, secs: u64) -> Self {
        self.ttl(Duration::from_secs(secs))
    }

    /// Set the TTL for cache entries in milliseconds. Equivalent to
    /// `ttl(Duration::from_millis(millis))`.
    ///
    /// Overrides any previously set ttl/ttl_secs/ttl_millis on this builder.
    #[must_use]
    pub fn ttl_millis(self, millis: u64) -> Self {
        self.ttl(Duration::from_millis(millis))
    }

    /// Set whether cache hits refresh the TTL of the accessed entry.
    #[must_use]
    pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Switch to a custom hash builder `S2`, returning a builder parameterized on `S2`.
    ///
    /// The hasher is used to hash keys in the internal backing `LruCache`. Calling this
    /// method changes the builder's `S` type parameter so `build()` returns an
    /// `LruTtlCache<K, V, S2>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{Cached, LruTtlCache};
    /// use cached::time::Duration;
    /// use std::collections::hash_map::RandomState;
    ///
    /// let mut cache = LruTtlCache::<u32, u32>::builder()
    ///     .max_size(10)
    ///     .ttl_secs(60)
    ///     .hasher(RandomState::new())
    ///     .build()
    ///     .unwrap();
    /// cache.cache_set(1, 100);
    /// assert_eq!(cache.cache_get(&1), Some(&100));
    /// ```
    #[doc(alias = "with_hasher")]
    #[must_use]
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> LruTtlCacheBuilder<K, V, E, S2> {
        LruTtlCacheBuilder {
            size: self.size,
            ttl: self.ttl,
            refresh: self.refresh,
            on_evict: self.on_evict,
            hasher,
            _evict: PhantomData,
        }
    }
}

// on_evict transitions the builder from NoEvict -> HasEvict
impl<K, V, S> LruTtlCacheBuilder<K, V, NoEvict, S> {
    /// Set a callback to be invoked when an entry is evicted. The callback fires for:
    /// - LRU capacity eviction: inserting past `max_size` evicts the least-recently-used entry.
    /// - Capacity shrink via [`set_max_size`](LruTtlCache::set_max_size) /
    ///   [`try_set_max_size`](LruTtlCache::try_set_max_size).
    /// - TTL-expiry sweeps via [`evict`](LruTtlCache::evict).
    /// - Lazy TTL-expiry sweeps on access: a [`cache_get`](crate::Cached::cache_get) /
    ///   `cache_get_mut` (and the `cache_get_or_set*` factory paths) that finds an expired
    ///   entry removes or replaces it and fires the callback.
    /// - Overwriting an already-expired entry via [`cache_set`](crate::Cached::cache_set) /
    ///   [`cache_try_set`](crate::Cached::cache_try_set): the displaced value is filtered from
    ///   the return (`None`), so it fires the callback and counts an eviction.
    /// - Explicit [`cache_remove`](crate::Cached::cache_remove) /
    ///   [`cache_remove_entry`](crate::Cached::cache_remove_entry), even when the removed
    ///   entry was already expired.
    ///
    /// Calling this method changes the builder's type to
    /// `LruTtlCacheBuilder<K, V, `[`HasEvict`]`>`, which requires `K: 'static`
    /// and `V: 'static` at [`build`](LruTtlCacheBuilder::build) time so the
    /// callback can be wired into the inner LRU eviction path.
    ///
    /// Does **not** fire on [`cache_clear`](crate::Cached::cache_clear).
    /// Use [`cache_clear_with_on_evict`](LruTtlCache::cache_clear_with_on_evict)
    /// instead to opt into callback firing and eviction counter increments when clearing
    /// all entries.
    #[must_use]
    pub fn on_evict(
        self,
        on_evict: impl Fn(&K, &V) + Send + Sync + 'static,
    ) -> LruTtlCacheBuilder<K, V, HasEvict, S> {
        LruTtlCacheBuilder {
            size: self.size,
            ttl: self.ttl,
            refresh: self.refresh,
            on_evict: Some(Arc::new(on_evict)),
            hasher: self.hasher,
            _evict: PhantomData,
        }
    }
}

// build without an eviction callback -- no 'static required
impl<K, V, S: BuildHasher> LruTtlCacheBuilder<K, V, NoEvict, S> {
    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `max_size` or `ttl` was not set, if `ttl` is zero, or if `max_size` is `0`.
    pub fn build(self) -> Result<LruTtlCache<K, V, S>, super::BuildError>
    where
        K: Hash + Eq + Clone,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("max_size"))?;
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        super::validate_ttl(ttl)?;
        LruTtlCache::new_internal(size, ttl, self.refresh, self.hasher)
    }
}

// build with an eviction callback -- 'static required for sync_on_evict
impl<K, V, S: BuildHasher> LruTtlCacheBuilder<K, V, HasEvict, S> {
    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `max_size` or `ttl` was not set, if `ttl` is zero, or if `max_size` is `0`.
    pub fn build(self) -> Result<LruTtlCache<K, V, S>, super::BuildError>
    where
        K: Hash + Eq + Clone + 'static,
        V: 'static,
    {
        let size = self
            .size
            .ok_or(super::BuildError::MissingRequired("max_size"))?;
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        super::validate_ttl(ttl)?;
        let mut cache = LruTtlCache::new_internal(size, ttl, self.refresh, self.hasher)?;
        cache.on_evict = self.on_evict;
        cache.sync_on_evict();
        Ok(cache)
    }
}

impl<K: Hash + Eq + Clone, V> LruTtlCache<K, V> {
    /// Construct a ready-to-use [`LruTtlCache`] holding up to `max_size` entries with
    /// the given `ttl`.
    ///
    /// For optional settings (`refresh_on_hit`, `on_evict`) use [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is `0`, if `ttl` is zero, or if pre-allocating the backing
    /// store for `max_size` entries fails (e.g. `usize::MAX`). Use [`builder`](Self::builder)
    /// with [`build`](LruTtlCacheBuilder::build) to handle those cases without panicking.
    #[must_use]
    pub fn new(max_size: usize, ttl: Duration) -> Self {
        Self::builder()
            .max_size(max_size)
            .ttl(ttl)
            .build()
            .expect("LruTtlCache::new requires a non-zero max_size with a valid allocation and a non-zero ttl")
    }

    /// Return a builder for constructing a [`LruTtlCache`].
    #[must_use]
    pub fn builder() -> LruTtlCacheBuilder<K, V> {
        LruTtlCacheBuilder {
            size: None,
            ttl: None,
            refresh: false,
            on_evict: None,
            hasher: super::new_default_hash_builder(),
            _evict: PhantomData,
        }
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> LruTtlCache<K, V, S> {
    pub(super) fn sync_on_evict(&mut self)
    where
        K: 'static,
        V: 'static,
    {
        if self.on_evict.is_some() {
            let on_evict_ext = self.on_evict.clone();
            self.store.on_evict = Some(Arc::new(move |k, entry| {
                if let Some(on_evict) = &on_evict_ext {
                    on_evict(k, &entry.value);
                }
            }));
        }
    }

    /// `true` if the entry is still live.
    /// `expires_at = None` means the entry never expires (TTL was disabled at insert time).
    #[inline]
    pub(super) fn entry_live(expires_at: Option<Instant>) -> bool {
        expires_at.is_none_or(|t| Instant::now() < t)
    }

    /// Insert `entry` for `key`, returning the previous value only if it was still live.
    ///
    /// A displaced expired value is filtered from the return (matching the get paths), so it is
    /// dropped silently from the caller's view; in that case fire `on_evict` and count an
    /// eviction. The inner `LruCache::cache_set` does not fire `on_evict` on an overwrite, so the
    /// callback fires exactly once here. The key is cloned only when a callback is configured.
    fn set_entry(&mut self, key: K, entry: TimedEntry<V>) -> Option<V> {
        let key_for_evict = self.on_evict.as_ref().map(|_| key.clone());
        match self.store.cache_set(key, entry) {
            Some(old) if Self::entry_live(old.expires_at) => Some(old.value),
            Some(old) => {
                if let (Some(on_evict), Some(k)) = (&self.on_evict, &key_for_evict) {
                    on_evict(k, &old.value);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
                None
            }
            None => None,
        }
    }

    /// Compute the expiry instant for a new or refreshed entry given the current TTL.
    /// Returns `None` when `ttl` is zero (expiry disabled), or `Some(now + ttl)`.
    /// Returns `Err(CacheSetError::TimeBounds)` on overflow.
    #[inline]
    pub(super) fn compute_expires_at(
        ttl: Duration,
        now: Instant,
    ) -> Result<Option<Instant>, super::CacheSetError> {
        if ttl.is_zero() {
            Ok(None)
        } else {
            now.checked_add(ttl)
                .map(Some)
                .ok_or(super::CacheSetError::TimeBounds)
        }
    }

    fn new_internal(
        size: usize,
        ttl: Duration,
        refresh: bool,
        hasher: S,
    ) -> Result<Self, super::BuildError> {
        let mut store = LruCache::builder().max_size(size).hasher(hasher).build()?;
        store.disable_hit_miss_tracking();
        Ok(LruTtlCache {
            store,
            size,
            ttl,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            refresh,
            on_evict: None,
        })
    }

    /// Return an iterator of key-value pairs with their expiry instants
    /// in the current order from most to least recently used.
    /// Items past their expiry will be excluded.
    pub fn iter_order(&self) -> Vec<(K, (Option<Instant>, V))>
    where
        K: Clone,
        V: Clone,
    {
        self.store
            .iter_order()
            .into_iter()
            .filter_map(|(k, entry)| {
                let expires_at = entry.expires_at;
                if Self::entry_live(expires_at) {
                    Some((k.clone(), (expires_at, entry.value.clone())))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Return a `Vec` of keys in the current order from most
    /// to least recently used.
    /// Items past their expiry will be excluded.
    pub fn key_order(&self) -> Vec<K>
    where
        K: Clone,
    {
        self.store
            .order
            .iter()
            .filter_map(|(k, entry)| {
                if Self::entry_live(entry.expires_at) {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Return a `Vec` of (expiry, value) pairs in the current order
    /// from most to least recently used.
    /// Items past their expiry will be excluded.
    pub fn value_order(&self) -> Vec<(Option<Instant>, V)>
    where
        V: Clone,
    {
        self.store
            .order
            .iter()
            .filter_map(|(_k, entry)| {
                let expires_at = entry.expires_at;
                if Self::entry_live(expires_at) {
                    Some((expires_at, entry.value.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns the maximum number of entries this cache will hold before evicting.
    ///
    /// This is the bound set via [`LruTtlCacheBuilder::max_size`], not the current number
    /// of entries — use [`cache_size`](crate::Cached::cache_size) for that.
    #[doc(alias = "size")]
    #[doc(alias = "max_size")]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.size
    }

    /// Change the maximum number of entries, returning the previous capacity;
    /// shrinking below the current entry count immediately evicts least-recently-used
    /// entries.
    ///
    /// Eviction on shrink fires `on_evict` and counts evictions until the cache
    /// fits. Growing the capacity does not pre-allocate; the backing stores grow
    /// on demand as entries are inserted.
    ///
    /// This is useful for sizing a `#[cached(create = "{ ... }")]` cache from a value
    /// loaded at startup (e.g. config), then adjusting it later as load changes.
    ///
    /// # Panics
    ///
    /// Panics if `max_size` is 0. Use [`try_set_max_size`](LruTtlCache::try_set_max_size)
    /// to validate first and avoid the panic.
    ///
    /// # See also
    ///
    /// [`LruCache::set_max_size`](super::LruCache::set_max_size) and
    /// [`TtlSortedCache::set_max_size`](super::TtlSortedCache::set_max_size) are
    /// parallel methods on the other LRU-family stores. All stores also provide a
    /// fallible `try_set_max_size` counterpart.
    pub fn set_max_size(&mut self, max_size: usize) -> Option<usize> {
        assert!(max_size > 0, "max_size must be greater than zero");
        let prev = self.store.set_max_size(max_size);
        self.size = self.store.capacity;
        prev
    }

    /// Fallible counterpart of [`set_max_size`](LruTtlCache::set_max_size): validates
    /// that `max_size` is non-zero and then delegates to `set_max_size`.
    /// Returns the previous capacity wrapped in `Some` on success.
    ///
    /// # Errors
    ///
    /// Returns [`SetMaxSizeError::ZeroSize`](super::SetMaxSizeError) if `max_size` is 0.
    pub fn try_set_max_size(
        &mut self,
        max_size: usize,
    ) -> Result<Option<usize>, super::SetMaxSizeError> {
        if max_size == 0 {
            return Err(super::SetMaxSizeError::ZeroSize);
        }
        Ok(self.set_max_size(max_size))
    }

    /// Evict expired values from the cache.
    #[must_use]
    pub fn evict(&mut self) -> usize {
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        let mut removed = 0;
        let now = Instant::now();
        self.store.retain_silent(|key, entry| {
            // None means never-expires; Some(t) expires when now >= t.
            if entry.expires_at.is_none_or(|t| now < t) {
                true
            } else {
                if let Some(on_evict) = on_evict {
                    on_evict(key, &entry.value);
                }
                evictions.fetch_add(1, Ordering::Relaxed);
                removed += 1;
                false
            }
        });
        removed
    }

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Entries where `keep` returns `false` (or that are already expired)
    /// are removed. `on_evict` is called and the eviction counter incremented
    /// for each removed entry.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) {
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        self.store.retain_silent(|key, entry| {
            let expired = !Self::entry_live(entry.expires_at);
            if expired || !keep(key, &entry.value) {
                if let Some(on_evict) = on_evict {
                    on_evict(key, &entry.value);
                }
                evictions.fetch_add(1, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }

    /// Remove all entries and fire the `on_evict` callback for each one, incrementing the
    /// evictions counter.
    ///
    /// Unlike [`cache_clear`](crate::Cached::cache_clear) (which removes entries silently),
    /// this method invokes `on_evict` for every removed entry (whether or not they had expired)
    /// and increments `evictions`. If no `on_evict` callback was configured, it falls back to
    /// the plain `cache_clear`.
    pub fn cache_clear_with_on_evict(&mut self) {
        if self.on_evict.is_none() {
            return self.cache_clear();
        }
        let keys = self.store.key_order();
        let mut removed = Vec::with_capacity(keys.len());
        for k in &keys {
            if let Some(pair) = self.store.pop_raw(k) {
                removed.push(pair);
            }
        }
        let count = removed.len() as u64;
        if count > 0 {
            self.evictions.fetch_add(count, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (k, entry) in &removed {
                on_evict(k, &entry.value);
            }
        }
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> Cached<K, V> for LruTtlCache<K, V, S> {
    type Error = super::CacheSetError;

    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(key);
        if let Some(index) = self.store.get_index(hash, key) {
            let entry = &self.store.order.get(index).1;
            if Self::entry_live(entry.expires_at) {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let now = Instant::now();
                    let new_exp = Self::compute_expires_at(self.ttl, now)
                        .ok()
                        .flatten()
                        .or(self.store.order.get(index).1.expires_at);
                    self.store.order.get_mut(index).1.expires_at = new_exp;
                }
                Some(&self.store.order.get(index).1.value)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((k, entry)) = self.store.pop_raw(key) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &entry.value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    fn cache_get_mut<Q>(&mut self, key: &Q) -> std::option::Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(key);
        if let Some(index) = self.store.get_index(hash, key) {
            let entry = &self.store.order.get(index).1;
            if Self::entry_live(entry.expires_at) {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let now = Instant::now();
                    let new_exp = Self::compute_expires_at(self.ttl, now)
                        .ok()
                        .flatten()
                        .or(self.store.order.get(index).1.expires_at);
                    self.store.order.get_mut(index).1.expires_at = new_exp;
                }
                Some(&mut self.store.order.get_mut(index).1.value)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((k, entry)) = self.store.pop_raw(key) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&k, &entry.value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        let key_for_evict = key.clone();
        let ttl = self.ttl;
        let setter = || {
            // Anchor the expiry AFTER the factory runs so a slow factory does
            // not eat into the fresh entry's TTL (CORE-3).
            let value = f();
            let now = Instant::now();
            let expires_at = Self::compute_expires_at(ttl, now).unwrap_or(None);
            TimedEntry { expires_at, value }
        };
        let (was_present, was_valid, old_entry, entry) =
            self.store
                .get_or_set_with_if(key, setter, |entry| Self::entry_live(entry.expires_at));
        if was_present && was_valid {
            if self.refresh {
                let now = Instant::now();
                let new_exp = Self::compute_expires_at(self.ttl, now)
                    .ok()
                    .flatten()
                    .or(entry.expires_at);
                entry.expires_at = new_exp;
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            if let Some(old) = old_entry {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old.value);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        &mut entry.value
    }

    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        let key_for_evict = key.clone();
        let ttl = self.ttl;
        let setter = || {
            // Anchor the expiry after the factory succeeds (CORE-3).
            let value = f()?;
            let now = Instant::now();
            let expires_at = Self::compute_expires_at(ttl, now).unwrap_or(None);
            Ok(TimedEntry { expires_at, value })
        };
        let (was_present, was_valid, old_entry, entry) =
            self.store
                .try_get_or_set_with_if(key, setter, |entry| Self::entry_live(entry.expires_at))?;
        if was_present && was_valid {
            if self.refresh {
                let now = Instant::now();
                let new_exp = Self::compute_expires_at(self.ttl, now)
                    .ok()
                    .flatten()
                    .or(entry.expires_at);
                entry.expires_at = new_exp;
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            if let Some(old) = old_entry {
                if let Some(on_evict) = &self.on_evict {
                    on_evict(&key_for_evict, &old.value);
                }
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        Ok(&mut entry.value)
    }

    /// Insert a key-value pair. Returns the previous value only if it had not yet expired.
    /// An expired previous value is filtered from the return; it fires `on_evict` and counts as
    /// an eviction, matching the other removal paths.
    ///
    /// Overwriting an existing key replaces the value in-place **without** refreshing the key's
    /// LRU recency; the entry keeps its position in the eviction order (its expiry is still reset
    /// from the current TTL). Read with `cache_get` first if you need to promote it.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let now = Instant::now();
        let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
        self.set_entry(
            key,
            TimedEntry {
                expires_at,
                value: val,
            },
        )
    }

    fn cache_try_set(&mut self, key: K, val: V) -> Result<Option<V>, super::CacheSetError> {
        let now = Instant::now();
        let expires_at = Self::compute_expires_at(self.ttl, now)?;
        Ok(self.set_entry(
            key,
            TimedEntry {
                expires_at,
                value: val,
            },
        ))
    }

    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, entry)) = self.store.pop_raw(k) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if Self::entry_live(entry.expires_at) {
                Some(entry.value)
            } else {
                None
            }
        } else {
            None
        }
    }

    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, entry)) = self.store.pop_raw(k) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
            Some((stored_k, entry.value))
        } else {
            None
        }
    }

    fn cache_clear(&mut self) {
        self.store.cache_clear();
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        // Delegate to the inner LruCache's reset which preserves the hash builder.
        self.store.cache_reset();
        self.cache_reset_metrics();
    }
    fn cache_reset_metrics(&mut self) {
        self.misses.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.store.cache_reset_metrics();
    }
    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }
    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits.load(Ordering::Relaxed))
    }
    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses.load(Ordering::Relaxed))
    }
    fn cache_evictions(&self) -> Option<u64> {
        // Combined evictions from underlying store and our time-based removals
        Some(self.evictions.load(Ordering::Relaxed) + self.store.cache_evictions().unwrap_or(0))
    }
    fn cache_capacity(&self) -> Option<usize> {
        Some(self.size)
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> CachedIter<K, V> for LruTtlCache<K, V, S> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        CachedIter::iter(&self.store).filter_map(move |(k, entry)| {
            if Self::entry_live(entry.expires_at) {
                Some((k, &entry.value))
            } else {
                None
            }
        })
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> CachedPeek<K, V> for LruTtlCache<K, V, S> {
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.cache_peek(k)
            && Self::entry_live(entry.expires_at)
        {
            return Some(&entry.value);
        }
        None
    }
}

impl<K: Hash + Eq + Clone, V, S: BuildHasher> crate::CacheTtl for LruTtlCache<K, V, S> {
    fn ttl(&self) -> Option<Duration> {
        // A zero TTL means expiry is disabled.
        if self.ttl.is_zero() {
            None
        } else {
            Some(self.ttl)
        }
    }
    /// A zero `ttl` disables expiry — exactly equivalent to `unset_ttl`.
    /// Returns the previous TTL, or `None` if expiry was already disabled.
    fn set_ttl(&mut self, ttl: Duration) -> Option<Duration> {
        let old = self.ttl;
        self.ttl = ttl;
        if old.is_zero() { None } else { Some(old) }
    }
    fn unset_ttl(&mut self) -> Option<Duration> {
        let old = self.ttl;
        self.ttl = Duration::ZERO;
        if old.is_zero() { None } else { Some(old) }
    }
    fn refresh_on_hit(&self) -> bool {
        self.refresh
    }
    fn set_refresh_on_hit(&mut self, refresh: bool) -> bool {
        let old = self.refresh;
        self.refresh = refresh;
        old
    }
}

impl<K: Hash + Eq + Clone, V: Clone, S: BuildHasher + Clone> CloneCached<K, V>
    for LruTtlCache<K, V, S>
{
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let hash = self.store.hash(k);
        if let Some(index) = self.store.get_index(hash, k) {
            let entry = &self.store.order.get(index).1;
            let expired = !Self::entry_live(entry.expires_at);
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(self.store.order.get(index).1.value.clone()), true)
            } else {
                self.store.order.move_to_front(index);
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let now = Instant::now();
                    let new_exp = Self::compute_expires_at(self.ttl, now)
                        .ok()
                        .flatten()
                        .or(self.store.order.get(index).1.expires_at);
                    self.store.order.get_mut(index).1.expires_at = new_exp;
                }
                (Some(self.store.order.get(index).1.value.clone()), false)
            }
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            (None, false)
        }
    }

    /// Peek at the entry (including expired entries) without any read side effects.
    ///
    /// Returns `(Some(v), true)` for an expired entry, `(Some(v), false)` for a live
    /// entry, and `(None, false)` when the key is absent. Does not update hit/miss
    /// counters, does not promote in LRU order, and does not renew the TTL.
    fn cache_peek_with_expiry_status<Q>(&self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        // Use the inner LruCache's `cache_peek` to avoid LRU promotion.
        if let Some(entry) = self.store.cache_peek(k) {
            let expired = !Self::entry_live(entry.expires_at);
            (Some(entry.value.clone()), expired)
        } else {
            (None, false)
        }
    }
}

#[cfg(feature = "async_core")]
impl<K, V, S> CachedGetOrSetAsync<K, V> for LruTtlCache<K, V, S>
where
    K: Hash + Eq + Clone + Send,
    S: BuildHasher + Send,
{
    fn async_cache_get_or_set_with_mut<'a, F, Fut>(
        &'a mut self,
        key: K,
        f: F,
    ) -> impl Future<Output = &'a mut V> + Send + 'a
    where
        K: 'a,
        V: Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: Future<Output = V> + Send + 'a,
    {
        async move {
            let key_for_evict = key.clone();
            let ttl = self.ttl;
            let setter = || async move {
                // Anchor the expiry after the factory resolves (CORE-3).
                let value = f().await;
                let now = Instant::now();
                let expires_at = Self::compute_expires_at(ttl, now).unwrap_or(None);
                TimedEntry { expires_at, value }
            };
            let (was_present, was_valid, old_entry, entry) = self
                .store
                .get_or_set_with_if_async(key, setter, |entry| Self::entry_live(entry.expires_at))
                .await;
            if was_present && was_valid {
                if self.refresh {
                    let now = Instant::now();
                    let new_exp = Self::compute_expires_at(self.ttl, now)
                        .ok()
                        .flatten()
                        .or(entry.expires_at);
                    entry.expires_at = new_exp;
                }
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Some(old) = old_entry {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key_for_evict, &old.value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            &mut entry.value
        }
    }

    fn async_cache_try_get_or_set_with_mut<'a, F, Fut, E>(
        &'a mut self,
        key: K,
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
            let key_for_evict = key.clone();
            let ttl = self.ttl;
            let setter = || async move {
                let new_val = f().await?;
                let now = Instant::now();
                let expires_at = Self::compute_expires_at(ttl, now).unwrap_or(None);
                Ok(TimedEntry {
                    expires_at,
                    value: new_val,
                })
            };
            let (was_present, was_valid, old_entry, entry) = self
                .store
                .try_get_or_set_with_if_async(key, setter, |entry| {
                    Self::entry_live(entry.expires_at)
                })
                .await?;
            if was_present && was_valid {
                if self.refresh {
                    let now = Instant::now();
                    let new_exp = Self::compute_expires_at(self.ttl, now)
                        .ok()
                        .flatten()
                        .or(entry.expires_at);
                    entry.expires_at = new_exp;
                }
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Some(old) = old_entry {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key_for_evict, &old.value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            Ok(&mut entry.value)
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V, S: BuildHasher> CacheEvict for LruTtlCache<K, V, S> {
    fn evict(&mut self) -> usize {
        LruTtlCache::evict(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cached, CachedExt};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::Arc;
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let before = c.cache_evictions().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(60));
        // The previous value has expired: overwriting filters it (None), fires on_evict once,
        // and counts one eviction.
        assert_eq!(c.cache_set(1, 200), None);
        assert_eq!(c.cache_evictions(), Some(before + 1));
        assert_eq!(fired.load(AtomicOrdering::Relaxed), 1);
        // Overwriting the now-live value returns it, no on_evict and no new eviction.
        assert_eq!(c.cache_set(1, 300), Some(200));
        assert_eq!(c.cache_evictions(), Some(before + 1));
        assert_eq!(fired.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn cache_set_over_existing_key_does_not_promote_recency() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        // Overwriting the least-recently-used key updates the value in-place and
        // returns the old (still-live) value, but must NOT move it to the front.
        assert_eq!(c.cache_set(1, 11), Some(10));
        assert_eq!(c.key_order(), vec![3, 2, 1]);
        assert_eq!(c.cache_get(&1), Some(&11));
    }

    #[test]
    fn new_returns_ready_cache_respecting_max_size_and_ttl() {
        use crate::CacheTtl;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(2, Duration::from_millis(50));
        assert_eq!(c.capacity(), 2);
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_millis(50)));
        assert_eq!(c.cache_set(1, 10), None);
        assert_eq!(c.cache_get(&1), Some(&10));
        // max_size respected.
        c.cache_set(2, 20);
        c.cache_set(3, 30); // evicts LRU (1)
        assert_eq!(c.cache_size(), 2);
        assert_eq!(c.cache_get(&1), None);
        // ttl respected.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(c.cache_get(&2), None, "entry must expire after ttl");
    }

    #[test]
    #[should_panic(expected = "non-zero max_size with a valid allocation and a non-zero ttl")]
    fn new_zero_max_size_panics() {
        let _c: LruTtlCache<u32, u32> = LruTtlCache::new(0, Duration::from_secs(1));
    }

    #[test]
    #[should_panic(expected = "non-zero max_size with a valid allocation and a non-zero ttl")]
    fn new_zero_ttl_panics() {
        let _c: LruTtlCache<u32, u32> = LruTtlCache::new(2, Duration::ZERO);
    }

    #[test]
    fn ttl_secs_and_ttl_millis_set_duration() {
        use crate::CacheTtl;
        let c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl_secs(7)
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(7)));

        let c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl_millis(250)
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_millis(250)));
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        use crate::CacheTtl;
        let c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(10))
            .ttl_secs(5)
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_secs(5)));

        let c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl_secs(10)
            .ttl_millis(500)
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(Duration::from_millis(500)));
    }

    #[test]
    fn status_does_not_inflate_inner_store_hits() {
        let mut cache = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        cache.store.cache_reset_metrics();

        // cache_get calls status() internally
        assert_eq!(cache.cache_get(&1), Some(&10));
        assert_eq!(
            cache.store.cache_hits(),
            Some(0),
            "inner LruCache must not record hits from status() promotion"
        );
        assert_eq!(
            cache.store.cache_misses(),
            Some(0),
            "inner LruCache must not record misses from status() promotion"
        );
    }

    #[test]
    fn capacity_returns_bound_not_live_size() {
        let mut cache = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(cache.capacity(), 3);
        assert_eq!(cache.cache_size(), 0);

        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        assert_eq!(cache.capacity(), 3);
        assert_eq!(cache.cache_size(), 2);

        // Eviction past the bound keeps capacity fixed while live count stays capped.
        cache.cache_set(3, 30);
        cache.cache_set(4, 40);
        assert_eq!(cache.capacity(), 3);
        assert_eq!(cache.cache_size(), 3);
    }

    #[test]
    fn reset_rebuilds_store_and_preserves_on_evict() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_for_callback = evicted.clone();
        let mut cache = LruTtlCache::builder()
            .max_size(1)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_key: &u8, _value: &u8| {
                evicted_for_callback.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();

        cache.set(1, 10);
        cache.cache_reset();
        assert_eq!(cache.cache_size(), 0);

        cache.set(2, 20);
        cache.set(3, 30);
        assert_eq!(evicted.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn try_new() {
        let c = LruTtlCache::<i32, i32>::builder()
            .max_size(0)
            .ttl(Duration::from_secs(1))
            .build();
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue {
                field: "max_size",
                ..
            }
        ));

        let c = LruTtlCache::<i32, i32>::builder()
            .max_size(usize::MAX)
            .ttl(Duration::from_secs(1))
            .build();
        assert!(matches!(
            c.unwrap_err(),
            super::super::BuildError::InvalidValue {
                field: "max_size",
                ..
            }
        ));
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruTtlCache::builder()
            .max_size(5)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(count.load(AtomicOrdering::Relaxed), 3);
        assert_eq!(c.evictions.load(AtomicOrdering::Relaxed), 3);
    }

    #[test]
    fn cache_clear_does_not_fire_on_evict() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c = LruTtlCache::builder()
            .max_size(5)
            .ttl(Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, AtomicOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            fired.load(AtomicOrdering::Relaxed),
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
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
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
    fn builder_does_not_require_static_without_on_evict() {
        // LruTtlCacheBuilder::build must not impose K: 'static or V: 'static
        // when no on_evict callback is configured.
        fn build_with_borrowed<'a>(_k: &'a str, _v: &'a str) -> LruTtlCache<&'a str, &'a str> {
            LruTtlCache::builder()
                .max_size(4)
                .ttl(Duration::from_secs(60))
                .build()
                .unwrap()
        }
        let mut cache = build_with_borrowed("key", "val");
        cache.cache_set("key", "val");
        assert_eq!(cache.cache_get(&"key"), Some(&"val"));
    }

    #[test]
    fn set_max_size_changes_capacity_and_evicts() {
        let mut cache: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        cache.cache_set(3, 30);
        assert_eq!(cache.capacity(), 3);

        // Shrink to 2: LRU entry (1) should be evicted.
        let prev = cache.set_max_size(2);
        assert_eq!(prev, Some(3));
        assert_eq!(cache.capacity(), 2);
        assert_eq!(cache.cache_size(), 2);

        // Insert beyond new cap triggers eviction.
        cache.cache_set(4, 40);
        assert_eq!(cache.cache_size(), 2);
    }

    #[test]
    fn set_max_size_shrink_fires_on_evict_and_counts_evictions() {
        use std::sync::Mutex;
        let evicted_keys: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let evicted_keys2 = evicted_keys.clone();
        let mut cache = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .on_evict(move |k: &u32, _v: &u32| {
                evicted_keys2.lock().unwrap().push(*k);
            })
            .build()
            .unwrap();

        cache.cache_set(1, 10);
        cache.cache_set(2, 20);
        cache.cache_set(3, 30);
        cache.cache_set(4, 40);
        // Touch 1 and 2 so 3 and 4 become least-recently-used.
        assert_eq!(cache.cache_get(&1), Some(&10));
        assert_eq!(cache.cache_get(&2), Some(&20));

        let evictions_before = cache.cache_evictions().expect("evictions tracked");
        let prev = cache.set_max_size(2);
        assert_eq!(prev, Some(4));
        assert_eq!(cache.capacity(), 2);
        assert_eq!(cache.cache_size(), 2);

        // Two entries were dropped; eviction counter must reflect that.
        assert_eq!(
            cache.cache_evictions().expect("evictions tracked") - evictions_before,
            2,
            "set_max_size shrink must increment cache_evictions by the number of dropped entries"
        );

        // on_evict must have fired for exactly the two LRU keys (3 and 4).
        let mut fired: Vec<u32> = evicted_keys.lock().unwrap().clone();
        fired.sort();
        assert_eq!(
            fired,
            vec![3, 4],
            "on_evict must fire for the evicted (least-recently-used) keys"
        );

        // The two most-recently-used entries must survive.
        assert_eq!(cache.cache_get(&1), Some(&10));
        assert_eq!(cache.cache_get(&2), Some(&20));
        assert_eq!(cache.cache_get(&3), None);
        assert_eq!(cache.cache_get(&4), None);
    }

    #[test]
    fn try_set_max_size_rejects_zero() {
        let mut cache: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        assert_eq!(
            cache.try_set_max_size(0),
            Err(super::super::SetMaxSizeError::ZeroSize)
        );
        assert_eq!(cache.try_set_max_size(5).unwrap(), Some(3));
    }

    #[test]
    #[should_panic(expected = "max_size must be greater than zero")]
    fn set_max_size_zero_panics() {
        let mut cache: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        cache.set_max_size(0);
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_async_trait() {
        use crate::CachedGetOrSetAsync;
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();

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
            CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 0, || async {
                _get(99).await
            })
            .await,
            &0
        );
    }

    #[test]
    fn test_diagnostics_and_traits() {
        let mut cache = LruTtlCache::builder()
            .max_size(3)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        cache.cache_set(1, 100);
        cache.cache_set(2, 200);

        // Debug
        let debug_str = format!("{:?}", cache);
        assert!(debug_str.contains("LruTtlCache"));
        assert!(debug_str.contains("size"));
        assert!(debug_str.contains("ttl"));
        assert!(debug_str.contains("hits"));
        assert!(debug_str.contains("misses"));

        // Clone
        let mut cloned = cache.clone();
        assert_eq!(cloned.cache_get(&1), Some(&100));
        assert_eq!(cloned.cache_get(&2), Some(&200));

        // Builder build errors
        let builder = LruTtlCache::<u32, u32>::builder();
        let built = builder.build();
        assert!(built.is_err()); // Missing both size and ttl

        let builder = LruTtlCache::<u32, u32>::builder().max_size(3);
        let built = builder.build();
        assert!(built.is_err()); // Missing ttl

        let builder = LruTtlCache::<u32, u32>::builder().ttl(Duration::from_secs(60));
        let built = builder.build();
        assert!(built.is_err()); // Missing size

        let builder = LruTtlCache::<u32, u32>::builder()
            .max_size(0)
            .ttl(Duration::from_secs(60));
        let built = builder.build();
        assert!(built.is_err()); // Size 0 is invalid

        let builder = LruTtlCache::<u32, u32>::builder()
            .max_size(3)
            .ttl(Duration::ZERO);
        let built = builder.build();
        assert!(built.is_err()); // Zero ttl is invalid
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        assert_eq!(c.cache_remove_entry(&999u32), None); // absent
        assert_eq!(c.cache_remove_entry(&1u32), Some((1u32, 100u32)));
        assert_eq!(c.cache_get(&1u32), None);
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(100));

        // cache_remove returns None for expired.
        assert_eq!(c.cache_remove(&1u32), None);

        // cache_remove_entry returns Some even for expired.
        c.cache_set(2u32, 200u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let removed = c.cache_remove_entry(&2u32);
        assert!(removed.is_some());
        assert_eq!(
            removed.expect("cache_remove_entry returns Some for expired"),
            (2u32, 200u32)
        );
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            c.cache_delete(&1u32),
            "cache_delete must be true even for expired entry"
        );
        assert!(!c.cache_delete(&1u32), "cache_delete false when absent");
    }

    #[test]
    fn cache_remove_entry_fires_on_evict_for_expired() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(50))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(100));

        let _ = c.cache_remove_entry(&1u32);
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "on_evict fires for expired entries"
        );

        let _ = c.cache_remove_entry(&999u32);
        assert_eq!(count.load(Ordering::Relaxed), 1, "no fire for absent key");
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let mut c = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(10))
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let before = c.cache_evictions().expect("evictions are always tracked");
        let _ = c.cache_remove_entry(&1u32); // expired but present -- must increment
        let _ = c.cache_remove_entry(&999u32); // absent -- must not increment
        assert_eq!(
            c.cache_evictions().expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }

    // --- custom hasher tests ---

    #[test]
    fn custom_hasher_get_set_round_trip() {
        use std::collections::hash_map::RandomState;
        let mut c = LruTtlCache::<u32, u32>::builder()
            .max_size(10)
            .ttl_secs(60)
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
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(5, Duration::from_secs(60));
        c.cache_set(1, 10);
        assert_eq!(c.cache_get(&1), Some(&10));
    }

    #[test]
    fn custom_hasher_respects_lru_eviction_and_ttl() {
        use std::collections::hash_map::RandomState;
        // Test LRU eviction
        let mut c = LruTtlCache::<u32, u32>::builder()
            .max_size(2)
            .ttl_secs(60)
            .hasher(RandomState::new())
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_get(&1); // make 1 most-recently-used
        c.cache_set(3, 30); // should evict 2
        assert_eq!(c.cache_get(&1), Some(&10));
        assert_eq!(c.cache_get(&2), None); // evicted
        assert_eq!(c.cache_get(&3), Some(&30));

        // Test TTL expiry
        let mut c2 = LruTtlCache::<u32, u32>::builder()
            .max_size(10)
            .ttl(Duration::from_millis(50))
            .hasher(RandomState::new())
            .build()
            .unwrap();
        c2.cache_set(1, 10);
        assert_eq!(c2.cache_get(&1), Some(&10));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(c2.cache_get(&1), None, "entry must expire after ttl");
    }

    // CORE-3: the sync get_or_set paths must anchor the expiry AFTER the factory
    // runs, so a factory slower than the TTL still yields a live entry.
    #[test]
    fn sync_expiry_anchored_after_factory() {
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(40))
            .build()
            .unwrap();
        let v = c.cache_get_or_set_with(1, || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            7
        });
        assert_eq!(*v, 7);
        assert_eq!(
            c.cache_get(&1),
            Some(&7),
            "entry must be live right after insert"
        );
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_expiry_anchored_after_factory() {
        use crate::CachedGetOrSetAsync;
        let mut c: LruTtlCache<u32, u32> = LruTtlCache::builder()
            .max_size(4)
            .ttl(Duration::from_millis(40))
            .build()
            .unwrap();
        let v = CachedGetOrSetAsync::async_cache_get_or_set_with(&mut c, 1, || async {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            7
        })
        .await;
        assert_eq!(*v, 7);
        assert_eq!(
            c.cache_get(&1),
            Some(&7),
            "entry must be live right after insert"
        );
    }
}
