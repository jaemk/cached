use super::{CacheEvict, Cached, DefaultHashBuilder, Expires, UnboundCache};
use crate::{CachedIter, CachedPeek, CloneCached};
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
/// (or `ExpiringLruCache` when `size` is also specified).
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
/// with a `size` bound to cap memory usage automatically.
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
    pub(super) store: UnboundCache<K, V, S>,
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
            Some(cap) => UnboundCache::builder()
                .initial_capacity(cap)
                .hasher(self.hasher)
                .build()
                .expect("infallible"),
            None => UnboundCache::builder()
                .hasher(self.hasher)
                .build()
                .expect("infallible"),
        };
        Ok(ExpiringCache {
            store,
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
        let on_evict = &self.on_evict;
        let evictions = &self.evictions;
        let mut removed = 0;
        self.store.store.retain(|key, value| {
            if value.is_expired() {
                if let Some(on_evict) = on_evict {
                    on_evict(key, value);
                }
                evictions.fetch_add(1, Ordering::Relaxed);
                removed += 1;
                false
            } else {
                true
            }
        });
        removed
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
        let entries: Vec<(K, V)> = self.store.store.drain().collect();
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
        // in stable Rust because returning `&'1 V` from inside an `if let` block ties the
        // borrow to lifetime `'1`, which prevents `remove_entry` (a mutable borrow) even on
        // the non-returning path. Polonius (nightly) would fix this.
        match self.store.store.get(k).map(|v| v.is_expired()) {
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Some(true) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.store.remove_entry(k) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
            Some(false) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.store.store.get(k)
            }
        }
    }

    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        // Two lookups on the hit path for the same reason as `cache_get` (NLL limitation).
        match self.store.store.get(k).map(|v| v.is_expired()) {
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Some(true) => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                if let Some((key, old)) = self.store.store.remove_entry(k) {
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(&key, &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None
            }
            Some(false) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.store.store.get_mut(k)
            }
        }
    }

    fn cache_get_or_set_with_mut<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V {
        match self.store.store.entry(k) {
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
                    //
                    // Fire on_evict while the old entry is still present in the map
                    // slot, matching TtlCache ordering (compute value, fire callback,
                    // then insert). A callback that peeks the cache sees the old value.
                    let new_val = f();
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), occupied.get());
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    occupied.insert(new_val);
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
        match self.store.store.entry(k) {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                if !occupied.get().is_expired() {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    Ok(occupied.into_mut())
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    // Same ordering fix as cache_get_or_set_with_mut: compute,
                    // fire on_evict while old entry is still in the map, then insert.
                    let new_val = f()?;
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), occupied.get());
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    occupied.insert(new_val);
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
        match self.store.store.entry(k) {
            Entry::Occupied(mut occupied) => {
                let old = occupied.insert(v);
                if old.is_expired() {
                    // The previous value had expired, so it is filtered from the return
                    // (matching `cache_remove`); fire `on_evict` and count an eviction so the
                    // silently-dropped value is cleaned up like every other removal path.
                    if let Some(on_evict) = &self.on_evict {
                        on_evict(occupied.key(), &old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
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
        self.cache_remove_entry(k)
            .and_then(|(_, v)| if v.is_expired() { None } else { Some(v) })
    }

    /// Removes the entry and returns it **regardless of expiry** (unlike
    /// [`cache_remove`](Cached::cache_remove), which filters expired values).
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if let Some((stored_k, v)) = self.store.store.remove_entry(k) {
            if let Some(on_evict) = &self.on_evict {
                on_evict(&stored_k, &v);
            }
            self.evictions.fetch_add(1, Ordering::Relaxed);
            Some((stored_k, v))
        } else {
            None
        }
    }

    fn cache_clear(&mut self) {
        self.store.cache_clear();
    }

    fn cache_reset(&mut self) {
        self.store.cache_reset();
        self.cache_reset_metrics();
    }

    fn cache_size(&self) -> usize {
        self.store.cache_size()
    }

    fn cache_capacity(&self) -> Option<usize> {
        None
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
        self.store.cache_reset_metrics();
    }
}

impl<K: Hash + Eq, V: Expires, S: BuildHasher> CachedIter<K, V> for ExpiringCache<K, V, S> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: 'a,
        V: 'a,
    {
        self.store
            .store
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
        self.store.store.get(key).and_then(|value| {
            if value.is_expired() {
                None
            } else {
                Some(value)
            }
        })
    }
}

#[cfg(feature = "async_core")]
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
            match self.store.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if !occupied.get().is_expired() {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        // Same ordering fix: compute, fire on_evict while old entry
                        // is still in the map slot, then insert.
                        let new_val = f().await;
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), occupied.get());
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        occupied.insert(new_val);
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
            let v = match self.store.store.entry(k) {
                Entry::Occupied(mut occupied) => {
                    if !occupied.get().is_expired() {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        occupied.into_mut()
                    } else {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        // Same ordering fix: compute, fire on_evict while old entry
                        // is still in the map slot, then insert.
                        let new_val = f().await?;
                        if let Some(on_evict) = &self.on_evict {
                            on_evict(occupied.key(), occupied.get());
                        }
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        occupied.insert(new_val);
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
        if let Some(value) = self.store.store.get(k) {
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
        if let Some(value) = self.store.store.get(k) {
            let expired = value.is_expired();
            (Some(value.clone()), expired)
        } else {
            (None, false)
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
        assert!(c.store.store.capacity() >= 32);
    }
}
