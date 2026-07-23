use crate::time::Duration;
use crate::time::Instant;
use std::cmp::Eq;
use std::hash::{BuildHasher, Hash};

use std::collections::{HashMap, hash_map::Entry};

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::future::Future};

use crate::{CachedIter, CachedPeek, CloneCached};

use super::{CacheEvict, Cached, DefaultHashBuilder, TimedEntry};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cache store bound by time
///
/// Values are timestamped when inserted and are
/// evicted if expired at time of retrieval.
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
/// [`TtlCacheBuilder::hasher`] to use a different hasher.
#[doc(alias = "TimedCache")]
pub struct TtlCache<K, V, S = DefaultHashBuilder> {
    pub(super) store: HashMap<K, TimedEntry<V>, S>,
    pub(super) ttl: Duration,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) initial_capacity: Option<usize>,
    pub(super) refresh: bool,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V, S> std::fmt::Debug for TtlCache<K, V, S> {
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

impl<K, V, S> Clone for TtlCache<K, V, S>
where
    K: Clone + Hash + Eq,
    V: Clone,
    S: Clone,
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
pub struct TtlCacheBuilder<K, V, S = DefaultHashBuilder> {
    ttl: Option<Duration>,
    capacity: Option<usize>,
    refresh: bool,
    on_evict: Option<super::OnEvict<K, V>>,
    hasher: S,
}

impl<K, V> Default for TtlCacheBuilder<K, V, DefaultHashBuilder> {
    fn default() -> Self {
        Self {
            ttl: None,
            capacity: None,
            refresh: false,
            on_evict: None,
            hasher: super::new_default_hash_builder(),
        }
    }
}

impl<K, V, S> TtlCacheBuilder<K, V, S> {
    /// Set the TTL for cache entries. Required -- `build()` returns
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
    pub fn initial_capacity(mut self, capacity: usize) -> Self {
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
    /// - Lazy TTL-expiry sweeps on access: a [`cache_get`](crate::Cached::cache_get) /
    ///   `cache_get_mut` (and the `cache_get_or_set*` factory paths) that finds an expired
    ///   entry removes or replaces it and fires the callback.
    /// - Overwriting an already-expired entry via [`cache_set`](crate::Cached::cache_set) /
    ///   [`cache_try_set`](crate::Cached::cache_try_set): the displaced value is filtered from
    ///   the return (`None`), so it fires the callback and counts an eviction.
    /// - Explicit [`cache_remove`](crate::Cached::cache_remove) /
    ///   [`cache_remove_entry`](crate::Cached::cache_remove_entry), even when the removed
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

    /// Switch to a custom hash builder `S2`, returning a builder parameterized on `S2`.
    ///
    /// The hasher is used to hash keys in the internal `HashMap`. Calling this method
    /// changes the builder's type parameter so `build()` returns a `TtlCache<K, V, S2>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{Cached, TtlCache};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let mut cache = TtlCache::<u32, u32>::builder()
    ///     .ttl_secs(60)
    ///     .hasher(RandomState::new())
    ///     .build()
    ///     .unwrap();
    /// cache.cache_set(1, 100);
    /// assert_eq!(cache.cache_get(&1), Some(&100));
    /// ```
    #[doc(alias = "with_hasher")]
    #[must_use]
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> TtlCacheBuilder<K, V, S2> {
        TtlCacheBuilder {
            ttl: self.ttl,
            capacity: self.capacity,
            refresh: self.refresh,
            on_evict: self.on_evict,
            hasher,
        }
    }

    /// Build the cache.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`](super::BuildError) if `ttl` was not set or is zero
    /// ([`BuildError::MissingRequired`](super::BuildError::MissingRequired) /
    /// [`BuildError::InvalidValue`](super::BuildError::InvalidValue)).
    pub fn build(self) -> Result<TtlCache<K, V, S>, super::BuildError>
    where
        K: Hash + Eq,
        S: BuildHasher,
    {
        let ttl = self.ttl.ok_or(super::BuildError::MissingRequired("ttl"))?;
        super::validate_ttl(ttl)?;
        let store = match self.capacity {
            Some(cap) => HashMap::with_capacity_and_hasher(cap, self.hasher),
            None => HashMap::with_hasher(self.hasher),
        };
        Ok(TtlCache {
            store,
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
        TtlCacheBuilder::default()
    }
}

impl<K: Hash + Eq, V, S: BuildHasher> TtlCache<K, V, S> {
    /// `true` if the entry is still live.
    /// `expires_at = None` means the entry never expires (TTL was disabled at insert time).
    #[inline]
    pub(super) fn entry_live(expires_at: Option<Instant>) -> bool {
        expires_at.is_none_or(|t| Instant::now() < t)
    }

    /// Insert `entry` for `key`, returning the previous value only if it was still live.
    ///
    /// When the displaced previous value had already expired it is filtered from the return
    /// (matching the get paths), so it is dropped silently from the caller's view; in that case
    /// fire `on_evict` and count an eviction so resource cleanup and metrics stay consistent
    /// with the other removal paths.
    fn set_entry(&mut self, key: K, entry: TimedEntry<V>) -> Option<V> {
        use std::collections::hash_map::Entry;
        match self.store.entry(key) {
            Entry::Occupied(mut occupied) => {
                let old = occupied.insert(entry);
                if Self::entry_live(old.expires_at) {
                    Some(old.value)
                } else {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old.value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
            Entry::Vacant(vacant) => {
                vacant.insert(entry);
                None
            }
        }
    }

    /// Compute the expiry instant for a new or refreshed entry given the current TTL.
    /// Returns `None` when `ttl` is zero (expiry disabled), or `Some(now + ttl)`.
    /// On overflow (`now + ttl` exceeds `Instant`'s representable range, a TTL on the
    /// order of hundreds of years) returns `None`: the entry never expires, matching
    /// the sharded TTL stores.
    #[inline]
    pub(super) fn compute_expires_at(ttl: Duration, now: Instant) -> Option<Instant> {
        if ttl.is_zero() {
            None
        } else {
            now.checked_add(ttl)
        }
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

impl<K: Hash + Eq, V, S: BuildHasher> Cached<K, V> for TtlCache<K, V, S> {
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Resolve hit / expired / absent from a SINGLE lookup: an absent key
        // (the common miss) must not pay a second `remove_entry` probe (CORE-7).
        let expired_present = match self.store.get_mut(key) {
            Some(entry) if Self::entry_live(entry.expires_at) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    entry.expires_at =
                        Self::compute_expires_at(self.ttl, Instant::now()).or(entry.expires_at);
                }
                // SAFETY: `ptr` points into a HashMap entry obtained from
                // `get_mut`. We return immediately without modifying the map, so
                // the entry is not moved while the returned reference is live.
                // The raw pointer is needed because the borrow checker cannot see
                // that the `&mut entry` borrow ends here when `refresh` mutated
                // `entry.expires_at` above.
                let ptr = &entry.value as *const V;
                return Some(unsafe { &*ptr });
            }
            Some(_) => true, // present but expired: sweep it below
            None => false,   // absent: plain miss, no second lookup
        };
        self.misses.fetch_add(1, Ordering::Relaxed);
        if expired_present && let Some((k, entry)) = self.store.remove_entry(key) {
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
        // Single lookup on the miss path, as in `cache_get` (CORE-7).
        let expired_present = match self.store.get_mut(key) {
            Some(entry) if Self::entry_live(entry.expires_at) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                if self.refresh {
                    entry.expires_at =
                        Self::compute_expires_at(self.ttl, Instant::now()).or(entry.expires_at);
                }
                // SAFETY: same as `cache_get` -- entry is not moved between
                // obtaining the pointer and returning, and `&mut self` prevents
                // concurrent access.
                let ptr = &mut entry.value as *mut V;
                return Some(unsafe { &mut *ptr });
            }
            Some(_) => true,
            None => false,
        };
        self.misses.fetch_add(1, Ordering::Relaxed);
        if expired_present && let Some((k, entry)) = self.store.remove_entry(key) {
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
                        let new_exp =
                            Self::compute_expires_at(self.ttl, now).or(occupied.get().expires_at);
                        occupied.get_mut().expires_at = new_exp;
                    }
                    self.hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    // Compute the replacement BEFORE firing the eviction side
                    // effects. If `f()` panics the expired entry is left in place,
                    // so firing on_evict / counting here would double-fire when the
                    // next call finally evicts the same physical entry (EXP-3).
                    let val = f();
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &occupied.get().value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now);
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
                let expires_at = Self::compute_expires_at(self.ttl, now);
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
                        let new_exp =
                            Self::compute_expires_at(self.ttl, now).or(occupied.get().expires_at);
                        occupied.get_mut().expires_at = new_exp;
                    }
                    self.hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    // Compute the replacement BEFORE firing the eviction side
                    // effects. On `Err` the expired entry is left in place, so
                    // firing on_evict / counting here would double-fire when the
                    // next call finally evicts the same physical entry (EXP-3).
                    let val = f()?;
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &occupied.get().value);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now);
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
                let expires_at = Self::compute_expires_at(self.ttl, now);
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
    /// with `expires_at = None` (never expires), matching the sharded TTL stores.
    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        let now = Instant::now();
        let expires_at = Self::compute_expires_at(self.ttl, now);
        self.set_entry(
            key,
            TimedEntry {
                expires_at,
                value: val,
            },
        )
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
        // We use clear + shrink_to rather than rebuilding so we don't need S: Clone.
        self.store.clear();
        self.store.shrink_to(self.initial_capacity.unwrap_or(0));
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

    /// Check whether the cache contains a live (non-expired) entry for `k`.
    ///
    /// Delegates to [`CachedPeek::cache_peek`], so it records no hit/miss
    /// metrics, performs no TTL refresh, and reports absent/expired entries
    /// as `false`.
    fn cache_contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        crate::CachedPeek::cache_peek(self, k).is_some()
    }
}

impl<K: Hash + Eq, V, S: BuildHasher> CachedIter<K, V> for TtlCache<K, V, S> {
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

impl<K: Hash + Eq, V, S: BuildHasher> CachedPeek<K, V> for TtlCache<K, V, S> {
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

impl<K: Hash + Eq, V, S: BuildHasher> crate::CacheTtl for TtlCache<K, V, S> {
    fn ttl(&self) -> Option<Duration> {
        // A zero TTL means expiry is disabled.
        if self.ttl.is_zero() {
            None
        } else {
            Some(self.ttl)
        }
    }
    /// A zero `ttl` disables expiry -- exactly equivalent to `unset_ttl`.
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
    for TtlCache<K, V, S>
{
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
                    let new_exp = Self::compute_expires_at(self.ttl, now).or(entry.expires_at);
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
impl<K, V, S> CachedGetOrSetAsync<K, V> for TtlCache<K, V, S>
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
            match self.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if Self::entry_live(occupied.get().expires_at) {
                        if self.refresh {
                            let now = Instant::now();
                            let new_exp = Self::compute_expires_at(self.ttl, now)
                                .or(occupied.get().expires_at);
                            occupied.get_mut().expires_at = new_exp;
                        }
                        self.hits.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        // Compute the replacement BEFORE firing the eviction side
                        // effects. If the future is dropped before completion the
                        // expired entry is left in place, so firing on_evict /
                        // counting here would double-fire when the next call finally
                        // evicts the same physical entry (EXP-3). Also anchor the
                        // expiry after the factory resolves so a slow factory does
                        // not eat into the fresh entry's TTL (CORE-3).
                        let val = f().await;
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &occupied.get().value);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        let now = Instant::now();
                        let expires_at = Self::compute_expires_at(self.ttl, now);
                        occupied.insert(TimedEntry {
                            expires_at,
                            value: val,
                        });
                    }
                    &mut occupied.into_mut().value
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    let val = f().await;
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now);
                    &mut vacant
                        .insert(TimedEntry {
                            expires_at,
                            value: val,
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
                                .or(occupied.get().expires_at);
                            occupied.get_mut().expires_at = new_exp;
                        }
                        self.hits.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        // Resolve the factory BEFORE firing the eviction side
                        // effects (EXP-3) and anchor the expiry after it
                        // (CORE-3). On `Err` the expired entry is left in place
                        // and nothing is fired, so the next call evicts it once.
                        let val = f().await?;
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &occupied.get().value);
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        let now = Instant::now();
                        let expires_at = Self::compute_expires_at(self.ttl, now);
                        occupied.insert(TimedEntry {
                            expires_at,
                            value: val,
                        });
                    }
                    &mut occupied.into_mut().value
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    let val = f().await?;
                    let now = Instant::now();
                    let expires_at = Self::compute_expires_at(self.ttl, now);
                    &mut vacant
                        .insert(TimedEntry {
                            expires_at,
                            value: val,
                        })
                        .value
                }
            };
            Ok(v)
        }
    }
}

impl<K: std::hash::Hash + Eq, V, S: BuildHasher> CacheEvict for TtlCache<K, V, S> {
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
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));
        // The previous value has expired: overwriting filters it from the return (None), fires
        // on_evict once, and counts one eviction.
        assert_eq!(c.cache_set(1, 200), None);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
        // Overwriting the now-live value returns it, no on_evict and no new eviction.
        assert_eq!(c.cache_set(1, 300), Some(200));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
    }

    // TEST-1: eviction counter increments when overwriting an expired entry even
    // without an on_evict callback configured.
    #[test]
    fn cache_set_over_expired_increments_eviction_counter_without_callback() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Overwriting an expired entry: returns None and increments evictions.
        assert_eq!(c.cache_set(1, 200), None);
        assert_eq!(c.cache_evictions(), Some(1));
        // Overwriting the now-live value: returns it and no new eviction.
        assert_eq!(c.cache_set(1, 300), Some(200));
        assert_eq!(c.cache_evictions(), Some(1));
    }

    // BUG-1 regression (sync): a panicking factory on the infallible get-or-set
    // path must not fire on_evict or increment evictions; the expired entry must
    // remain in place for the next access to evict it exactly once.
    #[test]
    fn cache_get_or_set_with_mut_panic_does_not_fire_on_evict() {
        use std::panic;

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        // Factory panics: side effects must NOT fire before the factory resolves.
        // Note: a caught panic prints to stderr; that is expected.
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _ = c.cache_get_or_set_with_mut(1u32, || -> u32 { panic!("factory panic") });
        }));
        assert!(result.is_err(), "expected panic to be caught");
        assert_eq!(
            fired.load(Ordering::Relaxed),
            0,
            "on_evict must not fire when factory panics"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(0),
            "evictions must remain 0 when factory panics"
        );
        assert_eq!(c.cache_size(), 1, "expired entry must still be present");

        // A subsequent successful factory evicts the entry exactly once.
        let _ = c.cache_get_or_set_with_mut(1u32, || 200u32);
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "on_evict must fire exactly once after successful replacement"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "evictions must be 1 after success"
        );
    }

    // BUG-1 regression (async): a factory future dropped before completion on the
    // infallible async get-or-set path must not fire on_evict or increment evictions;
    // the expired entry must remain in place for the next call to evict exactly once.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_cancel_does_not_fire_on_evict() {
        use crate::CachedGetOrSetAsync;
        use std::task::Poll;

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        // Create a future whose factory never resolves, poll it once (so it enters
        // the expired-entry branch and reaches `f().await`), then drop it.
        {
            let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                &mut c,
                1u32,
                std::future::pending::<u32>,
            ));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            // Must be Pending: the factory future never resolves.
            assert!(
                matches!(fut.as_mut().poll(&mut cx), Poll::Pending),
                "future must be pending while factory is unresolved"
            );
            // Drop `fut` here -- simulates cancellation mid-factory.
        }

        assert_eq!(
            fired.load(Ordering::Relaxed),
            0,
            "on_evict must not fire when factory future is dropped"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(0),
            "evictions must be 0 after factory cancellation"
        );
        assert_eq!(c.cache_size(), 1, "expired entry must still be present");

        // A subsequent successful factory evicts the entry exactly once.
        let _ =
            CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 1u32, || async { 200u32 })
                .await;
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "on_evict must fire exactly once after successful replacement"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "evictions must be 1 after success"
        );
    }

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
        let mut c = TtlCache::<u32, u32>::builder()
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
        let mut c: TtlCache<u32, u32> = TtlCache::new(crate::time::Duration::from_secs(60));
        c.cache_set(1, 10);
        assert_eq!(c.cache_get(&1), Some(&10));

        let mut b = TtlCache::<u32, u32>::builder()
            .ttl_secs(60)
            .build()
            .unwrap();
        b.cache_set(2, 20);
        assert_eq!(b.cache_get(&2), Some(&20));
    }

    #[test]
    fn custom_hasher_respects_ttl_expiry() {
        use std::collections::hash_map::RandomState;
        let mut c = TtlCache::<u32, u32>::builder()
            .ttl(crate::time::Duration::from_millis(50))
            .hasher(RandomState::new())
            .build()
            .unwrap();
        c.cache_set(1, 10);
        assert_eq!(c.cache_get(&1), Some(&10));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(c.cache_get(&1), None, "entry must expire after ttl");
    }

    #[test]
    fn builder_initial_capacity_method_exists_and_preallocates() {
        // Verifies the renamed builder method: initial_capacity() sets a preallocation hint.
        let c = TtlCache::<u32, u32>::builder()
            .ttl_secs(60)
            .initial_capacity(32)
            .build()
            .unwrap();
        // The backing store must have at least the requested capacity.
        assert!(c.store.capacity() >= 32);
    }

    // EXP-3: on the try-path, a failing factory over an expired entry must not
    // fire `on_evict` / count an eviction until the replacement succeeds, or the
    // next real eviction of the same physical entry double-fires.
    #[test]
    fn try_get_or_set_err_over_expired_does_not_double_fire_on_evict() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Factory fails over the expired entry: the entry is left in place and
        // nothing fires yet.
        let r: Result<&mut u32, ()> = c.cache_try_get_or_set_with_mut(1, || Err(()));
        assert!(r.is_err());
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        // A subsequent plain get evicts the still-expired entry exactly once.
        assert_eq!(c.cache_get(&1), None);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
    }

    // CORE-7: a plain miss (absent key) must not fire `on_evict`; only an
    // expired-entry miss evicts. (Pins the single-lookup miss path's behavior.)
    #[test]
    fn plain_miss_does_not_evict_expired_miss_does() {
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        // Absent key: miss, no eviction, no callback.
        assert_eq!(c.cache_get(&42), None);
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        // Expired key: miss that also evicts and fires once.
        c.cache_set(7, 1);
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(c.cache_get(&7), None);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.load(Ordering::Relaxed), 1);
    }

    // CORE-6: `CacheEvict` no longer requires `K: Clone`. A non-`Clone` key type
    // must still implement `CacheEvict` (this fails to compile if the bound
    // regresses).
    #[test]
    fn cache_evict_does_not_require_key_clone() {
        #[derive(Hash, PartialEq, Eq)]
        struct NoClone(u32);
        fn assert_impls<T: crate::CacheEvict>() {}
        assert_impls::<TtlCache<NoClone, u32>>();
    }

    // CORE-3: the async paths must anchor the expiry AFTER the factory resolves,
    // so a factory slower than the TTL still yields a live entry.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_expiry_anchored_after_factory() {
        use crate::CachedGetOrSetAsync;
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(40))
            .build()
            .unwrap();
        // Factory takes ~3x the TTL; anchoring after means the fresh entry is
        // still live immediately after insertion.
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

    // BUG-1 (miss-counter invariant, sync): the expired-occupant branch increments
    // `misses` BEFORE running the factory. A panicking factory must therefore leave
    // the miss counted exactly once (and never double-counted). Pins the counter so a
    // future reorder of the miss increment past the factory can't silently change it.
    #[test]
    fn cache_get_or_set_with_mut_panic_counts_miss_once() {
        use std::panic;

        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _ = c.cache_get_or_set_with_mut(1u32, || -> u32 { panic!("factory panic") });
        }));
        assert!(result.is_err(), "expected panic to be caught");
        assert_eq!(
            c.cache_misses(),
            Some(1),
            "the expired access is a single miss even when the factory panics"
        );
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(c.cache_size(), 1, "expired entry must still be present");
    }

    // BUG-1 (successful expired replacement, sync): on the expired-occupant path a
    // successful factory must fire `on_evict` exactly once WITH THE OLD (evicted)
    // value, increment `evictions` by exactly one, and leave the factory's NEW value
    // cached. Guards against an off-by-one on the counter and against a reorder that
    // would fire the callback with the new value (insert-before-callback regression).
    #[test]
    fn cache_get_or_set_with_mut_expired_replacement_fires_with_old_value() {
        let seen = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
        let seen2 = seen.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, v: &u32| {
                seen2.store(*v, Ordering::Relaxed);
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(60));

        let val = c.cache_get_or_set_with_mut(1u32, || 200u32);
        assert_eq!(*val, 200, "factory's new value must be returned");
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "on_evict must fire exactly once"
        );
        assert_eq!(
            seen.load(Ordering::Relaxed),
            100,
            "on_evict must receive the OLD (evicted) value, not the replacement"
        );
        assert_eq!(c.cache_evictions(), Some(1), "exactly one eviction");
        assert_eq!(
            c.cache_peek(&1),
            Some(&200),
            "the new value must be cached and live"
        );
    }

    // BUG-1 (hit path, sync): on a live occupant `cache_get_or_set_with_mut` must NOT
    // run the factory, must count a hit (not a miss/eviction), and must return the
    // existing value unchanged. Covers the previously untested Occupied-live branch.
    #[test]
    fn cache_get_or_set_with_mut_hit_does_not_call_factory() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_secs(60))
            .build()
            .unwrap();
        c.cache_set(1, 100);
        let val = c.cache_get_or_set_with_mut(1u32, move || {
            calls2.fetch_add(1, Ordering::Relaxed);
            999u32
        });
        assert_eq!(*val, 100, "live entry must be returned, factory ignored");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "factory must not run on a hit"
        );
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
        assert_eq!(c.cache_evictions(), Some(0));
    }

    // BUG-1 (refresh-on-hit, sync): with `refresh_on_hit(true)`, a hit on
    // `cache_get_or_set_with_mut` must renew the entry's TTL so it survives past its
    // original expiry. Covers the refresh branch of the Occupied-live path.
    #[test]
    fn cache_get_or_set_with_mut_refresh_extends_ttl_on_hit() {
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(120))
            .refresh_on_hit(true)
            .build()
            .unwrap();
        c.cache_set(1, 100);
        std::thread::sleep(std::time::Duration::from_millis(70));
        // Hit refreshes the TTL to now + 120ms.
        let val = c.cache_get_or_set_with_mut(1u32, || 999u32);
        assert_eq!(*val, 100, "still a hit, factory ignored");
        std::thread::sleep(std::time::Duration::from_millis(70));
        // 140ms since original set (would be expired without refresh) but only 70ms
        // since the refresh, so the entry must still be live.
        assert_eq!(
            c.cache_peek(&1),
            Some(&100),
            "refresh-on-hit must have extended the TTL past the original expiry"
        );
    }

    // BUG-1 (vacant-path cancellation, async): a factory future dropped before
    // completion on the VACANT path must insert NO entry and must not touch the
    // eviction counter/callback. The miss is counted once (incremented before the
    // factory). No async vacant-path cancellation test existed previously.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_cancel_on_vacant_inserts_nothing() {
        use crate::CachedGetOrSetAsync;
        use std::task::Poll;

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, _v: &u32| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        // Vacant key: poll a never-resolving factory once, then drop mid-factory.
        {
            let mut fut = Box::pin(CachedGetOrSetAsync::async_cache_get_or_set_with_mut(
                &mut c,
                42u32,
                std::future::pending::<u32>,
            ));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            assert!(
                matches!(fut.as_mut().poll(&mut cx), Poll::Pending),
                "future must be pending while factory is unresolved"
            );
        }

        assert_eq!(
            c.cache_size(),
            0,
            "no entry may be inserted when the vacant-path factory is cancelled"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(0),
            "vacant-path cancellation must not touch evictions"
        );
        assert_eq!(
            fired.load(Ordering::Relaxed),
            0,
            "vacant-path cancellation must not fire on_evict"
        );
        assert_eq!(
            c.cache_misses(),
            Some(1),
            "the vacant access is counted as a single miss"
        );

        // A subsequent successful factory inserts normally.
        let _ =
            CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 42u32, || async { 7u32 })
                .await;
        assert_eq!(c.cache_get(&42), Some(&7));
    }

    // BUG-1 (successful expired replacement, async): mirror of the sync old-value
    // test on the async path -- on_evict fires once with the OLD value, evictions
    // increments by one, and the factory's NEW value is cached.
    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_mut_expired_replacement_fires_with_old_value() {
        use crate::CachedGetOrSetAsync;

        let seen = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
        let seen2 = seen.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: TtlCache<u32, u32> = TtlCache::builder()
            .ttl(crate::time::Duration::from_millis(20))
            .on_evict(move |_k: &u32, v: &u32| {
                seen2.store(*v, Ordering::Relaxed);
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1, 100);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let val =
            CachedGetOrSetAsync::async_cache_get_or_set_with_mut(&mut c, 1u32, || async { 200u32 })
                .await;
        assert_eq!(*val, 200, "factory's new value must be returned");
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "on_evict must fire exactly once"
        );
        assert_eq!(
            seen.load(Ordering::Relaxed),
            100,
            "on_evict must receive the OLD (evicted) value, not the replacement"
        );
        assert_eq!(c.cache_evictions(), Some(1), "exactly one eviction");
        assert_eq!(
            c.cache_peek(&1),
            Some(&200),
            "the new value must be cached and live"
        );
    }
}
