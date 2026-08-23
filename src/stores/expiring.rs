use super::{CacheEvict, Cached, DefaultHashBuilder, Expires};
use crate::{CacheExpiry, CachedIter, CachedPeek, CloneCached};
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "async_core")]
use {super::CachedGetOrSetAsync, std::collections::hash_map::Entry, std::future::Future};

/// Size-unbounded cache where each value controls its own expiry via [`Expires`].
///
/// Unlike [`TtlCache`](crate::stores::TtlCache) which applies a single global TTL duration to
/// all entries, `ExpiringCache` has **no global TTL**. Each value determines its own expiration
/// by implementing [`Expires`]. The store checks `is_expired()` on every lookup and evicts
/// expired entries on access.
///
/// For a size-bounded variant that also evicts by LRU, see [`ExpiringLruCache`](crate::ExpiringLruCache).
/// When using the `#[cached]` proc macro, `expires = true` automatically selects this store
/// (or `ExpiringLruCache` when `max_size` is also specified).
///
/// **`cache_size` / `iter` / `evict` contract**: `cache_size()` returns the raw stored entry count
/// and may include expired-but-not-yet-swept entries. `iter()` omits expired entries
/// from the view but does not remove them. Call `evict()` (via [`CacheEvict`](crate::CacheEvict))
/// to physically remove expired entries, reclaim memory, and obtain an accurate live count.
///
/// ## Memory note
///
/// `ExpiringCache` is **unbounded** and only removes expired entries when the same key is
/// accessed again. Entries that expire and are never re-fetched stay in memory indefinitely.
/// For high-cardinality workloads, call [`evict()`](ExpiringCache::evict) periodically to
/// sweep and remove all expired entries, or prefer [`ExpiringLruCache`](crate::ExpiringLruCache)
/// with a `max_size` bound to cap memory usage automatically.
///
/// ```rust
/// use cached::{CachedExt, Expires, ExpiringCache};
///
/// struct Token {
///     #[allow(dead_code)]
///     value: String,
///     expired: bool,
/// }
/// impl Expires for Token {
///     fn is_expired(&self) -> bool { self.expired }
/// }
///
/// let mut cache: ExpiringCache<u32, Token> = ExpiringCache::new();
/// cache.set(1, Token { value: "live".into(), expired: false });
/// assert!(cache.get(&1).is_some());
/// cache.set(2, Token { value: "stale".into(), expired: true });
/// assert!(cache.get(&2).is_none()); // expired -> not returned
/// ```
///
/// Note: This cache is in-memory only.
pub struct ExpiringCache<K, V, S = DefaultHashBuilder> {
    pub(super) store: HashMap<K, V, S>,
    pub(super) initial_capacity: Option<usize>,
    pub(super) hits: AtomicU64,
    pub(super) misses: AtomicU64,
    pub(super) evictions: AtomicU64,
    pub(super) on_evict: Option<super::OnEvict<K, V>>,
}

impl<K, V, S> std::fmt::Debug for ExpiringCache<K, V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpiringCache")
            .field("hits", &self.hits.load(Ordering::Relaxed))
            .field("misses", &self.misses.load(Ordering::Relaxed))
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("on_evict", &self.on_evict.as_ref().map(|_| "on_evict"))
            .finish()
    }
}

/// Two `ExpiringCache` values are equal when their stored entries are equal.
/// Metrics (hits, misses, evictions) and the `on_evict` callback are not
/// part of the comparison.
impl<K, V, S> PartialEq for ExpiringCache<K, V, S>
where
    K: Hash + Eq,
    V: PartialEq,
    S: BuildHasher,
{
    fn eq(&self, other: &Self) -> bool {
        self.store == other.store
    }
}

impl<K, V, S> Eq for ExpiringCache<K, V, S>
where
    K: Hash + Eq,
    V: Eq,
    S: BuildHasher,
{
}

impl<K, V, S> Clone for ExpiringCache<K, V, S>
where
    K: Clone + Hash + Eq,
    V: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            initial_capacity: self.initial_capacity,
            hits: AtomicU64::new(self.hits.load(Ordering::Relaxed)),
            misses: AtomicU64::new(self.misses.load(Ordering::Relaxed)),
            evictions: AtomicU64::new(self.evictions.load(Ordering::Relaxed)),
            on_evict: self.on_evict.clone(),
        }
    }
}

/// Builder for [`ExpiringCache`].
///
/// Note: there is intentionally **no `.ttl()` setter**. An `ExpiringCache` has no global
/// expiry duration -- each value decides when it is expired via the [`Expires`] trait. For a
/// single global TTL applied to every entry, use [`TtlCache`](crate::stores::TtlCache) or
/// [`LruTtlCache`](crate::stores::LruTtlCache) instead.
#[doc(alias = "ttl")]
pub struct ExpiringCacheBuilder<K, V, S = DefaultHashBuilder> {
    capacity: Option<usize>,
    on_evict: Option<super::OnEvict<K, V>>,
    hasher: S,
}

impl<K, V> Default for ExpiringCacheBuilder<K, V, DefaultHashBuilder> {
    fn default() -> Self {
        Self {
            capacity: None,
            on_evict: None,
            hasher: super::new_default_hash_builder(),
        }
    }
}

impl<K, V> ExpiringCacheBuilder<K, V> {
    /// Create a builder with default settings. Equivalent to [`ExpiringCache::builder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, S> ExpiringCacheBuilder<K, V, S> {
    /// Set the initial allocation capacity (optional).
    #[must_use]
    pub fn initial_capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    /// Set a callback to be invoked when an entry is removed from the cache.
    ///
    /// The callback fires when an expired value is encountered during `cache_get`,
    /// `cache_get_mut`, `cache_get_or_set_with_mut`, `cache_try_get_or_set_with_mut`
    /// (the primary implementations), `cache_get_or_set_with`, `cache_try_get_or_set_with`
    /// (default-impl wrappers that delegate to the `_mut` variants),
    /// their async equivalents, an explicit `evict()` sweep, or an explicit
    /// `cache_remove` (including when the removed entry was already expired).
    /// It does **not** fire on `cache_clear` or `cache_reset` (consistent with
    /// [`ExpiringLruCache`](crate::ExpiringLruCache)).
    /// Use [`cache_clear_with_on_evict`](ExpiringCache::cache_clear_with_on_evict)
    /// instead of [`cache_clear`](crate::Cached::cache_clear) to opt into callback
    /// firing and eviction counter increments when clearing all entries.
    #[must_use]
    pub fn on_evict(mut self, on_evict: impl Fn(&K, &V) + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(on_evict));
        self
    }

    /// Switch to a custom hash builder `S2`, returning a builder parameterized on `S2`.
    ///
    /// The hasher is used to hash keys in the internal `UnboundCache`. Calling this method
    /// changes the builder's type parameter so `build()` returns an `ExpiringCache<K, V, S2>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cached::{Cached, Expires, ExpiringCache};
    /// use std::collections::hash_map::RandomState;
    ///
    /// struct Val(bool);
    /// impl Expires for Val { fn is_expired(&self) -> bool { self.0 } }
    ///
    /// let mut cache = ExpiringCache::<u32, Val>::builder()
    ///     .hasher(RandomState::new())
    ///     .build()
    ///     .unwrap();
    /// cache.cache_set(1, Val(false));
    /// assert!(cache.cache_get(&1).is_some());
    /// ```
    #[doc(alias = "with_hasher")]
    #[must_use]
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> ExpiringCacheBuilder<K, V, S2> {
        ExpiringCacheBuilder {
            capacity: self.capacity,
            on_evict: self.on_evict,
            hasher,
        }
    }

    /// Build the cache.
    ///
    /// `ExpiringCache` has no required fields and this call never fails.
    ///
    /// # Errors
    ///
    /// This method currently never returns an error.
    pub fn build(self) -> Result<ExpiringCache<K, V, S>, super::BuildError>
    where
        K: Hash + Eq,
        S: BuildHasher,
    {
        let store = match self.capacity {
            Some(cap) => HashMap::with_capacity_and_hasher(cap, self.hasher),
            None => HashMap::with_hasher(self.hasher),
        };
        Ok(ExpiringCache {
            store,
            initial_capacity: self.capacity,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            on_evict: self.on_evict,
        })
    }
}

impl<K: Hash + Eq, V: Expires> ExpiringCache<K, V> {
    /// Construct a ready-to-use [`ExpiringCache`] with default configuration.
    ///
    /// `ExpiringCache` has no required configuration, so this never fails. For
    /// optional settings (initial capacity, `on_evict`) use [`builder`](Self::builder).
    #[must_use]
    pub fn new() -> Self {
        Self::builder()
            .build()
            .expect("ExpiringCache default build is infallible")
    }

    /// Return a builder for constructing an [`ExpiringCache`].
    #[must_use]
    pub fn builder() -> ExpiringCacheBuilder<K, V> {
        ExpiringCacheBuilder::default()
    }
}

impl<K: Hash + Eq, V: Expires, S: BuildHasher> ExpiringCache<K, V, S> {
    /// Evict all expired entries from the cache.
    ///
    /// Returns the number of entries removed. Fires the `on_evict` callback for each
    /// removed entry. Use this periodically for high-cardinality workloads to reclaim
    /// memory from entries that expire but are never re-accessed.
    #[must_use]
    pub fn evict(&mut self) -> usize {
        // Two-phase: select, then remove, then count, then notify. Counting or notifying
        // from inside a `HashMap::retain` predicate would fire the side effects *before*
        // the map drops the entry, so a panicking `on_evict` would leave an entry counted
        // (and cleaned up) while still stored and served.
        let removed = self.take_doomed(|_key, value| value.is_expired());
        self.notify_evicted(&removed)
    }

    /// Phase 1 of a two-phase sweep: run `doomed` over every entry and hand back the
    /// entries it selected, removed from the store.
    ///
    /// See [`take_doomed`](crate::stores::take_doomed) for why the sweep is split in two and
    /// what ties the passes together.
    fn take_doomed<F: FnMut(&K, &V) -> bool>(&mut self, doomed: F) -> Vec<(K, V)> {
        crate::stores::take_doomed(&mut self.store, doomed)
    }

    /// Phase 2 of a two-phase sweep: count `removed` as evictions and then notify
    /// `on_evict` for each, returning how many entries were removed.
    ///
    /// The entries are already out of the store, and the whole batch is counted before the
    /// first notification, so a panicking `on_evict` can never leave an entry that has been
    /// cleaned up still reachable, nor an entry removed-but-uncounted.
    fn notify_evicted(&self, removed: &[(K, V)]) -> usize {
        if !removed.is_empty() {
            self.evictions
                .fetch_add(removed.len() as u64, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (k, v) in removed {
                on_evict(k, v);
            }
        }
        removed.len()
    }

    /// Remove all entries and fire the `on_evict` callback for each one, incrementing the
    /// evictions counter.
    ///
    /// Unlike [`cache_clear`](crate::Cached::cache_clear) (which removes entries silently),
    /// this method invokes `on_evict` for every removed entry (whether or not they had expired)
    /// and increments `evictions`. The eviction count does not depend on whether an `on_evict`
    /// callback is configured.
    pub fn cache_clear_with_on_evict(&mut self) {
        let entries: Vec<(K, V)> = self.store.drain().collect();
        let count = entries.len() as u64;
        if count > 0 {
            self.evictions.fetch_add(count, Ordering::Relaxed);
        }
        if let Some(on_evict) = &self.on_evict {
            for (k, v) in &entries {
                on_evict(k, v);
            }
        }
    }

    /// Retain only entries that are unexpired and satisfy `keep`.
    ///
    /// Removes every entry whose value reports [`is_expired`](Expires::is_expired)
    /// **or** for which `keep` returns `false` — expired entries are removed without
    /// consulting `keep`. `on_evict` is called and the eviction counter incremented
    /// for each removed entry. This matches
    /// [`ExpiringLruCache::retain`](crate::ExpiringLruCache::retain) and
    /// [`LruTtlCache::retain`](crate::LruTtlCache::retain); the plain
    /// [`LruCache::retain`](crate::LruCache::retain) has no expiry dimension and
    /// removes solely on the predicate.
    ///
    /// Returns the number of entries removed: the count folds together entries `keep`
    /// rejected and entries swept for having already expired, since expiry removal is
    /// unconditional regardless of what `keep` returns. `retain` is deliberately not
    /// `#[must_use]`: discarding the count is a legitimate and common use, matching
    /// existing bare `cache.retain(...);` call sites.
    pub fn retain<F: FnMut(&K, &V) -> bool>(&mut self, mut keep: F) -> usize {
        // Two-phase (see `take_doomed`): the selection pass must be side-effect free so a
        // panicking `keep` leaves the cache untouched rather than half-notified.
        let removed = self.take_doomed(|key, value| value.is_expired() || !keep(key, value));
        self.notify_evicted(&removed)
    }
}

impl<K: Hash + Eq, V: Expires> Default for ExpiringCache<K, V, DefaultHashBuilder> {
    fn default() -> Self {
        Self::builder().build().expect("infallible")
    }
}

impl<K: Hash + Eq, V: Expires, S: BuildHasher> Cached<K, V> for ExpiringCache<K, V, S> {
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Two lookups on the hit path: the first checks expiry (releasing the borrow via
        // `.map`), the second returns the reference. A single-lookup approach is not possible
        // SAFELY in stable Rust because returning `&'1 V` from inside an `if let` block ties
        // the borrow to lifetime `'1`, which prevents `remove_entry` (a mutable borrow) even on
        // the non-returning path. Polonius (nightly) would fix this without unsafe.
        // `TtlCache::cache_get` (src/stores/ttl.rs) already collapses this to a single lookup
        // via a documented `&entry.value as *const V` reborrow; that unsafe tradeoff is
        // intentionally not made here.
        match self.store.get(k).map(|v| v.is_expired()) {
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Some(true) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.remove_entry(k) {
                    // Count BEFORE notifying: a panicking callback must never leave
                    // an entry removed-but-uncounted.
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
                    }
                }
                None
            }
            Some(false) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.store.get(k)
            }
        }
    }

    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Two lookups on the hit path for the same reason as `cache_get` (NLL limitation).
        match self.store.get(k).map(|v| v.is_expired()) {
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Some(true) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.remove_entry(k) {
                    // Count BEFORE notifying: a panicking callback must never leave
                    // an entry removed-but-uncounted.
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
                    }
                }
                None
            }
            Some(false) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.store.get_mut(k)
            }
        }
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V {
        match self.store.entry(k) {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                if !occupied.get().is_expired() {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    occupied.into_mut()
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    // Compute the replacement BEFORE firing the eviction side effects.
                    // If `f()` panics the expired entry is left in place, so firing
                    // on_evict / counting here would double-fire when the next call
                    // finally evicts the same physical entry.
                    let new_val = f();
                    // Replace FIRST, then count, then notify -- as `cache_set` does.
                    // Firing the side effects while the expired entry is still installed
                    // would let a panicking `on_evict` leave it in place *and* counted, so
                    // the retry that finally replaces it counts a second eviction for one
                    // physical entry. (`on_evict` is `Fn(&K, &V)` and gets no handle on the
                    // cache, so nothing it can do observes the old value in the slot.)
                    let old = occupied.insert(new_val);
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old);
                    }
                    occupied.into_mut()
                }
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                vacant.insert(f())
            }
        }
    }

    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        k: K,
        f: F,
    ) -> Result<&mut V, E> {
        match self.store.entry(k) {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                if !occupied.get().is_expired() {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    Ok(occupied.into_mut())
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    // Same ordering as `cache_get_or_set_with_mut`: compute, replace,
                    // count, then notify.
                    let new_val = f()?;
                    let old = occupied.insert(new_val);
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old);
                    }
                    Ok(occupied.into_mut())
                }
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Ok(vacant.insert(f()?))
            }
        }
    }

    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        use std::collections::hash_map::Entry;
        match self.store.entry(k) {
            Entry::Occupied(mut occupied) => {
                let old = occupied.insert(v);
                if old.is_expired() {
                    // The previous value had expired, so it is filtered from the return
                    // (matching `cache_remove`); fire `on_evict` and count an eviction so the
                    // silently-dropped value is cleaned up like every other removal path.
                    // Count BEFORE notifying: a panicking callback must never leave
                    // an entry removed-but-uncounted.
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old);
                    }
                    None
                } else {
                    Some(old)
                }
            }
            Entry::Vacant(vacant) => {
                vacant.insert(v);
                None
            }
        }
    }

    /// Removes the entry and returns the value only if it is still live;
    /// an expired value is removed but reported as `None`. Use
    /// [`cache_remove_entry`](Cached::cache_remove_entry) to receive the
    /// value regardless of expiry.
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        let (stored_k, v) = self.store.remove_entry(k)?;
        // Judge expiry at the moment of removal, BEFORE the callback runs. Asking the value
        // again on the way out (as delegating to `cache_remove_entry` used to) would let a
        // slow `on_evict` push it past its deadline and report `None` for a value that was
        // live when it was taken out.
        let expired = v.is_expired();
        // Count BEFORE notifying: a panicking callback must never leave an
        // entry removed-but-uncounted.
        self.evictions.fetch_add(1, Ordering::Relaxed);
        if let Some(on_evict) = &self.on_evict {
            on_evict(&stored_k, &v);
        }
        if expired { None } else { Some(v) }
    }

    /// Removes the entry and returns it **regardless of expiry** (unlike
    /// [`cache_remove`](Cached::cache_remove), which filters expired values).
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, v)) = self.store.remove_entry(k) {
            // Count BEFORE notifying: a panicking callback must never leave an
            // entry removed-but-uncounted.
            self.evictions.fetch_add(1, Ordering::Relaxed);
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &v);
            }
            Some((stored_k, v))
        } else {
            None
        }
    }

    fn cache_clear(&mut self) {
        self.store.clear();
    }

    fn cache_reset(&mut self) {
        // Clear all entries and shrink capacity back toward the initial hint, matching
        // `UnboundCache::cache_reset` (which this store used to delegate to).
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

    fn cache_reset_metrics(&mut self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
    }

    /// Check whether the cache contains a live (non-expired) entry for `k`.
    ///
    /// Delegates to [`CachedPeek::cache_peek`], so it records no hit/miss
    /// metrics and reports absent/expired entries as `false`.
    fn cache_contains<Q>(&mut self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        crate::CachedPeek::cache_peek(self, k).is_some()
    }
}

impl<K: Hash + Eq, V: Expires, S: BuildHasher> CachedIter<K, V> for ExpiringCache<K, V, S> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.store
            .iter()
            .filter_map(|(k, v)| if v.is_expired() { None } else { Some((k, v)) })
    }
}

impl<K: Hash + Eq, V: Expires, S: BuildHasher> CachedPeek<K, V> for ExpiringCache<K, V, S> {
    fn cache_peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.get(key).and_then(|value| {
            if value.is_expired() {
                None
            } else {
                Some(value)
            }
        })
    }
}

#[cfg(feature = "async_core")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_core")))]
impl<K, V, S> CachedGetOrSetAsync<K, V> for ExpiringCache<K, V, S>
where
    K: Hash + Eq + Send,
    V: Expires + Send,
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
                    if !occupied.get().is_expired() {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        // Same ordering as the sync path: compute, replace, count,
                        // then notify.
                        let new_val = f().await;
                        let old = occupied.insert(new_val);
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &old);
                        }
                        occupied.into_mut()
                    }
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    vacant.insert(f().await)
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
                    if !occupied.get().is_expired() {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        // Same ordering as the sync path: compute, replace, count,
                        // then notify.
                        let new_val = f().await?;
                        let old = occupied.insert(new_val);
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), &old);
                        }
                        occupied.into_mut()
                    }
                }
                Entry::Vacant(vacant) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    vacant.insert(f().await?)
                }
            };
            Ok(v)
        }
    }
}

impl<K: Hash + Eq, V: Expires + Clone, S: BuildHasher> CloneCached<K, V>
    for ExpiringCache<K, V, S>
{
    // Unlike `cache_get`, this intentionally leaves an expired entry in the map so the
    // `result_fallback` path can clone and return it as a stale-but-present value on `Err`.
    // The entry remains counted by `cache_size()` (but is skipped by `CachedIter`, which
    // omits expired entries) until the next `cache_get`, `evict()`, or an explicit `cache_remove`.
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some(value) = self.store.get(k) {
            let expired = value.is_expired();
            if expired {
                self.misses.fetch_add(1, Ordering::Relaxed);
                (Some(value.clone()), true)
            } else {
                self.hits.fetch_add(1, Ordering::Relaxed);
                (Some(value.clone()), false)
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
    /// counters or remove the entry.
    fn cache_peek_with_expiry_status<Q>(&self, k: &Q) -> (Option<V>, bool)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        if let Some(value) = self.store.get(k) {
            let expired = value.is_expired();
            (Some(value.clone()), expired)
        } else {
            (None, false)
        }
    }
}

impl<K: Hash + Eq, V: Expires + Clone, S: BuildHasher> CacheExpiry<K, V>
    for ExpiringCache<K, V, S>
{
    /// Returns the stored value and its expiry instant, with no read side effects.
    ///
    /// The instant is whatever [`Expires::expires_at`] reports for the value, and on
    /// this store that is advisory only: it is `None` unless the value type overrides
    /// `expires_at` (including for an entry that is expired), and it may be in the
    /// past for an entry [`Expires::is_expired`] reports as live. `is_expired` remains
    /// the authority on liveness here; use
    /// [`cache_peek_with_expiry_status`](CloneCached::cache_peek_with_expiry_status) for
    /// that. Uses the same lookup as that peek: no hit/miss counting, no removal of an
    /// expired entry.
    fn cache_peek_expires_at<Q>(&self, k: &Q) -> (Option<V>, Option<crate::time::Instant>)
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        V: Clone,
    {
        if let Some(value) = self.store.get(k) {
            (Some(value.clone()), value.expires_at())
        } else {
            (None, None)
        }
    }
}

impl<K: std::hash::Hash + Eq, V: Expires, S: BuildHasher> CacheEvict for ExpiringCache<K, V, S> {
    fn evict(&mut self) -> usize {
        ExpiringCache::evict(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cached, CachedExt};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ExpiredU8(pub u8);

    impl Expires for ExpiredU8 {
        fn is_expired(&self) -> bool {
            self.0 > 10
        }
    }

    #[test]
    fn new_returns_ready_cache() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::new();
        assert_eq!(c.set(1, ExpiredU8(2)), None);
        assert_eq!(c.get(&1), Some(&ExpiredU8(2)));
        // Expired values are not returned.
        c.set(2, ExpiredU8(15));
        assert_eq!(c.get(&2), None);
    }

    #[test]
    fn expiring_cache_get_miss() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        assert!(c.get(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_get_hit() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        assert!(c.set(1, ExpiredU8(2)).is_none());
        assert_eq!(c.get(&1), Some(&ExpiredU8(2)));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_cache_get_expired() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        assert!(c.set(2, ExpiredU8(12)).is_none());
        assert!(c.get(&2).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_evictions(), Some(1));
    }

    #[test]
    fn expiring_cache_builder() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .initial_capacity(10)
            .on_evict(|_k: &u8, v: &ExpiredU8| {
                assert!(v.0 > 10);
            })
            .build()
            .unwrap();
        assert!(c.set(1, ExpiredU8(15)).is_none());
        assert!(c.get(&1).is_none());
        assert_eq!(c.cache_evictions(), Some(1));
    }

    #[test]
    fn expiring_cache_evict_fires_callback() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &u8, _v: &ExpiredU8| {
                fired2.lock().unwrap().push(*k);
            })
            .build()
            .unwrap();
        c.set(1, ExpiredU8(15)); // expired
        c.set(2, ExpiredU8(3)); // live
        let n = c.evict();
        assert_eq!(n, 1);
        assert_eq!(c.cache_evictions(), Some(1));
        let mut keys = fired.lock().unwrap().clone();
        keys.sort();
        assert_eq!(keys, vec![1]);
        assert_eq!(c.cache_size(), 1);
    }

    #[test]
    fn expiring_cache_remove_fires_on_evict() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AOrdering},
        };
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.set(1, ExpiredU8(5)); // live
        // Removing a live entry returns Some and fires on_evict.
        assert_eq!(c.cache_remove(&1), Some(ExpiredU8(5)));
        assert_eq!(
            count.load(AOrdering::Relaxed),
            1,
            "on_evict must fire on cache_remove"
        );
        assert_eq!(c.cache_evictions(), Some(1));

        c.set(2, ExpiredU8(15)); // expired
        // Removing an expired entry fires on_evict but returns None.
        assert_eq!(c.cache_remove(&2), None);
        assert_eq!(
            count.load(AOrdering::Relaxed),
            2,
            "on_evict fires even for expired entries"
        );
        assert_eq!(c.cache_evictions(), Some(2));
    }

    #[test]
    fn expiring_cache_get_mut_hit() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(2));
        let v = c.cache_get_mut(&1).expect("should be a cache hit");
        assert_eq!(*v, ExpiredU8(2));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_cache_get_mut_expired() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(15)); // expired
        assert!(c.cache_get_mut(&1).is_none());
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn expiring_cache_get_or_set_with_hit_no_closure() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(5));
        let mut called = false;
        let v = c.cache_get_or_set_with(1, || {
            called = true;
            ExpiredU8(99)
        });
        assert!(!called, "closure must not be called on cache hit");
        assert_eq!(*v, ExpiredU8(5));
        assert_eq!(c.cache_hits(), Some(1));
    }

    #[test]
    fn expiring_cache_get_or_set_with_expired_fires_on_evict() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &u8, _v: &ExpiredU8| {
                fired2.lock().unwrap().push(*k);
            })
            .build()
            .unwrap();
        c.set(1, ExpiredU8(15)); // expired
        let v = c.cache_get_or_set_with(1, || ExpiredU8(3));
        assert_eq!(*v, ExpiredU8(3));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
    }

    #[test]
    fn cache_set_over_expired_returns_none_fires_on_evict_and_counts() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &u8, _v: &ExpiredU8| fired2.lock().unwrap().push(*k))
            .build()
            .unwrap();
        c.set(1, ExpiredU8(15)); // expired (>10)
        // Overwriting an expired value: filtered from the return (None), fires on_evict once,
        // counts one eviction.
        assert_eq!(c.cache_set(1, ExpiredU8(3)), None);
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
        // Overwriting a live value returns it, and does not fire on_evict or count.
        assert_eq!(c.cache_set(1, ExpiredU8(4)), Some(ExpiredU8(3)));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
    }

    #[test]
    fn expiring_cache_try_get_or_set_with_err_keeps_expired() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(15)); // expired
        let result: Result<&ExpiredU8, &str> = c.cache_try_get_or_set_with(1, || Err("fail"));
        assert!(result.is_err());
        assert_eq!(c.cache_size(), 1, "expired entry must remain after Err");
        assert_eq!(c.cache_evictions(), Some(0));
        // miss is counted before f() is called, so it's Some(1) even on Err
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_try_get_or_set_with_ok_evicts_expired() {
        use std::sync::{Arc, Mutex};
        let fired: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &u8, _v: &ExpiredU8| {
                fired2.lock().unwrap().push(*k);
            })
            .build()
            .unwrap();
        c.set(1, ExpiredU8(15)); // expired
        let result: Result<&ExpiredU8, &str> = c.cache_try_get_or_set_with(1, || Ok(ExpiredU8(3)));
        assert_eq!(*result.unwrap(), ExpiredU8(3));
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(fired.lock().unwrap().clone(), vec![1]);
    }

    #[test]
    fn cache_clear_with_on_evict_fires_for_all_entries() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AOrdering},
        };
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.set(1, ExpiredU8(5)); // live
        c.set(2, ExpiredU8(15)); // expired (value > 10)
        c.cache_clear_with_on_evict();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            count.load(AOrdering::Relaxed),
            2,
            "on_evict fires for all entries including expired"
        );
        assert_eq!(c.cache_evictions(), Some(2));
    }

    #[test]
    fn expiring_cache_clear_no_on_evict() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AOrdering},
        };
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.set(1, ExpiredU8(5));
        c.set(2, ExpiredU8(15));
        c.cache_clear();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(
            count.load(AOrdering::Relaxed),
            0,
            "on_evict must not fire on cache_clear"
        );
    }

    #[test]
    fn expiring_cache_reset_clears_metrics_and_entries() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AOrdering},
        };
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, AOrdering::Relaxed);
            })
            .build()
            .unwrap();
        c.set(1, ExpiredU8(5));
        c.get(&1); // 1 hit
        c.cache_reset();
        assert_eq!(c.cache_size(), 0);
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(0));
        assert_eq!(c.cache_evictions(), Some(0));
        assert_eq!(
            count.load(AOrdering::Relaxed),
            0,
            "on_evict must not fire on cache_reset"
        );
    }

    #[test]
    fn expiring_cache_peek_expired_no_metrics_no_removal() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(15)); // expired
        assert!(c.cache_peek(&1).is_none());
        // metrics unchanged
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(0));
        assert_eq!(c.cache_evictions(), Some(0));
        // entry still present (peek does not remove)
        assert_eq!(c.cache_size(), 1);
    }

    #[test]
    fn expiring_cache_peek_live_no_metrics_change() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(5));
        assert_eq!(c.cache_peek(&1), Some(&ExpiredU8(5)));
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[test]
    fn expiring_cache_iter_excludes_expired() {
        use crate::CachedIter;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(5)); // live
        c.set(2, ExpiredU8(15)); // expired
        c.set(3, ExpiredU8(3)); // live
        let mut live: Vec<u8> = CachedIter::iter(&c).map(|(k, _)| *k).collect();
        live.sort();
        assert_eq!(live, vec![1, 3]);
    }

    #[test]
    fn expiring_cache_get_with_expiry_status_hit() {
        use crate::CloneCached;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(5));
        let (val, expired) = c.cache_get_with_expiry_status(&1);
        assert_eq!(val, Some(ExpiredU8(5)));
        assert!(!expired);
        assert_eq!(c.cache_hits(), Some(1));
    }

    #[test]
    fn expiring_cache_get_with_expiry_status_expired() {
        use crate::CloneCached;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(15));
        let (val, expired) = c.cache_get_with_expiry_status(&1);
        assert_eq!(val, Some(ExpiredU8(15)));
        assert!(expired);
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_get_with_expiry_status_miss() {
        use crate::CloneCached;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let (val, expired) = c.cache_get_with_expiry_status(&99u8);
        assert_eq!(val, None);
        assert!(!expired);
        assert_eq!(c.cache_misses(), Some(1));
    }

    #[test]
    fn expiring_cache_debug_format() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(5));
        c.get(&1); // 1 hit
        let s = format!("{:?}", c);
        assert!(s.contains("ExpiringCache"), "missing struct name in Debug");
        assert!(s.contains("hits"), "missing hits field in Debug");
        assert!(s.contains("misses"), "missing misses field in Debug");
        assert!(s.contains("evictions"), "missing evictions field in Debug");
    }

    #[test]
    fn expiring_cache_clone_independent() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.set(1, ExpiredU8(5));
        c.get(&1); // 1 hit
        let mut c2 = c.clone();
        assert_eq!(c2.cache_hits(), Some(1));
        assert_eq!(c2.cache_size(), 1);
        // mutations to c2 don't affect c
        c2.get(&1);
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c2.cache_hits(), Some(2));
    }

    #[test]
    fn expiring_cache_try_build() {
        let result: Result<ExpiringCache<u8, ExpiredU8>, _> =
            ExpiringCache::builder().initial_capacity(10).build();
        assert!(result.is_ok());
        let c = result.unwrap();
        assert_eq!(c.cache_size(), 0);
    }

    /// A key whose `Hash`/`Eq` cover only `label`, so two keys carrying different `payload`s
    /// compare EQUAL and an overwrite has an observable choice of which instance to store.
    #[derive(Clone, Debug)]
    struct CoarseKey {
        label: &'static str,
        payload: u32,
    }

    impl std::hash::Hash for CoarseKey {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.label.hash(state);
        }
    }
    impl PartialEq for CoarseKey {
        fn eq(&self, other: &Self) -> bool {
            self.label == other.label
        }
    }
    impl Eq for CoarseKey {}

    /// `cache_set` over an existing key takes `HashMap`'s native fast path: the value is
    /// replaced in place and the FIRST-inserted key stays stored, so `cache_remove_entry` and
    /// `on_evict` report that key rather than the caller's. Re-keying the slot (an explicit
    /// `remove_entry` + `insert`) would report the last-written payload instead.
    #[test]
    fn cache_set_overwrite_keeps_the_first_stored_key() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let seen2 = seen.clone();
        let mut c: ExpiringCache<CoarseKey, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &CoarseKey, _v: &ExpiredU8| seen2.lock().unwrap().push(k.payload))
            .build()
            .unwrap();
        let first = CoarseKey {
            label: "a",
            payload: 1,
        };
        let second = CoarseKey {
            label: "a",
            payload: 2,
        };
        assert_eq!(first, second, "the two keys compare equal");

        c.cache_set(first, ExpiredU8(1));
        assert_eq!(
            c.cache_set(second.clone(), ExpiredU8(2)),
            Some(ExpiredU8(1))
        );
        assert_eq!(c.cache_size(), 1);

        let (stored, value) = c.cache_remove_entry(&second).expect("present");
        assert_eq!(
            stored.payload, 1,
            "an overwrite keeps the incumbent key, so the first payload is the stored one"
        );
        assert_eq!(value, ExpiredU8(2));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![1u32],
            "the removal callback receives the stored key"
        );
    }

    /// Same fast path when the displaced value had expired: the value is replaced in place,
    /// the eviction is counted and `on_evict` fires with the stored key -- and the key that
    /// remains stored is still the first one.
    #[test]
    fn cache_set_over_expired_keeps_the_first_stored_key() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let seen2 = seen.clone();
        let mut c: ExpiringCache<CoarseKey, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |k: &CoarseKey, _v: &ExpiredU8| seen2.lock().unwrap().push(k.payload))
            .build()
            .unwrap();
        let first = CoarseKey {
            label: "a",
            payload: 1,
        };
        let second = CoarseKey {
            label: "a",
            payload: 2,
        };
        c.cache_set(first, ExpiredU8(20)); // expired: 20 > 10

        assert_eq!(
            c.cache_set(second.clone(), ExpiredU8(2)),
            None,
            "an expired displaced value is filtered from the return"
        );
        assert_eq!(c.cache_evictions(), Some(1));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![1u32],
            "on_evict receives the key that was physically stored"
        );

        let (stored, value) = c.cache_remove_entry(&second).expect("present");
        assert_eq!(
            stored.payload, 1,
            "replacing an expired value still keeps the incumbent key"
        );
        assert_eq!(value, ExpiredU8(2));
    }

    #[test]
    fn cache_remove_entry_returns_some_for_live_entry() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(5)); // not expired: 5 <= 10
        let removed = c.cache_remove_entry(&1u8);
        assert_eq!(removed, Some((1u8, ExpiredU8(5))));
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn cache_remove_entry_returns_some_for_expired_entry() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(20)); // expired: 20 > 10

        // cache_remove returns None for an expired entry.
        assert_eq!(c.cache_remove(&2u8), None);
        c.cache_set(2, ExpiredU8(20));
        assert_eq!(c.cache_remove(&2u8), None);

        // cache_remove_entry returns Some even for an expired entry.
        let removed = c.cache_remove_entry(&1u8);
        assert_eq!(
            removed.expect("cache_remove_entry must return Some for expired entry"),
            (1u8, ExpiredU8(20))
        );
    }

    #[test]
    fn cache_delete_returns_true_for_expired_entry() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(20)); // expired
        assert!(
            c.cache_delete(&1u8),
            "cache_delete must return true even for expired entry"
        );
        assert!(!c.cache_delete(&1u8), "cache_delete false when absent");
    }

    #[test]
    fn cache_remove_entry_fires_on_evict_for_expired() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count2 = count.clone();
        let mut c = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        c.cache_set(1u8, ExpiredU8(20)); // expired

        let _ = c.cache_remove_entry(&1u8);
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "on_evict fires for expired entries"
        );

        let _ = c.cache_remove_entry(&99u8);
        assert_eq!(count.load(Ordering::Relaxed), 1, "no fire for absent key");
    }

    #[test]
    fn cache_remove_entry_with_panicking_on_evict_still_counts_eviction() {
        // The entry is popped and counted BEFORE `on_evict` runs, so a panicking
        // callback must not leave the removed entry uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(|_k: &u8, _v: &ExpiredU8| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u8, ExpiredU8(1)); // live
        let r = catch_unwind(AssertUnwindSafe(|| c.cache_remove_entry(&1u8)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_size(),
            0,
            "entry must still be removed from the store"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn retain_with_panicking_on_evict_still_counts_eviction() {
        // Same invariant on the `retain` path: the predicate closure counts BEFORE
        // notifying, so a panicking callback still leaves the eviction counted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(|_k: &u8, _v: &ExpiredU8| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u8, ExpiredU8(1)); // live
        let r = catch_unwind(AssertUnwindSafe(|| c.retain(|_, _| false)));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn retain_returns_count_folding_expired_and_predicate_rejections() {
        // The returned count must fold together BOTH predicate-rejected entries and
        // entries removed for having already expired, and must agree with the
        // `cache_size()` delta and the number of `on_evict` invocations.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = fired.clone();
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(move |_k: &u8, _v: &ExpiredU8| {
                fired2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        c.cache_set(1, ExpiredU8(11)); // expired (n > 10): swept regardless of predicate
        c.cache_set(2, ExpiredU8(2)); // even, live: kept
        c.cache_set(3, ExpiredU8(3)); // odd, live: rejected by predicate
        c.cache_set(4, ExpiredU8(4)); // even, live: kept

        let size_before = c.cache_size();
        let removed = c.retain(|_, v| v.0 % 2 == 0);
        let size_after = c.cache_size();

        assert_eq!(
            removed, 2,
            "one expired sweep (key 1) + one predicate rejection (key 3)"
        );
        assert_eq!(size_before - size_after, removed);
        assert_eq!(fired.load(Ordering::Relaxed), removed);
        assert!(c.cache_get(&2).is_some());
        assert!(c.cache_get(&3).is_none());
        assert!(c.cache_get(&4).is_some());
    }

    #[test]
    fn cache_get_lazy_sweep_with_panicking_on_evict_still_counts_eviction() {
        // `cache_get` on an expired entry removes it and counts the eviction BEFORE
        // `on_evict` runs, so a panicking callback must not leave it uncounted.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(|_k: &u8, _v: &ExpiredU8| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u8, ExpiredU8(15)); // already expired (>10)
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = c.cache_get(&1u8);
        }));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_size(),
            0,
            "the expired entry must still be swept from the store"
        );
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn cache_set_over_expired_with_panicking_on_evict_still_counts_eviction() {
        // Overwriting an expired entry fires `on_evict` for the displaced value;
        // `cache_set` counts the eviction BEFORE notifying.
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(|_k: &u8, _v: &ExpiredU8| panic!("boom"))
            .build()
            .unwrap();
        c.cache_set(1u8, ExpiredU8(15)); // already expired (>10)
        let r = catch_unwind(AssertUnwindSafe(|| c.cache_set(1u8, ExpiredU8(1))));
        assert!(r.is_err(), "on_evict should have panicked");
        assert_eq!(
            c.cache_evictions(),
            Some(1),
            "eviction must be counted even though on_evict panicked"
        );
    }

    #[test]
    fn cache_remove_entry_absent_returns_none() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        assert_eq!(c.cache_remove_entry(&42u8), None);
    }

    #[test]
    fn cache_remove_entry_increments_eviction_counter() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1u8, ExpiredU8(20)); // expired: value > 10
        let before = c.cache_evictions().expect("evictions are always tracked");
        let _ = c.cache_remove_entry(&1u8); // expired but present — must increment
        let _ = c.cache_remove_entry(&99u8); // absent — must not increment
        assert_eq!(
            c.cache_evictions().expect("evictions are always tracked") - before,
            1,
            "cache_remove_entry must increment evictions for present key only"
        );
    }

    #[test]
    fn eq_same_entries_compare_equal() {
        let mut a: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let mut b: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        a.cache_set(1, ExpiredU8(5));
        a.cache_set(2, ExpiredU8(6));
        // Insert in a different order: HashMap-backed equality is order-independent.
        b.cache_set(2, ExpiredU8(6));
        b.cache_set(1, ExpiredU8(5));
        assert_eq!(
            a, b,
            "caches with the same stored entries must compare equal"
        );
    }

    #[test]
    fn eq_ignores_metrics_and_on_evict() {
        // Equality is over stored entries only: differing hit/miss/eviction
        // counters and an `on_evict` callback on one side must not break it.
        let mut a: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let mut b: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .on_evict(|_k: &u8, _v: &ExpiredU8| {})
            .build()
            .unwrap();
        a.cache_set(1, ExpiredU8(5));
        b.cache_set(1, ExpiredU8(5));
        // Drive `a`'s metrics away from `b`'s.
        a.get(&1);
        a.get(&99);
        assert_ne!(a.cache_hits(), b.cache_hits());
        assert_eq!(
            a, b,
            "metrics and on_evict must not participate in equality"
        );
    }

    #[test]
    fn ne_differing_entries() {
        let mut a: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let mut b: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        a.cache_set(1, ExpiredU8(5));
        b.cache_set(1, ExpiredU8(6)); // same key, different value
        assert_ne!(a, b, "differing values must compare unequal");

        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(5));
        c.cache_set(2, ExpiredU8(5)); // extra key
        assert_ne!(a, c, "differing key sets must compare unequal");

        // An empty cache differs from a populated one and equals another empty one.
        let empty1: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let empty2: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        assert_eq!(empty1, empty2);
        assert_ne!(empty1, a);
    }

    #[test]
    fn builder_initial_capacity_method_exists_and_preallocates() {
        // Verifies the renamed builder method: initial_capacity() sets a preallocation hint.
        let c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder()
            .initial_capacity(32)
            .build()
            .unwrap();
        // The backing store must have at least the requested capacity.
        assert!(c.store.capacity() >= 32);
    }

    #[test]
    fn struct_holds_hashmap_directly_not_an_unbound_cache() {
        // `ExpiringCache` used to wrap a full `UnboundCache` (HashMap + two
        // `StripedCounter`s, each a `Box<[Slot]>` fat-pointer field, plus a dead
        // `on_evict` field) purely for `cache_clear` / `cache_reset` / `cache_size`
        // / `cache_reset_metrics`, while every read/write bypassed it entirely.
        // Now the struct holds a `HashMap` directly (plus an `initial_capacity`
        // hint for `cache_reset`'s shrink behavior).
        //
        // The prior version of this test hard-coded the expected byte size of
        // `Option<usize>` / `AtomicU64` / `Option<Arc<dyn Fn>>` and a padding
        // slack constant. `#[repr(Rust)]` layout (padding, niche optimization,
        // field reordering) is not guaranteed by the language, so that arithmetic
        // could drift on a different toolchain or target and flake without any
        // real regression. Comparing directly against the sibling `UnboundCache`
        // struct (same K/V, built in the same compilation) sidesteps all of that:
        // whatever the concrete field layout is on a given target, `ExpiringCache`
        // holding its own bare `HashMap` + `initial_capacity` + 3x `AtomicU64` +
        // `on_evict` is strictly smaller than `UnboundCache` holding a `HashMap` +
        // 2x `StripedCounter` + `initial_capacity` + `on_evict` (one extra
        // `AtomicU64` costs less than a second `StripedCounter`). If `ExpiringCache`
        // ever again embeds an `UnboundCache` (directly or via a wrapper), its size
        // becomes `UnboundCache`'s size *plus* its own extra fields, which flips
        // this comparison and fails the assertion on any target.
        let expiring = std::mem::size_of::<ExpiringCache<u8, ExpiredU8>>();
        let unbound = std::mem::size_of::<crate::UnboundCache<u8, ExpiredU8>>();
        assert!(
            expiring < unbound,
            "ExpiringCache ({expiring} bytes) must be smaller than UnboundCache \
             ({unbound} bytes) for the same K/V types; equal-or-larger implies \
             ExpiringCache is once again wrapping a full UnboundCache instead of \
             holding a bare HashMap directly"
        );
    }

    #[test]
    fn cache_reset_shrinks_toward_initial_capacity_hint_matching_unbound_cache() {
        // Exercise the `shrink_to(initial_capacity.unwrap_or(0))` branch with a
        // real, non-default hint (the other reset test uses the default builder,
        // so `initial_capacity` is `None` and this branch collapses to
        // `shrink_to(0)`, leaving it unexercised).
        let init_capacity = 4usize;
        let n: u32 = 200;
        let mut c: ExpiringCache<u32, ExpiredU8> = ExpiringCache::builder()
            .initial_capacity(init_capacity)
            .build()
            .unwrap();
        for i in 0..n {
            c.cache_set(i, ExpiredU8(1));
        }
        let grown_capacity = c.store.capacity();
        assert!(
            grown_capacity >= n as usize,
            "sanity: inserting well beyond the hint must have grown the map"
        );

        c.cache_reset();
        assert_eq!(c.cache_size(), 0);
        let reset_capacity = c.store.capacity();
        assert!(
            reset_capacity < grown_capacity,
            "cache_reset must shrink the map back down from the grown capacity \
             ({grown_capacity}), not leave it in place (got {reset_capacity})"
        );
        assert!(
            reset_capacity >= init_capacity,
            "cache_reset must settle near the initial_capacity hint ({init_capacity}), \
             not shrink all the way to 0 (got {reset_capacity})"
        );

        // Equivalence contract: `ExpiringCache::cache_reset` is documented to
        // reproduce `UnboundCache::cache_reset`'s shrink behavior. For the same
        // key type, initial_capacity hint, and insert sequence -- both back onto
        // `HashMap<u32, _, DefaultHashBuilder>` -- they must settle on the exact
        // same capacity.
        let mut u: crate::UnboundCache<u32, ExpiredU8> = crate::UnboundCache::builder()
            .initial_capacity(init_capacity)
            .build()
            .unwrap();
        for i in 0..n {
            u.cache_set(i, ExpiredU8(1));
        }
        u.cache_reset();
        assert_eq!(
            reset_capacity,
            u.store.capacity(),
            "ExpiringCache::cache_reset must settle on the same capacity as \
             UnboundCache::cache_reset for the same initial_capacity hint"
        );
    }

    #[test]
    fn clone_preserves_initial_capacity_hint_for_reset() {
        // `Clone` now also copies `initial_capacity`; verify that copy is not
        // just structural but actually drives the clone's own `cache_reset`
        // shrink behavior, and that the clone remains independent of the
        // original (mutating/resetting the clone does not affect the source).
        let init_capacity = 8usize;
        let n: u32 = 100;
        let mut c: ExpiringCache<u32, ExpiredU8> = ExpiringCache::builder()
            .initial_capacity(init_capacity)
            .build()
            .unwrap();
        for i in 0..n {
            c.cache_set(i, ExpiredU8(1));
        }
        let mut clone = c.clone();
        assert_eq!(clone.cache_size(), n as usize);

        let grown_capacity = clone.store.capacity();
        clone.cache_reset();
        assert_eq!(clone.cache_size(), 0);
        assert!(
            clone.store.capacity() < grown_capacity,
            "clone must shrink on reset just like the original would"
        );
        assert!(
            clone.store.capacity() >= init_capacity,
            "clone must carry its own initial_capacity hint after Clone, not \
             default to shrinking all the way to 0"
        );
        // The original is untouched by resetting the clone.
        assert_eq!(c.cache_size(), n as usize);
    }

    #[test]
    fn cache_size_includes_expired_but_iter_excludes_and_evict_removes_them() {
        // Pins the documented `cache_size` / `iter` / `evict` contract: `cache_size`
        // is the raw stored-entry count (including expired-but-unswept entries),
        // `iter()` filters expired entries from the view without removing them,
        // and `evict()` is the only one of the three that physically removes them.
        use crate::{CacheEvict, CachedIter};
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(5)); // live
        c.cache_set(2, ExpiredU8(15)); // expired, never re-accessed so stays physically present
        c.cache_set(3, ExpiredU8(20)); // expired

        assert_eq!(
            c.cache_size(),
            3,
            "cache_size includes unswept expired entries"
        );
        assert_eq!(
            CachedIter::iter(&c).count(),
            1,
            "iter excludes expired entries"
        );
        assert_eq!(
            c.cache_size(),
            3,
            "iter must not physically remove anything"
        );

        let removed = CacheEvict::evict(&mut c);
        assert_eq!(removed, 2, "evict must sweep both expired entries");
        assert_eq!(c.cache_size(), 1);
        assert_eq!(CachedIter::iter(&c).count(), 1);
    }

    #[test]
    fn cache_get_or_set_with_miss_inserts_value_and_counts() {
        // The Vacant arm of `cache_get_or_set_with_mut` (a plain cache miss on an
        // absent key) has no direct coverage elsewhere in this module -- existing
        // tests only exercise the Occupied (hit / expired) arms.
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let mut called = false;
        let v = c.cache_get_or_set_with(1, || {
            called = true;
            ExpiredU8(9)
        });
        assert!(called, "closure must run on cache miss");
        assert_eq!(*v, ExpiredU8(9));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_hits(), Some(0));
        assert_eq!(c.cache_size(), 1);
        assert_eq!(
            c.cache_peek(&1),
            Some(&ExpiredU8(9)),
            "the value must actually be stored, not just returned transiently"
        );
    }

    #[test]
    fn cache_try_get_or_set_with_miss_ok_inserts_value() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let result: Result<&ExpiredU8, &str> = c.cache_try_get_or_set_with(1, || Ok(ExpiredU8(7)));
        assert_eq!(*result.unwrap(), ExpiredU8(7));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_size(), 1);
    }

    #[test]
    fn cache_try_get_or_set_with_miss_err_inserts_nothing() {
        // The Vacant + Err arm: `f()?` short-circuits before `vacant.insert` runs,
        // so a failing factory on an absent key must leave the cache empty.
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let result: Result<&ExpiredU8, &str> = c.cache_try_get_or_set_with(1, || Err("boom"));
        assert!(result.is_err());
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(
            c.cache_size(),
            0,
            "a failing factory on a vacant key must not insert anything"
        );
    }

    #[cfg(feature = "async_core")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_hit_does_not_call_factory() {
        // The Occupied + live arm of `async_cache_get_or_set_with_mut` has no
        // coverage anywhere in the crate; only the Occupied + expired arm is
        // covered (tests/v3_expiring_evict_order.rs).
        use crate::CachedGetOrSetAsync;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering as AOrdering};
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(5)); // live
        let called = Arc::new(AtomicBool::new(false));
        let called2 = called.clone();
        let v = c
            .async_cache_get_or_set_with_mut(1, move || async move {
                called2.store(true, AOrdering::Relaxed);
                ExpiredU8(99)
            })
            .await;
        assert!(
            !called.load(AOrdering::Relaxed),
            "factory must not run on cache hit"
        );
        assert_eq!(*v, ExpiredU8(5));
        assert_eq!(c.cache_hits(), Some(1));
        assert_eq!(c.cache_misses(), Some(0));
    }

    #[cfg(feature = "async_core")]
    #[tokio::test]
    async fn async_cache_get_or_set_with_miss_inserts_value_and_counts() {
        // The Vacant arm has no coverage anywhere in the crate.
        use crate::CachedGetOrSetAsync;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let v = c
            .async_cache_get_or_set_with_mut(1, || async { ExpiredU8(9) })
            .await;
        assert_eq!(*v, ExpiredU8(9));
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_size(), 1);
    }

    #[cfg(feature = "async_core")]
    #[tokio::test]
    async fn async_cache_try_get_or_set_with_miss_err_inserts_nothing() {
        // The Vacant + Err arm has no coverage anywhere in the crate.
        use crate::CachedGetOrSetAsync;
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        let result: Result<&mut ExpiredU8, &str> = c
            .async_cache_try_get_or_set_with_mut(1, || async { Err("boom") })
            .await;
        assert!(result.is_err());
        assert_eq!(c.cache_misses(), Some(1));
        assert_eq!(c.cache_size(), 0);
    }

    // --- CacheExpiry::cache_peek_expires_at ---

    /// A value type that overrides `expires_at` with a concrete deadline.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct TimedValue {
        deadline: crate::time::Instant,
    }

    impl Expires for TimedValue {
        fn is_expired(&self) -> bool {
            crate::time::Instant::now() >= self.deadline
        }

        fn expires_at(&self) -> Option<crate::time::Instant> {
            Some(self.deadline)
        }
    }

    /// A value type whose `is_expired` reports live while `expires_at` (advisory)
    /// reports a deadline already in the past, pinning that the two may disagree.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct LiveDespitePastDeadline {
        past: crate::time::Instant,
    }

    impl Expires for LiveDespitePastDeadline {
        fn is_expired(&self) -> bool {
            false
        }

        fn expires_at(&self) -> Option<crate::time::Instant> {
            Some(self.past)
        }
    }

    /// A value type whose `is_expired` reports EXPIRED while `expires_at` (advisory)
    /// reports a deadline still in the future -- the other direction of disagreement
    /// from [`LiveDespitePastDeadline`]. Pins that `cache_peek_expires_at` surfaces the
    /// advisory deadline unreconciled even when it contradicts `is_expired` by claiming
    /// the entry is still good.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct ExpiredDespiteFutureDeadline {
        future: crate::time::Instant,
    }

    impl Expires for ExpiredDespiteFutureDeadline {
        fn is_expired(&self) -> bool {
            true
        }

        fn expires_at(&self) -> Option<crate::time::Instant> {
            Some(self.future)
        }
    }

    #[test]
    fn peek_expires_at_absent_key_returns_none_none() {
        let c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        assert_eq!(c.cache_peek_expires_at(&1u8), (None, None));
    }

    #[test]
    fn peek_expires_at_alias_agrees_with_required_method() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(2));
        assert_eq!(
            c.peek_expires_at(&1u8),
            c.cache_peek_expires_at(&1u8),
            "the alias must agree with the required method"
        );
    }

    #[test]
    fn peek_expires_at_alias_agrees_with_required_method_across_all_return_shapes() {
        // The trait docs enumerate four return shapes: (None, None) absent,
        // (Some(v), None) no known deadline, (Some(v), Some(t)) with t in the future,
        // and (Some(v), Some(t)) with t in the past. The alias must agree with the
        // required method on every one of them, not just the first shape exercised
        // above.
        let mut c: ExpiringCache<u8, TimedValue> = ExpiringCache::builder().build().unwrap();

        // Shape 1: (None, None) -- absent key.
        assert_eq!(c.peek_expires_at(&1u8), c.cache_peek_expires_at(&1u8));
        assert_eq!(c.peek_expires_at(&1u8), (None, None));

        // Shape 2: (Some(v), Some(t)) with t in the future -- present, live, deadline known.
        let future = crate::time::Instant::now() + std::time::Duration::from_secs(60);
        c.cache_set(1, TimedValue { deadline: future });
        assert_eq!(c.peek_expires_at(&1u8), c.cache_peek_expires_at(&1u8));
        assert_eq!(
            c.peek_expires_at(&1u8),
            (Some(TimedValue { deadline: future }), Some(future))
        );

        // Shape 3: (Some(v), Some(t)) with t in the past -- deadline known but stale.
        let past = crate::time::Instant::now() - std::time::Duration::from_secs(60);
        c.cache_set(2, TimedValue { deadline: past });
        assert_eq!(c.peek_expires_at(&2u8), c.cache_peek_expires_at(&2u8));
        assert_eq!(
            c.peek_expires_at(&2u8),
            (Some(TimedValue { deadline: past }), Some(past))
        );

        // Shape 4: (Some(v), None) -- present, no known deadline.
        let mut d: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        d.cache_set(1, ExpiredU8(2));
        assert_eq!(d.peek_expires_at(&1u8), d.cache_peek_expires_at(&1u8));
        assert_eq!(d.peek_expires_at(&1u8), (Some(ExpiredU8(2)), None));
    }

    #[test]
    fn peek_expires_at_value_overriding_expires_at_returns_its_deadline() {
        let deadline = crate::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut c: ExpiringCache<u8, TimedValue> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, TimedValue { deadline });

        let (value, expires_at) = c.cache_peek_expires_at(&1u8);
        assert_eq!(value, Some(TimedValue { deadline }));
        assert_eq!(
            expires_at,
            Some(deadline),
            "the reported deadline must be the one the value reports"
        );
    }

    #[test]
    fn peek_expires_at_value_not_overriding_expires_at_returns_no_deadline() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(2)); // live: is_expired() is false
        assert_eq!(
            c.cache_peek_expires_at(&1u8),
            (Some(ExpiredU8(2)), None),
            "a value type that does not override expires_at must report no deadline"
        );
    }

    #[test]
    fn peek_expires_at_expired_entry_without_override_returns_no_deadline() {
        // Pins the documented caveat: `None` does not imply live. The entry IS
        // expired (is_expired() == true) but the value type never overrode
        // expires_at, so the advisory deadline is still None, and the entry is
        // kept (not removed) by the peek.
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(99)); // 99 > 10, so is_expired() is true

        let (value, expires_at) = c.cache_peek_expires_at(&1u8);
        assert_eq!(
            value,
            Some(ExpiredU8(99)),
            "an expired entry is still returned"
        );
        assert_eq!(
            expires_at, None,
            "None on this store does not mean live: the value never tracked a deadline"
        );
        assert_eq!(
            c.cache_peek_with_expiry_status(&1u8),
            (Some(ExpiredU8(99)), true),
            "is_expired, not expires_at, remains the authority on liveness"
        );
        assert_eq!(c.cache_size(), 1, "the peek must not remove the entry");
    }

    #[test]
    fn peek_expires_at_advisory_past_deadline_survives_while_is_expired_reports_live() {
        // Pins the documented caveat in the other direction: the advisory deadline
        // can be in the past for an entry is_expired() reports as live, and
        // cache_peek_expires_at must surface that stale deadline unchanged rather
        // than reconciling it against is_expired().
        let past = crate::time::Instant::now() - std::time::Duration::from_secs(3600);
        let mut c: ExpiringCache<u8, LiveDespitePastDeadline> =
            ExpiringCache::builder().build().unwrap();
        c.cache_set(1, LiveDespitePastDeadline { past });

        let (value, expires_at) = c.cache_peek_expires_at(&1u8);
        assert_eq!(value, Some(LiveDespitePastDeadline { past }));
        assert_eq!(
            expires_at,
            Some(past),
            "the advisory deadline must be surfaced unchanged, even though it is in the past"
        );
        assert_eq!(
            c.cache_peek_with_expiry_status(&1u8),
            (Some(LiveDespitePastDeadline { past }), false),
            "cache_peek_with_expiry_status must still report the entry as live"
        );
    }

    #[test]
    fn peek_expires_at_advisory_future_deadline_survives_while_is_expired_reports_expired() {
        // The other direction of disagreement from the past-deadline-while-live test
        // above: a future advisory deadline while is_expired() reports EXPIRED.
        // cache_peek_expires_at must surface the future deadline unreconciled, but
        // is_expired must still be the authority the store itself acts on -- a real
        // access (cache_get) must treat the entry as gone despite the future-looking
        // advisory deadline.
        let future = crate::time::Instant::now() + std::time::Duration::from_secs(3600);
        let mut c: ExpiringCache<u8, ExpiredDespiteFutureDeadline> =
            ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredDespiteFutureDeadline { future });

        let (value, expires_at) = c.cache_peek_expires_at(&1u8);
        assert_eq!(value, Some(ExpiredDespiteFutureDeadline { future }));
        assert_eq!(
            expires_at,
            Some(future),
            "the advisory deadline must be surfaced unchanged, even though it is in the future"
        );
        assert_eq!(
            c.cache_peek_with_expiry_status(&1u8),
            (Some(ExpiredDespiteFutureDeadline { future }), true),
            "cache_peek_with_expiry_status must still report the entry as expired"
        );
        assert_eq!(c.cache_size(), 1, "the peek must not remove the entry");

        // The store's real read path must obey is_expired, not the advisory deadline.
        assert_eq!(
            c.cache_get(&1u8),
            None,
            "is_expired remains the authority the store acts on, regardless of a \
             future-looking advisory deadline"
        );
        assert_eq!(
            c.cache_size(),
            0,
            "the expired entry must be swept on the real access"
        );
    }

    #[test]
    fn peek_expires_at_reflects_new_deadline_after_overwrite() {
        // Overwriting a key with a value carrying a different advisory deadline must
        // not leave the old deadline visible: the store re-reads the currently stored
        // value on every peek rather than caching a stale expires_at snapshot.
        let first_deadline = crate::time::Instant::now() + std::time::Duration::from_secs(60);
        let second_deadline = crate::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut c: ExpiringCache<u8, TimedValue> = ExpiringCache::builder().build().unwrap();

        c.cache_set(
            1,
            TimedValue {
                deadline: first_deadline,
            },
        );
        assert_eq!(
            c.cache_peek_expires_at(&1u8),
            (
                Some(TimedValue {
                    deadline: first_deadline
                }),
                Some(first_deadline)
            )
        );

        c.cache_set(
            1,
            TimedValue {
                deadline: second_deadline,
            },
        );
        assert_eq!(
            c.cache_peek_expires_at(&1u8),
            (
                Some(TimedValue {
                    deadline: second_deadline
                }),
                Some(second_deadline)
            ),
            "an overwrite must replace the visible deadline, not retain the old one"
        );
    }

    #[test]
    fn peek_expires_at_reports_absent_after_evict_removes_the_entry() {
        // The peek deliberately keeps an expired entry, so "expired" and "gone" must
        // stay distinguishable: once `evict()` physically removes it, the same peek
        // must report (None, None) rather than the value it kept before.
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(99)); // 99 > 10, so is_expired() is true

        assert_eq!(
            c.cache_peek_expires_at(&1u8),
            (Some(ExpiredU8(99)), None),
            "the expired entry is still stored before the sweep"
        );

        assert_eq!(
            c.evict(),
            1,
            "evict must physically remove the expired entry"
        );
        assert_eq!(
            c.cache_peek_expires_at(&1u8),
            (None, None),
            "a physically removed entry must be reported as absent"
        );
        assert_eq!(c.cache_size(), 0);
    }

    #[test]
    fn peek_expires_at_reports_absent_after_cache_remove() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(2));
        assert_eq!(c.cache_remove(&1u8), Some(ExpiredU8(2)));

        assert_eq!(c.cache_peek_expires_at(&1u8), (None, None));
        assert_eq!(
            c.peek_expires_at(&1u8),
            (None, None),
            "the alias must agree on the removed key too"
        );
    }

    #[test]
    fn peek_expires_at_does_not_touch_hit_or_miss_counters() {
        let mut c: ExpiringCache<u8, ExpiredU8> = ExpiringCache::builder().build().unwrap();
        c.cache_set(1, ExpiredU8(2));
        let hits = c.cache_hits();
        let misses = c.cache_misses();

        let _ = c.cache_peek_expires_at(&1u8); // present
        let _ = c.cache_peek_expires_at(&2u8); // absent

        assert_eq!(c.cache_hits(), hits, "a peek must not count a hit");
        assert_eq!(c.cache_misses(), misses, "a peek must not count a miss");
    }
}
