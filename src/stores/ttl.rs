use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::Hash;

#[cfg(feature = "ahash")]
use ahash::RandomState;

#[cfg(not(feature = "ahash"))]
use std::collections::hash_map::RandomState;

use std::collections::{HashMap, hash_map::Entry};

#[cfg(feature = "async_core")]
use {super::CachedAsync, std::future::Future};

use crate::{CachedIter, CachedPeek, CloneCached};

use super::{CacheEvict, Cached, TimedEntry};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cache store bound by time
///
/// Values are timestamped when inserted and are
/// evicted if expired at time of retrieval.
///
/// Note: This cache is in-memory only
pub struct TtlCache<K, V> {
    pub(super) store: HashMap<K, TimedEntry<V>, RandomState>,
    pub(super) ttl: Duration,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) initial_capacity: Option<usize>,
    pub(super) refresh: bool,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> std::fmt::Debug for TtlCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtlCache")
            .field("ttl", &self.ttl)
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("initial_capacity", &self.initial_capacity)
            .field("refresh", &self.refresh)
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

impl<K, V> Clone for TtlCache<K, V>
where
    K: Clone + Hash + Eq,
    V: Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            ttl: self.ttl,
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            initial_capacity: self.initial_capacity,
            refresh: self.refresh,
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Builder for [`TtlCache`].
pub struct TtlCacheBuilder<K, V> {
    ttl: Option<Duration>,
    capacity: Option<usize>,
    refresh: bool,
    on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V> TtlCacheBuilder<K, V> {
    /// Set the TTL for cache entries. Required — `build()` returns
    /// `Err(BuildError::MissingRequired("ttl"))` if not set.
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

    /// Set the initial allocation capacity (optional).
    #[must_use]
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Set whether cache hits refresh the TTL of the accessed entry.
    #[must_use]
    pub fn refresh_on_hit(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set a callback to be invoked when an entry is evicted. The callback fires for:
    /// - TTL-expiry sweeps via [`evict`](TtlCache::evict).
    /// - Explicit [`cache_remove`](crate::Cached::cache_remove), even when the removed
    ///   entry was already expired (`cache_remove` returns `None` but still fires the
    ///   callback and increments the evictions counter).
    ///
    /// Does **not** fire on [`cache_clear`](crate::Cached::cache_clear).
    /// Use [`cache_clear_with_on_evict`](TtlCache::cache_clear_with_on_evict)
    /// instead to opt into callback firing when clearing all entries.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `ttl` was not set or is zero
    /// ([`BuildError::MissingRequired`](super::BuildError::MissingRequired) /
    /// [`BuildError::InvalidValue`](super::BuildError::InvalidValue)).
    pub fn build(self) -> Result<TtlCache<K, V>, super::BuildError>
    where
        K: Hash + Eq,
    {
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        super::validate_ttl(ttl)?;
        Ok(TtlCache {
            store: TtlCache::<K, V>::new_store(self.capacity),
            ttl,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            initial_capacity: self.capacity,
            refresh: self.refresh,
            on_evict: self.on_evict,
        })
    }
}

impl<K: Hash + Eq, V> TtlCache<K, V> {
    /// Construct a ready-to-use [`TtlCache`] with the given `ttl`.
    ///
    /// For optional settings (initial capacity, `refresh_on_hit`, `on_evict`) use
    /// [`builder`](Self::builder).
    ///
    /// # Panics
    ///
    /// Panics if `ttl` is zero. Use [`builder`](Self::builder) with
    /// [`build`](TtlCacheBuilder::build) to handle a zero TTL without panicking.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self::builder()
            .ttl(ttl)
            .build()
            .expect("TtlCache::new requires a non-zero ttl")
    }

    /// Return a builder for constructing a [`TtlCache`].
    #[must_use]
    pub fn builder() -> TtlCacheBuilder<K, V> {
        TtlCacheBuilder {
            ttl: None,
            capacity: None,
            refresh: false,
            on_evict: None,
        }
    }

    /// `true` if the entry is still live.
    /// `expires_at = None` means the entry never expires (TTL was disabled at insert time).
    #[inline]
    pub(super) fn entry_live(expires_at: Option<Instant>) -> bool {
        expires_at.is_none_or(|t| Instant::now() < t)
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

    fn new_store(capacity: Option<usize>) -> HashMap<K, TimedEntry<V>, RandomState> {
        capacity.map_or_else(
            || HashMap::with_hasher(RandomState::new()),
            |cap| HashMap::with_capacity_and_hasher(cap, RandomState::new()),
        )
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
        let entries: Vec<(K, TimedEntry<V>)> = self.store.drain().collect();
        let count = entries.len() as u64;
        if count > 0 {
            self.evictions.fetch_add(count, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (k, entry) in &entries {
                on_evict(k, &entry.value);
            }
        }
    }

    /// Evict expired values from the cache.
    #[must_use]
    pub fn evict(&mut self) -> usize {
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        let mut removed = 0;
        let now = Instant::now();
        self.store.retain(|key, entry| {
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
}

impl<K: Hash + Eq, V> Cached<K, V> for TtlCache<K, V> {
    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get_mut(key)
            && Self::entry_live(entry.expires_at)
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            if self.refresh {
                entry.expires_at = Self::compute_expires_at(self.ttl, Instant::now())
                    .ok()
                    .flatten()
                    .or(entry.expires_at);
            }
            // SAFETY: `ptr` points into a HashMap entry obtained from `get_mut`.
            // We return immediately without modifying the map, so the entry is
            // not moved while the returned reference is live. The raw pointer is
            // needed because the borrow checker cannot see that the `&mut entry`
            // borrow ends here when `refresh` mutated `entry.expires_at` above.
            let ptr = &entry.value as *const V;
            return Some(unsafe { &*ptr });
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        if let Some((k, entry)) = self.store.remove_entry(key) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    fn cache_get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get_mut(key)
            && Self::entry_live(entry.expires_at)
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            if self.refresh {
                entry.expires_at = Self::compute_expires_at(self.ttl, Instant::now())
                    .ok()
                    .flatten()
                    .or(entry.expires_at);
            }
            // SAFETY: same as `cache_get` — entry is not moved between obtaining
            // the pointer and returning, and `&mut self` prevents concurrent access.
            let ptr = &mut entry.value as *mut V;
            return Some(unsafe { &mut *ptr });
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        if let Some((k, entry)) = self.store.remove_entry(key) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&k, &entry.value);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                if Self::entry_live(occupied.get().expires_at) {
                    if self.refresh {
                        let now = Instant::now();
                        let new_exp = Self::compute_expires_at(self.ttl, now)
                            .ok()
                            .flatten()
                            .or(occupied.get().expires_at);
                        occupied.get_mut().expires_at = new_exp;
                    }
                    self.hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &occupied.get().value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    let val = f();
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                    occupied.insert(TimedEntry {
                        expires_at,
                        value: val,
                    });
                }
                &mut occupied.into_mut().value
            }
            Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                let val = f();
                let now = Instant::now();
                let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                &mut vacant
                    .insert(TimedEntry {
                        expires_at,
                        value: val,
                    })
                    .value
            }
        }
    }

    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        key: K,
        f: F,
    ) -> Result<&mut V, E> {
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                if Self::entry_live(occupied.get().expires_at) {
                    if self.refresh {
                        let now = Instant::now();
                        let new_exp = Self::compute_expires_at(self.ttl, now)
                            .ok()
                            .flatten()
                            .or(occupied.get().expires_at);
                        occupied.get_mut().expires_at = new_exp;
                    }
                    self.hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &occupied.get().value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    let val = f()?;
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                    occupied.insert(TimedEntry {
                        expires_at,
                        value: val,
                    });
                }
                Ok(&mut occupied.into_mut().value)
            }
            Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                let val = f()?;
                let now = Instant::now();
                let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                Ok(&mut vacant
                    .insert(TimedEntry {
                        expires_at,
                        value: val,
                    })
                    .value)
            }
        }
    }

    /// Insert a key-value pair. Returns the previous value only if it had not yet expired.
    /// Expired previous values are silently discarded.
    ///
    /// If computing the expiry instant overflows (very large TTL), the entry is stored
    /// with `expires_at = None` (never expires). Use [`cache_try_set`](crate::Cached::cache_try_set)
    /// when you need to detect this overflow condition.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let now = Instant::now();
        let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
        let entry = TimedEntry {
            expires_at,
            value: val,
        };
        self.store.insert(key, entry).and_then(|entry| {
            if Self::entry_live(entry.expires_at) {
                Some(entry.value)
            } else {
                None
            }
        })
    }

    fn cache_try_set(&mut self, key: K, val: V) -> Result<Option<V>, super::CacheSetError> {
        let now = Instant::now();
        let expires_at = Self::compute_expires_at(self.ttl, now)?;
        let entry = TimedEntry {
            expires_at,
            value: val,
        };
        Ok(self.store.insert(key, entry).and_then(|entry| {
            if Self::entry_live(entry.expires_at) {
                Some(entry.value)
            } else {
                None
            }
        }))
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, entry)) = self.store.remove_entry(k) {
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
        if let Some((stored_k, entry)) = self.store.remove_entry(k) {
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
        self.store.clear();
    }
    fn cache_reset_metrics(&mut self) {
        self.misses.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
    }
    fn cache_reset(&mut self) {
        // Entries are dropped in-place; `on_evict` is NOT called for cleared entries.
        self.store = Self::new_store(self.initial_capacity);
        self.cache_reset_metrics();
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
}

impl<K: Hash + Eq, V> CachedIter<K, V> for TtlCache<K, V> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.store.iter().filter_map(move |(k, entry)| {
            if Self::entry_live(entry.expires_at) {
                Some((k, &entry.value))
            } else {
                None
            }
        })
    }
}

impl<K: Hash + Eq, V> CachedPeek<K, V> for TtlCache<K, V> {
    fn cache_peek<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get(k)
            && Self::entry_live(entry.expires_at)
        {
            return Some(&entry.value);
        }
        None
    }
}

impl<K: Hash + Eq, V> crate::CacheTtl for TtlCache<K, V> {
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

impl<K: Hash + Eq + Clone, V: Clone> CloneCached<K, V> for TtlCache<K, V> {
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.store.get_mut(k) {
            let expired = !Self::entry_live(entry.expires_at);
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(entry.value.clone()), true)
            } else {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    let now = Instant::now();
                    let new_exp = Self::compute_expires_at(self.ttl, now)
                        .ok()
                        .flatten()
                        .or(entry.expires_at);
                    entry.expires_at = new_exp;
                }
                (Some(entry.value.clone()), false)
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
        if let Some(entry) = self.store.get(k) {
            let expired = !Self::entry_live(entry.expires_at);
            (Some(entry.value.clone()), expired)
        } else {
            (None, false)
        }
    }
}

#[cfg(feature = "async_core")]
impl<K, V> CachedAsync<K, V> for TtlCache<K, V>
where
    K: Hash + Eq + Clone + Send,
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
            match self.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if Self::entry_live(occupied.get().expires_at) {
                        if self.refresh {
                            let now = Instant::now();
                            let new_exp = Self::compute_expires_at(self.ttl, now)
                                .ok()
                                .flatten()
                                .or(occupied.get().expires_at);
                            occupied.get_mut().expires_at = new_exp;
                        }
                        self.hits.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &occupied.get().value);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        let now = Instant::now();
                        let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                        occupied.insert(TimedEntry {
                            expires_at,
                            value: f().await,
                        });
                    }
                    &mut occupied.into_mut().value
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                    &mut vacant
                        .insert(TimedEntry {
                            expires_at,
                            value: f().await,
                        })
                        .value
                }
            }
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
            let v = match self.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if Self::entry_live(occupied.get().expires_at) {
                        if self.refresh {
                            let now = Instant::now();
                            let new_exp = Self::compute_expires_at(self.ttl, now)
                                .ok()
                                .flatten()
                                .or(occupied.get().expires_at);
                            occupied.get_mut().expires_at = new_exp;
                        }
                        self.hits.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &occupied.get().value);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        let now = Instant::now();
                        let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                        occupied.insert(TimedEntry {
                            expires_at,
                            value: f().await?,
                        });
                    }
                    &mut occupied.into_mut().value
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now).unwrap_or(None);
                    &mut vacant
                        .insert(TimedEntry {
                            expires_at,
                            value: f().await?,
                        })
                        .value
                }
            };
            Ok(v)
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V> CacheEvict for TtlCache<K, V> {
    fn evict(&mut self) -> usize {
        TtlCache::evict(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::Cached;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn new_returns_ready_cache_respecting_ttl() {
        use crate::CacheTtl;
        let mut c: TtlCache<u32, u32> = TtlCache::new(crate::time::Duration::from_millis(50));
        assert_eq!(
            CacheTtl::ttl(&c),
            Some(crate::time::Duration::from_millis(50))
        );
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_get(&1), Some(&100));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(c.cache_get(&1), None, "entry must expire after ttl");
    }

    #[test]
    #[should_panic(expected = "non-zero ttl")]
    fn new_zero_ttl_panics() {
        let _c: TtlCache<u32, u32> = TtlCache::new(crate::time::Duration::ZERO);
    }

    #[test]
    fn ttl_secs_and_ttl_millis_set_duration() {
        use crate::CacheTtl;
        let c: TtlCache<u32, u32> = TtlCache::builder().ttl_secs(7).build().unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(crate::time::Duration::from_secs(7)));

        let c: TtlCache<u32, u32> = TtlCache::builder().ttl_millis(250).build().unwrap();
        assert_eq!(
            CacheTtl::ttl(&c),
            Some(crate::time::Duration::from_millis(250))
        );
    }

    #[test]
    fn ttl_setters_override_last_writer_wins() {
        use crate::CacheTtl;
        // ttl(secs=10) then ttl_secs(5) -> 5s
        let c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(10))
            .ttl_secs(5)
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(crate::time::Duration::from_secs(5)));

        // ttl_secs then ttl_millis -> the millis value
        let c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl_secs(10)
            .ttl_millis(500)
            .build()
            .unwrap();
        assert_eq!(
            CacheTtl::ttl(&c),
            Some(crate::time::Duration::from_millis(500))
        );

        // ttl_millis then ttl -> the ttl value
        let c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl_millis(500)
            .ttl(crate::time::Duration::from_secs(3))
            .build()
            .unwrap();
        assert_eq!(CacheTtl::ttl(&c), Some(crate::time::Duration::from_secs(3)));
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_set(3, 30);
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(count.load(Ordering::Relaxed), 3);
        assert_eq!(c.cache_evictions(), Some(3));
    }

    #[test]
    fn cache_clear_does_not_fire_on_evict() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 10);
        c.cache_set(2, 20);
        c.cache_clear();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            count.load(Ordering::Relaxed),
            0,
            "cache_clear must not fire on_evict"
        );
    }

    #[test]
    fn cache_reset_does_not_fire_on_evict() {
        let evict_count = Arc::new(AtomicUsize::new(0));
        let evict_count2 = evict_count.clone();
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
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
        let mut cache = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        cache.cache_set(1, 100);
        cache.cache_set(2, 200);

        // Debug
        let debug_str = format!("{:?}", cache);
        assert!(debug_str.contains("TtlCache"));
        assert!(debug_str.contains("ttl"));
        assert!(debug_str.contains("hits"));
        assert!(debug_str.contains("misses"));

        // Clone
        let mut cloned = cache.clone();
        assert_eq!(cloned.cache_get(&1), Some(&100));
        assert_eq!(cloned.cache_get(&2), Some(&200));

        // Builder build errors
        let builder = TtlCache::<u32, u32>::builder();
        let built = builder.build();
        assert!(built.is_err()); // Missing required ttl

        let builder = TtlCache::<u32, u32>::builder().ttl(crate::time::Duration::ZERO);
        let built = builder.build();
        assert!(built.is_err()); // Zero ttl is invalid
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        assert_eq!(c.cache_remove_entry(&999u32), None); // absent
        assert_eq!(c.cache_remove_entry(&1u32), Some((1u32, 100u32)));
        assert_eq!(c.cache_get(&1u32), None);
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(50))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(100));

        // cache_remove returns None for an expired entry.
        assert_eq!(
            c.cache_remove(&1u32),
            None,
            "cache_remove: None for expired"
        );

        // Re-insert and verify cache_remove_entry returns Some even though expired.
        c.cache_set(2u32, 200u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let removed = c.cache_remove_entry(&2u32);
        assert!(
            removed.is_some(),
            "cache_remove_entry must return Some even for expired entries"
        );
        assert_eq!(
            removed.expect("cache_remove_entry must return Some for a present entry"),
            (2u32, 200u32)
        );
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(50))
            .build()
            .unwrap();
        c.cache_set(1u32, 100u32);
        std::thread::sleep(std::time::Duration::from_millis(100));

        // cache_delete must return true even though the entry is expired.
        assert!(
            c.cache_delete(&1u32),
            "cache_delete must return true when entry deleted, even if expired"
        );

        // Entry is now gone.
        assert!(
            !c.cache_delete(&1u32),
            "cache_delete returns false when key absent"
        );
    }

    #[test]
    fn cache_remove_entry_fires_on_evict() {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(50))
            .on_evict(move |_k: &u32, _v: &u32| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Even for an expired entry, on_evict must fire.
        let _ = c.cache_remove_entry(&1u32);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        // No fire for absent key.
        let _ = c.cache_remove_entry(&999u32);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let mut c = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(10))
            .build()
            .unwrap();
        c.cache_set(1u32, 10u32);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let before = c.cache_evictions().expect("evictions are always tracked");
        let _ = c.cache_remove_entry(&1u32); // expired but present — must increment
        let _ = c.cache_remove_entry(&999u32); // absent — must not increment
        assert_eq!(
            c.cache_evictions().expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }
}
